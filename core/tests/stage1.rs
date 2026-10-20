use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use twinpane_core::*;

fn write(p: &Path, s: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, s).unwrap();
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

/// Is there a file anywhere under `dir` whose content equals `content`?
fn find_in(dir: &Path, content: &str) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if find_in(&p, content) {
                return true;
            }
        } else if fs::read_to_string(&p).map(|s| s == content).unwrap_or(false) {
            return true;
        }
    }
    false
}

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

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

// ---------------------------------------------------------------- listing

#[test]
fn list_sorts_dirs_first_and_naturally() {
    let e = env();
    let d = e.root.join("l");
    for f in ["file10.txt", "file2.txt", "File1.txt"] {
        write(&d.join(f), "x");
    }
    fs::create_dir_all(d.join("dirB")).unwrap();
    fs::create_dir_all(d.join("dira")).unwrap();
    let list = list_dir(&d, &ListOptions::default()).unwrap();
    assert_eq!(
        names(&list),
        ["dira", "dirB", "File1.txt", "file2.txt", "file10.txt"]
    );
}

#[test]
fn list_hides_dotfiles_unless_asked() {
    let e = env();
    let d = e.root.join("l");
    write(&d.join(".secret"), "x");
    write(&d.join("shown"), "x");
    let hidden = list_dir(&d, &ListOptions::default()).unwrap();
    assert_eq!(names(&hidden), ["shown"]);
    let all = list_dir(
        &d,
        &ListOptions {
            show_hidden: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(names(&all), [".secret", "shown"]);
}

#[test]
fn list_sorts_by_size_descending_and_reports_metadata() {
    let e = env();
    let d = e.root.join("l");
    write(&d.join("a"), "1");
    write(&d.join("b"), &"x".repeat(100));
    write(&d.join("c.TXT"), &"x".repeat(10));
    let list = list_dir(
        &d,
        &ListOptions {
            sort: SortKey::Size,
            order: SortOrder::Desc,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(names(&list), ["b", "c.TXT", "a"]);
    assert_eq!(list[0].size, 100);
    assert_eq!(list[1].extension.as_deref(), Some("txt"));
    assert!(list[0].modified.is_some());
}

#[test]
fn list_missing_dir_is_an_error() {
    let e = env();
    assert!(list_dir(&e.root.join("nope"), &ListOptions::default()).is_err());
}

// ------------------------------------------------------------------- copy

#[test]
fn copy_file_and_directory_tree() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "A");
    write(&e.root.join("src/d/b.txt"), "B");
    write(&e.root.join("src/d/deep/c.txt"), "C");
    let dest = e.root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let rep = e
        .engine
        .copy(
            &[e.root.join("src/a.txt"), e.root.join("src/d")],
            &dest,
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert!(rep.is_clean(), "{rep:?}");
    assert_eq!(rep.done.len(), 2);
    assert_eq!(read(&dest.join("a.txt")), "A");
    assert_eq!(read(&dest.join("d/b.txt")), "B");
    assert_eq!(read(&dest.join("d/deep/c.txt")), "C");
    // originals untouched
    assert_eq!(read(&e.root.join("src/a.txt")), "A");
    assert_eq!(read(&e.root.join("src/d/b.txt")), "B");
}

#[test]
fn copy_preserves_modified_time() {
    let mut e = env();
    let src = e.root.join("src/a.txt");
    write(&src, "A");
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs::OpenOptions::new()
        .write(true)
        .open(&src)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let dest = e.root.join("dest");
    fs::create_dir_all(&dest).unwrap();
    e.engine.copy(&[src], &dest, ConflictPolicy::Fail).unwrap();
    let m = fs::metadata(dest.join("a.txt")).unwrap().modified().unwrap();
    assert_eq!(m, old);
}

#[test]
fn conflict_fail_reports_error_and_changes_nothing() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "new");
    write(&e.root.join("dest/a.txt"), "old");
    let rep = e
        .engine
        .copy(
            &[e.root.join("src/a.txt")],
            &e.root.join("dest"),
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(rep.errors.len(), 1);
    assert!(rep.done.is_empty());
    assert_eq!(read(&e.root.join("dest/a.txt")), "old");
    assert!(!e.engine.can_undo(), "a no-op must not create an undo step");
}

#[test]
fn conflict_skip_leaves_existing_item() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "new");
    write(&e.root.join("dest/a.txt"), "old");
    let rep = e
        .engine
        .copy(
            &[e.root.join("src/a.txt")],
            &e.root.join("dest"),
            ConflictPolicy::Skip,
        )
        .unwrap();
    assert_eq!(rep.skipped.len(), 1);
    assert_eq!(read(&e.root.join("dest/a.txt")), "old");
}

#[test]
fn conflict_keep_both_picks_unique_name() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "new");
    write(&e.root.join("dest/a.txt"), "old");
    write(&e.root.join("dest/a (2).txt"), "older");
    let rep = e
        .engine
        .copy(
            &[e.root.join("src/a.txt")],
            &e.root.join("dest"),
            ConflictPolicy::KeepBoth,
        )
        .unwrap();
    assert!(rep.is_clean());
    assert_eq!(rep.done[0], e.root.join("dest/a (3).txt"));
    assert_eq!(read(&e.root.join("dest/a (3).txt")), "new");
    assert_eq!(read(&e.root.join("dest/a.txt")), "old");
    assert_eq!(read(&e.root.join("dest/a (2).txt")), "older");
}

#[test]
fn conflict_overwrite_keeps_old_item_recoverable() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "new");
    write(&e.root.join("dest/a.txt"), "old");
    let rep = e
        .engine
        .copy(
            &[e.root.join("src/a.txt")],
            &e.root.join("dest"),
            ConflictPolicy::Overwrite,
        )
        .unwrap();
    assert!(rep.is_clean());
    assert_eq!(read(&e.root.join("dest/a.txt")), "new");
    // The replaced file is not gone, it is held.
    assert!(find_in(e.engine.holding_dir(), "old"));

    e.engine.undo().unwrap();
    assert_eq!(read(&e.root.join("dest/a.txt")), "old");
    // Undoing a copy never deletes the copy; it is parked in holding.
    assert!(find_in(e.engine.holding_dir(), "new"));
}

#[test]
fn copy_onto_itself_is_skipped_not_destroyed() {
    let mut e = env();
    write(&e.root.join("d/a.txt"), "keep me");
    let rep = e
        .engine
        .copy(
            &[e.root.join("d/a.txt")],
            &e.root.join("d"),
            ConflictPolicy::Overwrite,
        )
        .unwrap();
    assert_eq!(rep.skipped.len(), 1);
    assert_eq!(read(&e.root.join("d/a.txt")), "keep me");
}

#[test]
fn copy_directory_into_itself_is_rejected() {
    let mut e = env();
    write(&e.root.join("d/a.txt"), "A");
    fs::create_dir_all(e.root.join("d/sub")).unwrap();
    let rep = e
        .engine
        .copy(
            &[e.root.join("d")],
            &e.root.join("d/sub"),
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(rep.errors.len(), 1);
    assert!(rep.errors[0].1.contains("into itself"), "{:?}", rep.errors);
    assert!(!e.root.join("d/sub/d").exists());
}

#[test]
fn batch_continues_after_a_missing_source() {
    let mut e = env();
    write(&e.root.join("src/ok.txt"), "ok");
    fs::create_dir_all(e.root.join("dest")).unwrap();
    let rep = e
        .engine
        .copy(
            &[e.root.join("src/missing.txt"), e.root.join("src/ok.txt")],
            &e.root.join("dest"),
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(rep.errors.len(), 1);
    assert_eq!(rep.done.len(), 1);
    assert_eq!(read(&e.root.join("dest/ok.txt")), "ok");
}

#[test]
fn copy_to_missing_destination_is_an_error() {
    let mut e = env();
    write(&e.root.join("a.txt"), "A");
    assert!(e
        .engine
        .copy(
            &[e.root.join("a.txt")],
            &e.root.join("nope"),
            ConflictPolicy::Fail
        )
        .is_err());
}

#[cfg(unix)]
#[test]
fn copy_keeps_symlinks_as_symlinks() {
    let mut e = env();
    write(&e.root.join("src/real.txt"), "R");
    std::os::unix::fs::symlink("real.txt", e.root.join("src/link")).unwrap();
    fs::create_dir_all(e.root.join("dest")).unwrap();
    e.engine
        .copy(
            &[e.root.join("src/link")],
            &e.root.join("dest"),
            ConflictPolicy::Fail,
        )
        .unwrap();
    let m = fs::symlink_metadata(e.root.join("dest/link")).unwrap();
    assert!(m.file_type().is_symlink());
}

// ------------------------------------------------------------------- move

#[test]
fn move_and_undo_round_trip() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "A");
    write(&e.root.join("src/d/b.txt"), "B");
    let dest = e.root.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let rep = e
        .engine
        .move_items(
            &[e.root.join("src/a.txt"), e.root.join("src/d")],
            &dest,
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert!(rep.is_clean());
    assert!(!e.root.join("src/a.txt").exists());
    assert_eq!(read(&dest.join("d/b.txt")), "B");

    let msg = e.engine.undo().unwrap();
    assert!(msg.starts_with("Undid: Move"));
    assert_eq!(read(&e.root.join("src/a.txt")), "A");
    assert_eq!(read(&e.root.join("src/d/b.txt")), "B");
    assert!(!dest.join("a.txt").exists());
}

#[test]
fn move_within_same_directory_is_a_noop() {
    let mut e = env();
    write(&e.root.join("d/a.txt"), "A");
    let rep = e
        .engine
        .move_items(
            &[e.root.join("d/a.txt")],
            &e.root.join("d"),
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(rep.skipped.len(), 1);
    assert!(rep.errors.is_empty());
    assert_eq!(read(&e.root.join("d/a.txt")), "A");
}

#[test]
fn move_overwrite_then_undo_restores_both() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "new");
    write(&e.root.join("dest/a.txt"), "old");
    e.engine
        .move_items(
            &[e.root.join("src/a.txt")],
            &e.root.join("dest"),
            ConflictPolicy::Overwrite,
        )
        .unwrap();
    assert_eq!(read(&e.root.join("dest/a.txt")), "new");
    e.engine.undo().unwrap();
    assert_eq!(read(&e.root.join("dest/a.txt")), "old");
    assert_eq!(read(&e.root.join("src/a.txt")), "new");
}

// ----------------------------------------------------------------- delete

#[test]
fn delete_is_recoverable() {
    let mut e = env();
    write(&e.root.join("x/f.txt"), "precious");
    write(&e.root.join("x/dir/g.txt"), "also precious");
    let rep = e
        .engine
        .delete(&[e.root.join("x/f.txt"), e.root.join("x/dir")])
        .unwrap();
    assert_eq!(rep.done.len(), 2);
    assert!(!e.root.join("x/f.txt").exists());
    assert!(find_in(e.engine.holding_dir(), "precious"));
    assert!(find_in(e.engine.holding_dir(), "also precious"));

    e.engine.undo().unwrap();
    assert_eq!(read(&e.root.join("x/f.txt")), "precious");
    assert_eq!(read(&e.root.join("x/dir/g.txt")), "also precious");
}

#[test]
fn delete_of_missing_path_is_reported() {
    let mut e = env();
    let rep = e.engine.delete(&[e.root.join("ghost")]).unwrap();
    assert_eq!(rep.errors.len(), 1);
    assert!(!e.engine.can_undo());
}

#[test]
fn cannot_delete_a_folder_that_contains_the_holding_area() {
    let mut e = env();
    // The holding area lives under e.root, so deleting e.root must be refused.
    let rep = e.engine.delete(&[e.root.clone()]).unwrap();
    assert_eq!(rep.errors.len(), 1);
    assert!(rep.errors[0].1.contains("holding area"), "{:?}", rep.errors);
    assert!(e.root.exists());
}

// ---------------------------------------------------------- rename / mkdir

#[test]
fn rename_and_undo() {
    let mut e = env();
    write(&e.root.join("a.txt"), "A");
    let new = e.engine.rename(&e.root.join("a.txt"), "b.txt").unwrap();
    assert_eq!(new, e.root.join("b.txt"));
    assert!(!e.root.join("a.txt").exists());
    e.engine.undo().unwrap();
    assert_eq!(read(&e.root.join("a.txt")), "A");
}

#[test]
fn rename_rejects_bad_names_and_clobbering() {
    let mut e = env();
    write(&e.root.join("a.txt"), "A");
    write(&e.root.join("b.txt"), "B");
    for bad in ["", ".", "..", "x/y", "x\\y"] {
        assert!(
            matches!(
                e.engine.rename(&e.root.join("a.txt"), bad),
                Err(Error::InvalidName(_))
            ),
            "{bad:?} should be rejected"
        );
    }
    assert!(matches!(
        e.engine.rename(&e.root.join("a.txt"), "b.txt"),
        Err(Error::AlreadyExists(_))
    ));
    assert_eq!(read(&e.root.join("b.txt")), "B");
    assert!(!e.engine.can_undo());
}

#[test]
fn rename_to_same_name_is_a_noop() {
    let mut e = env();
    write(&e.root.join("a.txt"), "A");
    e.engine.rename(&e.root.join("a.txt"), "a.txt").unwrap();
    assert!(!e.engine.can_undo());
}

#[test]
fn mkdir_and_undo_removes_empty_folder() {
    let mut e = env();
    let p = e.engine.mkdir(&e.root, "new").unwrap();
    assert!(p.is_dir());
    assert!(matches!(
        e.engine.mkdir(&e.root, "new"),
        Err(Error::AlreadyExists(_))
    ));
    e.engine.undo().unwrap();
    assert!(!p.exists());
}

#[test]
fn undo_of_mkdir_keeps_files_the_user_added() {
    let mut e = env();
    let p = e.engine.mkdir(&e.root, "new").unwrap();
    write(&p.join("mine.txt"), "mine");
    e.engine.undo().unwrap();
    assert!(!p.exists());
    assert!(find_in(e.engine.holding_dir(), "mine"));
}

// -------------------------------------------------------------------- undo

#[test]
fn nothing_to_undo() {
    let mut e = env();
    assert!(matches!(e.engine.undo(), Err(Error::NothingToUndo)));
}

#[test]
fn undo_is_blocked_when_original_location_is_occupied_and_retryable() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "moved");
    fs::create_dir_all(e.root.join("dest")).unwrap();
    e.engine
        .move_items(
            &[e.root.join("src/a.txt")],
            &e.root.join("dest"),
            ConflictPolicy::Fail,
        )
        .unwrap();
    // Someone creates a new file where the old one was.
    write(&e.root.join("src/a.txt"), "blocker");

    let err = e.engine.undo().unwrap_err();
    assert!(matches!(err, Error::Undo(_)), "{err}");
    assert_eq!(read(&e.root.join("src/a.txt")), "blocker");
    assert_eq!(read(&e.root.join("dest/a.txt")), "moved");
    assert!(e.engine.can_undo(), "failed undo must stay in the log");

    fs::remove_file(e.root.join("src/a.txt")).unwrap();
    e.engine.undo().unwrap();
    assert_eq!(read(&e.root.join("src/a.txt")), "moved");
}

#[test]
fn undo_steps_go_back_in_order() {
    let mut e = env();
    write(&e.root.join("a.txt"), "A");
    e.engine.rename(&e.root.join("a.txt"), "b.txt").unwrap();
    e.engine.rename(&e.root.join("b.txt"), "c.txt").unwrap();
    assert_eq!(e.engine.undo_log().len(), 2);
    e.engine.undo().unwrap();
    assert!(e.root.join("b.txt").exists());
    e.engine.undo().unwrap();
    assert!(e.root.join("a.txt").exists());
}

#[test]
fn undo_log_survives_save_and_load() {
    let mut e = env();
    write(&e.root.join("src/a.txt"), "A");
    let dest = e.root.join("dest");
    fs::create_dir_all(&dest).unwrap();
    e.engine
        .move_items(&[e.root.join("src/a.txt")], &dest, ConflictPolicy::Fail)
        .unwrap();

    let log_path = e.root.join("undo.json");
    e.engine.undo_log().save(&log_path).unwrap();
    let loaded = UndoLog::load(&log_path).unwrap();
    assert_eq!(loaded.len(), 1);

    // A brand-new engine (like after an app restart) can still undo.
    let mut engine2 = Engine::with_undo_log(e.root.join("holding"), loaded).unwrap();
    engine2.undo().unwrap();
    assert_eq!(read(&e.root.join("src/a.txt")), "A");
}

#[test]
fn loading_a_missing_log_gives_an_empty_one() {
    let e = env();
    let log = UndoLog::load(&e.root.join("none.json")).unwrap();
    assert!(log.is_empty());
}

#[test]
fn loading_a_corrupt_log_is_an_error_not_a_panic() {
    let e = env();
    let p = e.root.join("bad.json");
    fs::write(&p, "{ not json").unwrap();
    assert!(UndoLog::load(&p).is_err());
}

#[test]
fn empty_holding_is_permanent_and_clears_undo() {
    let mut e = env();
    write(&e.root.join("f.txt"), "gone soon");
    e.engine.delete(&[e.root.join("f.txt")]).unwrap();
    assert!(find_in(e.engine.holding_dir(), "gone soon"));
    e.engine.empty_holding().unwrap();
    assert!(!find_in(e.engine.holding_dir(), "gone soon"));
    assert!(!e.engine.can_undo());
}
