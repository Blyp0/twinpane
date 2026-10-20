//! One JSON command layer used by every front end (the Tauri desktop shell
//! and the sandboxed browser-preview server), so the UI behaves identically
//! and there is a single place to test and secure.
//!
//! `Api::call(command, args)` takes a JSON object and returns JSON. All paths
//! must be absolute. An optional *sandbox root* confines every path (used by
//! the browser preview so a web page can never touch files outside it).

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::listing::{ListOptions, SortKey, SortOrder};
use crate::metadata::{list_rows, RowSort};
use crate::ops::{ConflictPolicy, Engine, OpReport};
use crate::rename::{apply_plan, build_plan, RenameOptions, RenameRule};
use crate::search::{search, SearchQuery};
use crate::sync::{
    apply_sync, compare_dirs, plan_sync, CompareOptions, DiffStatus, SyncMode, SyncPlan,
};
use crate::undo::UndoLog;

pub struct Api {
    engine: Mutex<Engine>,
    undo_path: Option<PathBuf>,
    sandbox: Option<PathBuf>,
    cancel: AtomicBool,
}

type Out = std::result::Result<Value, String>;

fn ok<T: Serialize>(v: T) -> Out {
    serde_json::to_value(v).map_err(|e| format!("internal error: {e}"))
}

fn parse<T: DeserializeOwned>(args: Value) -> std::result::Result<T, String> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| format!("bad arguments: {e}"))
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Resolve `.` and `..` without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c.as_os_str());
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Api {
    /// `holding_dir`: where deleted/replaced items are kept.
    /// `undo_path`: where the undo log is saved (optional).
    /// `sandbox`: confine all paths to this folder (optional).
    pub fn new(
        holding_dir: impl AsRef<Path>,
        undo_path: Option<PathBuf>,
        sandbox: Option<PathBuf>,
    ) -> Result<Api> {
        let log = match &undo_path {
            Some(p) => UndoLog::load(p).unwrap_or_else(|_| {
                // Keep a corrupt log for inspection and start fresh.
                let _ = fs::rename(p, p.with_extension("corrupt"));
                UndoLog::new()
            }),
            None => UndoLog::new(),
        };
        let engine = Engine::with_undo_log(holding_dir, log)?;
        let sandbox = match sandbox {
            Some(s) => Some(fs::canonicalize(&s).map_err(|e| crate::error::io_err(&s, e))?),
            None => None,
        };
        Ok(Api {
            engine: Mutex::new(engine),
            undo_path,
            sandbox,
            cancel: AtomicBool::new(false),
        })
    }

    fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self, engine: &Engine) {
        if let Some(p) = &self.undo_path {
            if let Err(e) = engine.undo_log().save(p) {
                eprintln!("twinpane: could not save undo log: {e}");
            }
        }
    }

    /// Validate and normalise a path argument.
    fn path(&self, s: &str) -> std::result::Result<PathBuf, String> {
        let p = normalize(Path::new(s));
        if !p.is_absolute() {
            return Err(format!("path must be absolute: {s}"));
        }
        if let Some(root) = &self.sandbox {
            let mut cur: &Path = &p;
            let mut tail = PathBuf::new();
            loop {
                match fs::canonicalize(cur) {
                    Ok(c) => {
                        if c.join(&tail).starts_with(root) {
                            break;
                        }
                        return Err(format!("outside the preview sandbox: {s}"));
                    }
                    Err(_) => match (cur.parent(), cur.file_name()) {
                        (Some(parent), Some(name)) => {
                            tail = PathBuf::from(name).join(&tail);
                            cur = parent;
                        }
                        _ => return Err(format!("outside the preview sandbox: {s}")),
                    },
                }
            }
        }
        Ok(p)
    }

    fn paths(&self, list: &[String]) -> std::result::Result<Vec<PathBuf>, String> {
        list.iter().map(|s| self.path(s)).collect()
    }

    /// Run a command. See the `match` for the full list.
    pub fn call(&self, cmd: &str, args: Value) -> Out {
        match cmd {
            "home_dir" => {
                if let Some(root) = &self.sandbox {
                    return ok(root.to_string_lossy());
                }
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map_err(|_| "could not find your home folder".to_string())?;
                ok(home)
            }
            "roots" => ok(self.roots()),
            "list_rows" => {
                #[derive(Deserialize)]
                struct A {
                    path: String,
                    #[serde(default)]
                    show_hidden: bool,
                    sort: Option<RowSort>,
                    order: Option<SortOrder>,
                    #[serde(default)]
                    with_meta: bool,
                }
                let a: A = parse(args)?;
                let path = self.path(&a.path)?;
                let order = a.order.unwrap_or(SortOrder::Asc);
                let opts = ListOptions {
                    show_hidden: a.show_hidden,
                    sort: SortKey::Name,
                    order,
                    dirs_first: true,
                };
                let sort = a.sort.unwrap_or(RowSort::Basic(SortKey::Name));
                let rows = list_rows(&path, &opts, sort, a.with_meta).map_err(err)?;
                ok(json!({ "path": path.to_string_lossy(), "rows": rows }))
            }
            "copy" | "move" => {
                #[derive(Deserialize)]
                struct A {
                    sources: Vec<String>,
                    dest: String,
                    policy: ConflictPolicy,
                }
                let a: A = parse(args)?;
                let sources = self.paths(&a.sources)?;
                let dest = self.path(&a.dest)?;
                let mut engine = self.engine();
                let report = if cmd == "copy" {
                    engine.copy(&sources, &dest, a.policy)
                } else {
                    engine.move_items(&sources, &dest, a.policy)
                }
                .map_err(err)?;
                self.persist(&engine);
                ok(report)
            }
            "delete" => {
                #[derive(Deserialize)]
                struct A {
                    paths: Vec<String>,
                }
                let a: A = parse(args)?;
                let paths = self.paths(&a.paths)?;
                let mut engine = self.engine();
                let report = engine.delete(&paths).map_err(err)?;
                self.persist(&engine);
                ok(report)
            }
            "rename" => {
                #[derive(Deserialize)]
                struct A {
                    path: String,
                    new_name: String,
                }
                let a: A = parse(args)?;
                let path = self.path(&a.path)?;
                let mut engine = self.engine();
                let new = engine.rename(&path, &a.new_name).map_err(err)?;
                self.persist(&engine);
                ok(new.to_string_lossy())
            }
            "mkdir" => {
                #[derive(Deserialize)]
                struct A {
                    parent: String,
                    name: String,
                }
                let a: A = parse(args)?;
                let parent = self.path(&a.parent)?;
                let mut engine = self.engine();
                let new = engine.mkdir(&parent, &a.name).map_err(err)?;
                self.persist(&engine);
                ok(new.to_string_lossy())
            }
            "undo" => {
                let mut engine = self.engine();
                let message = engine.undo().map_err(err);
                self.persist(&engine);
                let message = message?;
                ok(json!({ "message": message, "remaining": engine.undo_log().len() }))
            }
            "undo_state" => {
                let engine = self.engine();
                ok(json!({ "descriptions": engine.undo_log().descriptions() }))
            }
            "empty_holding" => {
                let mut engine = self.engine();
                engine.empty_holding().map_err(err)?;
                self.persist(&engine);
                ok(json!({}))
            }
            "rename_preview" | "rename_apply" => {
                #[derive(Deserialize)]
                struct A {
                    paths: Vec<String>,
                    rules: Vec<RenameRule>,
                    #[serde(default)]
                    options: Option<RenameOptions>,
                }
                let a: A = parse(args)?;
                let paths = self.paths(&a.paths)?;
                let plan =
                    build_plan(&paths, &a.rules, &a.options.unwrap_or_default()).map_err(err)?;
                if cmd == "rename_preview" {
                    return ok(json!({
                        "items": plan.items,
                        "conflicts": plan.conflicts(),
                        "changes": plan.changes(),
                        "applicable": plan.is_applicable(),
                    }));
                }
                let mut engine = self.engine();
                let n = apply_plan(&mut engine, &plan).map_err(err)?;
                self.persist(&engine);
                ok(json!({ "renamed": n }))
            }
            "search" => {
                #[derive(Deserialize)]
                struct A {
                    query: SearchQuery,
                }
                let mut a: A = parse(args)?;
                a.query.root = self.path(&a.query.root.to_string_lossy())?;
                self.cancel.store(false, Ordering::Relaxed);
                let outcome = search(&a.query, Some(&self.cancel)).map_err(err)?;
                ok(outcome)
            }
            "cancel_search" => {
                self.cancel.store(true, Ordering::Relaxed);
                ok(json!({}))
            }
            "compare" => {
                #[derive(Deserialize)]
                struct A {
                    left: String,
                    right: String,
                    #[serde(default)]
                    options: Option<CompareOptions>,
                    mode: SyncMode,
                }
                let a: A = parse(args)?;
                let left = self.path(&a.left)?;
                let right = self.path(&a.right)?;
                let diff =
                    compare_dirs(&left, &right, &a.options.unwrap_or_default()).map_err(err)?;
                let plan = plan_sync(&diff, a.mode);
                let same_count = diff.iter().filter(|d| d.status == DiffStatus::Same).count();
                let items: Vec<_> = diff
                    .into_iter()
                    .filter(|d| d.status != DiffStatus::Same)
                    .collect();
                ok(json!({ "items": items, "same_count": same_count, "plan": plan }))
            }
            "sync_apply" => {
                #[derive(Deserialize)]
                struct A {
                    left: String,
                    right: String,
                    plan: SyncPlan,
                }
                let a: A = parse(args)?;
                let left = self.path(&a.left)?;
                let right = self.path(&a.right)?;
                let mut engine = self.engine();
                let report = apply_sync(&mut engine, &left, &right, &a.plan).map_err(err)?;
                self.persist(&engine);
                ok::<OpReport>(report)
            }
            "open" => {
                #[derive(Deserialize)]
                struct A {
                    path: String,
                }
                let a: A = parse(args)?;
                if self.sandbox.is_some() {
                    return Err("opening files is not available in the browser preview".into());
                }
                let path = self.path(&a.path)?;
                open_with_default_app(&path)?;
                ok(json!({}))
            }
            other => Err(format!("unknown command: {other}")),
        }
    }

    fn roots(&self) -> Vec<String> {
        if let Some(root) = &self.sandbox {
            return vec![root.to_string_lossy().into_owned()];
        }
        #[cfg(windows)]
        {
            (b'A'..=b'Z')
                .map(|c| format!("{}:\\", c as char))
                .filter(|d| Path::new(d).exists())
                .collect()
        }
        #[cfg(not(windows))]
        {
            vec!["/".to_string()]
        }
    }
}

fn open_with_default_app(path: &Path) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // explorer.exe takes the path as a plain argument (no shell parsing).
        let mut c = Command::new("explorer.exe");
        c.arg(path);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}
