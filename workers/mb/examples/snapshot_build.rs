//! Build R2 snapshot objects from the curated ETL TSVs (phase 0 spike).
//!
//! ```text
//! snapshot_build artist    <out_dir> <mbdump-artist.tsv[.gz]>
//! snapshot_build recording <out_dir> <mbdump-recording.tsv[.gz]> [passes]
//! snapshot_build release   <out_dir> <mbdump-release.tsv[.gz]> <mbdump-artist.tsv[.gz]> [passes]
//! ```
//!
//! `release` writes both the release lookup kind and the release-search
//! index. Inputs are the curated files `containers/mb-import/etl.py`
//! publishes (COPY text, trailing `updated_at`); `.gz` inputs are read via
//! `gzip -dc`. Memory stays bounded by splitting the bucket space into
//! `passes` ranges, re-reading the input once per range. Each kind writes
//! `<stem>.idx`, `<stem>.dat.<part>` and `build-<stem>.json` (counts, sizes,
//! timings). Format: `workers/mb/src/snapshot.rs`.

use nntune_mb::snapshot::{
    deflate_block, encode_idx_entry, idx_object, key_bucket, label_sort_key, mbid_bucket,
    title_key, word_key, words, ArtistRow, BlockRef, Posting, RecordingRow, ReleaseRow, BUCKETS,
    IDX_ENTRY_LEN, IDX_LEN, MAX_PART_BYTES, MAX_SNAPSHOT_BLOCK_BYTES, MAX_TITLE_POSTINGS,
    MAX_WORD_POSTINGS, SEARCH_KIND,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

type Fallible<T> = Result<T, String>;

/// Open a curated TSV, decompressing `.gz` through `gzip -dc`.
struct Input {
    reader: BufReader<Box<dyn Read>>,
    child: Option<Child>,
}

impl Input {
    fn open(path: &Path) -> Fallible<Input> {
        if path.extension().is_some_and(|ext| ext == "gz") {
            let mut child = Command::new("gzip")
                .arg("-dc")
                .arg(path)
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| format!("gzip -dc {}: {e}", path.display()))?;
            let stdout = child.stdout.take().ok_or("gzip stdout")?;
            Ok(Input {
                reader: BufReader::with_capacity(1 << 20, Box::new(stdout)),
                child: Some(child),
            })
        } else {
            let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Input {
                reader: BufReader::with_capacity(1 << 20, Box::new(file)),
                child: None,
            })
        }
    }

    /// Visit every row's fields (COPY text, still escaped).
    fn for_each(
        mut self,
        expected: usize,
        mut visit: impl FnMut(&[&str]) -> Fallible<()>,
    ) -> Fallible<u64> {
        let mut line = String::new();
        let mut rows = 0u64;
        loop {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            rows += 1;
            let trimmed = line.strip_suffix('\n').unwrap_or(&line);
            let fields: Vec<&str> = trimmed.split('\t').collect();
            if fields.len() != expected {
                return Err(format!(
                    "row {rows}: expected {expected} fields, got {}",
                    fields.len()
                ));
            }
            visit(&fields)?;
        }
        if let Some(mut child) = self.child.take() {
            let status = child.wait().map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!("gzip -dc failed: {status}"));
            }
        }
        Ok(rows)
    }
}

/// PostgreSQL COPY text decoding: `\N` is NULL; backslash escapes
/// (`\b \f \n \r \t \v \\`, octal `\ddd`, hex `\xhh`) decode to bytes.
fn copy_text(field: &str) -> Fallible<Option<String>> {
    if field == r"\N" {
        return Ok(None);
    }
    if !field.contains('\\') {
        return Ok(Some(field.to_owned()));
    }
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let next = bytes[i + 1];
        i += 2;
        match next {
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut value = u32::from(next - b'0');
                let mut digits = 1;
                while digits < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                    value = value * 8 + u32::from(bytes[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push((value & 0xff) as u8);
            }
            b'x' if i < bytes.len() && bytes[i].is_ascii_hexdigit() => {
                let mut value = 0u32;
                let mut digits = 0;
                while digits < 2 && i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    value = value * 16 + (bytes[i] as char).to_digit(16).unwrap_or(0);
                    i += 1;
                    digits += 1;
                }
                out.push(value as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8(out)
        .map(Some)
        .map_err(|_| format!("invalid UTF-8 after COPY decoding: {field:?}"))
}

fn is_uuid(raw: &str) -> bool {
    let b = raw.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Rows the mirror importer would refuse (DLQ) are counted and skipped.
#[derive(Default)]
struct Counts {
    rows: u64,
    kept: u64,
    rejected: u64,
}

fn opt_int<T: std::str::FromStr>(field: &str) -> Result<Option<T>, ()> {
    match field {
        r"\N" => Ok(None),
        raw => raw.parse::<T>().map(Some).map_err(|_| ()),
    }
}

/// One kind's output: index plus size-capped `.dat` parts, written in
/// ascending bucket order.
struct KindWriter {
    dir: PathBuf,
    stem: String,
    idx: Vec<u8>,
    part: u32,
    part_len: u64,
    dat: BufWriter<File>,
    last_bucket: Option<usize>,
    bytes_plain: u64,
    bytes_stored: u64,
    max_block: usize,
    keys: u64,
}

impl KindWriter {
    fn new(dir: &Path, stem: &str) -> Fallible<KindWriter> {
        let dat = Self::open_part(dir, stem, 0)?;
        Ok(KindWriter {
            dir: dir.to_owned(),
            stem: stem.to_owned(),
            idx: vec![0u8; IDX_LEN],
            part: 0,
            part_len: 0,
            dat,
            last_bucket: None,
            bytes_plain: 0,
            bytes_stored: 0,
            max_block: 0,
            keys: 0,
        })
    }

    fn open_part(dir: &Path, stem: &str, part: u32) -> Fallible<BufWriter<File>> {
        let path = dir.join(nntune_mb::snapshot::dat_object("", stem, part));
        File::create(&path)
            .map(|f| BufWriter::with_capacity(1 << 20, f))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Write one bucket: `lines` must be sorted by key with unique keys.
    fn bucket(&mut self, bucket: usize, lines: &[(String, String)]) -> Fallible<()> {
        if lines.is_empty() {
            return Ok(());
        }
        if self.last_bucket.is_some_and(|last| bucket <= last) {
            return Err(format!(
                "{}: bucket {bucket} written out of order",
                self.stem
            ));
        }
        self.last_bucket = Some(bucket);
        let mut plain = String::new();
        for (key, value) in lines {
            if key.contains(['\t', '\n']) || value.contains('\n') {
                return Err(format!(
                    "{}: key/value would break the line format",
                    self.stem
                ));
            }
            plain.push_str(key);
            plain.push('\t');
            plain.push_str(value);
            plain.push('\n');
        }
        if plain.len() > MAX_SNAPSHOT_BLOCK_BYTES {
            return Err(format!(
                "{}: bucket {bucket} is {} bytes, above the {MAX_SNAPSHOT_BLOCK_BYTES}-byte block cap",
                self.stem,
                plain.len()
            ));
        }
        let raw = deflate_block(plain.as_bytes());
        if self.part_len > 0 && self.part_len + raw.len() as u64 > MAX_PART_BYTES {
            self.dat.flush().map_err(|e| e.to_string())?;
            self.part += 1;
            self.part_len = 0;
            self.dat = Self::open_part(&self.dir, &self.stem, self.part)?;
        }
        let block = BlockRef {
            part: self.part,
            len: u32::try_from(raw.len()).map_err(|_| "block too large")?,
            offset: self.part_len,
        };
        self.dat.write_all(&raw).map_err(|e| e.to_string())?;
        self.part_len += raw.len() as u64;
        self.idx[bucket * IDX_ENTRY_LEN..(bucket + 1) * IDX_ENTRY_LEN]
            .copy_from_slice(&encode_idx_entry(block));
        self.bytes_plain += plain.len() as u64;
        self.bytes_stored += raw.len() as u64;
        self.max_block = self.max_block.max(plain.len());
        self.keys += lines.len() as u64;
        Ok(())
    }

    fn finish(mut self) -> Fallible<serde_json::Value> {
        self.dat.flush().map_err(|e| e.to_string())?;
        let idx_path = self.dir.join(idx_object("", &self.stem));
        std::fs::write(&idx_path, &self.idx).map_err(|e| format!("{}: {e}", idx_path.display()))?;
        Ok(serde_json::json!({
            "stem": self.stem,
            "keys": self.keys,
            "parts": self.part + 1,
            "bytes_plain": self.bytes_plain,
            "bytes_stored": self.bytes_stored + IDX_LEN as u64,
            "max_block_plain": self.max_block,
        }))
    }
}

/// Bucket ranges for `passes` passes over the input.
fn pass_ranges(passes: usize) -> Vec<std::ops::Range<usize>> {
    let passes = passes.clamp(1, 256);
    (0..passes)
        .map(|p| (BUCKETS * p / passes)..(BUCKETS * (p + 1) / passes))
        .collect()
}

/// Entity kind: `row` maps fields to `(mbid, stored JSON)`, or `Err(())`
/// for a row the mirror would refuse.
fn build_entity(
    out: &Path,
    stem: &str,
    input: &Path,
    expected: usize,
    passes: usize,
    row: impl Fn(&[&str]) -> Fallible<Result<(String, String), ()>>,
) -> Fallible<serde_json::Value> {
    let started = Instant::now();
    let mut writer = KindWriter::new(out, stem)?;
    let mut counts = Counts::default();
    for range in pass_ranges(passes) {
        let mut by_bucket: HashMap<usize, Vec<(String, String)>> = HashMap::new();
        let mut pass_counts = Counts::default();
        Input::open(input)?.for_each(expected, |fields| {
            pass_counts.rows += 1;
            match row(fields)? {
                Ok((mbid, json)) => {
                    let bucket = mbid_bucket(&mbid).ok_or("bucket")?;
                    if range.contains(&bucket) {
                        pass_counts.kept += 1;
                        by_bucket.entry(bucket).or_default().push((mbid, json));
                    }
                }
                Err(()) => pass_counts.rejected += 1,
            }
            Ok(())
        })?;
        counts.rows = pass_counts.rows;
        counts.rejected = pass_counts.rejected;
        counts.kept += pass_counts.kept;
        let mut buckets: Vec<usize> = by_bucket.keys().copied().collect();
        buckets.sort_unstable();
        for bucket in buckets {
            let mut lines = by_bucket.remove(&bucket).unwrap_or_default();
            lines.sort_by(|a, b| a.0.cmp(&b.0));
            if lines.windows(2).any(|w| w[0].0 == w[1].0) {
                return Err(format!("{stem}: duplicate MBID in bucket {bucket}"));
            }
            writer.bucket(bucket, &lines)?;
        }
    }
    let mut stats = writer.finish()?;
    stats["rows"] = counts.rows.into();
    stats["kept"] = counts.kept.into();
    stats["rejected"] = counts.rejected.into();
    stats["passes"] = passes.into();
    stats["seconds"] = started.elapsed().as_secs_f64().into();
    Ok(stats)
}

fn artist_row(f: &[&str]) -> Fallible<Result<(String, String), ()>> {
    let mbid = f[0].to_ascii_lowercase();
    let (Some(name), Some(sort_name)) = (copy_text(f[1])?, copy_text(f[2])?) else {
        return Ok(Err(()));
    };
    if !is_uuid(&mbid) {
        return Ok(Err(()));
    }
    let row = ArtistRow {
        name,
        sort_name,
        artist_type: copy_text(f[3])?,
        country: copy_text(f[4])?,
    };
    Ok(Ok((
        mbid,
        serde_json::to_string(&row).map_err(|e| e.to_string())?,
    )))
}

/// Curated release columns: gid, artist_gid, title, type, year, caa_url,
/// group_gid, updated_at.
fn release_fields(f: &[&str]) -> Fallible<Result<(String, ReleaseRow), ()>> {
    let mbid = f[0].to_ascii_lowercase();
    let artist_mbid = copy_text(f[1])?.map(|a| a.to_ascii_lowercase());
    let Some(title) = copy_text(f[2])? else {
        return Ok(Err(()));
    };
    let Ok(year) = opt_int::<i16>(f[4]) else {
        return Ok(Err(()));
    };
    if !is_uuid(&mbid) || artist_mbid.as_deref().is_some_and(|a| !is_uuid(a)) {
        return Ok(Err(()));
    }
    Ok(Ok((
        mbid,
        ReleaseRow {
            artist_mbid,
            title,
            release_type: copy_text(f[3])?,
            year,
            caa_url: copy_text(f[5])?,
        },
    )))
}

fn recording_row(f: &[&str]) -> Fallible<Result<(String, String), ()>> {
    let mbid = f[0].to_ascii_lowercase();
    let release_mbid = copy_text(f[1])?.map(|r| r.to_ascii_lowercase());
    let Some(title) = copy_text(f[2])? else {
        return Ok(Err(()));
    };
    let (Ok(track_no), Ok(duration_ms)) = (opt_int::<i16>(f[3]), opt_int::<i32>(f[4])) else {
        return Ok(Err(()));
    };
    if !is_uuid(&mbid) || release_mbid.as_deref().is_some_and(|r| !is_uuid(r)) {
        return Ok(Err(()));
    }
    let row = RecordingRow {
        release_mbid,
        title,
        track_no,
        duration_ms,
    };
    Ok(Ok((
        mbid,
        serde_json::to_string(&row).map_err(|e| e.to_string())?,
    )))
}

/// Release-search index: exact-title keys (title asc, MBID asc, capped at
/// the deepest reachable page) and word keys (shortest titles first,
/// capped), each posting carrying the display fields.
fn build_search(
    out: &Path,
    release_tsv: &Path,
    artist_names: &HashMap<String, String>,
    passes: usize,
) -> Fallible<serde_json::Value> {
    let started = Instant::now();
    let mut writer = KindWriter::new(out, SEARCH_KIND)?;
    let mut postings_total = 0u64;
    let mut truncated_keys = 0u64;
    for range in pass_ranges(passes) {
        let mut by_key: HashMap<String, Vec<Posting>> = HashMap::new();
        Input::open(release_tsv)?.for_each(8, |f| {
            let Ok((mbid, row)) = release_fields(f)? else {
                return Ok(());
            };
            let title_words = words(&row.title);
            if title_words.is_empty() {
                return Ok(());
            }
            let artist_name = row
                .artist_mbid
                .as_ref()
                .and_then(|a| artist_names.get(a).cloned());
            let posting = Posting(
                mbid,
                row.title.clone(),
                row.artist_mbid.clone(),
                artist_name,
            );
            let mut keys = vec![title_key(&row.title)];
            let mut uniq = title_words;
            uniq.sort_unstable();
            uniq.dedup();
            keys.extend(uniq.iter().map(|w| word_key(w)));
            for key in keys {
                if range.contains(&key_bucket(&key)) {
                    by_key.entry(key).or_default().push(posting.clone());
                }
            }
            Ok(())
        })?;
        let mut by_bucket: HashMap<usize, Vec<(String, String)>> = HashMap::new();
        for (key, mut list) in by_key {
            if key.starts_with("t:") {
                list.sort_by_cached_key(|p| (label_sort_key(&p.1), p.0.clone()));
                if list.len() > MAX_TITLE_POSTINGS {
                    truncated_keys += 1;
                    list.truncate(MAX_TITLE_POSTINGS);
                }
            } else {
                list.sort_by_cached_key(|p| {
                    (p.1.chars().count(), label_sort_key(&p.1), p.0.clone())
                });
                if list.len() > MAX_WORD_POSTINGS {
                    truncated_keys += 1;
                    list.truncate(MAX_WORD_POSTINGS);
                }
            }
            postings_total += list.len() as u64;
            let json = serde_json::to_string(&list).map_err(|e| e.to_string())?;
            by_bucket
                .entry(key_bucket(&key))
                .or_default()
                .push((key, json));
        }
        let mut buckets: Vec<usize> = by_bucket.keys().copied().collect();
        buckets.sort_unstable();
        for bucket in buckets {
            let mut lines = by_bucket.remove(&bucket).unwrap_or_default();
            lines.sort_by(|a, b| a.0.cmp(&b.0));
            writer.bucket(bucket, &lines)?;
        }
    }
    let mut stats = writer.finish()?;
    stats["postings"] = postings_total.into();
    stats["truncated_keys"] = truncated_keys.into();
    stats["passes"] = passes.into();
    stats["seconds"] = started.elapsed().as_secs_f64().into();
    Ok(stats)
}

fn load_artist_names(path: &Path) -> Fallible<HashMap<String, String>> {
    let mut names = HashMap::new();
    Input::open(path)?.for_each(6, |f| {
        if let Some(name) = copy_text(f[1])? {
            names.insert(f[0].to_ascii_lowercase(), name);
        }
        Ok(())
    })?;
    Ok(names)
}

fn write_stats(out: &Path, stem: &str, stats: &serde_json::Value) -> Fallible<()> {
    let path = out.join(format!("build-{stem}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(stats).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))
}

fn run(args: &[String]) -> Fallible<()> {
    let usage =
        "usage: snapshot_build <artist|recording|release> <out_dir> <tsv> [artist_tsv] [passes]";
    let kind = args.get(1).ok_or(usage)?;
    let out = PathBuf::from(args.get(2).ok_or(usage)?);
    let input = PathBuf::from(args.get(3).ok_or(usage)?);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let passes_at = |i: usize, default: usize| {
        args.get(i)
            .map(|p| p.parse::<usize>().map_err(|_| usage.to_owned()))
            .unwrap_or(Ok(default))
    };
    match kind.as_str() {
        "artist" => {
            let stats = build_entity(&out, "artist", &input, 6, 1, artist_row)?;
            write_stats(&out, "artist", &stats)
        }
        "recording" => {
            let passes = passes_at(4, 4)?;
            let stats = build_entity(&out, "recording", &input, 6, passes, recording_row)?;
            write_stats(&out, "recording", &stats)
        }
        "release" => {
            let artist_tsv = PathBuf::from(args.get(4).ok_or(usage)?);
            let passes = passes_at(5, 4)?;
            let stats = build_entity(&out, "release", &input, 8, 1, |f| {
                Ok(match release_fields(f)? {
                    Ok((mbid, row)) => Ok((
                        mbid,
                        serde_json::to_string(&row).map_err(|e| e.to_string())?,
                    )),
                    Err(()) => Err(()),
                })
            })?;
            write_stats(&out, "release", &stats)?;
            let names = load_artist_names(&artist_tsv)?;
            let stats = build_search(&out, &input, &names, passes)?;
            write_stats(&out, SEARCH_KIND, &stats)
        }
        _ => Err(usage.to_owned()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(err) = run(&args) {
        eprintln!("snapshot_build: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_text_decodes_postgres_escapes() {
        assert_eq!(copy_text(r"\N").unwrap(), None);
        assert_eq!(copy_text("plain").unwrap().unwrap(), "plain");
        assert_eq!(copy_text(r"a\tb\\c\nd").unwrap().unwrap(), "a\tb\\c\nd");
        assert_eq!(copy_text(r"\101\x42").unwrap().unwrap(), "AB");
    }

    #[test]
    fn pass_ranges_cover_every_bucket_once() {
        let ranges = pass_ranges(3);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, BUCKETS);
        assert!(ranges.windows(2).all(|w| w[0].end == w[1].start));
    }

    #[test]
    fn uuid_shape_is_checked() {
        assert!(is_uuid("0383dadf-2a4e-4d10-a46a-e9e041da8eb3"));
        assert!(!is_uuid("0383dadf2a4e4d10a46ae9e041da8eb3"));
    }
}
