//! Directory listing and sorting.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{io_err, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub hidden: bool,
    pub readonly: bool,
    /// Lower-case, without the dot. `None` for directories and dotfiles.
    pub extension: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Created,
    Extension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ListOptions {
    pub show_hidden: bool,
    pub sort: SortKey,
    pub order: SortOrder,
    pub dirs_first: bool,
}

impl Default for ListOptions {
    fn default() -> Self {
        ListOptions {
            show_hidden: false,
            sort: SortKey::Name,
            order: SortOrder::Asc,
            dirs_first: true,
        }
    }
}

fn to_unix(t: io::Result<SystemTime>) -> Option<i64> {
    let t = t.ok()?;
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        Err(e) => Some(-(e.duration().as_secs() as i64)),
    }
}

#[cfg(windows)]
fn platform_hidden(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn platform_hidden(_meta: &fs::Metadata) -> bool {
    false
}

impl Entry {
    /// Build an entry for `path` without following a final symlink.
    pub fn from_path(path: &Path) -> Result<Entry> {
        let meta = fs::symlink_metadata(path).map_err(|e| io_err(path, e))?;
        Ok(Entry::from_meta(path, &meta))
    }

    pub(crate) fn from_meta(path: &Path, meta: &fs::Metadata) -> Entry {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let extension = if kind == EntryKind::Dir {
            None
        } else {
            Path::new(&name)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
        };
        Entry {
            hidden: name.starts_with('.') || platform_hidden(meta),
            name,
            path: path.to_path_buf(),
            kind,
            size: if kind == EntryKind::Dir { 0 } else { meta.len() },
            modified: to_unix(meta.modified()),
            created: to_unix(meta.created()),
            readonly: meta.permissions().readonly(),
            extension,
        }
    }
}

/// Case-insensitive "natural" comparison: digit runs compare by value, so
/// `file2` sorts before `file10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let na = take_digits(&mut ai);
                    let nb = take_digits(&mut bi);
                    let ta = na.trim_start_matches('0');
                    let tb = nb.trim_start_matches('0');
                    let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if o != Ordering::Equal {
                        return o;
                    }
                } else {
                    let xl = x.to_lowercase().next().unwrap_or(x);
                    let yl = y.to_lowercase().next().unwrap_or(y);
                    if xl != yl {
                        return xl.cmp(&yl);
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

fn take_digits<I: Iterator<Item = char>>(it: &mut std::iter::Peekable<I>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s
}

pub fn sort_entries(entries: &mut [Entry], opts: &ListOptions) {
    entries.sort_by(|a, b| {
        if opts.dirs_first {
            let ad = a.kind == EntryKind::Dir;
            let bd = b.kind == EntryKind::Dir;
            if ad != bd {
                return bd.cmp(&ad);
            }
        }
        let key = match opts.sort {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Created => a.created.cmp(&b.created),
            SortKey::Extension => a.extension.cmp(&b.extension),
        };
        let key = if opts.order == SortOrder::Desc {
            key.reverse()
        } else {
            key
        };
        key.then_with(|| natural_cmp(&a.name, &b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// List the contents of `dir`. Symlinks are reported as symlinks (not followed).
/// Entries whose metadata cannot be read are still listed, as `Other`.
pub fn list_dir(dir: &Path, opts: &ListOptions) -> Result<Vec<Entry>> {
    let rd = fs::read_dir(dir).map_err(|e| io_err(dir, e))?;
    let mut out = Vec::new();
    for item in rd {
        let item = item.map_err(|e| io_err(dir, e))?;
        let path = item.path();
        let entry = match fs::symlink_metadata(&path) {
            Ok(meta) => Entry::from_meta(&path, &meta),
            Err(_) => Entry {
                name: item.file_name().to_string_lossy().into_owned(),
                path: path.clone(),
                kind: EntryKind::Other,
                size: 0,
                modified: None,
                created: None,
                hidden: false,
                readonly: false,
                extension: None,
            },
        };
        if entry.hidden && !opts.show_hidden {
            continue;
        }
        out.push(entry);
    }
    sort_entries(&mut out, opts);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_compares_numbers_by_value() {
        assert_eq!(natural_cmp("img2", "img10"), Ordering::Less);
        assert_eq!(natural_cmp("img10", "img2"), Ordering::Greater);
        assert_eq!(natural_cmp("img007", "img7"), Ordering::Equal);
    }

    #[test]
    fn natural_order_ignores_case_and_handles_prefixes() {
        assert_eq!(natural_cmp("b", "C"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("same", "SAME"), Ordering::Equal);
    }
}
