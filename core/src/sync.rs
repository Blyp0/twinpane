//! Folder compare and sync.
//!
//! Workflow: [`compare_dirs`] -> [`plan_sync`] (a dry-run you can show the
//! user) -> [`apply_sync`]. Applying goes through the [`Engine`], so replaced
//! and deleted items are kept in the holding area and the whole sync is ONE
//! undo step.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{io_err, Error, Result};
use crate::listing::{natural_cmp, Entry, EntryKind};
use crate::ops::{exists_nofollow, ConflictPolicy, Engine, OpReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffStatus {
    Same,
    LeftOnly,
    RightOnly,
    LeftNewer,
    RightNewer,
    /// Content or size differs but the timestamps are equal, so there is no
    /// safe "winner".
    Different,
    /// A file on one side and a folder (or other type) on the other.
    TypeMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffItem {
    /// Path relative to both roots.
    pub rel: PathBuf,
    pub status: DiffStatus,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompareMethod {
    /// Same size and modified time (within tolerance) means identical. Fast.
    #[default]
    Quick,
    /// Same size means compare the bytes. Slower, but catches edits that
    /// kept the size and timestamp, and ignores timestamp-only differences.
    Content,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompareOptions {
    pub method: CompareMethod,
    /// FAT stores times to 2 seconds, so a small tolerance avoids false diffs.
    pub mtime_tolerance_secs: i64,
    pub include_hidden: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            method: CompareMethod::Quick,
            mtime_tolerance_secs: 2,
            include_hidden: true,
        }
    }
}

fn list_map(dir: &Path, include_hidden: bool) -> Result<BTreeMap<String, Entry>> {
    let mut map = BTreeMap::new();
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(map),
        Err(e) => return Err(io_err(dir, e)),
    };
    for item in rd {
        let item = item.map_err(|e| io_err(dir, e))?;
        let path = item.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
        let entry = Entry::from_meta(&path, &meta);
        if entry.hidden && !include_hidden {
            continue;
        }
        map.insert(entry.name.clone(), entry);
    }
    Ok(map)
}

fn files_equal(a: &Path, b: &Path) -> io::Result<bool> {
    let mut fa = File::open(a)?;
    let mut fb = File::open(b)?;
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = r.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

fn newer_or_different(l: &Entry, r: &Entry, tol: i64) -> DiffStatus {
    let lm = l.modified.unwrap_or(0);
    let rm = r.modified.unwrap_or(0);
    if lm > rm + tol {
        DiffStatus::LeftNewer
    } else if rm > lm + tol {
        DiffStatus::RightNewer
    } else {
        DiffStatus::Different
    }
}

fn compare_files(l: &Entry, r: &Entry, opts: &CompareOptions) -> Result<DiffStatus> {
    let tol = opts.mtime_tolerance_secs;
    if l.size != r.size {
        return Ok(newer_or_different(l, r, tol));
    }
    match opts.method {
        CompareMethod::Quick => {
            let diff = (l.modified.unwrap_or(0) - r.modified.unwrap_or(0)).abs();
            if diff <= tol {
                Ok(DiffStatus::Same)
            } else {
                Ok(newer_or_different(l, r, tol))
            }
        }
        CompareMethod::Content => {
            let same = files_equal(&l.path, &r.path).map_err(|e| io_err(&l.path, e))?;
            if same {
                Ok(DiffStatus::Same)
            } else {
                Ok(newer_or_different(l, r, tol))
            }
        }
    }
}

fn walk(
    left: &Path,
    right: &Path,
    rel: &Path,
    opts: &CompareOptions,
    out: &mut Vec<DiffItem>,
) -> Result<()> {
    let lmap = list_map(&left.join(rel), opts.include_hidden)?;
    let rmap = list_map(&right.join(rel), opts.include_hidden)?;
    let mut names: Vec<&String> = lmap.keys().chain(rmap.keys()).collect();
    names.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
    names.dedup();

    for name in names {
        let item_rel = rel.join(name);
        match (lmap.get(name), rmap.get(name)) {
            (Some(l), None) => out.push(DiffItem {
                rel: item_rel,
                status: DiffStatus::LeftOnly,
                left: Some(l.clone()),
                right: None,
            }),
            (None, Some(r)) => out.push(DiffItem {
                rel: item_rel,
                status: DiffStatus::RightOnly,
                left: None,
                right: Some(r.clone()),
            }),
            (Some(l), Some(r)) => match (l.kind, r.kind) {
                (EntryKind::Dir, EntryKind::Dir) => walk(left, right, &item_rel, opts, out)?,
                (EntryKind::File, EntryKind::File) => {
                    let status = compare_files(l, r, opts)?;
                    out.push(DiffItem {
                        rel: item_rel,
                        status,
                        left: Some(l.clone()),
                        right: Some(r.clone()),
                    });
                }
                (EntryKind::Symlink, EntryKind::Symlink) => {
                    let same = fs::read_link(&l.path).ok() == fs::read_link(&r.path).ok();
                    out.push(DiffItem {
                        rel: item_rel,
                        status: if same {
                            DiffStatus::Same
                        } else {
                            newer_or_different(l, r, opts.mtime_tolerance_secs)
                        },
                        left: Some(l.clone()),
                        right: Some(r.clone()),
                    });
                }
                _ => out.push(DiffItem {
                    rel: item_rel,
                    status: DiffStatus::TypeMismatch,
                    left: Some(l.clone()),
                    right: Some(r.clone()),
                }),
            },
            (None, None) => {}
        }
    }
    Ok(())
}

/// Compare two folders recursively. Folders that exist on only one side are
/// reported once (not descended into). Folders present on both sides are not
/// listed themselves, only their contents. Files that are identical are
/// listed with `DiffStatus::Same` so a UI can show them if it wants.
pub fn compare_dirs(left: &Path, right: &Path, opts: &CompareOptions) -> Result<Vec<DiffItem>> {
    for p in [left, right] {
        if !p.is_dir() {
            return Err(Error::NotFound(p.to_path_buf()));
        }
    }
    let mut out = Vec::new();
    walk(left, right, Path::new(""), opts, &mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------- planning

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
    /// Make the right side identical to the left: copy new and changed items
    /// left to right, and remove (to holding) items that exist only on the right.
    Mirror,
    /// Copy missing and newer items left to right. Never removes anything and
    /// never overwrites a newer right-hand file.
    Update,
    /// Copy missing items to the side that lacks them and let the newest
    /// version win. Cannot tell a deletion from a new file, so it never deletes.
    TwoWay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncKind {
    CopyToRight,
    CopyToLeft,
    DeleteRight,
    DeleteLeft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAction {
    pub kind: SyncKind,
    pub rel: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    pub actions: Vec<SyncAction>,
    /// Items a sync cannot decide on (conflicts), with the reason.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Turn a comparison into a list of actions without touching anything.
pub fn plan_sync(diff: &[DiffItem], mode: SyncMode) -> SyncPlan {
    let mut plan = SyncPlan::default();
    let act = |plan: &mut SyncPlan, kind, rel: &Path, reason: &str| {
        plan.actions.push(SyncAction {
            kind,
            rel: rel.to_path_buf(),
            reason: reason.to_string(),
        })
    };
    let skip = |plan: &mut SyncPlan, rel: &Path, reason: &str| {
        plan.skipped.push((rel.to_path_buf(), reason.to_string()))
    };

    for item in diff {
        let rel = item.rel.as_path();
        match (item.status, mode) {
            (DiffStatus::Same, _) => {}
            (DiffStatus::TypeMismatch, _) => {
                skip(&mut plan, rel, "a file on one side, a folder on the other")
            }
            (DiffStatus::Different, SyncMode::Mirror) => {
                act(&mut plan, SyncKind::CopyToRight, rel, "differs; mirror makes right match left")
            }
            (DiffStatus::Different, _) => skip(
                &mut plan,
                rel,
                "differs but has the same modified time, so there is no safe winner",
            ),
            (DiffStatus::LeftOnly, _) => {
                act(&mut plan, SyncKind::CopyToRight, rel, "only on the left")
            }
            (DiffStatus::RightOnly, SyncMode::Mirror) => {
                act(&mut plan, SyncKind::DeleteRight, rel, "only on the right")
            }
            (DiffStatus::RightOnly, SyncMode::TwoWay) => {
                act(&mut plan, SyncKind::CopyToLeft, rel, "only on the right")
            }
            (DiffStatus::RightOnly, SyncMode::Update) => {}
            (DiffStatus::LeftNewer, _) => {
                act(&mut plan, SyncKind::CopyToRight, rel, "left is newer")
            }
            (DiffStatus::RightNewer, SyncMode::Mirror) => act(
                &mut plan,
                SyncKind::CopyToRight,
                rel,
                "right is newer, but mirror makes right match left",
            ),
            (DiffStatus::RightNewer, SyncMode::TwoWay) => {
                act(&mut plan, SyncKind::CopyToLeft, rel, "right is newer")
            }
            (DiffStatus::RightNewer, SyncMode::Update) => {}
        }
    }
    plan
}

// ---------------------------------------------------------------- applying

fn safe_rel(rel: &Path) -> Result<()> {
    let ok = rel.components().count() > 0
        && rel.components().all(|c| matches!(c, Component::Normal(_)));
    if ok {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "unsafe path in sync plan: {}",
            rel.display()
        )))
    }
}

/// Create missing folders one at a time through the engine so undo removes them.
fn ensure_dir(engine: &mut Engine, base: &Path, rel_dir: &Path) -> Result<PathBuf> {
    let mut cur = base.to_path_buf();
    for comp in rel_dir.components() {
        if let Component::Normal(name) = comp {
            let next = cur.join(name);
            if !exists_nofollow(&next) {
                engine.mkdir(&cur, &name.to_string_lossy())?;
            }
            cur = next;
        }
    }
    Ok(cur)
}

fn merge(into: &mut OpReport, from: OpReport) {
    into.done.extend(from.done);
    into.skipped.extend(from.skipped);
    into.errors.extend(from.errors);
}

/// Carry out a plan. One undo step covers the whole sync. Failures on single
/// items are reported and do not stop the rest.
pub fn apply_sync(
    engine: &mut Engine,
    left: &Path,
    right: &Path,
    plan: &SyncPlan,
) -> Result<OpReport> {
    let lc = fs::canonicalize(left).map_err(|e| io_err(left, e))?;
    let rc = fs::canonicalize(right).map_err(|e| io_err(right, e))?;
    if lc == rc || lc.starts_with(&rc) || rc.starts_with(&lc) {
        return Err(Error::Other(
            "the two folders must be separate (neither can contain the other)".into(),
        ));
    }
    for a in &plan.actions {
        safe_rel(&a.rel)?;
    }

    let desc = format!("Sync {} item(s)", plan.actions.len());
    let report = engine.group(&desc, |engine| {
        let mut report = OpReport::default();
        for a in &plan.actions {
            let (from_root, to_root) = match a.kind {
                SyncKind::CopyToRight => (left, right),
                SyncKind::CopyToLeft => (right, left),
                SyncKind::DeleteRight | SyncKind::DeleteLeft => (left, right),
            };
            let result: Result<OpReport> = match a.kind {
                SyncKind::CopyToRight | SyncKind::CopyToLeft => (|| {
                    let src = from_root.join(&a.rel);
                    let parent_rel = a.rel.parent().unwrap_or_else(|| Path::new(""));
                    let dest_dir = ensure_dir(engine, to_root, parent_rel)?;
                    engine.copy(&[src], &dest_dir, ConflictPolicy::Overwrite)
                })(),
                SyncKind::DeleteRight => engine.delete(&[right.join(&a.rel)]),
                SyncKind::DeleteLeft => engine.delete(&[left.join(&a.rel)]),
            };
            match result {
                Ok(r) => merge(&mut report, r),
                Err(e) => report.errors.push((a.rel.clone(), e.to_string())),
            }
        }
        report
    });
    Ok(report)
}
