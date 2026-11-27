//! Offsite backup bundle format (v1).
//!
//! A bundle is a directory — the unit the operator copies to R2
//! (`offsite/<env>/...`, see `docs/offsite-backup-runbook.md`). Every
//! file except the manifest itself is content-addressed by SHA-256 in
//! the manifest, so `verify_bundle` detects truncation or corruption
//! before any restore. The manifest itself is unauthenticated, so it
//! is not tamper evidence on its own: tampering that rewrites both a
//! file and its manifest entry is caught downstream by the AEAD row
//! verification (`verify_rows`), which fails closed. The bundle carries ciphertext and wrapped keys
//! only: KEK values never appear here (keyring entries are version
//! numbers only, never material; date ranges live in the operator
//! manifest, not the bundle).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bundle format version. Bump on any layout change; readers refuse
/// newer versions fail-closed.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
/// Bound metadata allocation independently of the size of the backup files.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

/// One content file inside the bundle, addressed by SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleFile {
    pub path: String,
    pub sha256_hex: String,
    pub bytes: u64,
}

/// Row counts per dumped table (source of the "object counts" drill
/// evidence). No row contents live in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableCount {
    pub table: String,
    pub rows: u64,
}

/// Keyring coverage metadata: which KEK versions the backed-up rows
/// reference. Version numbers only — never key material, and no date
/// ranges: the active date ranges live in the operator keyring
/// manifest (`docs/offsite-backup-runbook.md` §1), which this bundle
/// manifest only mirrors by version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyringCoverage {
    pub versions: Vec<u32>,
}

/// Bundle manifest: the only file read before verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub format_version: u32,
    pub env: String,
    pub created_at_unix: i64,
    pub tool: String,
    pub tables: Vec<TableCount>,
    pub keyring: KeyringCoverage,
    pub files: Vec<BundleFile>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum BundleError {
    Io(String),
    Json(String),
    UnsupportedFormat(u32),
    DuplicatePath,
    ManifestTooLarge,
    HashMismatch { path: String },
    MissingFile { path: String },
}

impl core::fmt::Display for BundleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BundleError::Io(_) => f.write_str("bundle io failure"),
            BundleError::Json(_) => f.write_str("invalid bundle JSON"),
            BundleError::UnsupportedFormat(_) => f.write_str("unsupported bundle format"),
            BundleError::DuplicatePath => f.write_str("duplicate bundle path"),
            BundleError::ManifestTooLarge => f.write_str("bundle manifest too large"),
            BundleError::HashMismatch { .. } => f.write_str("bundle size or hash mismatch"),
            BundleError::MissingFile { .. } => f.write_str("bundle missing file"),
        }
    }
}

// Parser diagnostics and file paths may contain member data.
impl core::fmt::Debug for BundleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for BundleError {}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_of(&h.finalize())
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// In-progress bundle writer. Files are staged under `dir`, then
/// [`finish`] hashes every staged file and writes the manifest last,
/// so a crashed write never leaves a manifest pointing at partial
/// files.
pub struct BundleWriter {
    dir: PathBuf,
    env: String,
    created_at_unix: i64,
    tool: String,
    tables: Vec<TableCount>,
    keyring_versions: BTreeMap<u32, ()>,
    staged: BTreeMap<String, String>,
}

fn content_path_key(rel: &str) -> Result<String, BundleError> {
    let key = rel.to_uppercase();
    let first = key.split_once('/').map_or(key.as_str(), |(first, _)| first);
    if rel.contains(['\\', ':', '\0'])
        || rel
            .split('/')
            .any(|p| p.is_empty() || p.ends_with(['.', ' ']))
        || first.eq_ignore_ascii_case(MANIFEST_FILE)
    {
        return Err(BundleError::Io("invalid bundle path".into()));
    }
    Ok(key)
}

fn missing(rel: &str) -> BundleError {
    BundleError::MissingFile { path: rel.into() }
}

/// Check each directory before descending so a static symlink cannot redirect
/// a read or write. The caller owns the scratch tree and keeps it stable.
fn entry_path(dir: &Path, rel: &str, create_parents: bool) -> Result<PathBuf, BundleError> {
    // Normalize a trailing slash or "." before checking the root itself.
    let mut path: PathBuf = dir.components().collect();
    for part in rel.split('/') {
        if create_parents {
            fs::create_dir_all(&path)
                .map_err(|_| BundleError::Io("bundle directory creation".into()))?;
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| missing(rel))?;
        if !meta.is_dir() {
            return Err(BundleError::Io("invalid bundle directory".into()));
        }
        path.push(part);
    }
    Ok(path)
}

fn open_bundle_file(dir: &Path, rel: &str) -> Result<File, BundleError> {
    let path = entry_path(dir, rel, false)?;
    let meta = fs::symlink_metadata(&path).map_err(|_| missing(rel))?;
    if !meta.is_file() {
        return Err(BundleError::Io("invalid bundle file type".into()));
    }
    File::open(path).map_err(|_| missing(rel))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| BundleError::Io("bundle file creation".into()))?;
    if file.write_all(bytes).is_err() {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(BundleError::Io("bundle file write".into()));
    }
    Ok(())
}

fn hash_file(mut input: impl Read) -> Result<(u64, String), BundleError> {
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 32 * 1024];
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|_| BundleError::Io("bundle file read".into()))?;
        if n == 0 {
            return Ok((bytes, hex_of(&digest.finalize())));
        }
        digest.update(&buffer[..n]);
        bytes += n as u64;
    }
}

impl BundleWriter {
    pub fn new(dir: &Path, env: &str, tool: &str, created_at_unix: i64) -> Self {
        BundleWriter {
            dir: dir.to_path_buf(),
            env: env.to_string(),
            tool: tool.to_string(),
            created_at_unix,
            tables: Vec::new(),
            keyring_versions: BTreeMap::new(),
            staged: BTreeMap::new(),
        }
    }

    /// Stage one new file under its canonical bundle-relative path.
    pub fn stage_file(&mut self, rel: &str, bytes: &[u8]) -> Result<(), BundleError> {
        let key = content_path_key(rel)?;
        if self.staged.contains_key(&key) {
            return Err(BundleError::DuplicatePath);
        }
        let path = entry_path(&self.dir, rel, true)?;
        write_new_file(&path, bytes)?;
        self.staged.insert(key, rel.into());
        Ok(())
    }

    pub fn record_table(&mut self, table: &str, rows: u64) {
        self.tables.push(TableCount {
            table: table.to_string(),
            rows,
        });
    }

    pub fn record_kek_version(&mut self, version: u32) {
        self.keyring_versions.insert(version, ());
    }

    pub fn finish(self) -> Result<Manifest, BundleError> {
        let mut files = Vec::with_capacity(self.staged.len());
        for rel in self.staged.values() {
            let (bytes, sha256_hex) = hash_file(open_bundle_file(&self.dir, rel)?)?;
            files.push(BundleFile {
                path: rel.clone(),
                sha256_hex,
                bytes,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = Manifest {
            format_version: BUNDLE_FORMAT_VERSION,
            env: self.env,
            created_at_unix: self.created_at_unix,
            tool: self.tool,
            tables: self.tables,
            keyring: KeyringCoverage {
                versions: self.keyring_versions.keys().copied().collect(),
            },
            files,
        };
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|_| BundleError::Json("manifest serialization".into()))?;
        if json.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(BundleError::ManifestTooLarge);
        }
        write_new_file(&entry_path(&self.dir, MANIFEST_FILE, true)?, &json)?;
        Ok(manifest)
    }
}

/// Read bounded manifest metadata and reject invalid format versions and paths.
pub fn read_manifest(dir: &Path) -> Result<Manifest, BundleError> {
    let file = open_bundle_file(dir, MANIFEST_FILE)?;
    let mut raw = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| BundleError::Io("manifest read".into()))?;
    if raw.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(BundleError::ManifestTooLarge);
    }
    let manifest: Manifest =
        serde_json::from_slice(&raw).map_err(|_| BundleError::Json("manifest parsing".into()))?;
    if manifest.format_version != BUNDLE_FORMAT_VERSION {
        return Err(BundleError::UnsupportedFormat(manifest.format_version));
    }
    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        if !paths.insert(content_path_key(&file.path)?) {
            return Err(BundleError::DuplicatePath);
        }
    }
    Ok(manifest)
}

/// Verify every manifest-listed regular file against its size and SHA-256.
/// The caller must keep the offline bundle tree stable through verification
/// and any subsequent restore reads.
pub fn verify_bundle(dir: &Path) -> Result<Manifest, BundleError> {
    let manifest = read_manifest(dir)?;
    for f in &manifest.files {
        let file = open_bundle_file(dir, &f.path)?;
        let mismatch = || BundleError::HashMismatch {
            path: f.path.clone(),
        };
        if file.metadata().map_err(|_| missing(&f.path))?.len() != f.bytes {
            return Err(mismatch());
        }
        let (bytes, hash) = hash_file(file.take(f.bytes.saturating_add(1)))?;
        if bytes != f.bytes || hash != f.sha256_hex {
            return Err(mismatch());
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer_in(dir: &Path) -> BundleWriter {
        BundleWriter::new(dir, "synthetic", "test", 1_700_000_000)
    }

    #[test]
    fn round_trip_and_verify() {
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 1));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        w.stage_file("tables/track_sources.jsonl", b"{}\n").unwrap();
        w.record_table("track_sources", 1);
        w.record_kek_version(7);
        let m = w.finish().unwrap();
        assert_eq!(m.format_version, BUNDLE_FORMAT_VERSION);
        assert_eq!(m.keyring.versions, vec![7]);
        verify_bundle(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tamper_detected() {
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 2));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        w.stage_file("blobs/x.bin", b"secretbox-bytes").unwrap();
        w.finish().unwrap();
        std::fs::write(dir.join("blobs/x.bin"), b"secretbox-byte!").unwrap();
        assert!(matches!(
            verify_bundle(&dir),
            Err(BundleError::HashMismatch { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn symlinks_refused() {
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 4));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        w.stage_file("blobs/x.bin", b"secretbox-bytes").unwrap();
        w.finish().unwrap();
        // Swap a verified file for a symlink to bytes outside the
        // bundle: verification must refuse, not follow.
        let outside =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-outside", std::process::id()));
        std::fs::write(&outside, b"secretbox-bytes").unwrap();
        std::fs::remove_file(dir.join("blobs/x.bin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join("blobs/x.bin")).unwrap();
        #[cfg(unix)]
        assert!(matches!(verify_bundle(&dir), Err(BundleError::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_parent_dir_refused() {
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 5));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        w.stage_file("blobs/x.bin", b"secretbox-bytes").unwrap();
        w.finish().unwrap();
        // Swap the intermediate directory for a symlink to a
        // lookalike tree outside the bundle: verification must refuse,
        // not follow it — even though the final file name resolves.
        let outside = std::env::temp_dir().join(format!(
            "nntune-backup-test-{}-outside-dir",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x.bin"), b"secretbox-bytes").unwrap();
        std::fs::remove_dir_all(dir.join("blobs")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join("blobs")).unwrap();
        let root = dir.join("blobs/.");
        assert!(matches!(read_manifest(&root), Err(BundleError::Io(_))));
        #[cfg(unix)]
        assert!(matches!(verify_bundle(&dir), Err(BundleError::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn manifest_paths_and_error_redaction() {
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 3));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        const PRIVATE: &str = "synthetic-member@example.invalid";
        let rel = format!("data/{PRIVATE}");
        w.stage_file(&rel, b"x").unwrap();
        for alias in [&rel, &rel.to_uppercase()] {
            assert_eq!(w.stage_file(alias, b"y"), Err(BundleError::DuplicatePath));
        }
        assert!(writer_in(&dir).stage_file(&rel, b"y").is_err());
        assert_eq!(fs::read(dir.join(&rel)).unwrap(), b"x");
        let mut manifest = w.finish().unwrap();
        assert_eq!(verify_bundle(&dir), Ok(manifest.clone()));
        let path = dir.join(MANIFEST_FILE);
        manifest.files.push(manifest.files[0].clone());
        for alias in [&rel, &rel.to_uppercase()] {
            manifest.files[1].path.clone_from(alias);
            fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert_eq!(verify_bundle(&dir), Err(BundleError::DuplicatePath));
        }
        manifest.files.pop();
        for bad in [
            format!("../{PRIVATE}"),
            format!("data/./{PRIVATE}"),
            "manifest.json".into(),
        ] {
            manifest.files[0].path = bad;
            fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert!(matches!(read_manifest(&dir), Err(BundleError::Io(_))));
        }
        manifest.files[0].path.clone_from(&rel);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        fs::write(dir.join(&rel), b"").unwrap();
        let mismatch = verify_bundle(&dir).unwrap_err();
        fs::remove_file(dir.join(&rel)).unwrap();
        fs::create_dir(dir.join(&rel)).unwrap();
        let err = verify_bundle(&dir).unwrap_err();
        assert_eq!(err, BundleError::Io("invalid bundle file type".into()));
        fs::write(&path, format!(r#"{{"format_version":"{PRIVATE}"}}"#)).unwrap();
        let malformed = read_manifest(&dir).unwrap_err();
        for err in [mismatch, malformed] {
            assert!(!format!("{err} / {err:?}").contains(PRIVATE));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_bounds_and_version() {
        let dir = std::env::temp_dir().join(format!("nntune-backup-test-{}-8", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut manifest = writer_in(&dir).finish().unwrap();
        manifest.format_version = BUNDLE_FORMAT_VERSION + 1;
        let path = dir.join(MANIFEST_FILE);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            read_manifest(&dir),
            Err(BundleError::UnsupportedFormat(manifest.format_version))
        );
        let file = File::create(path).unwrap();
        file.set_len(MAX_MANIFEST_BYTES + 1).unwrap();
        assert_eq!(read_manifest(&dir), Err(BundleError::ManifestTooLarge));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_spellings_refused() {
        // Drive-qualified, backslash and `:` spellings must refuse at
        // stage time and at verify time alike: on Windows
        // `dir.join("C:/evil")` escapes the bundle root even though the
        // Unix `starts_with('/')`/`..` check would pass them.
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 6));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        for rel in [
            "C:/evil/x.bin",
            "C:\\evil\\x.bin",
            "blobs\\x.bin",
            "\\\\server\\share\\x.bin",
            "a:b",
        ] {
            assert!(w.stage_file(rel, b"x").is_err(), "staged {rel}");
        }
        let _ = std::fs::remove_dir_all(&dir);

        // Verify-time: a crafted manifest listing a drive-qualified
        // path refuses before any read, not after joining.
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 7));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = Manifest {
            format_version: BUNDLE_FORMAT_VERSION,
            env: "synthetic".to_string(),
            created_at_unix: 1_700_000_000,
            tool: "test".to_string(),
            tables: vec![],
            keyring: KeyringCoverage { versions: vec![] },
            files: vec![BundleFile {
                path: "C:/evil/x.bin".to_string(),
                sha256_hex: sha256_hex(b"x"),
                bytes: 1,
            }],
        };
        std::fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        assert!(matches!(verify_bundle(&dir), Err(BundleError::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
