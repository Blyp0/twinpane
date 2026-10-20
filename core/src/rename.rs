//! Batch rename: a chain of rules applied to each name, producing a *plan*
//! that can be previewed (with conflicts flagged) before anything touches disk.
//!
//! Tokens usable in rule text (`Replace.with`, `Insert.text`):
//! `{n}` / `{n:3}` counter (optionally zero-padded), `{name}` `{stem}` `{ext}`
//! (original name parts), `{parent}` folder name, `{taken:FMT}` EXIF date taken,
//! `{modified:FMT}` `{created:FMT}` file times, `{camera}` `{make}` `{model}`
//! `{width}` `{height}`. `FMT` uses `%Y %y %m %d %H %M %S`.
//! Unknown `{...}` text is left as written.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::{NoExpand, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::listing::{Entry, EntryKind};
use crate::metadata::{read_meta, DateTime, FileMeta};
use crate::ops::{exists_nofollow, validate_name, Engine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Target {
    /// The name without its extension (the default).
    #[default]
    Stem,
    Extension,
    /// The whole name, extension included.
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Position {
    Start,
    End,
    /// Character index (0-based), clamped to the text length.
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseMode {
    Lower,
    Upper,
    /// Capitalise the first letter of every word.
    Title,
    /// Capitalise only the first letter.
    Sentence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RenameRule {
    /// Find and replace. With `regex`, `with` may use `$1`, `${name}` captures.
    Replace {
        find: String,
        with: String,
        #[serde(default)]
        regex: bool,
        #[serde(default)]
        ignore_case: bool,
        #[serde(default)]
        target: Target,
    },
    Insert {
        text: String,
        at: Position,
        #[serde(default)]
        target: Target,
    },
    Case {
        mode: CaseMode,
        #[serde(default)]
        target: Target,
    },
    /// Trim the ends and collapse runs of whitespace to one space.
    Trim {
        #[serde(default)]
        target: Target,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RenameOptions {
    pub counter_start: i64,
    pub counter_step: i64,
    /// Offset applied to `{modified}` / `{created}` (UTC by default).
    pub utc_offset_minutes: i32,
}

impl Default for RenameOptions {
    fn default() -> Self {
        RenameOptions {
            counter_start: 1,
            counter_step: 1,
            utc_offset_minutes: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemStatus {
    /// The name does not change.
    Unchanged,
    /// Ready to rename.
    Ok,
    Invalid(String),
    /// Another item in the batch would get the same name.
    Duplicate(String),
    /// A different file with that name already exists on disk.
    Exists(String),
    /// A token needs metadata this file does not have.
    MissingMetadata(String),
}

impl ItemStatus {
    pub fn is_conflict(&self) -> bool {
        !matches!(self, ItemStatus::Unchanged | ItemStatus::Ok)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub from: PathBuf,
    pub old_name: String,
    pub new_name: String,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePlan {
    pub items: Vec<PlanItem>,
}

impl RenamePlan {
    pub fn conflicts(&self) -> usize {
        self.items.iter().filter(|i| i.status.is_conflict()).count()
    }
    pub fn changes(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == ItemStatus::Ok)
            .count()
    }
    /// At least one change and no conflicts.
    pub fn is_applicable(&self) -> bool {
        self.conflicts() == 0 && self.changes() > 0
    }
}

// ---------------------------------------------------------------- internals

enum Compiled {
    Replace {
        re: Option<Regex>, // None => no-op (empty literal find)
        with: String,
        regex_mode: bool,
        target: Target,
    },
    Insert {
        text: String,
        at: Position,
        target: Target,
    },
    Case(CaseMode, Target),
    Trim(Target),
}

fn compile(rules: &[RenameRule]) -> Result<Vec<Compiled>> {
    rules
        .iter()
        .map(|r| match r {
            RenameRule::Replace {
                find,
                with,
                regex,
                ignore_case,
                target,
            } => {
                let re = if !*regex && find.is_empty() {
                    None
                } else {
                    let pattern = if *regex {
                        find.clone()
                    } else {
                        regex::escape(find)
                    };
                    Some(
                        RegexBuilder::new(&pattern)
                            .case_insensitive(*ignore_case)
                            .build()
                            .map_err(|e| Error::Pattern(e.to_string()))?,
                    )
                };
                Ok(Compiled::Replace {
                    re,
                    with: with.clone(),
                    regex_mode: *regex,
                    target: *target,
                })
            }
            RenameRule::Insert { text, at, target } => Ok(Compiled::Insert {
                text: text.clone(),
                at: *at,
                target: *target,
            }),
            RenameRule::Case { mode, target } => Ok(Compiled::Case(*mode, *target)),
            RenameRule::Trim { target } => Ok(Compiled::Trim(*target)),
        })
        .collect()
}

/// Split into (stem, extension). A leading dot or a trailing dot is not an
/// extension separator.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn join_name(stem: &str, ext: &str) -> String {
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

struct Ctx<'a> {
    n: i64,
    name: &'a str,
    stem: &'a str,
    ext: &'a str,
    parent: String,
    taken: Option<DateTime>,
    modified: Option<DateTime>,
    created: Option<DateTime>,
    meta: Option<&'a FileMeta>,
}

const DEFAULT_DATE_FMT: &str = "%Y-%m-%d_%H%M%S";

/// Expand `{tokens}`. On a missing metadata value returns `Err(token_name)`.
fn expand(template: &str, ctx: &Ctx, escape_dollar: bool) -> std::result::Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let inner = &after[..close];
        let (name, arg) = match inner.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (inner, None),
        };
        let value: Option<std::result::Result<String, String>> = match name {
            "n" => {
                let width = arg.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
                Some(Ok(if ctx.n < 0 {
                    format!("-{:0>width$}", -ctx.n, width = width)
                } else {
                    format!("{:0>width$}", ctx.n, width = width)
                }))
            }
            "name" => Some(Ok(ctx.name.to_string())),
            "stem" => Some(Ok(ctx.stem.to_string())),
            "ext" => Some(Ok(ctx.ext.to_string())),
            "parent" => Some(Ok(ctx.parent.clone())),
            "taken" => Some(date_value(ctx.taken, arg, "taken")),
            "modified" => Some(date_value(ctx.modified, arg, "modified")),
            "created" => Some(date_value(ctx.created, arg, "created")),
            "camera" => Some(
                ctx.meta
                    .and_then(|m| m.camera())
                    .ok_or_else(|| "camera".to_string()),
            ),
            "make" => Some(
                ctx.meta
                    .and_then(|m| m.make.clone())
                    .ok_or_else(|| "make".to_string()),
            ),
            "model" => Some(
                ctx.meta
                    .and_then(|m| m.model.clone())
                    .ok_or_else(|| "model".to_string()),
            ),
            "width" => Some(
                ctx.meta
                    .and_then(|m| m.width.map(|v| v.to_string()))
                    .ok_or_else(|| "width".to_string()),
            ),
            "height" => Some(
                ctx.meta
                    .and_then(|m| m.height.map(|v| v.to_string()))
                    .ok_or_else(|| "height".to_string()),
            ),
            _ => None,
        };
        match value {
            Some(Ok(v)) => {
                if escape_dollar {
                    out.push_str(&v.replace('$', "$$"));
                } else {
                    out.push_str(&v);
                }
            }
            Some(Err(missing)) => return Err(missing),
            None => {
                // Unknown token: keep it literally.
                out.push('{');
                out.push_str(inner);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn date_value(
    dt: Option<DateTime>,
    fmt: Option<&str>,
    label: &str,
) -> std::result::Result<String, String> {
    dt.map(|d| d.format(fmt.unwrap_or(DEFAULT_DATE_FMT)))
        .ok_or_else(|| label.to_string())
}

fn uses_exif(rules: &[RenameRule]) -> bool {
    const NEEDLES: [&str; 6] = ["{taken", "{camera", "{make", "{model", "{width", "{height"];
    rules.iter().any(|r| {
        let text = match r {
            RenameRule::Replace { with, .. } => with,
            RenameRule::Insert { text, .. } => text,
            _ => return false,
        };
        NEEDLES.iter().any(|n| text.contains(n))
    })
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start_of_word = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if start_of_word {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start_of_word = false;
        } else {
            out.push(c);
            start_of_word = true;
        }
    }
    out
}

fn sentence_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for c in s.chars() {
        if first && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            first = false;
        } else {
            out.extend(c.to_lowercase());
            if c.is_alphanumeric() {
                first = false;
            }
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Apply `f` to the part of the name selected by `target`.
fn on_target(
    stem: &mut String,
    ext: &mut String,
    target: Target,
    is_dir: bool,
    f: &mut dyn FnMut(&str) -> std::result::Result<String, String>,
) -> std::result::Result<(), String> {
    match target {
        Target::Stem => *stem = f(stem)?,
        Target::Extension => {
            if !is_dir {
                *ext = f(ext)?;
            }
        }
        Target::Name => {
            let whole = join_name(stem, ext);
            let new = f(&whole)?;
            if is_dir {
                *stem = new;
                ext.clear();
            } else {
                let (s, e) = split_name(&new);
                *stem = s.to_string();
                *ext = e.to_string();
            }
        }
    }
    Ok(())
}

fn apply_rules(
    rules: &[Compiled],
    ctx: &Ctx,
    is_dir: bool,
) -> std::result::Result<String, String> {
    let (s, e) = if is_dir {
        (ctx.name, "")
    } else {
        split_name(ctx.name)
    };
    let mut stem = s.to_string();
    let mut ext = e.to_string();
    for rule in rules {
        match rule {
            Compiled::Replace {
                re,
                with,
                regex_mode,
                target,
            } => {
                let Some(re) = re else { continue };
                let with_x = expand(with, ctx, *regex_mode)?;
                on_target(&mut stem, &mut ext, *target, is_dir, &mut |text| {
                    Ok(if *regex_mode {
                        re.replace_all(text, with_x.as_str()).into_owned()
                    } else {
                        re.replace_all(text, NoExpand(&with_x)).into_owned()
                    })
                })?;
            }
            Compiled::Insert { text, at, target } => {
                let text_x = expand(text, ctx, false)?;
                on_target(&mut stem, &mut ext, *target, is_dir, &mut |cur| {
                    let chars: Vec<char> = cur.chars().collect();
                    let idx = match at {
                        Position::Start => 0,
                        Position::End => chars.len(),
                        Position::Index(i) => (*i).min(chars.len()),
                    };
                    let mut s: String = chars[..idx].iter().collect();
                    s.push_str(&text_x);
                    s.extend(chars[idx..].iter());
                    Ok(s)
                })?;
            }
            Compiled::Case(mode, target) => {
                on_target(&mut stem, &mut ext, *target, is_dir, &mut |cur| {
                    Ok(match mode {
                        CaseMode::Lower => cur.to_lowercase(),
                        CaseMode::Upper => cur.to_uppercase(),
                        CaseMode::Title => title_case(cur),
                        CaseMode::Sentence => sentence_case(cur),
                    })
                })?;
            }
            Compiled::Trim(target) => {
                on_target(&mut stem, &mut ext, *target, is_dir, &mut |cur| {
                    Ok(collapse_whitespace(cur))
                })?;
            }
        }
    }
    Ok(join_name(&stem, &ext))
}

// ------------------------------------------------------------------- public

/// Build a preview plan. Nothing on disk changes. Items are numbered in the
/// order given (`{n}`). Errors only for an invalid regex.
pub fn build_plan(
    paths: &[PathBuf],
    rules: &[RenameRule],
    opts: &RenameOptions,
) -> Result<RenamePlan> {
    let compiled = compile(rules)?;
    let needs_exif = uses_exif(rules);
    let mut items: Vec<PlanItem> = Vec::with_capacity(paths.len());

    for (i, path) in paths.iter().enumerate() {
        let entry = Entry::from_path(path)?;
        let is_dir = entry.kind == EntryKind::Dir;
        let (stem, ext) = if is_dir {
            (entry.name.as_str(), "")
        } else {
            split_name(&entry.name)
        };
        let meta = if needs_exif && !is_dir {
            read_meta(path).ok()
        } else {
            None
        };
        let ctx = Ctx {
            n: opts.counter_start + (i as i64) * opts.counter_step,
            name: &entry.name,
            stem,
            ext,
            parent: path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            taken: meta.as_ref().and_then(|m| m.date_taken),
            modified: entry
                .modified
                .map(|t| DateTime::from_unix(t, opts.utc_offset_minutes)),
            created: entry
                .created
                .map(|t| DateTime::from_unix(t, opts.utc_offset_minutes)),
            meta: meta.as_ref(),
        };
        let (new_name, mut status) = match apply_rules(&compiled, &ctx, is_dir) {
            Ok(n) => (n, ItemStatus::Ok),
            Err(missing) => (entry.name.clone(), ItemStatus::MissingMetadata(missing)),
        };
        if status == ItemStatus::Ok {
            if new_name == entry.name {
                status = ItemStatus::Unchanged;
            } else if let Err(e) = validate_name(&new_name) {
                status = ItemStatus::Invalid(e.to_string());
            }
        }
        items.push(PlanItem {
            from: path.clone(),
            old_name: entry.name.clone(),
            new_name,
            status,
        });
    }

    // Duplicates: every item (changed or not) occupies its final name.
    let mut counts: HashMap<(PathBuf, String), usize> = HashMap::new();
    for it in &items {
        let parent = it.from.parent().map(Path::to_path_buf).unwrap_or_default();
        *counts
            .entry((parent, it.new_name.to_lowercase()))
            .or_insert(0) += 1;
    }
    for it in &mut items {
        if it.status != ItemStatus::Ok {
            continue;
        }
        let parent = it.from.parent().map(Path::to_path_buf).unwrap_or_default();
        if counts[&(parent, it.new_name.to_lowercase())] > 1 {
            it.status = ItemStatus::Duplicate(format!(
                "another item would also be named {:?}",
                it.new_name
            ));
        }
    }

    // Collisions with files outside the batch.
    let sources: HashSet<PathBuf> = items.iter().map(|i| i.from.clone()).collect();
    for it in &mut items {
        if it.status != ItemStatus::Ok {
            continue;
        }
        let target = it
            .from
            .parent()
            .map(|p| p.join(&it.new_name))
            .unwrap_or_else(|| PathBuf::from(&it.new_name));
        let case_only = it.new_name.to_lowercase() == it.old_name.to_lowercase();
        if !case_only && exists_nofollow(&target) && !sources.contains(&target) {
            it.status = ItemStatus::Exists(format!("{:?} already exists", it.new_name));
        }
    }

    Ok(RenamePlan { items })
}

/// Apply a plan as one all-or-nothing, single-undo-step operation.
/// Refuses if the plan has conflicts or nothing to do. Returns the number of
/// items renamed.
pub fn apply_plan(engine: &mut Engine, plan: &RenamePlan) -> Result<usize> {
    if plan.conflicts() > 0 {
        return Err(Error::Other(format!(
            "the rename plan has {} conflict(s); fix them first",
            plan.conflicts()
        )));
    }
    let pairs: Vec<(PathBuf, PathBuf)> = plan
        .items
        .iter()
        .filter(|i| i.status == ItemStatus::Ok)
        .map(|i| {
            let target = i
                .from
                .parent()
                .map(|p| p.join(&i.new_name))
                .unwrap_or_else(|| PathBuf::from(&i.new_name));
            (i.from.clone(), target)
        })
        .collect();
    if pairs.is_empty() {
        return Ok(0);
    }
    engine.rename_many(&pairs, &format!("Rename {} item(s)", pairs.len()))?;
    Ok(pairs.len())
}
