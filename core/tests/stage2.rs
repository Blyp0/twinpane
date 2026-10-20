use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use twinpane_core::*;

// ------------------------------------------------------------ test helpers

fn entry(t: &mut Vec<u8>, tag: u16, typ: u16, count: u32, val: u32) {
    t.extend_from_slice(&tag.to_le_bytes());
    t.extend_from_slice(&typ.to_le_bytes());
    t.extend_from_slice(&count.to_le_bytes());
    t.extend_from_slice(&val.to_le_bytes());
}

/// Build a minimal JPEG containing an EXIF block (little-endian TIFF).
fn exif_jpeg(date: &str, make: &str, model: &str, w: u32, h: u32) -> Vec<u8> {
    let mk = format!("{make}\0");
    let md = format!("{model}\0");
    let dt = format!("{date}\0");
    let pad = |n: usize| (n + n % 2) as u32;
    let ifd_size = 2 + 3 * 12 + 4;
    let make_off = 8 + ifd_size;
    let model_off = make_off + pad(mk.len());
    let exif_off = model_off + pad(md.len());
    let date_off = exif_off + ifd_size;

    let mut t = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    entry(&mut t, 0x010F, 2, mk.len() as u32, make_off);
    entry(&mut t, 0x0110, 2, md.len() as u32, model_off);
    entry(&mut t, 0x8769, 4, 1, exif_off);
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(mk.as_bytes());
    if mk.len() % 2 == 1 {
        t.push(0);
    }
    t.extend_from_slice(md.as_bytes());
    if md.len() % 2 == 1 {
        t.push(0);
    }
    assert_eq!(t.len() as u32, exif_off);
    t.extend_from_slice(&3u16.to_le_bytes());
    entry(&mut t, 0x9003, 2, dt.len() as u32, date_off);
    entry(&mut t, 0xA002, 4, 1, w);
    entry(&mut t, 0xA003, 4, 1, h);
    t.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(t.len() as u32, date_off);
    t.extend_from_slice(dt.as_bytes());

    let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
    j.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    j.extend_from_slice(b"Exif\0\0");
    j.extend_from_slice(&t);
    j.extend_from_slice(&[0xFF, 0xD9]);
    j
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
    fs::create_dir_all(root.join("d")).unwrap();
    Env {
        _tmp: tmp,
        root,
        engine,
    }
}

fn touch(p: &Path, content: &str) -> PathBuf {
    fs::write(p, content).unwrap();
    p.to_path_buf()
}

fn set_mtime(p: &Path, secs: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn replace(find: &str, with: &str) -> RenameRule {
    RenameRule::Replace {
        find: find.into(),
        with: with.into(),
        regex: false,
        ignore_case: false,
        target: Target::Stem,
    }
}

fn regex_replace(find: &str, with: &str) -> RenameRule {
    RenameRule::Replace {
        find: find.into(),
        with: with.into(),
        regex: true,
        ignore_case: false,
        target: Target::Stem,
    }
}

fn new_names(plan: &RenamePlan) -> Vec<&str> {
    plan.items.iter().map(|i| i.new_name.as_str()).collect()
}

// ------------------------------------------------------------- date / time

#[test]
fn date_parse_accepts_exif_and_rejects_junk() {
    let d = DateTime::parse_exif("2023:04:05 17:27:09").unwrap();
    assert_eq!(
        (d.year, d.month, d.day, d.hour, d.minute, d.second),
        (2023, 4, 5, 17, 27, 9)
    );
    assert!(DateTime::parse_exif("2023-04-05 17:27:09").is_some());
    assert!(DateTime::parse_exif("0000:00:00 00:00:00").is_none());
    assert!(DateTime::parse_exif("2023:13:05 17:27:09").is_none());
    assert!(DateTime::parse_exif("2023:04:05 25:00:00").is_none());
    assert!(DateTime::parse_exif("garbage").is_none());
    assert!(DateTime::parse_exif("").is_none());
}

#[test]
fn date_unix_conversion_round_trips_and_matches_known_values() {
    let epoch = DateTime::from_unix(0, 0);
    assert_eq!((epoch.year, epoch.month, epoch.day), (1970, 1, 1));
    assert_eq!(epoch.to_naive_unix(), 0);
    // 2000-03-01 00:00:00 UTC
    let d = DateTime::from_unix(951_868_800, 0);
    assert_eq!((d.year, d.month, d.day), (2000, 3, 1));
    // Leap day and a pre-1970 date
    for secs in [951_782_400i64, -86_400, 1_700_000_000, 4_102_444_800] {
        assert_eq!(DateTime::from_unix(secs, 0).to_naive_unix(), secs);
    }
    // Offsets shift the wall clock.
    let shifted = DateTime::from_unix(1_700_000_000, 120);
    assert_eq!((shifted.day, shifted.hour), (15, 0));
}

#[test]
fn date_format_codes() {
    let d = DateTime::parse_exif("2023:04:05 07:08:09").unwrap();
    assert_eq!(d.format("%Y-%m-%d %H:%M:%S"), "2023-04-05 07:08:09");
    assert_eq!(d.format("%y%m%d"), "230405");
    assert_eq!(d.format("100%% %q"), "100% %q");
    assert!(DateTime::parse_exif("2022:12:31 23:59:59").unwrap() < d);
}

// ---------------------------------------------------------------- metadata

#[test]
fn reads_exif_date_taken_camera_and_size() {
    let e = env();
    let p = e.root.join("d/a.jpg");
    fs::write(&p, exif_jpeg("2023:04:05 17:27:00", "Canon", "EOS R5", 8192, 5464)).unwrap();
    let m = read_meta(&p).unwrap();
    assert_eq!(m.date_taken, DateTime::parse_exif("2023:04:05 17:27:00"));
    assert_eq!(m.make.as_deref(), Some("Canon"));
    assert_eq!(m.model.as_deref(), Some("EOS R5"));
    assert_eq!(m.camera().as_deref(), Some("Canon EOS R5"));
    assert_eq!((m.width, m.height), (Some(8192), Some(5464)));
}

#[test]
fn camera_label_does_not_repeat_the_make() {
    let m = FileMeta {
        make: Some("Canon".into()),
        model: Some("Canon EOS R5".into()),
        ..Default::default()
    };
    assert_eq!(m.camera().as_deref(), Some("Canon EOS R5"));
}

#[test]
fn files_without_exif_give_empty_metadata_not_errors() {
    let e = env();
    let txt = touch(&e.root.join("d/a.txt"), "hello");
    assert_eq!(read_meta(&txt).unwrap(), FileMeta::default());
    let fake = touch(&e.root.join("d/fake.jpg"), "not really a jpeg");
    assert_eq!(read_meta(&fake).unwrap(), FileMeta::default());
    assert!(read_meta(&e.root.join("d/missing.jpg")).is_err());
}

#[test]
fn rows_sort_by_date_taken_with_missing_values_last() {
    let e = env();
    let d = e.root.join("d");
    fs::write(d.join("b.jpg"), exif_jpeg("2021:06:01 10:00:00", "Nikon", "Z6", 1, 1)).unwrap();
    fs::write(d.join("a.jpg"), exif_jpeg("2023:01:01 10:00:00", "Nikon", "Z6", 1, 1)).unwrap();
    fs::write(d.join("c.jpg"), exif_jpeg("2019:03:03 10:00:00", "Sony", "A7", 1, 1)).unwrap();
    touch(&d.join("none.txt"), "x");
    fs::create_dir_all(d.join("folder")).unwrap();

    let opts = ListOptions::default();
    let asc = list_rows(&d, &opts, RowSort::DateTaken, true).unwrap();
    let n: Vec<&str> = asc.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(n, ["folder", "c.jpg", "b.jpg", "a.jpg", "none.txt"]);

    let desc_opts = ListOptions {
        order: SortOrder::Desc,
        ..Default::default()
    };
    let desc = list_rows(&d, &desc_opts, RowSort::DateTaken, true).unwrap();
    let n: Vec<&str> = desc.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(n, ["folder", "a.jpg", "b.jpg", "c.jpg", "none.txt"]);

    let by_cam = list_rows(&d, &opts, RowSort::Camera, true).unwrap();
    let n: Vec<&str> = by_cam.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(n, ["folder", "a.jpg", "b.jpg", "c.jpg", "none.txt"]); // Nikon, Nikon, Sony

    // Metadata is only read on request.
    let lazy = list_rows(&d, &opts, RowSort::Basic(SortKey::Name), false).unwrap();
    assert!(lazy.iter().all(|r| r.meta.is_none()));
}

// ------------------------------------------------------------------ rename

#[test]
fn regex_replace_uses_captures_and_keeps_extension() {
    let e = env();
    let a = touch(&e.root.join("d/holiday_001.txt"), "");
    let b = touch(&e.root.join("d/holiday_002.txt"), "");
    let plan = build_plan(
        &[a, b],
        &[regex_replace(r"holiday_(\d+)", "trip-$1")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["trip-001.txt", "trip-002.txt"]);
    assert!(plan.is_applicable());
}

#[test]
fn literal_replace_is_not_treated_as_regex() {
    let e = env();
    let a = touch(&e.root.join("d/a.b(1).txt"), "");
    let plan = build_plan(
        &[a],
        &[RenameRule::Replace {
            find: ".b(1)".into(),
            with: "$1".into(),
            regex: false,
            ignore_case: false,
            target: Target::Stem,
        }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["a$1.txt"]);
}

#[test]
fn ignore_case_replace() {
    let e = env();
    let a = touch(&e.root.join("d/Photo PHOTO photo.txt"), "");
    let plan = build_plan(
        &[a],
        &[RenameRule::Replace {
            find: "photo".into(),
            with: "p".into(),
            regex: false,
            ignore_case: true,
            target: Target::Stem,
        }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["p p p.txt"]);
}

#[test]
fn counter_token_with_padding_start_and_step() {
    let e = env();
    let files: Vec<PathBuf> = ["x.jpg", "y.jpg", "z.jpg"]
        .iter()
        .map(|n| touch(&e.root.join("d").join(n), ""))
        .collect();
    let plan = build_plan(
        &files,
        &[regex_replace("^.*$", "photo_{n:3}")],
        &RenameOptions {
            counter_start: 10,
            counter_step: 5,
            utc_offset_minutes: 0,
        },
    )
    .unwrap();
    assert_eq!(
        new_names(&plan),
        ["photo_010.jpg", "photo_015.jpg", "photo_020.jpg"]
    );
}

#[test]
fn taken_token_uses_exif_and_flags_files_without_it() {
    let mut e = env();
    let d = e.root.join("d");
    let with = d.join("IMG_1.jpg");
    fs::write(&with, exif_jpeg("2023:04:05 17:27:00", "Canon", "EOS R5", 1, 1)).unwrap();
    let without = touch(&d.join("IMG_2.jpg"), "no exif here");

    let rules = [regex_replace("^.*$", "{taken:%Y%m%d_%H%M%S}")];
    let plan = build_plan(&[with.clone(), without], &rules, &RenameOptions::default()).unwrap();
    assert_eq!(plan.items[0].new_name, "20230405_172700.jpg");
    assert_eq!(plan.items[0].status, ItemStatus::Ok);
    assert_eq!(
        plan.items[1].status,
        ItemStatus::MissingMetadata("taken".into())
    );
    assert_eq!(plan.items[1].new_name, "IMG_2.jpg");
    assert!(!plan.is_applicable());
    assert!(apply_plan(&mut e.engine, &plan).is_err());
    assert!(with.exists(), "a refused plan must not touch any file");

    // Renaming just the file that has the data works.
    let ok = build_plan(&[with], &rules, &RenameOptions::default()).unwrap();
    assert_eq!(apply_plan(&mut e.engine, &ok).unwrap(), 1);
    assert!(d.join("20230405_172700.jpg").exists());
}

#[test]
fn camera_and_size_tokens() {
    let e = env();
    let p = e.root.join("d/a.jpg");
    fs::write(&p, exif_jpeg("2023:04:05 17:27:00", "Canon", "EOS R5", 800, 600)).unwrap();
    let plan = build_plan(
        &[p],
        &[RenameRule::Insert {
            text: "_{camera}_{width}x{height}".into(),
            at: Position::End,
            target: Target::Stem,
        }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["a_Canon EOS R5_800x600.jpg"]);
}

#[test]
fn modified_token_honours_utc_offset() {
    let e = env();
    let p = touch(&e.root.join("d/a.txt"), "x");
    set_mtime(&p, 1_700_000_000); // 2023-11-14 22:13:20 UTC
    let rules = [regex_replace("^.*$", "{modified:%Y-%m-%d_%H}")];
    let utc = build_plan(&[p.clone()], &rules, &RenameOptions::default()).unwrap();
    assert_eq!(new_names(&utc), ["2023-11-14_22.txt"]);
    let plus2 = build_plan(
        &[p],
        &rules,
        &RenameOptions {
            utc_offset_minutes: 120,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(new_names(&plus2), ["2023-11-15_00.txt"]);
}

#[test]
fn name_stem_ext_and_parent_tokens() {
    let e = env();
    let p = touch(&e.root.join("d/report.final.pdf"), "x");
    let plan = build_plan(
        &[p],
        &[RenameRule::Insert {
            text: "{parent}-".into(),
            at: Position::Start,
            target: Target::Stem,
        }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["d-report.final.pdf"]);

    let p2 = touch(&e.root.join("d/song.mp3"), "x");
    let plan2 = build_plan(
        &[p2],
        &[replace("song", "{stem}[{ext}]")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan2), ["song[mp3].mp3"]);
}

#[test]
fn unknown_tokens_are_left_alone() {
    let e = env();
    let p = touch(&e.root.join("d/a.txt"), "x");
    let plan = build_plan(
        &[p],
        &[replace("a", "{foo}-{n")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["{foo}-{n.txt"]);
}

#[test]
fn dollar_signs_in_token_values_are_not_captures() {
    let e = env();
    // A file whose name contains `$1` must survive {stem} inside a regex replace.
    let p = touch(&e.root.join("d/cost$1.txt"), "x");
    let plan = build_plan(
        &[p],
        &[regex_replace("^(.*)$", "[{stem}]")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["[cost$1].txt"]);
}

#[test]
fn insert_positions() {
    let e = env();
    let p = touch(&e.root.join("d/abcdef.txt"), "x");
    let mk = |at| {
        build_plan(
            &[p.clone()],
            &[RenameRule::Insert {
                text: "-".into(),
                at,
                target: Target::Stem,
            }],
            &RenameOptions::default(),
        )
        .unwrap()
    };
    assert_eq!(new_names(&mk(Position::Start)), ["-abcdef.txt"]);
    assert_eq!(new_names(&mk(Position::End)), ["abcdef-.txt"]);
    assert_eq!(new_names(&mk(Position::Index(3))), ["abc-def.txt"]);
    assert_eq!(new_names(&mk(Position::Index(99))), ["abcdef-.txt"]);
}

#[test]
fn case_and_trim_rules() {
    let e = env();
    let p = touch(&e.root.join("d/my_big FILE.TXT"), "x");
    let case = |mode, target| {
        build_plan(
            &[p.clone()],
            &[RenameRule::Case { mode, target }],
            &RenameOptions::default(),
        )
        .unwrap()
    };
    assert_eq!(new_names(&case(CaseMode::Upper, Target::Stem)), ["MY_BIG FILE.TXT"]);
    assert_eq!(new_names(&case(CaseMode::Title, Target::Stem)), ["My_Big File.TXT"]);
    assert_eq!(new_names(&case(CaseMode::Lower, Target::Name)), ["my_big file.txt"]);
    assert_eq!(new_names(&case(CaseMode::Sentence, Target::Stem)), ["My_big file.TXT"]);

    let spaced = touch(&e.root.join("d/  a   b .txt"), "x");
    let plan = build_plan(
        &[spaced],
        &[RenameRule::Trim { target: Target::Stem }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["a b.txt"]);
}

#[test]
fn extension_target_leaves_directories_alone() {
    let e = env();
    let f = touch(&e.root.join("d/a.jpeg"), "x");
    let dir = e.root.join("d/pics.jpeg");
    fs::create_dir_all(&dir).unwrap();
    let plan = build_plan(
        &[f, dir],
        &[RenameRule::Replace {
            find: "jpeg".into(),
            with: "jpg".into(),
            regex: false,
            ignore_case: false,
            target: Target::Extension,
        }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.items[0].new_name, "a.jpg");
    assert_eq!(plan.items[1].status, ItemStatus::Unchanged);
}

#[test]
fn rules_chain_in_order() {
    let e = env();
    let p = touch(&e.root.join("d/IMG 001.JPG"), "x");
    let plan = build_plan(
        &[p],
        &[
            replace("IMG ", "holiday_"),
            RenameRule::Case {
                mode: CaseMode::Lower,
                target: Target::Name,
            },
            RenameRule::Insert {
                text: "2024_".into(),
                at: Position::Start,
                target: Target::Stem,
            },
        ],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["2024_holiday_001.jpg"]);
}

#[test]
fn invalid_regex_is_a_pattern_error() {
    let e = env();
    let p = touch(&e.root.join("d/a.txt"), "x");
    let r = build_plan(&[p], &[regex_replace("(", "x")], &RenameOptions::default());
    assert!(matches!(r, Err(Error::Pattern(_))));
}

#[test]
fn invalid_resulting_names_are_flagged() {
    let e = env();
    let p = touch(&e.root.join("d/a.txt"), "x");
    let plan = build_plan(&[p.clone()], &[replace("a", "x/y")], &RenameOptions::default()).unwrap();
    assert!(matches!(plan.items[0].status, ItemStatus::Invalid(_)));
    let empty = build_plan(&[p], &[replace("a", ""), RenameRule::Replace {
        find: ".txt".into(),
        with: "".into(),
        regex: false,
        ignore_case: false,
        target: Target::Name,
    }], &RenameOptions::default())
    .unwrap();
    assert!(matches!(empty.items[0].status, ItemStatus::Invalid(_)));
}

#[test]
fn duplicate_targets_in_the_batch_are_flagged_including_unchanged_items() {
    let e = env();
    let a = touch(&e.root.join("d/a.txt"), "A");
    let b = touch(&e.root.join("d/b.txt"), "B");
    // b -> a collides with a, which does not change.
    let plan = build_plan(
        &[a.clone(), b.clone()],
        &[replace("b", "a")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.items[0].status, ItemStatus::Unchanged);
    assert!(matches!(plan.items[1].status, ItemStatus::Duplicate(_)));

    // Two items becoming the same name are both flagged.
    let c = touch(&e.root.join("d/c1.txt"), "");
    let d = touch(&e.root.join("d/c2.txt"), "");
    let plan2 = build_plan(
        &[c, d],
        &[regex_replace(r"\d", "")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert!(plan2.items.iter().all(|i| matches!(i.status, ItemStatus::Duplicate(_))));
}

#[test]
fn collisions_with_files_outside_the_batch_are_flagged() {
    let e = env();
    touch(&e.root.join("d/a.txt"), "existing");
    let b = touch(&e.root.join("d/b.txt"), "B");
    let plan = build_plan(&[b], &[replace("b", "a")], &RenameOptions::default()).unwrap();
    assert!(matches!(plan.items[0].status, ItemStatus::Exists(_)));
}

#[test]
fn apply_renames_everything_in_one_undo_step() {
    let mut e = env();
    let files: Vec<PathBuf> = (1..=3)
        .map(|i| touch(&e.root.join(format!("d/f{i}.txt")), &format!("content{i}")))
        .collect();
    let plan = build_plan(
        &files,
        &[regex_replace("^f", "renamed_")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(apply_plan(&mut e.engine, &plan).unwrap(), 3);
    for i in 1..=3 {
        assert_eq!(
            fs::read_to_string(e.root.join(format!("d/renamed_{i}.txt"))).unwrap(),
            format!("content{i}")
        );
        assert!(!e.root.join(format!("d/f{i}.txt")).exists());
    }
    assert_eq!(e.engine.undo_log().len(), 1);
    e.engine.undo().unwrap();
    for i in 1..=3 {
        assert_eq!(
            fs::read_to_string(e.root.join(format!("d/f{i}.txt"))).unwrap(),
            format!("content{i}")
        );
    }
}

#[test]
fn swapping_two_names_works_and_undoes() {
    let mut e = env();
    let a = touch(&e.root.join("d/a.txt"), "was-a");
    let b = touch(&e.root.join("d/b.txt"), "was-b");
    // a->b and b->a using a chain of rules.
    let plan = build_plan(
        &[a, b],
        &[replace("a", "@"), replace("b", "a"), replace("@", "b")],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(new_names(&plan), ["b.txt", "a.txt"]);
    assert!(plan.is_applicable(), "{plan:?}");
    apply_plan(&mut e.engine, &plan).unwrap();
    assert_eq!(fs::read_to_string(e.root.join("d/b.txt")).unwrap(), "was-a");
    assert_eq!(fs::read_to_string(e.root.join("d/a.txt")).unwrap(), "was-b");
    e.engine.undo().unwrap();
    assert_eq!(fs::read_to_string(e.root.join("d/a.txt")).unwrap(), "was-a");
    assert_eq!(fs::read_to_string(e.root.join("d/b.txt")).unwrap(), "was-b");
}

#[test]
fn case_only_rename_applies() {
    let mut e = env();
    let p = touch(&e.root.join("d/Photo.JPG"), "x");
    let plan = build_plan(
        &[p],
        &[RenameRule::Case {
            mode: CaseMode::Lower,
            target: Target::Name,
        }],
        &RenameOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.items[0].status, ItemStatus::Ok);
    apply_plan(&mut e.engine, &plan).unwrap();
    let names: Vec<String> = fs::read_dir(e.root.join("d"))
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"photo.jpg".to_string()), "{names:?}");
}

#[test]
fn rename_many_is_all_or_nothing() {
    let mut e = env();
    let a = touch(&e.root.join("d/a.txt"), "A");
    let b = touch(&e.root.join("d/b.txt"), "B");
    touch(&e.root.join("d/blocker.txt"), "blocker");
    // The second target already exists outside the set.
    let err = e.engine.rename_many(
        &[
            (a.clone(), e.root.join("d/a2.txt")),
            (b.clone(), e.root.join("d/blocker.txt")),
        ],
        "test",
    );
    assert!(err.is_err());
    assert!(a.exists() && b.exists(), "nothing may have changed");
    assert!(!e.root.join("d/a2.txt").exists());
    assert_eq!(
        fs::read_to_string(e.root.join("d/blocker.txt")).unwrap(),
        "blocker"
    );
    assert!(!e.engine.can_undo());
}

#[test]
fn rename_many_rejects_two_items_with_one_name() {
    let mut e = env();
    let a = touch(&e.root.join("d/a.txt"), "A");
    let b = touch(&e.root.join("d/b.txt"), "B");
    let err = e.engine.rename_many(
        &[
            (a.clone(), e.root.join("d/same.txt")),
            (b.clone(), e.root.join("d/SAME.txt")),
        ],
        "test",
    );
    assert!(err.is_err());
    assert!(a.exists() && b.exists());
}

#[test]
fn plan_with_no_changes_is_not_applicable() {
    let e = env();
    let p = touch(&e.root.join("d/a.txt"), "x");
    let plan = build_plan(&[p], &[], &RenameOptions::default()).unwrap();
    assert_eq!(plan.items[0].status, ItemStatus::Unchanged);
    assert!(!plan.is_applicable());
}

#[test]
fn rules_serialize_for_the_ui() {
    let json = r#"[
        {"type":"Replace","find":"a","with":"b","regex":true},
        {"type":"Insert","text":"x","at":{"Index":2}},
        {"type":"Insert","text":"y","at":"End","target":"Extension"},
        {"type":"Case","mode":"Title"},
        {"type":"Trim"}
    ]"#;
    let rules: Vec<RenameRule> = serde_json::from_str(json).unwrap();
    assert_eq!(rules.len(), 5);
    assert!(matches!(
        &rules[0],
        RenameRule::Replace { regex: true, ignore_case: false, target: Target::Stem, .. }
    ));
    assert!(matches!(
        &rules[1],
        RenameRule::Insert { at: Position::Index(2), .. }
    ));
}
