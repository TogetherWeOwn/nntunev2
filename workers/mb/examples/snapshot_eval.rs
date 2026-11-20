//! Run the Worker's snapshot read path against local snapshot files
//! (phase 0 spike): the recall measurement uses exactly the code the
//! `R2SnapshotRepo` Worker serves, only the byte source differs.
//!
//! ```text
//! snapshot_eval search <snap_dir> < golden.tsv > hits.tsv
//! snapshot_eval lookup <snap_dir> <artist|release|recording> < mbids.txt
//! ```
//!
//! `golden.tsv` rows are `id\tq` (extra columns ignored); `q` is normalized
//! like the HTTP layer (trimmed, lowercased, whitespace collapsed) and the
//! top 5 hits are written as `id\trank\tmbid\ttitle\tartist_name\tscore`.
//! `lookup` prints `found`/`missing` counts and exits non-zero on a missing
//! MBID (the input is sampled from the same dump, so every one must hit).

use nntune_mb::snapshot::{RangeSource, SnapshotRepo};
use nntune_mb::{Kind, MbRepo, RepoError, RepoStage};
use std::future::Future;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::task::{Context, Poll, Waker};

/// Snapshot objects as files in one directory (object key = file name).
struct DirSource {
    dir: PathBuf,
}

impl RangeSource for DirSource {
    async fn read_all(&self, key: &str) -> Result<Option<Vec<u8>>, RepoError> {
        match std::fs::read(self.dir.join(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RepoError::new(RepoStage::Query)),
        }
    }

    async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u32,
    ) -> Result<Option<Vec<u8>>, RepoError> {
        let mut file = match std::fs::File::open(self.dir.join(key)) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepoError::new(RepoStage::Query)),
        };
        let mut buf = vec![0u8; len as usize];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut buf))
            .map_err(|_| RepoError::new(RepoStage::Query))?;
        Ok(Some(buf))
    }
}

/// File reads complete synchronously, so every future is ready at once.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
}

/// The HTTP layer's search normalization (trim, lowercase, collapse runs
/// of whitespace) — the repository receives this form.
fn normalize_query(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean(field: &str) -> String {
    field.replace(['\t', '\n', '\r'], " ")
}

fn validate_lookup_sample(found: u64, missing: u64) -> Result<(), String> {
    if found + missing == 0 {
        return Err("no nonblank sampled MBIDs".to_owned());
    }
    if missing > 0 {
        return Err(format!("{missing} sampled MBIDs missing from the snapshot"));
    }
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: snapshot_eval <search <dir>|lookup <dir> <kind>>";
    let mode = args.get(1).ok_or(usage)?;
    let dir = PathBuf::from(args.get(2).ok_or(usage)?);
    let repo = SnapshotRepo::new(DirSource { dir }, "");
    let stdin = std::io::stdin();
    let mut stdout = std::io::BufWriter::new(std::io::stdout());
    match mode.as_str() {
        "search" => {
            let mut queries = 0u64;
            for line in stdin.lock().lines() {
                let line = line.map_err(|e| e.to_string())?;
                let mut cols = line.split('\t');
                let (Some(id), Some(q)) = (cols.next(), cols.next()) else {
                    continue;
                };
                queries += 1;
                let hits = block_on(repo.search(Kind::Release, &normalize_query(q), 5, 0))
                    .map_err(|e| format!("query {id}: repository failure {:?}", e.stage()))?;
                for (rank, hit) in hits.iter().enumerate() {
                    writeln!(
                        stdout,
                        "{id}\t{}\t{}\t{}\t{}\t{}",
                        rank + 1,
                        hit.mbid,
                        clean(&hit.label),
                        clean(hit.artist_name.as_deref().unwrap_or("")),
                        hit.score
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            eprintln!("snapshot_eval: {queries} queries");
        }
        "lookup" => {
            let kind = Kind::parse(args.get(3).cloned()).ok_or(usage)?;
            let (mut found, mut missing) = (0u64, 0u64);
            for line in stdin.lock().lines() {
                let mbid = line.map_err(|e| e.to_string())?;
                let mbid = mbid.trim();
                if mbid.is_empty() {
                    continue;
                }
                match block_on(repo.lookup(kind, mbid))
                    .map_err(|e| format!("{mbid}: repository failure {:?}", e.stage()))?
                {
                    Some(_) => found += 1,
                    None => missing += 1,
                }
            }
            writeln!(stdout, "found={found} missing={missing}").map_err(|e| e.to_string())?;
            validate_lookup_sample(found, missing)?;
        }
        _ => return Err(usage.to_owned()),
    }
    stdout.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_lookup_sample_is_not_success() {
        assert!(super::validate_lookup_sample(0, 0).is_err());
        assert!(super::validate_lookup_sample(0, 1).is_err());
        assert!(super::validate_lookup_sample(1, 1).is_err());
        assert!(super::validate_lookup_sample(1, 0).is_ok());
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(err) = run(&args) {
        eprintln!("snapshot_eval: {err}");
        std::process::exit(1);
    }
}
