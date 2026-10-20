//! File metadata beyond what the file system gives: EXIF "Date Taken", camera
//! make/model and pixel size, plus the row type used for list columns.

use std::cmp::Ordering;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{io_err, Result};
use crate::listing::{list_dir, sort_entries, Entry, EntryKind, ListOptions, SortKey, SortOrder};

/// A calendar date and wall-clock time with no time zone (that is how EXIF
/// stores it). Field order makes the derived `Ord` chronological.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

// Howard Hinnant's civil-date algorithms (public domain).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl DateTime {
    /// Parse `YYYY:MM:DD HH:MM:SS` (also accepts `-` as the date separator).
    /// All-zero dates, which some cameras write for "unknown", give `None`.
    pub fn parse_exif(s: &str) -> Option<DateTime> {
        let s = s.trim().trim_matches('\0');
        let (date, time) = s.split_once(' ')?;
        let mut d = date.split(|c| c == ':' || c == '-');
        let year: i32 = d.next()?.parse().ok()?;
        let month: u8 = d.next()?.parse().ok()?;
        let day: u8 = d.next()?.parse().ok()?;
        let mut t = time.split(':');
        let hour: u8 = t.next()?.parse().ok()?;
        let minute: u8 = t.next()?.parse().ok()?;
        let second: u8 = t.next()?.trim().parse().ok()?;
        if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        if hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        Some(DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second: second.min(59),
        })
    }

    /// Convert Unix seconds to a date-time, shifted by `utc_offset_minutes`.
    pub fn from_unix(secs: i64, utc_offset_minutes: i32) -> DateTime {
        let secs = secs + i64::from(utc_offset_minutes) * 60;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        DateTime {
            year: y as i32,
            month: m as u8,
            day: d as u8,
            hour: (rem / 3600) as u8,
            minute: ((rem % 3600) / 60) as u8,
            second: (rem % 60) as u8,
        }
    }

    /// Seconds since 1970-01-01 treating this value as if it were UTC.
    /// Only meaningful for ordering and arithmetic.
    pub fn to_naive_unix(&self) -> i64 {
        days_from_civil(
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        ) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// Format with `%Y %y %m %d %H %M %S` and `%%`. Unknown `%x` codes are
    /// kept as written.
    pub fn format(&self, fmt: &str) -> String {
        let mut out = String::new();
        let mut chars = fmt.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('Y') => out.push_str(&format!("{:04}", self.year)),
                Some('y') => out.push_str(&format!("{:02}", self.year.rem_euclid(100))),
                Some('m') => out.push_str(&format!("{:02}", self.month)),
                Some('d') => out.push_str(&format!("{:02}", self.day)),
                Some('H') => out.push_str(&format!("{:02}", self.hour)),
                Some('M') => out.push_str(&format!("{:02}", self.minute)),
                Some('S') => out.push_str(&format!("{:02}", self.second)),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub date_taken: Option<DateTime>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl FileMeta {
    /// "Canon EOS R5"-style label (make is skipped if the model already has it).
    pub fn camera(&self) -> Option<String> {
        match (&self.make, &self.model) {
            (Some(make), Some(model)) => {
                if model.to_lowercase().starts_with(&make.to_lowercase()) {
                    Some(model.clone())
                } else {
                    Some(format!("{make} {model}"))
                }
            }
            (None, Some(model)) => Some(model.clone()),
            (Some(make), None) => Some(make.clone()),
            (None, None) => None,
        }
    }
}

/// File extensions we try to read EXIF from.
pub fn has_exif_extension(ext: Option<&str>) -> bool {
    matches!(
        ext,
        Some("jpg" | "jpeg" | "jpe" | "tif" | "tiff" | "heic" | "heif" | "avif" | "png" | "webp")
    )
}

fn ascii_field(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Ascii(parts) => {
            let s = String::from_utf8_lossy(parts.first()?)
                .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                .to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        _ => None,
    }
}

fn uint_field(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    exif.get_field(tag, exif::In::PRIMARY)?.value.get_uint(0)
}

/// Read metadata from `path`. Files without EXIF (or of a type we don't read)
/// return an empty [`FileMeta`], not an error. Only failing to open the file
/// is an error.
pub fn read_meta(path: &Path) -> Result<FileMeta> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    if !has_exif_extension(ext.as_deref()) {
        return Ok(FileMeta::default());
    }
    let file = File::open(path).map_err(|e| io_err(path, e))?;
    let mut reader = BufReader::new(file);
    let exif = match exif::Reader::new().read_from_container(&mut reader) {
        Ok(x) => x,
        Err(_) => return Ok(FileMeta::default()),
    };
    let date_taken = [
        exif::Tag::DateTimeOriginal,
        exif::Tag::DateTimeDigitized,
        exif::Tag::DateTime,
    ]
    .iter()
    .find_map(|t| ascii_field(&exif, *t).and_then(|s| DateTime::parse_exif(&s)));
    Ok(FileMeta {
        date_taken,
        make: ascii_field(&exif, exif::Tag::Make),
        model: ascii_field(&exif, exif::Tag::Model),
        width: uint_field(&exif, exif::Tag::PixelXDimension)
            .or_else(|| uint_field(&exif, exif::Tag::ImageWidth)),
        height: uint_field(&exif, exif::Tag::PixelYDimension)
            .or_else(|| uint_field(&exif, exif::Tag::ImageLength)),
    })
}

/// A list row: the file-system entry plus optional metadata columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub entry: Entry,
    pub meta: Option<FileMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowSort {
    Basic(SortKey),
    DateTaken,
    Camera,
}

/// Sort rows. Rows with no value for a metadata key always sort last,
/// whichever direction is chosen.
pub fn sort_rows(rows: &mut [Row], key: RowSort, order: SortOrder, dirs_first: bool) {
    if let RowSort::Basic(k) = key {
        // Reuse the entry sorter, keeping meta attached.
        let mut entries: Vec<Entry> = rows.iter().map(|r| r.entry.clone()).collect();
        sort_entries(
            &mut entries,
            &ListOptions {
                show_hidden: true,
                sort: k,
                order,
                dirs_first,
            },
        );
        let mut by_path: std::collections::HashMap<_, _> = rows
            .iter()
            .map(|r| (r.entry.path.clone(), r.clone()))
            .collect();
        for (slot, e) in rows.iter_mut().zip(entries) {
            if let Some(r) = by_path.remove(&e.path) {
                *slot = r;
            }
        }
        return;
    }
    rows.sort_by(|a, b| {
        if dirs_first {
            let ad = a.entry.kind == EntryKind::Dir;
            let bd = b.entry.kind == EntryKind::Dir;
            if ad != bd {
                return bd.cmp(&ad);
            }
        }
        let ord = match key {
            RowSort::DateTaken => cmp_opt(
                a.meta.as_ref().and_then(|m| m.date_taken),
                b.meta.as_ref().and_then(|m| m.date_taken),
                order,
            ),
            RowSort::Camera => cmp_opt(
                a.meta.as_ref().and_then(|m| m.camera().map(|c| c.to_lowercase())),
                b.meta.as_ref().and_then(|m| m.camera().map(|c| c.to_lowercase())),
                order,
            ),
            RowSort::Basic(_) => Ordering::Equal,
        };
        ord.then_with(|| crate::listing::natural_cmp(&a.entry.name, &b.entry.name))
            .then_with(|| a.entry.name.cmp(&b.entry.name))
    });
}

fn cmp_opt<T: Ord>(a: Option<T>, b: Option<T>, order: SortOrder) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => {
            let o = x.cmp(&y);
            if order == SortOrder::Desc {
                o.reverse()
            } else {
                o
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// List a directory as rows. With `with_meta`, EXIF is read for image files
/// (this is slower, so UIs should only ask when a metadata column is visible).
pub fn list_rows(
    dir: &Path,
    opts: &ListOptions,
    sort: RowSort,
    with_meta: bool,
) -> Result<Vec<Row>> {
    let entries = list_dir(dir, opts)?;
    let mut rows: Vec<Row> = entries
        .into_iter()
        .map(|entry| {
            let meta = if with_meta
                && entry.kind == EntryKind::File
                && has_exif_extension(entry.extension.as_deref())
            {
                read_meta(&entry.path).ok()
            } else {
                None
            };
            Row { entry, meta }
        })
        .collect();
    sort_rows(&mut rows, sort, opts.order, opts.dirs_first);
    Ok(rows)
}
