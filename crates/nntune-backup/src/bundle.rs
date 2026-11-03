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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bundle format version. Bump on any layout change; readers refuse
/// newer versions fail-closed.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    Io(String),
    Json(String),
    UnsupportedFormat(u32),
    HashMismatch { path: String },
    MissingFile { path: String },
}

impl core::fmt::Display for BundleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BundleError::Io(e) => write!(f, "bundle io: {e}"),
            BundleError::Json(e) => write!(f, "bundle json: {e}"),
            BundleError::UnsupportedFormat(v) => {
                write!(f, "unsupported bundle format: {v}")
            }
            BundleError::HashMismatch { path } => {
                write!(f, "bundle hash mismatch: {path}")
            }
            BundleError::MissingFile { path } => {
                write!(f, "bundle missing file: {path}")
            }
        }
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
    staged: Vec<String>,
}

/// Reject bundle-relative paths that could escape the bundle root.
/// `Path::components` parses per-platform (on Unix `C:\x` is an
/// ordinary file name), so check both the parsed components (prefix,
/// root, parent) and the Windows spellings explicitly: backslash
/// separators and drive-letter/`:` spellings are never valid in a
/// bundle path, and the bundle must verify identically on every
/// platform the tool builds on.
fn is_unsafe_bundle_path(rel: &str) -> bool {
    if rel.is_empty() {
        return true;
    }
    if rel.contains('\\') || rel.contains(':') {
        return true;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return true;
    }
    path.components().any(|c| {
        matches!(
            c,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::ParentDir
        )
    })
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
            staged: Vec::new(),
        }
    }

    /// Stage one file (relative path inside the bundle). Parent
    /// directories are created. Refuses absolute, prefixed, rooted
    /// and parent paths, including Windows spellings (see
    /// [`is_unsafe_bundle_path`]).
    pub fn stage_file(&mut self, rel: &str, bytes: &[u8]) -> Result<(), BundleError> {
        if is_unsafe_bundle_path(rel) {
            return Err(BundleError::Io(format!("unsafe bundle path: {rel}")));
        }
        let full = self.dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| BundleError::Io(format!("mkdir: {e}")))?;
        }
        std::fs::write(&full, bytes).map_err(|e| BundleError::Io(format!("write: {e}")))?;
        self.staged.push(rel.to_string());
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
        for rel in &self.staged {
            let bytes = std::fs::read(self.dir.join(rel))
                .map_err(|e| BundleError::Io(format!("read back: {e}")))?;
            files.push(BundleFile {
                path: rel.clone(),
                sha256_hex: sha256_hex(&bytes),
                bytes: bytes.len() as u64,
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
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| BundleError::Json(e.to_string()))?;
        std::fs::write(self.dir.join(MANIFEST_FILE), json)
            .map_err(|e| BundleError::Io(format!("manifest write: {e}")))?;
        Ok(manifest)
    }
}

/// Read the manifest without trusting it: unknown format versions
/// refuse fail-closed before any restore. A symlinked manifest is
/// refused outright: it could point outside the bundle boundary.
pub fn read_manifest(dir: &Path) -> Result<Manifest, BundleError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let meta = std::fs::symlink_metadata(&manifest_path).map_err(|_| BundleError::MissingFile {
        path: MANIFEST_FILE.to_string(),
    })?;
    if meta.file_type().is_symlink() {
        return Err(BundleError::Io(format!("refuses symlink: {MANIFEST_FILE}")));
    }
    let raw = std::fs::read(&manifest_path).map_err(|_| BundleError::MissingFile {
        path: MANIFEST_FILE.to_string(),
    })?;
    let manifest: Manifest =
        serde_json::from_slice(&raw).map_err(|e| BundleError::Json(e.to_string()))?;
    if manifest.format_version != BUNDLE_FORMAT_VERSION {
        return Err(BundleError::UnsupportedFormat(manifest.format_version));
    }
    Ok(manifest)
}

/// Verify every manifest-listed file exists with the recorded hash.
/// Returns the manifest on success. Symlinks are refused at every
/// level: `fs::read` follows them, so without this check a crafted
/// bundle could verify bytes from outside the bundle boundary — either
/// via a symlinked file or via a symlinked intermediate directory
/// (e.g. `blobs/` pointing elsewhere).
pub fn verify_bundle(dir: &Path) -> Result<Manifest, BundleError> {
    let manifest = read_manifest(dir)?;
    for f in &manifest.files {
        if is_unsafe_bundle_path(&f.path) {
            return Err(BundleError::Io(format!("unsafe bundle path: {}", f.path)));
        }
        let full = dir.join(&f.path);
        // Walk the file and every ancestor directory down to (but
        // excluding) the bundle root: any symlink in the chain could
        // redirect reads outside the bundle. `full` is
        // `dir.join(rel)` with `rel` relative and `..`-free, so the
        // ancestor chain always reaches `dir`.
        let mut cur: &Path = &full;
        loop {
            let meta = std::fs::symlink_metadata(cur).map_err(|_| BundleError::MissingFile {
                path: f.path.clone(),
            })?;
            if meta.file_type().is_symlink() {
                return Err(BundleError::Io(format!("refuses symlink: {}", f.path)));
            }
            if cur == dir {
                break;
            }
            match cur.parent() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
        let bytes = std::fs::read(&full).map_err(|_| BundleError::MissingFile {
            path: f.path.clone(),
        })?;
        if bytes.len() as u64 != f.bytes || sha256_hex(&bytes) != f.sha256_hex {
            return Err(BundleError::HashMismatch {
                path: f.path.clone(),
            });
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
        std::fs::write(dir.join("blobs/x.bin"), b"tampered").unwrap();
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
        #[cfg(unix)]
        assert!(matches!(verify_bundle(&dir), Err(BundleError::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn unsafe_paths_refused() {
        let dir =
            std::env::temp_dir().join(format!("nntune-backup-test-{}-{}", std::process::id(), 3));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = writer_in(&dir);
        assert!(w.stage_file("../evil", b"x").is_err());
        assert!(w.stage_file("/abs", b"x").is_err());
        assert!(w.stage_file("a/../../evil", b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
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
