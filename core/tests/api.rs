use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tempfile::TempDir;
use twinpane_core::api::Api;

struct Env {
    _tmp: TempDir,
    root: PathBuf,
    outside: PathBuf,
    state: PathBuf,
    api: Api,
}

fn env_with(sandboxed: bool) -> Env {
    let tmp = TempDir::new().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("sandbox");
    let outside = base.join("outside");
    let state = base.join("state");
    for d in [&root, &outside, &state] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(outside.join("secret.txt"), "secret").unwrap();
    let api = Api::new(
        state.join("holding"),
        Some(state.join("undo.json")),
        if sandboxed { Some(root.clone()) } else { None },
    )
    .unwrap();
    Env {
        _tmp: tmp,
        root,
        outside,
        state,
        api,
    }
}

fn env() -> Env {
    env_with(true)
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn names(v: &Value) -> Vec<String> {
    v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["entry"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn list_rows_returns_normalised_path_and_rows() {
    let e = env();
    fs::write(e.root.join("b.txt"), "x").unwrap();
    fs::create_dir(e.root.join("adir")).unwrap();
    let dotted = format!("{}/adir/..", s(&e.root));
    let v = e
        .api
        .call("list_rows", json!({ "path": dotted, "sort": {"Basic":"Name"}, "order": "Asc" }))
        .unwrap();
    assert_eq!(v["path"], s(&e.root));
    assert_eq!(names(&v), ["adir", "b.txt"]);
    assert_eq!(v["rows"][1]["entry"]["kind"], "File");
}

#[test]
fn sandbox_blocks_paths_outside_the_root() {
    let e = env();
    for path in [
        s(&e.outside),
        format!("{}/../outside", s(&e.root)),
        "/".to_string(),
        s(&e.outside.join("secret.txt")),
    ] {
        let r = e.api.call("list_rows", json!({ "path": path }));
        assert!(r.is_err(), "{path} should be refused");
        assert!(r.unwrap_err().contains("sandbox"));
    }
    // Every mutating command is guarded too.
    let secret = s(&e.outside.join("secret.txt"));
    assert!(e.api.call("delete", json!({ "paths": [secret.clone()] })).is_err());
    assert!(e
        .api
        .call("copy", json!({ "sources": [secret.clone()], "dest": s(&e.root), "policy": "Fail" }))
        .is_err());
    assert!(e
        .api
        .call("copy", json!({ "sources": [s(&e.root)], "dest": s(&e.outside), "policy": "Fail" }))
        .is_err());
    assert!(e.api.call("rename", json!({ "path": secret, "new_name": "x" })).is_err());
    assert!(e
        .api
        .call("mkdir", json!({ "parent": s(&e.outside), "name": "x" }))
        .is_err());
    assert!(e
        .api
        .call("search", json!({ "query": { "root": s(&e.outside) } }))
        .is_err());
    assert!(e
        .api
        .call("compare", json!({ "left": s(&e.root), "right": s(&e.outside), "mode": "Mirror" }))
        .is_err());
    assert_eq!(fs::read_to_string(e.outside.join("secret.txt")).unwrap(), "secret");
}

#[cfg(unix)]
#[test]
fn sandbox_blocks_symlink_escapes() {
    let e = env();
    std::os::unix::fs::symlink(&e.outside, e.root.join("sneaky")).unwrap();
    let via = s(&e.root.join("sneaky"));
    assert!(e.api.call("list_rows", json!({ "path": via })).is_err());
    let via_file = s(&e.root.join("sneaky/secret.txt"));
    assert!(e.api.call("delete", json!({ "paths": [via_file] })).is_err());
    // A path that does not exist yet but would resolve through the link.
    let new_via = s(&e.root.join("sneaky/new_dir"));
    assert!(e
        .api
        .call("mkdir", json!({ "parent": s(&e.root.join("sneaky")), "name": "new_dir" }))
        .is_err());
    assert!(!e.outside.join("new_dir").exists());
    let _ = new_via;
}

#[test]
fn relative_paths_are_rejected() {
    let e = env_with(false);
    assert!(e.api.call("list_rows", json!({ "path": "relative/dir" })).is_err());
}

#[test]
fn unknown_commands_and_bad_arguments_give_clear_errors() {
    let e = env();
    assert!(e.api.call("format_c_drive", json!({})).unwrap_err().contains("unknown command"));
    assert!(e.api.call("list_rows", json!({})).unwrap_err().contains("bad arguments"));
    assert!(e
        .api
        .call("copy", json!({ "sources": [], "dest": s(&e.root), "policy": "Nonsense" }))
        .unwrap_err()
        .contains("bad arguments"));
}

#[test]
fn copy_move_delete_rename_mkdir_and_undo_through_json() {
    let e = env();
    fs::write(e.root.join("a.txt"), "A").unwrap();
    let dest = e.root.join("dest");
    e.api.call("mkdir", json!({ "parent": s(&e.root), "name": "dest" })).unwrap();

    let r = e
        .api
        .call("copy", json!({ "sources": [s(&e.root.join("a.txt"))], "dest": s(&dest), "policy": "Fail" }))
        .unwrap();
    assert_eq!(r["errors"].as_array().unwrap().len(), 0);
    assert!(dest.join("a.txt").exists());

    e.api
        .call("rename", json!({ "path": s(&dest.join("a.txt")), "new_name": "b.txt" }))
        .unwrap();
    assert!(dest.join("b.txt").exists());

    e.api.call("delete", json!({ "paths": [s(&dest.join("b.txt"))] })).unwrap();
    assert!(!dest.join("b.txt").exists());

    // Undo delete, rename, copy, mkdir in order.
    for expect in ["Delete", "Rename", "Copy", "Create folder"] {
        let u = e.api.call("undo", json!({})).unwrap();
        assert!(u["message"].as_str().unwrap().contains(expect), "{u}");
    }
    assert!(!dest.exists());
    assert!(e.api.call("undo", json!({})).unwrap_err().contains("nothing to undo"));
}

#[test]
fn undo_survives_a_restart() {
    let e = env();
    fs::write(e.root.join("a.txt"), "A").unwrap();
    e.api
        .call("rename", json!({ "path": s(&e.root.join("a.txt")), "new_name": "z.txt" }))
        .unwrap();
    // A new Api over the same state files, like relaunching the app.
    let again = Api::new(
        e.state.join("holding"),
        Some(e.state.join("undo.json")),
        Some(e.root.clone()),
    )
    .unwrap();
    let st = again.call("undo_state", json!({})).unwrap();
    assert_eq!(st["descriptions"].as_array().unwrap().len(), 1);
    again.call("undo", json!({})).unwrap();
    assert!(e.root.join("a.txt").exists());
}

#[test]
fn corrupt_undo_log_is_set_aside_not_fatal() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("undo.json"), "{{{ not json").unwrap();
    let api = Api::new(state.join("holding"), Some(state.join("undo.json")), None).unwrap();
    let st = api.call("undo_state", json!({})).unwrap();
    assert_eq!(st["descriptions"].as_array().unwrap().len(), 0);
    assert!(state.join("undo.corrupt").exists());
}

#[test]
fn rename_preview_and_apply_through_json() {
    let e = env();
    for n in ["a.txt", "b.txt"] {
        fs::write(e.root.join(n), "x").unwrap();
    }
    let args = json!({
        "paths": [s(&e.root.join("a.txt")), s(&e.root.join("b.txt"))],
        "rules": [{ "type": "Replace", "find": "^.*$", "with": "file_{n:2}", "regex": true }],
        "options": { "counter_start": 1, "counter_step": 1, "utc_offset_minutes": 0 }
    });
    let p = e.api.call("rename_preview", args.clone()).unwrap();
    assert_eq!(p["applicable"], true);
    assert_eq!(p["items"][0]["new_name"], "file_01.txt");
    assert_eq!(p["items"][1]["new_name"], "file_02.txt");
    assert!(e.root.join("a.txt").exists(), "preview must not change anything");

    let r = e.api.call("rename_apply", args).unwrap();
    assert_eq!(r["renamed"], 2);
    assert!(e.root.join("file_01.txt").exists());

    // Conflicts are reported by preview and refused by apply.
    let bad = json!({
        "paths": [s(&e.root.join("file_01.txt")), s(&e.root.join("file_02.txt"))],
        "rules": [{ "type": "Replace", "find": "^.*$", "with": "same", "regex": true }]
    });
    assert_eq!(e.api.call("rename_preview", bad.clone()).unwrap()["applicable"], false);
    assert!(e.api.call("rename_apply", bad).is_err());
    // A bad regex is reported straight away, even before any file is looked at.
    let bad_regex = e
        .api
        .call("rename_preview", json!({
            "paths": [], "rules": [{ "type": "Replace", "find": "(", "with": "", "regex": true }]
        }))
        .unwrap_err();
    assert!(bad_regex.contains("invalid pattern"), "{bad_regex}");
}

#[test]
fn search_and_compare_and_sync_through_json() {
    let e = env();
    let (l, r) = (e.root.join("L"), e.root.join("R"));
    fs::create_dir_all(&l).unwrap();
    fs::create_dir_all(&r).unwrap();
    fs::write(l.join("only_left.txt"), "hello world").unwrap();
    fs::write(r.join("only_right.txt"), "bye").unwrap();

    let hits = e
        .api
        .call("search", json!({ "query": {
            "root": s(&e.root), "name": ".txt", "content": "hello", "ignore_case": true
        }}))
        .unwrap();
    assert_eq!(hits["hits"].as_array().unwrap().len(), 1);
    assert_eq!(hits["hits"][0]["line"], 1);

    let c = e
        .api
        .call("compare", json!({ "left": s(&l), "right": s(&r), "mode": "Mirror" }))
        .unwrap();
    assert_eq!(c["items"].as_array().unwrap().len(), 2);
    assert_eq!(c["plan"]["actions"].as_array().unwrap().len(), 2);

    let rep = e
        .api
        .call("sync_apply", json!({ "left": s(&l), "right": s(&r), "plan": c["plan"] }))
        .unwrap();
    assert_eq!(rep["errors"].as_array().unwrap().len(), 0);
    assert!(r.join("only_left.txt").exists());
    assert!(!r.join("only_right.txt").exists());

    // The whole sync is one undo step.
    e.api.call("undo", json!({})).unwrap();
    assert!(r.join("only_right.txt").exists());
    assert!(!r.join("only_left.txt").exists());
}

#[test]
fn sync_apply_refuses_a_hand_crafted_escaping_plan() {
    let e = env();
    let (l, r) = (e.root.join("L"), e.root.join("R"));
    fs::create_dir_all(&l).unwrap();
    fs::create_dir_all(&r).unwrap();
    fs::write(e.root.join("keep.txt"), "keep").unwrap();
    let plan = json!({ "actions": [
        { "kind": "DeleteRight", "rel": "../keep.txt", "reason": "" }
    ], "skipped": [] });
    assert!(e
        .api
        .call("sync_apply", json!({ "left": s(&l), "right": s(&r), "plan": plan }))
        .is_err());
    assert!(e.root.join("keep.txt").exists());
}

#[test]
fn roots_and_home_in_sandbox_mode_are_the_sandbox() {
    let e = env();
    assert_eq!(e.api.call("home_dir", json!({})).unwrap(), json!(s(&e.root)));
    assert_eq!(e.api.call("roots", json!({})).unwrap(), json!([s(&e.root)]));
}

#[test]
fn open_is_disabled_in_the_sandbox() {
    let e = env();
    fs::write(e.root.join("a.txt"), "x").unwrap();
    assert!(e
        .api
        .call("open", json!({ "path": s(&e.root.join("a.txt")) }))
        .unwrap_err()
        .contains("preview"));
}

#[test]
fn list_rows_with_meta_includes_metadata_fields() {
    let e = env();
    fs::write(e.root.join("notes.txt"), "x").unwrap();
    let v = e
        .api
        .call("list_rows", json!({ "path": s(&e.root), "with_meta": true, "sort": "DateTaken", "order": "Desc" }))
        .unwrap();
    assert_eq!(names(&v), ["notes.txt"]);
}
