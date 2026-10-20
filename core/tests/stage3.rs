use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use twinpane_core::*;

struct Env {
    _tmp: TempDir,
    root: PathBuf,
    engine: Engine,
}

fn env() -> Env {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let engine = Engine::new(root.join("holding")).unwrap();
    Env {
        _tmp: tmp,
        root,
        engine,
    }
}

fn write(p: &Path, s: &str) -> PathBuf {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, s).unwrap();
    p.to_path_buf()
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn set_mtime(p: &Path, secs: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn hit_names(o: &SearchOutcome) -> Vec<&str> {
    o.hits.iter().map(|h| h.entry.name.as_str()).collect()
}

/// A tree used by the search tests.
fn search_tree(root: &Path) -> PathBuf {
    let s = root.join("s");
    write(&s.join("notes.txt"), "todo: buy milk\nsecond line\n");
    write(&s.join("Report.TXT"), "quarterly report\nTODO: send\n");
    write(&s.join("img_001.jpg"), "jpegdata");
    write(&s.join("img_002.jpg"), "jpegdata-longer");
    write(&s.join("sub/deep/img_003.jpg"), "x");
    write(&s.join("sub/readme.md"), "# title\nTodo item\n");
    write(&s.join(".hidden/secret.txt"), "todo hidden");
    write(&s.join("data.bin"), "todo\0binary");
    fs::create_dir_all(s.join("emptydir")).unwrap();
    s
}

// ------------------------------------------------------------------ search

#[test]
fn substring_search_ignoring_case() {
    let e = env();
    let s = search_tree(&e.root);
    let mut q = SearchQuery::new(&s);
    q.name = Some("REPORT".into());
    q.ignore_case = true;
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["Report.TXT"]);
    q.ignore_case = false;
    assert!(search(&q, None).unwrap().hits.is_empty());
}

#[test]
fn glob_matches_the_whole_name() {
    let e = env();
    let s = search_tree(&e.root);
    let mut q = SearchQuery::new(&s);
    q.name = Some("img_00?.jpg".into());
    q.name_mode = NameMode::Glob;
    assert_eq!(
        hit_names(&search(&q, None).unwrap()),
        ["img_001.jpg", "img_002.jpg", "img_003.jpg"]
    );
    q.name = Some("img".into()); // no wildcard: must match exactly
    assert!(search(&q, None).unwrap().hits.is_empty());
    q.name = Some("*.md".into());
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["readme.md"]);
}

#[test]
fn regex_search_and_invalid_pattern() {
    let e = env();
    let s = search_tree(&e.root);
    let mut q = SearchQuery::new(&s);
    q.name = Some(r"^img_00[12]\.jpg$".into());
    q.name_mode = NameMode::Regex;
    assert_eq!(
        hit_names(&search(&q, None).unwrap()),
        ["img_001.jpg", "img_002.jpg"]
    );
    q.name = Some("(".into());
    assert!(matches!(search(&q, None), Err(Error::Pattern(_))));
}

#[test]
fn filters_by_kind_extension_and_size() {
    let e = env();
    let s = search_tree(&e.root);

    let mut dirs = SearchQuery::new(&s);
    dirs.kind = KindFilter::Dir;
    assert_eq!(
        hit_names(&search(&dirs, None).unwrap()),
        ["emptydir", "sub", "deep"]
    );

    let mut jpgs = SearchQuery::new(&s);
    jpgs.extensions = vec![".JPG".into()];
    assert_eq!(search(&jpgs, None).unwrap().hits.len(), 3);

    let mut big = SearchQuery::new(&s);
    big.extensions = vec!["jpg".into()];
    big.min_size = Some(10);
    assert_eq!(hit_names(&search(&big, None).unwrap()), ["img_002.jpg"]);
    big.min_size = None;
    big.max_size = Some(1);
    assert_eq!(hit_names(&search(&big, None).unwrap()), ["img_003.jpg"]);
}

#[test]
fn filters_by_modified_time() {
    let e = env();
    let s = search_tree(&e.root);
    set_mtime(&s.join("notes.txt"), 1_000_000_000);
    set_mtime(&s.join("Report.TXT"), 1_500_000_000);
    let mut q = SearchQuery::new(&s);
    q.extensions = vec!["txt".into()];
    q.modified_before = Some(1_200_000_000);
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["notes.txt"]);
    q.modified_before = None;
    q.modified_after = Some(1_200_000_000);
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["Report.TXT"]);
}

#[test]
fn hidden_items_are_skipped_unless_requested() {
    let e = env();
    let s = search_tree(&e.root);
    let mut q = SearchQuery::new(&s);
    q.name = Some("secret".into());
    assert!(search(&q, None).unwrap().hits.is_empty());
    q.include_hidden = true;
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["secret.txt"]);
}

#[test]
fn depth_and_result_limits() {
    let e = env();
    let s = search_tree(&e.root);
    let mut q = SearchQuery::new(&s);
    q.name = Some("img_".into());
    q.max_depth = Some(0);
    assert_eq!(search(&q, None).unwrap().hits.len(), 2); // not sub/deep
    q.max_depth = Some(1);
    assert_eq!(search(&q, None).unwrap().hits.len(), 2); // deep is depth 2
    q.max_depth = Some(2);
    assert_eq!(search(&q, None).unwrap().hits.len(), 3);

    q.max_depth = None;
    q.max_results = Some(1);
    let out = search(&q, None).unwrap();
    assert_eq!(out.hits.len(), 1);
    assert!(out.truncated);
}

#[test]
fn content_search_reports_line_and_skips_binary_files() {
    let e = env();
    let s = search_tree(&e.root);
    let mut q = SearchQuery::new(&s);
    q.content = Some("todo".into());
    q.ignore_case = true;
    let out = search(&q, None).unwrap();
    // notes.txt (line 1), Report.TXT (line 2), readme.md (line 2); not the
    // binary file and not the hidden folder.
    assert_eq!(hit_names(&out), ["notes.txt", "Report.TXT", "readme.md"]);
    assert_eq!(out.hits[0].line, Some(1));
    assert_eq!(out.hits[1].line, Some(2));
    assert_eq!(out.hits[1].snippet.as_deref(), Some("TODO: send"));

    q.ignore_case = false; // case-sensitive: only the lowercase "todo"
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["notes.txt"]);
}

#[test]
fn content_regex_and_size_cap() {
    let e = env();
    let s = e.root.join("c");
    write(&s.join("a.txt"), "id=42\n");
    write(&s.join("b.txt"), "id=abc\n");
    write(&s.join("huge.txt"), &format!("{}id=7\n", "x".repeat(2000)));
    let mut q = SearchQuery::new(&s);
    q.content = Some(r"id=\d+".into());
    q.content_regex = true;
    q.max_content_bytes = 1000;
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["a.txt"]);
    q.max_content_bytes = 1_000_000;
    assert_eq!(hit_names(&search(&q, None).unwrap()), ["a.txt", "huge.txt"]);
}

#[test]
fn search_can_be_cancelled() {
    let e = env();
    let s = search_tree(&e.root);
    let cancel = AtomicBool::new(true);
    let out = search(&SearchQuery::new(&s), Some(&cancel)).unwrap();
    assert!(out.cancelled);
    assert!(out.hits.is_empty());
}

#[test]
fn missing_or_non_directory_root_is_an_error() {
    let e = env();
    assert!(search(&SearchQuery::new(e.root.join("nope")), None).is_err());
    let f = write(&e.root.join("file.txt"), "x");
    assert!(search(&SearchQuery::new(f), None).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_loops_do_not_hang_the_search() {
    let e = env();
    let s = e.root.join("loop");
    write(&s.join("a/file.txt"), "x");
    std::os::unix::fs::symlink(&s, s.join("a/back")).unwrap();
    let mut q = SearchQuery::new(&s);
    q.name = Some("file".into());
    let out = search(&q, None).unwrap();
    assert_eq!(hit_names(&out), ["file.txt"]);
}

#[test]
fn query_deserializes_from_minimal_json() {
    let q: SearchQuery = serde_json::from_str(r#"{"root":"/tmp","name":"x"}"#).unwrap();
    assert_eq!(q.name_mode, NameMode::Substring);
    assert!(q.max_content_bytes > 0);
}

// ----------------------------------------------------------------- compare

fn pair(root: &Path) -> (PathBuf, PathBuf) {
    let l = root.join("L");
    let r = root.join("R");
    fs::create_dir_all(&l).unwrap();
    fs::create_dir_all(&r).unwrap();
    (l, r)
}

fn status_of(diff: &[DiffItem], rel: &str) -> DiffStatus {
    diff.iter()
        .find(|d| d.rel == Path::new(rel))
        .unwrap_or_else(|| panic!("{rel} not in diff: {diff:#?}"))
        .status
}

#[test]
fn compare_classifies_every_case() {
    let e = env();
    let (l, r) = pair(&e.root);
    write(&l.join("same.txt"), "same");
    write(&r.join("same.txt"), "same");
    set_mtime(&l.join("same.txt"), 1_000);
    set_mtime(&r.join("same.txt"), 1_001); // within tolerance

    write(&l.join("only_left.txt"), "L");
    write(&r.join("only_right.txt"), "R");

    write(&l.join("newer_left.txt"), "new");
    write(&r.join("newer_left.txt"), "old");
    set_mtime(&l.join("newer_left.txt"), 5_000);
    set_mtime(&r.join("newer_left.txt"), 1_000);

    write(&l.join("newer_right.txt"), "old!!");
    write(&r.join("newer_right.txt"), "new");
    set_mtime(&l.join("newer_right.txt"), 1_000);
    set_mtime(&r.join("newer_right.txt"), 9_000);

    write(&l.join("tie.txt"), "aaa");
    write(&r.join("tie.txt"), "bbbb");
    set_mtime(&l.join("tie.txt"), 2_000);
    set_mtime(&r.join("tie.txt"), 2_000);

    write(&l.join("clash"), "i am a file");
    write(&r.join("clash/inner.txt"), "i am in a folder");

    let diff = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert_eq!(status_of(&diff, "same.txt"), DiffStatus::Same);
    assert_eq!(status_of(&diff, "only_left.txt"), DiffStatus::LeftOnly);
    assert_eq!(status_of(&diff, "only_right.txt"), DiffStatus::RightOnly);
    assert_eq!(status_of(&diff, "newer_left.txt"), DiffStatus::LeftNewer);
    assert_eq!(status_of(&diff, "newer_right.txt"), DiffStatus::RightNewer);
    assert_eq!(status_of(&diff, "tie.txt"), DiffStatus::Different);
    assert_eq!(status_of(&diff, "clash"), DiffStatus::TypeMismatch);
}

#[test]
fn compare_recurses_and_reports_one_sided_folders_once() {
    let e = env();
    let (l, r) = pair(&e.root);
    write(&l.join("both/a.txt"), "1");
    write(&r.join("both/a.txt"), "1");
    write(&l.join("both/only_l.txt"), "x");
    write(&l.join("newdir/x.txt"), "x");
    write(&l.join("newdir/y.txt"), "y");
    write(&r.join("olddir/z.txt"), "z");
    let diff = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert_eq!(status_of(&diff, "both/only_l.txt"), DiffStatus::LeftOnly);
    assert_eq!(status_of(&diff, "newdir"), DiffStatus::LeftOnly);
    assert_eq!(status_of(&diff, "olddir"), DiffStatus::RightOnly);
    assert!(
        !diff.iter().any(|d| d.rel == Path::new("newdir/x.txt")),
        "one-sided folders are not descended into"
    );
}

#[test]
fn content_method_catches_same_size_same_time_edits() {
    let e = env();
    let (l, r) = pair(&e.root);
    write(&l.join("f.txt"), "AAAA");
    write(&r.join("f.txt"), "BBBB");
    set_mtime(&l.join("f.txt"), 3_000);
    set_mtime(&r.join("f.txt"), 3_000);

    let quick = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert_eq!(status_of(&quick, "f.txt"), DiffStatus::Same); // documented limit

    let deep = compare_dirs(
        &l,
        &r,
        &CompareOptions {
            method: CompareMethod::Content,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(status_of(&deep, "f.txt"), DiffStatus::Different);
}

#[test]
fn content_method_ignores_timestamp_only_differences() {
    let e = env();
    let (l, r) = pair(&e.root);
    write(&l.join("f.txt"), "identical");
    write(&r.join("f.txt"), "identical");
    set_mtime(&l.join("f.txt"), 9_000);
    set_mtime(&r.join("f.txt"), 1_000);
    let quick = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert_eq!(status_of(&quick, "f.txt"), DiffStatus::LeftNewer);
    let deep = compare_dirs(
        &l,
        &r,
        &CompareOptions {
            method: CompareMethod::Content,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(status_of(&deep, "f.txt"), DiffStatus::Same);
}

#[test]
fn hidden_files_can_be_excluded_from_compare() {
    let e = env();
    let (l, r) = pair(&e.root);
    write(&l.join(".dotfile"), "x");
    let with = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert_eq!(with.len(), 1);
    let without = compare_dirs(
        &l,
        &r,
        &CompareOptions {
            include_hidden: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(without.is_empty());
}

#[test]
fn compare_rejects_missing_folders() {
    let e = env();
    let (l, _) = pair(&e.root);
    assert!(compare_dirs(&l, &e.root.join("nope"), &CompareOptions::default()).is_err());
}

// -------------------------------------------------------------------- plan

/// L: only_left, newer_left, tie, same   R: only_right, newer_right(newer), tie, same
fn plan_fixture(root: &Path) -> (PathBuf, PathBuf, Vec<DiffItem>) {
    let (l, r) = pair(root);
    write(&l.join("only_left.txt"), "L");
    write(&r.join("only_right.txt"), "R");
    write(&l.join("newer_left.txt"), "new");
    write(&r.join("newer_left.txt"), "old");
    set_mtime(&l.join("newer_left.txt"), 5_000);
    set_mtime(&r.join("newer_left.txt"), 1_000);
    write(&l.join("newer_right.txt"), "old!!");
    write(&r.join("newer_right.txt"), "new");
    set_mtime(&l.join("newer_right.txt"), 1_000);
    set_mtime(&r.join("newer_right.txt"), 9_000);
    write(&l.join("tie.txt"), "aaa");
    write(&r.join("tie.txt"), "bbbb");
    set_mtime(&l.join("tie.txt"), 2_000);
    set_mtime(&r.join("tie.txt"), 2_000);
    write(&l.join("same.txt"), "s");
    write(&r.join("same.txt"), "s");
    set_mtime(&l.join("same.txt"), 100);
    set_mtime(&r.join("same.txt"), 100);
    let diff = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    (l, r, diff)
}

fn kinds(plan: &SyncPlan) -> Vec<(String, SyncKind)> {
    plan.actions
        .iter()
        .map(|a| (a.rel.to_string_lossy().into_owned(), a.kind))
        .collect()
}

#[test]
fn update_mode_never_deletes_or_overwrites_newer_files() {
    let e = env();
    let (_, _, diff) = plan_fixture(&e.root);
    let plan = plan_sync(&diff, SyncMode::Update);
    assert_eq!(
        kinds(&plan),
        [
            ("newer_left.txt".to_string(), SyncKind::CopyToRight),
            ("only_left.txt".to_string(), SyncKind::CopyToRight),
        ]
    );
    assert_eq!(plan.skipped.len(), 1); // tie.txt
    assert_eq!(plan.skipped[0].0, Path::new("tie.txt"));
}

#[test]
fn mirror_mode_makes_right_match_left_and_removes_extras() {
    let e = env();
    let (_, _, diff) = plan_fixture(&e.root);
    let plan = plan_sync(&diff, SyncMode::Mirror);
    let k = kinds(&plan);
    assert!(k.contains(&("only_right.txt".into(), SyncKind::DeleteRight)));
    assert!(k.contains(&("newer_right.txt".into(), SyncKind::CopyToRight)));
    assert!(k.contains(&("tie.txt".into(), SyncKind::CopyToRight)));
    assert!(plan.skipped.is_empty());
}

#[test]
fn two_way_mode_lets_the_newest_win_and_never_deletes() {
    let e = env();
    let (_, _, diff) = plan_fixture(&e.root);
    let plan = plan_sync(&diff, SyncMode::TwoWay);
    let k = kinds(&plan);
    assert!(k.contains(&("only_left.txt".into(), SyncKind::CopyToRight)));
    assert!(k.contains(&("only_right.txt".into(), SyncKind::CopyToLeft)));
    assert!(k.contains(&("newer_left.txt".into(), SyncKind::CopyToRight)));
    assert!(k.contains(&("newer_right.txt".into(), SyncKind::CopyToLeft)));
    assert!(!k.iter().any(|(_, kind)| matches!(
        kind,
        SyncKind::DeleteLeft | SyncKind::DeleteRight
    )));
    assert_eq!(plan.skipped.len(), 1);
}

// ------------------------------------------------------------------- apply

#[test]
fn mirror_apply_then_one_undo_restores_everything() {
    let mut e = env();
    let (l, r, diff) = plan_fixture(&e.root);
    write(&l.join("nested/deeper/new.txt"), "brand new");
    let diff2 = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert!(diff2.len() > diff.len());
    let plan = plan_sync(&diff2, SyncMode::Mirror);

    let report = apply_sync(&mut e.engine, &l, &r, &plan).unwrap();
    assert!(report.is_clean(), "{report:?}");

    // Right now matches left.
    assert_eq!(read(&r.join("only_left.txt")), "L");
    assert_eq!(read(&r.join("newer_left.txt")), "new");
    assert_eq!(read(&r.join("newer_right.txt")), "old!!");
    assert_eq!(read(&r.join("tie.txt")), "aaa");
    assert_eq!(read(&r.join("nested/deeper/new.txt")), "brand new");
    assert!(!r.join("only_right.txt").exists());
    // Left is untouched.
    assert_eq!(read(&l.join("newer_right.txt")), "old!!");
    // Re-comparing shows nothing left to do.
    let again = compare_dirs(&l, &r, &CompareOptions::default()).unwrap();
    assert!(plan_sync(&again, SyncMode::Mirror).actions.is_empty());

    // A single undo puts everything back.
    assert_eq!(e.engine.undo_log().len(), 1);
    e.engine.undo().unwrap();
    assert_eq!(read(&r.join("only_right.txt")), "R");
    assert_eq!(read(&r.join("newer_left.txt")), "old");
    assert_eq!(read(&r.join("newer_right.txt")), "new");
    assert_eq!(read(&r.join("tie.txt")), "bbbb");
    assert!(!r.join("only_left.txt").exists());
    assert!(!r.join("nested").exists(), "created folders are undone too");
    assert_eq!(read(&l.join("tie.txt")), "aaa");
}

#[test]
fn two_way_apply_copies_both_directions() {
    let mut e = env();
    let (l, r, diff) = plan_fixture(&e.root);
    let plan = plan_sync(&diff, SyncMode::TwoWay);
    let report = apply_sync(&mut e.engine, &l, &r, &plan).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(read(&l.join("only_right.txt")), "R");
    assert_eq!(read(&r.join("only_left.txt")), "L");
    assert_eq!(read(&l.join("newer_right.txt")), "new");
    assert_eq!(read(&r.join("newer_left.txt")), "new");
    // The conflicting tie was left alone on both sides.
    assert_eq!(read(&l.join("tie.txt")), "aaa");
    assert_eq!(read(&r.join("tie.txt")), "bbbb");
}

#[test]
fn replaced_files_stay_recoverable_in_holding() {
    let mut e = env();
    let (l, r, diff) = plan_fixture(&e.root);
    let plan = plan_sync(&diff, SyncMode::Mirror);
    apply_sync(&mut e.engine, &l, &r, &plan).unwrap();
    // Before any undo, the deleted/replaced right-hand versions are in holding.
    fn find(dir: &Path, content: &str) -> bool {
        fs::read_dir(dir)
            .map(|rd| {
                rd.flatten().any(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        find(&p, content)
                    } else {
                        fs::read_to_string(&p).map(|s| s == content).unwrap_or(false)
                    }
                })
            })
            .unwrap_or(false)
    }
    assert!(find(e.engine.holding_dir(), "R")); // deleted only_right.txt
    assert!(find(e.engine.holding_dir(), "new")); // overwritten newer_right.txt
}

#[test]
fn apply_refuses_nested_or_identical_roots() {
    let mut e = env();
    let (l, _) = pair(&e.root);
    write(&l.join("sub/x.txt"), "x");
    let plan = SyncPlan::default();
    assert!(apply_sync(&mut e.engine, &l, &l, &plan).is_err());
    assert!(apply_sync(&mut e.engine, &l, &l.join("sub"), &plan).is_err());
    assert!(apply_sync(&mut e.engine, &l.join("sub"), &l, &plan).is_err());
}

#[test]
fn apply_refuses_paths_that_escape_the_roots() {
    let mut e = env();
    let (l, r) = pair(&e.root);
    write(&e.root.join("outside.txt"), "precious");
    for bad in ["../outside.txt", "/etc/passwd", "", "a/../../outside.txt"] {
        let plan = SyncPlan {
            actions: vec![SyncAction {
                kind: SyncKind::DeleteRight,
                rel: PathBuf::from(bad),
                reason: String::new(),
            }],
            skipped: vec![],
        };
        assert!(apply_sync(&mut e.engine, &l, &r, &plan).is_err(), "{bad:?}");
    }
    assert_eq!(read(&e.root.join("outside.txt")), "precious");
}

#[test]
fn a_failing_item_does_not_stop_the_rest() {
    let mut e = env();
    let (l, r) = pair(&e.root);
    write(&l.join("good.txt"), "good");
    let plan = SyncPlan {
        actions: vec![
            SyncAction {
                kind: SyncKind::CopyToRight,
                rel: PathBuf::from("missing.txt"),
                reason: String::new(),
            },
            SyncAction {
                kind: SyncKind::CopyToRight,
                rel: PathBuf::from("good.txt"),
                reason: String::new(),
            },
        ],
        skipped: vec![],
    };
    let report = apply_sync(&mut e.engine, &l, &r, &plan).unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(read(&r.join("good.txt")), "good");
}

// ------------------------------------------------------------------- group

#[test]
fn group_merges_operations_into_one_undo_step() {
    let mut e = env();
    let a = write(&e.root.join("a.txt"), "A");
    let b = write(&e.root.join("b.txt"), "B");
    e.engine.group("Two renames", |eng| {
        eng.rename(&a, "a2.txt").unwrap();
        eng.rename(&b, "b2.txt").unwrap();
    });
    assert_eq!(e.engine.undo_log().len(), 1);
    assert_eq!(e.engine.undo_log().descriptions(), ["Two renames"]);
    e.engine.undo().unwrap();
    assert_eq!(read(&e.root.join("a.txt")), "A");
    assert_eq!(read(&e.root.join("b.txt")), "B");
}

#[test]
fn group_with_nothing_or_one_step_behaves() {
    let mut e = env();
    e.engine.group("nothing", |_| {});
    assert!(!e.engine.can_undo());
    let a = write(&e.root.join("a.txt"), "A");
    e.engine.group("just one", |eng| {
        eng.rename(&a, "z.txt").unwrap();
    });
    assert_eq!(e.engine.undo_log().descriptions(), ["just one"]);
}
