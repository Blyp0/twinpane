//! File operations with a safety net.
//!
//! Design rules:
//! * Nothing is ever permanently deleted by a normal operation. Deletes,
//!   overwritten files and undone copies are moved into a *holding area* so
//!   they stay recoverable. Only [`Engine::empty_holding`] destroys data, and
//!   it is a separate, explicit call.
//! * A batch never aborts on one bad item: failures are reported per item.
//! * Every successful change is recorded in the undo log.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{io_err, Error, Result};
use crate::undo::{Action, Transaction, UndoLog};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictPolicy {
    /// Report an error for the item.
    Fail,
    /// Leave the existing item alone.
    Skip,
    /// Replace the existing item. The old one is kept in the holding area.
    Overwrite,
    /// Keep both: the new item gets a name like `file (2).txt`.
    KeepBoth,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OpReport {
    /// Resulting paths of items that were processed.
    pub done: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub errors: Vec<(PathBuf, String)>,
}

impl OpReport {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

enum Outcome {
    Done(PathBuf),
    Skipped(String),
}

pub struct Engine {
    holding: PathBuf,
    undo: UndoLog,
}

fn bad_name(name: &str, why: &str) -> Error {
    Error::InvalidName(format!("{name:?}: {why}"))
}

/// Check that `name` is a single, legal path component.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(bad_name(name, "name is empty"));
    }
    if name == "." || name == ".." {
        return Err(bad_name(name, "reserved name"));
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c == '\0') {
        return Err(bad_name(name, "contains a path separator"));
    }
    #[cfg(windows)]
    {
        if name.chars().any(|c| "<>:\"|?*".contains(c)) {
            return Err(bad_name(name, "contains a character Windows forbids"));
        }
    }
    Ok(())
}

pub(crate) fn exists_nofollow(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

fn unique_path(target: &Path, is_dir: bool) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new(""));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = if is_dir {
        (name.clone(), String::new())
    } else {
        match Path::new(&name).extension() {
            Some(e) => {
                let ext = format!(".{}", e.to_string_lossy());
                let stem = name[..name.len() - ext.len()].to_string();
                (stem, ext)
            }
            None => (name.clone(), String::new()),
        }
    };
    let mut n = 2;
    loop {
        let candidate = parent.join(format!("{stem} ({n}){ext}"));
        if !exists_nofollow(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn is_cross_device(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(18) // EXDEV
    }
    #[cfg(windows)]
    {
        e.raw_os_error() == Some(17) // ERROR_NOT_SAME_DEVICE
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = fs::read_link(src).map_err(|e| io_err(src, e))?;
    std::os::unix::fs::symlink(target, dst).map_err(|e| io_err(dst, e))
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, _dst: &Path) -> Result<()> {
    Err(Error::Other(format!(
        "copying symbolic links is not supported on this platform yet: {}",
        src.display()
    )))
}

fn preserve_mtime(meta: &fs::Metadata, dst: &Path) {
    if let Ok(m) = meta.modified() {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(dst) {
            let _ = f.set_modified(m);
        }
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src).map_err(|e| io_err(src, e))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        copy_symlink(src, dst)
    } else if ft.is_dir() {
        fs::create_dir(dst).map_err(|e| io_err(dst, e))?;
        for entry in fs::read_dir(src).map_err(|e| io_err(src, e))? {
            let entry = entry.map_err(|e| io_err(src, e))?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
        // Apply permissions last so a read-only source dir does not block us.
        let _ = fs::set_permissions(dst, meta.permissions());
        Ok(())
    } else {
        fs::copy(src, dst).map_err(|e| io_err(src, e))?;
        preserve_mtime(&meta, dst);
        Ok(())
    }
}

fn remove_recursive(p: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(p).map_err(|e| io_err(p, e))?;
    if meta.is_dir() {
        fs::remove_dir_all(p).map_err(|e| io_err(p, e))
    } else {
        fs::remove_file(p).map_err(|e| io_err(p, e))
    }
}

/// Move `src` to `dst` (which must not exist). Falls back to copy-then-remove
/// across file systems; if the copy fails the partial copy is cleaned up and
/// the source is left untouched.
fn move_path(src: &Path, dst: &Path) -> Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            if let Err(err) = copy_recursive(src, dst) {
                let _ = remove_recursive(dst);
                return Err(err);
            }
            remove_recursive(src)
        }
        Err(e) => Err(io_err(src, e)),
    }
}

/// Is `src` already located directly inside `dest_canon`?
fn same_location(src: &Path, dest_canon: &Path) -> bool {
    let parent = match src.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => return false,
    };
    fs::canonicalize(parent)
        .map(|p| p == dest_canon)
        .unwrap_or(false)
}

impl Engine {
    /// Create an engine whose holding area lives at `holding_dir`
    /// (created if missing). Keep it on the same drive as your files for
    /// instant moves.
    pub fn new(holding_dir: impl AsRef<Path>) -> Result<Engine> {
        Engine::with_undo_log(holding_dir, UndoLog::new())
    }

    pub fn with_undo_log(holding_dir: impl AsRef<Path>, undo: UndoLog) -> Result<Engine> {
        let dir = holding_dir.as_ref();
        fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        let holding = fs::canonicalize(dir).map_err(|e| io_err(dir, e))?;
        Ok(Engine { holding, undo })
    }

    pub fn holding_dir(&self) -> &Path {
        &self.holding
    }

    pub fn undo_log(&self) -> &UndoLog {
        &self.undo
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// PERMANENTLY delete everything in the holding area. Because held items
    /// are gone afterwards, the undo log is cleared too.
    pub fn empty_holding(&mut self) -> Result<()> {
        fs::remove_dir_all(&self.holding).map_err(|e| io_err(&self.holding, e))?;
        fs::create_dir_all(&self.holding).map_err(|e| io_err(&self.holding, e))?;
        self.undo.clear();
        Ok(())
    }

    /// Move `path` into the holding area; returns where it now lives.
    fn hold(&self, path: &Path, txn_id: u64, tag: &str) -> Result<PathBuf> {
        if !is_symlink(path) {
            if let Ok(pc) = fs::canonicalize(path) {
                if self.holding.starts_with(&pc) {
                    return Err(Error::Other(format!(
                        "cannot move {} because it contains the holding area",
                        path.display()
                    )));
                }
            }
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".to_string());
        let dir = self.holding.join(format!("{txn_id:08}"));
        fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let mut held = dir.join(format!("{tag}_{name}"));
        if exists_nofollow(&held) {
            held = unique_path(&held, false);
        }
        move_path(path, &held)?;
        Ok(held)
    }

    fn commit(&mut self, id: u64, description: String, actions: Vec<Action>) {
        if actions.is_empty() {
            // Nothing happened; tidy the (empty) holding sub-folder.
            let _ = fs::remove_dir(self.holding.join(format!("{id:08}")));
            return;
        }
        self.undo.push(Transaction {
            id,
            description,
            actions,
        });
    }

    // ---- copy / move -----------------------------------------------------

    pub fn copy(
        &mut self,
        sources: &[PathBuf],
        dest_dir: &Path,
        policy: ConflictPolicy,
    ) -> Result<OpReport> {
        self.transfer(sources, dest_dir, policy, false)
    }

    pub fn move_items(
        &mut self,
        sources: &[PathBuf],
        dest_dir: &Path,
        policy: ConflictPolicy,
    ) -> Result<OpReport> {
        self.transfer(sources, dest_dir, policy, true)
    }

    fn transfer(
        &mut self,
        sources: &[PathBuf],
        dest_dir: &Path,
        policy: ConflictPolicy,
        is_move: bool,
    ) -> Result<OpReport> {
        if !dest_dir.is_dir() {
            return Err(Error::NotFound(dest_dir.to_path_buf()));
        }
        let dest_canon = fs::canonicalize(dest_dir).map_err(|e| io_err(dest_dir, e))?;
        let id = self.undo.allocate_id();
        let mut report = OpReport::default();
        let mut actions: Vec<Action> = Vec::new();

        for (idx, src) in sources.iter().enumerate() {
            match self.transfer_one(src, dest_dir, &dest_canon, policy, is_move, id, idx, &mut actions)
            {
                Ok(Outcome::Done(p)) => report.done.push(p),
                Ok(Outcome::Skipped(msg)) => report.skipped.push((src.clone(), msg)),
                Err(e) => report.errors.push((src.clone(), e.to_string())),
            }
        }

        let verb = if is_move { "Move" } else { "Copy" };
        let desc = format!("{verb} {} item(s) to {}", report.done.len(), dest_dir.display());
        self.commit(id, desc, actions);
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn transfer_one(
        &mut self,
        src: &Path,
        dest_dir: &Path,
        dest_canon: &Path,
        policy: ConflictPolicy,
        is_move: bool,
        id: u64,
        idx: usize,
        actions: &mut Vec<Action>,
    ) -> Result<Outcome> {
        let meta = fs::symlink_metadata(src).map_err(|_| Error::NotFound(src.to_path_buf()))?;
        let name = src
            .file_name()
            .ok_or_else(|| Error::InvalidName(src.display().to_string()))?;
        let mut target = dest_dir.join(name);

        if meta.is_dir() {
            let src_canon = fs::canonicalize(src).map_err(|e| io_err(src, e))?;
            if dest_canon.starts_with(&src_canon) {
                return Err(Error::IntoSelf(src.to_path_buf()));
            }
        }

        let mut held_for_overwrite: Option<PathBuf> = None;
        if exists_nofollow(&target) {
            if same_location(src, dest_canon) {
                if is_move {
                    return Ok(Outcome::Skipped("already in the destination".into()));
                }
                if policy != ConflictPolicy::KeepBoth {
                    return Ok(Outcome::Skipped(
                        "source and destination are the same".into(),
                    ));
                }
            }
            match policy {
                ConflictPolicy::Fail => return Err(Error::AlreadyExists(target)),
                ConflictPolicy::Skip => {
                    return Ok(Outcome::Skipped("destination already exists".into()))
                }
                ConflictPolicy::Overwrite => {
                    if let (Ok(sc), Ok(tc)) = (fs::canonicalize(src), fs::canonicalize(&target)) {
                        if sc != tc && sc.starts_with(&tc) {
                            return Err(Error::Other(
                                "the source is inside the item it would replace".into(),
                            ));
                        }
                    }
                    let held = self.hold(&target, id, &format!("h{idx}"))?;
                    actions.push(Action::Trashed {
                        original: target.clone(),
                        held: held.clone(),
                    });
                    held_for_overwrite = Some(held);
                }
                ConflictPolicy::KeepBoth => {
                    target = unique_path(&target, meta.is_dir());
                }
            }
        }

        let result = if is_move {
            move_path(src, &target)
        } else {
            copy_recursive(src, &target).map_err(|e| {
                // `target` did not exist before this call, so this only
                // removes our own partial copy.
                let _ = remove_recursive(&target);
                e
            })
        };

        if let Err(e) = result {
            // Put the replaced item back so a failed overwrite loses nothing.
            if let Some(held) = held_for_overwrite {
                if move_path(&held, &target).is_ok() {
                    actions.pop();
                }
            }
            return Err(e);
        }

        if is_move {
            actions.push(Action::Moved {
                from: src.to_path_buf(),
                to: target.clone(),
            });
        } else {
            actions.push(Action::Copied {
                created: target.clone(),
            });
        }
        Ok(Outcome::Done(target))
    }

    // ---- delete / rename / mkdir ----------------------------------------

    /// "Delete": move items into the holding area. Fully undoable.
    pub fn delete(&mut self, paths: &[PathBuf]) -> Result<OpReport> {
        let id = self.undo.allocate_id();
        let mut report = OpReport::default();
        let mut actions = Vec::new();
        for (idx, p) in paths.iter().enumerate() {
            if !exists_nofollow(p) {
                report
                    .errors
                    .push((p.clone(), Error::NotFound(p.clone()).to_string()));
                continue;
            }
            match self.hold(p, id, &format!("h{idx}")) {
                Ok(held) => {
                    actions.push(Action::Trashed {
                        original: p.clone(),
                        held,
                    });
                    report.done.push(p.clone());
                }
                Err(e) => report.errors.push((p.clone(), e.to_string())),
            }
        }
        let desc = format!("Delete {} item(s)", report.done.len());
        self.commit(id, desc, actions);
        Ok(report)
    }

    /// Rename `path` to `new_name` (a single name, not a path).
    /// Case-only renames work on case-insensitive file systems too.
    pub fn rename(&mut self, path: &Path, new_name: &str) -> Result<PathBuf> {
        validate_name(new_name)?;
        if !exists_nofollow(path) {
            return Err(Error::NotFound(path.to_path_buf()));
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let target = parent.join(new_name);
        if target == path {
            return Ok(target);
        }
        let id = self.undo.allocate_id();
        rename_checked(path, &target, id)?;
        self.commit(
            id,
            format!("Rename {} to {new_name}", display_name(path)),
            vec![Action::Renamed {
                from: path.to_path_buf(),
                to: target.clone(),
            }],
        );
        Ok(target)
    }

    /// Rename several items at once (`(from, to)` pairs in the same or
    /// different folders). Safe for swaps like a<->b. All-or-nothing, and one
    /// undo step.
    pub fn rename_many(&mut self, pairs: &[(PathBuf, PathBuf)], description: &str) -> Result<()> {
        for (_, to) in pairs {
            if let Some(n) = to.file_name() {
                validate_name(&n.to_string_lossy())?;
            }
        }
        let effective: Vec<(PathBuf, PathBuf)> =
            pairs.iter().filter(|(f, t)| f != t).cloned().collect();
        if effective.is_empty() {
            return Ok(());
        }
        let id = self.undo.allocate_id();
        rename_set(&effective, id)?;
        self.commit(
            id,
            description.to_string(),
            vec![Action::RenameBatch { pairs: effective }],
        );
        Ok(())
    }

    /// Run several operations and record them as ONE undo step.
    pub fn group<R>(&mut self, description: &str, f: impl FnOnce(&mut Engine) -> R) -> R {
        let before = self.undo.pushed_count();
        let out = f(self);
        let n = (self.undo.pushed_count() - before) as usize;
        let mut merged = Vec::new();
        for _ in 0..n {
            if let Some(t) = self.undo.pop() {
                merged.push(t);
            }
        }
        merged.reverse(); // oldest first
        if merged.len() > 1 {
            let id = self.undo.allocate_id();
            let actions = merged.into_iter().flat_map(|t| t.actions).collect();
            self.undo.push(Transaction {
                id,
                description: description.to_string(),
                actions,
            });
        } else if let Some(mut t) = merged.pop() {
            t.description = description.to_string();
            self.undo.push(t);
        }
        out
    }

    pub fn mkdir(&mut self, parent: &Path, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        let target = parent.join(name);
        if exists_nofollow(&target) {
            return Err(Error::AlreadyExists(target));
        }
        fs::create_dir(&target).map_err(|e| io_err(&target, e))?;
        let id = self.undo.allocate_id();
        self.commit(
            id,
            format!("Create folder {name}"),
            vec![Action::CreatedDir {
                path: target.clone(),
            }],
        );
        Ok(target)
    }

    // ---- undo --------------------------------------------------------------

    /// Revert the most recent transaction. If one step cannot be undone (for
    /// example the original location is now occupied) undo stops, nothing
    /// further is touched, and the remaining steps stay in the log so you can
    /// fix the problem and try again.
    pub fn undo(&mut self) -> Result<String> {
        let mut txn = self.undo.pop().ok_or(Error::NothingToUndo)?;
        while let Some(action) = txn.actions.last().cloned() {
            if let Err(e) = self.undo_action(&action, txn.id, txn.actions.len()) {
                self.undo.push(txn);
                return Err(Error::Undo(e.to_string()));
            }
            txn.actions.pop();
        }
        Ok(format!("Undid: {}", txn.description))
    }

    fn undo_action(&self, action: &Action, id: u64, step: usize) -> Result<()> {
        match action {
            Action::Copied { created } => {
                if !exists_nofollow(created) {
                    return Err(Error::Other(format!(
                        "the copy no longer exists: {}",
                        created.display()
                    )));
                }
                self.hold(created, id, &format!("u{step}")).map(|_| ())
            }
            Action::Moved { from, to } => {
                if exists_nofollow(from) {
                    return Err(Error::Other(format!(
                        "the original location is occupied: {}",
                        from.display()
                    )));
                }
                if !exists_nofollow(to) {
                    return Err(Error::Other(format!(
                        "the moved item is no longer at {}",
                        to.display()
                    )));
                }
                ensure_parent(from)?;
                move_path(to, from)
            }
            Action::Trashed { original, held } => {
                if exists_nofollow(original) {
                    return Err(Error::Other(format!(
                        "cannot restore, the location is occupied: {}",
                        original.display()
                    )));
                }
                if !exists_nofollow(held) {
                    return Err(Error::Other(format!(
                        "the held copy is missing: {}",
                        held.display()
                    )));
                }
                ensure_parent(original)?;
                move_path(held, original)
            }
            Action::CreatedDir { path } => {
                if !exists_nofollow(path) {
                    return Ok(());
                }
                let empty = fs::read_dir(path)
                    .map(|mut r| r.next().is_none())
                    .unwrap_or(false);
                if empty {
                    fs::remove_dir(path).map_err(|e| io_err(path, e))
                } else {
                    // The user put things in it since; keep them safe.
                    self.hold(path, id, &format!("u{step}")).map(|_| ())
                }
            }
            Action::Renamed { from, to } => {
                if !exists_nofollow(to) {
                    return Err(Error::Other(format!(
                        "the renamed item is no longer at {}",
                        to.display()
                    )));
                }
                rename_checked(to, from, id)
            }
            Action::RenameBatch { pairs } => {
                let reversed: Vec<(PathBuf, PathBuf)> =
                    pairs.iter().map(|(f, t)| (t.clone(), f.clone())).collect();
                rename_set(&reversed, id)
            }
        }
    }
}

/// Rename a set of items together. `pairs` are (from, to). Safe for swaps and
/// cycles: everything is first moved to temporary names, then to final names.
/// All-or-nothing: if a step fails, completed steps are rolled back.
fn rename_set(pairs: &[(PathBuf, PathBuf)], id: u64) -> Result<()> {
    let todo: Vec<&(PathBuf, PathBuf)> = pairs.iter().filter(|(f, t)| f != t).collect();
    let froms: HashSet<&PathBuf> = todo.iter().map(|(f, _)| f).collect();
    let mut seen = HashSet::new();
    for (from, to) in &todo {
        if !exists_nofollow(from) {
            return Err(Error::NotFound(from.clone()));
        }
        if !seen.insert(to.to_string_lossy().to_lowercase()) {
            return Err(Error::Other(format!(
                "two items would get the same name: {}",
                to.display()
            )));
        }
        if exists_nofollow(to) && !froms.contains(to) {
            let same_entry = match (fs::canonicalize(from), fs::canonicalize(to)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if !same_entry {
                return Err(Error::AlreadyExists(to.clone()));
            }
        }
    }

    // Phase 1: move everything out of the way.
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new(); // (from, tmp, to)
    for (i, (from, to)) in todo.iter().enumerate() {
        let parent = from.parent().unwrap_or_else(|| Path::new("."));
        let tmp = parent.join(format!(".twinpane-tmp-{id}-{i}"));
        if let Err(e) = fs::rename(from, &tmp) {
            for (f, t, _) in staged.iter().rev() {
                let _ = fs::rename(t, f);
            }
            return Err(io_err(from, e));
        }
        staged.push((from.clone(), tmp, to.clone()));
    }

    // Phase 2: move to the final names.
    let mut finished: Vec<usize> = Vec::new();
    for (idx, (_, tmp, to)) in staged.iter().enumerate() {
        let failure = if exists_nofollow(to) {
            Some(Error::AlreadyExists(to.clone()))
        } else {
            fs::rename(tmp, to).err().map(|e| io_err(to, e))
        };
        if let Some(err) = failure {
            for k in finished.iter().rev() {
                let (_, t, to_k) = &staged[*k];
                let _ = fs::rename(to_k, t);
            }
            for (f, t, _) in staged.iter().rev() {
                let _ = fs::rename(t, f);
            }
            return Err(err);
        }
        finished.push(idx);
    }
    Ok(())
}

fn display_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
    }
    Ok(())
}

/// Rename `from` to `to`, refusing to clobber a different existing item, but
/// allowing case-only renames on case-insensitive file systems.
fn rename_checked(from: &Path, to: &Path, id: u64) -> Result<()> {
    if exists_nofollow(to) {
        let same_entry = match (fs::canonicalize(from), fs::canonicalize(to)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if !same_entry {
            return Err(Error::AlreadyExists(to.to_path_buf()));
        }
        // Same underlying item: a case-only rename. Go via a temporary name.
        let parent = to.parent().unwrap_or_else(|| Path::new("."));
        let tmp = parent.join(format!(".twinpane-rename-{id}"));
        fs::rename(from, &tmp).map_err(|e| io_err(from, e))?;
        return fs::rename(&tmp, to).map_err(|e| io_err(to, e));
    }
    fs::rename(from, to).map_err(|e| io_err(from, e))
}
