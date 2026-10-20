//! File finder: name (substring, glob or regex), type, extension, size,
//! modified date, EXIF date taken, and optional content search.
//!
//! The walk never follows symbolic links, so link loops cannot hang it, and it
//! can be cancelled from another thread.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{io_err, Error, Result};
use crate::listing::{natural_cmp, Entry, EntryKind};
use crate::metadata::{has_exif_extension, read_meta, DateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NameMode {
    /// Plain text anywhere in the name.
    #[default]
    Substring,
    /// `*` and `?` wildcards, matching the whole name.
    Glob,
    /// A regular expression searched anywhere in the name.
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KindFilter {
    #[default]
    Any,
    File,
    Dir,
}

fn default_max_content_bytes() -> u64 {
    20 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub root: PathBuf,
    /// Empty or `None` matches every name.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub name_mode: NameMode,
    /// Applies to the name pattern and the content pattern.
    #[serde(default)]
    pub ignore_case: bool,
    #[serde(default)]
    pub kind: KindFilter,
    /// Without dots, any case. Only files can match when this is non-empty.
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub min_size: Option<u64>,
    #[serde(default)]
    pub max_size: Option<u64>,
    /// Unix seconds.
    #[serde(default)]
    pub modified_after: Option<i64>,
    #[serde(default)]
    pub modified_before: Option<i64>,
    /// EXIF date taken. Files with no date taken never match when set.
    #[serde(default)]
    pub taken_after: Option<DateTime>,
    #[serde(default)]
    pub taken_before: Option<DateTime>,
    /// Search inside files (text only; binary files are skipped).
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub content_regex: bool,
    #[serde(default = "default_max_content_bytes")]
    pub max_content_bytes: u64,
    #[serde(default)]
    pub include_hidden: bool,
    /// 0 = only the root folder's direct children.
    #[serde(default)]
    pub max_depth: Option<usize>,
    #[serde(default)]
    pub max_results: Option<usize>,
}

impl SearchQuery {
    pub fn new(root: impl Into<PathBuf>) -> SearchQuery {
        SearchQuery {
            root: root.into(),
            name: None,
            name_mode: NameMode::Substring,
            ignore_case: false,
            kind: KindFilter::Any,
            extensions: Vec::new(),
            min_size: None,
            max_size: None,
            modified_after: None,
            modified_before: None,
            taken_after: None,
            taken_before: None,
            content: None,
            content_regex: false,
            max_content_bytes: default_max_content_bytes(),
            include_hidden: false,
            max_depth: None,
            max_results: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub entry: Entry,
    /// 1-based line of the first content match.
    pub line: Option<u64>,
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    /// Stopped because `max_results` was reached (there may be more matches).
    pub truncated: bool,
    pub cancelled: bool,
    /// Folders or files that could not be read and were skipped.
    pub unreadable: usize,
}

fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

fn build_regex(pattern: &str, ignore_case: bool) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| Error::Pattern(e.to_string()))
}

fn snippet(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() > 200 {
        let cut: String = t.chars().take(200).collect();
        format!("{cut}…")
    } else {
        t.to_string()
    }
}

/// First matching line of a text file, or `None` (no match, too big, binary,
/// or unreadable).
fn grep_file(path: &Path, re: &Regex, max_bytes: u64) -> Option<(u64, String)> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > max_bytes {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    for (i, line) in text.lines().enumerate() {
        if re.is_match(line) {
            return Some((i as u64 + 1, snippet(line)));
        }
    }
    None
}

/// Run a search. Pass a flag to cancel from another thread.
pub fn search(query: &SearchQuery, cancel: Option<&AtomicBool>) -> Result<SearchOutcome> {
    let root_meta = fs::metadata(&query.root).map_err(|e| io_err(&query.root, e))?;
    if !root_meta.is_dir() {
        return Err(Error::NotFound(query.root.clone()));
    }

    let name_re = match query.name.as_deref() {
        None | Some("") => None,
        Some(text) => {
            let pattern = match query.name_mode {
                NameMode::Substring => regex::escape(text),
                NameMode::Glob => glob_to_regex(text),
                NameMode::Regex => text.to_string(),
            };
            Some(build_regex(&pattern, query.ignore_case)?)
        }
    };
    let content_re = match query.content.as_deref() {
        None | Some("") => None,
        Some(text) => {
            let pattern = if query.content_regex {
                text.to_string()
            } else {
                regex::escape(text)
            };
            Some(build_regex(&pattern, query.ignore_case)?)
        }
    };
    let exts: Vec<String> = query
        .extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let file_only_filters = !exts.is_empty()
        || query.min_size.is_some()
        || query.max_size.is_some()
        || query.taken_after.is_some()
        || query.taken_before.is_some()
        || content_re.is_some();

    let mut out = SearchOutcome::default();
    let mut stack: Vec<(PathBuf, usize)> = vec![(query.root.clone(), 0)];

    'walk: while let Some((dir, depth)) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                out.unreadable += 1;
                continue;
            }
        };
        let mut entries: Vec<Entry> = Vec::new();
        for item in rd.flatten() {
            let path = item.path();
            match fs::symlink_metadata(&path) {
                Ok(meta) => entries.push(Entry::from_meta(&path, &meta)),
                Err(_) => out.unreadable += 1,
            }
        }
        entries.sort_by(|a, b| natural_cmp(&a.name, &b.name).then_with(|| a.name.cmp(&b.name)));

        let mut subdirs: Vec<PathBuf> = Vec::new();
        for entry in entries {
            if cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) {
                out.cancelled = true;
                break 'walk;
            }
            if entry.hidden && !query.include_hidden {
                continue;
            }
            if entry.kind == EntryKind::Dir
                && query.max_depth.map(|m| depth < m).unwrap_or(true)
            {
                subdirs.push(entry.path.clone());
            }

            // ---- filters
            match query.kind {
                KindFilter::Any => {}
                KindFilter::File if entry.kind != EntryKind::File => continue,
                KindFilter::Dir if entry.kind != EntryKind::Dir => continue,
                _ => {}
            }
            if file_only_filters && entry.kind != EntryKind::File {
                continue;
            }
            if let Some(re) = &name_re {
                if !re.is_match(&entry.name) {
                    continue;
                }
            }
            if !exts.is_empty() {
                match &entry.extension {
                    Some(e) if exts.contains(e) => {}
                    _ => continue,
                }
            }
            if query.min_size.map(|m| entry.size < m).unwrap_or(false)
                || query.max_size.map(|m| entry.size > m).unwrap_or(false)
            {
                continue;
            }
            if query.modified_after.is_some() || query.modified_before.is_some() {
                let Some(m) = entry.modified else { continue };
                if query.modified_after.map(|a| m < a).unwrap_or(false)
                    || query.modified_before.map(|b| m > b).unwrap_or(false)
                {
                    continue;
                }
            }
            if query.taken_after.is_some() || query.taken_before.is_some() {
                if !has_exif_extension(entry.extension.as_deref()) {
                    continue;
                }
                let Some(taken) = read_meta(&entry.path).ok().and_then(|m| m.date_taken) else {
                    continue;
                };
                if query.taken_after.map(|a| taken < a).unwrap_or(false)
                    || query.taken_before.map(|b| taken > b).unwrap_or(false)
                {
                    continue;
                }
            }
            let (mut line, mut snip) = (None, None);
            if let Some(re) = &content_re {
                match grep_file(&entry.path, re, query.max_content_bytes) {
                    Some((l, s)) => {
                        line = Some(l);
                        snip = Some(s);
                    }
                    None => continue,
                }
            }

            out.hits.push(SearchHit {
                entry,
                line,
                snippet: snip,
            });
            if query
                .max_results
                .map(|m| out.hits.len() >= m)
                .unwrap_or(false)
            {
                out.truncated = true;
                break 'walk;
            }
        }
        // Visit sub-folders in name order.
        for d in subdirs.into_iter().rev() {
            stack.push((d, depth + 1));
        }
    }
    Ok(out)
}
