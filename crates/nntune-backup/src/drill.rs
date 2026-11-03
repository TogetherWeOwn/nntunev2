//! Proven restore drill: synthetic fixture end to end.
//!
//! Pipeline (mirrors `docs/offsite-backup-runbook.md` §4):
//! backup a synthetic posted-file fixture (envelope row + rclone
//! ciphertext) into a bundle, wipe every live copy, then restore from
//! the bundle into an isolated directory and decrypt the full
//! plaintext from the restored material only, via ranged reads (the
//! same article-window shape the stream path uses). Evidence carries
//! timestamps, object counts and the hash-of-plaintext match — never
//! keys, nonces, ciphertext or article references.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::bundle::{self, BundleError, BundleWriter, Manifest, TableCount};
use crate::envelope::{
    self, Envelope, EnvelopeError, ENVELOPE_VERSION_V2, KEK_LEN, PAYLOAD_FORMAT_MEDIA_KEY_V1,
};

/// Drill KEK version: high sentinel, never a production version.
pub const DRILL_KEK_VERSION: u32 = 4294967000;
/// Drill user + track pins (synthetic; never a real identity).
pub const DRILL_USER_ID: &str = "drill-user";
pub const DRILL_TRACK_ID: &str = "drill-track-1";

/// rclone layout constants (mirror `nntune-core::decrypt::rclone`).
const RCLONE_MAGIC: [u8; 8] = *b"RCLONE\x00\x00";
const HEADER_NONCE_LEN: usize = 24;
const PLAIN_BLOCK: usize = 64 * 1024;
const DERIVED_LEN: usize = 80;
/// First ranged-read length (article-window shaped). The fixture must
/// exceed it so *both* ranged reads authenticate ciphertext, and it
/// already exceeds one 64 KiB rclone block, so the drill also proves
/// decrypt across a block boundary.
const FIRST_READ_LEN: u64 = 70_000;
/// Minimum fixture size: anything smaller would report success without
/// authenticating a ciphertext block (`--bytes 0` is the degenerate
/// case: two zero-length reads, matching empty hashes).
pub const MIN_FIXTURE_BYTES: u64 = FIRST_READ_LEN + 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrillError {
    Envelope(EnvelopeError),
    Bundle(BundleError),
    Crypto(String),
    KeyringMissingVersion(u32),
    HashMismatch,
    FixtureTooSmall,
}

impl core::fmt::Display for DrillError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DrillError::Envelope(e) => write!(f, "drill envelope: {e}"),
            DrillError::Bundle(e) => write!(f, "drill bundle: {e}"),
            DrillError::Crypto(e) => write!(f, "drill crypto: {e}"),
            DrillError::KeyringMissingVersion(v) => {
                write!(f, "drill keyring missing version: {v}")
            }
            DrillError::HashMismatch => write!(f, "drill hash mismatch"),
            DrillError::FixtureTooSmall => write!(
                f,
                "drill fixture too small: must exceed {FIRST_READ_LEN} bytes"
            ),
        }
    }
}

impl std::error::Error for DrillError {}

impl From<EnvelopeError> for DrillError {
    fn from(e: EnvelopeError) -> Self {
        DrillError::Envelope(e)
    }
}

impl From<BundleError> for DrillError {
    fn from(e: BundleError) -> Self {
        DrillError::Bundle(e)
    }
}

/// Offsite keyring recovery stand-in: version-pinned KEK material as
/// recovered from the offsite keyring backup at restore time. No
/// `Debug`, no `Serialize` — this value can never land in evidence,
/// logs or the bundle by construction. Backed by a `Vec` (not a map)
/// so the whole entry list — versions and material — is zeroized on
/// drop via the derived impls.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct KeyringRecovery {
    keys: Vec<(u32, [u8; KEK_LEN])>,
}

impl KeyringRecovery {
    pub fn new(keys: BTreeMap<u32, [u8; KEK_LEN]>) -> Self {
        KeyringRecovery {
            keys: keys.into_iter().collect(),
        }
    }

    pub fn get(&self, version: u32) -> Result<[u8; KEK_LEN], DrillError> {
        self.keys
            .iter()
            .find(|(v, _)| *v == version)
            .map(|(_, k)| *k)
            .ok_or(DrillError::KeyringMissingVersion(version))
    }

    pub fn versions(&self) -> Vec<u32> {
        self.keys.iter().map(|(v, _)| *v).collect()
    }
}

/// One synthetic `track_sources`-shaped row (JSONL in the bundle).
/// Field bytes are base64 (standard engine) — drill-local encoding;
/// production backups carry the raw `BYTEA` columns via `pg_dump`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DrillRow {
    user_id: String,
    row_key: String,
    envelope_version: u16,
    payload_format: u16,
    kek_version: u32,
    dek_wrapped_nonce_b64: String,
    dek_wrapped_b64: String,
    payload_nonce_b64: String,
    ciphertext_b64: String,
}

/// Evidence for the card: timestamps, object counts, hash match. No
/// key, nonce, ciphertext or article-reference field exists on this
/// struct, so serialization cannot leak them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrillEvidence {
    pub tool: String,
    pub bundle_format_version: u32,
    pub env: String,
    pub started_at_unix: i64,
    pub finished_at_unix: i64,
    pub tables: Vec<TableCount>,
    pub blob_bytes: u64,
    pub cipher_blocks: u64,
    pub keyring_versions: Vec<u32>,
    pub plaintext_bytes: u64,
    pub plaintext_sha256: String,
    pub restored_sha256: String,
    pub hash_match: bool,
    pub bundle_manifest_sha256: String,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn fill_random(buf: &mut [u8]) -> Result<(), DrillError> {
    getrandom::fill(buf).map_err(|_| DrillError::Envelope(EnvelopeError::RandomnessUnavailable))
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Seal one synthetic posted-file fixture: random media key, rclone
/// ciphertext, v2 envelope row. Returns the bundle directory and the
/// keyring recovery stand-in (the "offsite keyring backup" side).
pub fn backup_fixture(
    bundle_dir: &Path,
    plaintext: &[u8],
    tool: &str,
) -> Result<(Manifest, KeyringRecovery), DrillError> {
    let mut kek = [0u8; KEK_LEN];
    fill_random(&mut kek)?;
    let mut media_key = [0u8; 32];
    fill_random(&mut media_key)?;
    let mut header_nonce = [0u8; HEADER_NONCE_LEN];
    fill_random(&mut header_nonce)?;

    // rclone ciphertext: 32-byte header + sealed 64 KiB blocks.
    let mut derived = [0u8; DERIVED_LEN];
    derived[..32].copy_from_slice(&media_key);
    let blocks = plaintext.len().div_ceil(PLAIN_BLOCK);
    let mut blob = Vec::with_capacity(32 + blocks * (PLAIN_BLOCK + 16));
    blob.extend_from_slice(&RCLONE_MAGIC);
    blob.extend_from_slice(&header_nonce);
    for (i, chunk) in plaintext.chunks(PLAIN_BLOCK).enumerate() {
        let mut sealed = vec![0u8; chunk.len() + 16];
        nntune_core::decrypt::rclone::seal_block(
            &derived,
            &header_nonce,
            i as u64,
            chunk,
            &mut sealed,
        )
        .map_err(|e| DrillError::Crypto(e.to_string()))?;
        blob.extend_from_slice(&sealed);
    }

    // Envelope the media key (the "wrapped per-file key") under the KEK.
    let row_key = format!("{DRILL_USER_ID}:{DRILL_TRACK_ID}");
    let env =
        envelope::seal_media_key(&kek, DRILL_KEK_VERSION, DRILL_USER_ID, &row_key, &media_key)?;
    media_key.zeroize();
    derived.zeroize();

    let row = DrillRow {
        user_id: DRILL_USER_ID.to_string(),
        row_key,
        envelope_version: ENVELOPE_VERSION_V2,
        payload_format: PAYLOAD_FORMAT_MEDIA_KEY_V1,
        kek_version: DRILL_KEK_VERSION,
        dek_wrapped_nonce_b64: b64(&env.dek_wrapped_nonce),
        dek_wrapped_b64: b64(&env.dek_wrapped),
        payload_nonce_b64: b64(&env.payload_nonce),
        ciphertext_b64: b64(&env.ciphertext),
    };
    let mut jsonl = serde_json::to_vec(&row)
        .map_err(|e| DrillError::Bundle(BundleError::Json(e.to_string())))?;
    jsonl.push(b'\n');

    let mut keyring_manifest = String::from("{\"versions\":[");
    keyring_manifest.push_str(&DRILL_KEK_VERSION.to_string());
    keyring_manifest.push_str("]}");

    let mut w = BundleWriter::new(bundle_dir, "synthetic", tool, now_unix());
    w.stage_file("tables/track_sources.jsonl", &jsonl)?;
    w.stage_file("blobs/drill-track-1.bin", &blob)?;
    w.stage_file("keyring/manifest.json", keyring_manifest.as_bytes())?;
    w.record_table("track_sources", 1);
    w.record_kek_version(DRILL_KEK_VERSION);
    let manifest = w.finish()?;

    let mut keys = BTreeMap::new();
    keys.insert(DRILL_KEK_VERSION, kek);
    kek.zeroize();
    Ok((manifest, KeyringRecovery::new(keys)))
}

/// Restore from the bundle into `restore_dir` (isolated from the
/// backup side) and decrypt the fixture end to end using the restored
/// material + the recovered keyring only. The caller must drop every
/// other copy first; this function takes no key material except
/// `keyring`.
pub fn restore_and_decrypt(
    bundle_dir: &Path,
    restore_dir: &Path,
    keyring: &KeyringRecovery,
    tool: &str,
) -> Result<DrillEvidence, DrillError> {
    let started = now_unix();

    // Fail closed before touching any row: the bundle must verify,
    // both at the backup store and again after the copy into the
    // isolated restore environment. All reads below come from
    // `restore_dir`, never `bundle_dir`.
    bundle::verify_bundle(bundle_dir).map_err(DrillError::Bundle)?;
    copy_dir(bundle_dir, restore_dir).map_err(|e| DrillError::Bundle(BundleError::Io(e)))?;
    let manifest = bundle::verify_bundle(restore_dir).map_err(DrillError::Bundle)?;

    let row_raw = std::fs::read(restore_dir.join("tables/track_sources.jsonl"))
        .map_err(|e| DrillError::Bundle(BundleError::Io(format!("row read: {e}"))))?;
    let row: DrillRow =
        serde_json::from_slice(row_raw.split(|b| *b == b'\n').next().unwrap_or(&row_raw))
            .map_err(|e| DrillError::Bundle(BundleError::Json(e.to_string())))?;

    let mut kek = keyring.get(row.kek_version)?;
    let env = Envelope {
        dek_wrapped_nonce: b64_nonce(&row.dek_wrapped_nonce_b64)?,
        dek_wrapped: b64_bytes(&row.dek_wrapped_b64, 48)?,
        payload_nonce: b64_nonce(&row.payload_nonce_b64)?,
        ciphertext: b64_bytes(&row.ciphertext_b64, 0)?,
    };
    let mut media_key = match envelope::open_media_key(
        &kek,
        &row.user_id,
        &row.row_key,
        row.envelope_version,
        row.payload_format,
        row.kek_version,
        &env,
    ) {
        Ok(k) => k,
        Err(e) => {
            kek.zeroize();
            return Err(DrillError::Envelope(e));
        }
    };
    kek.zeroize();
    if media_key.len() != 32 {
        media_key.zeroize();
        return Err(DrillError::Crypto("media key length".to_string()));
    }
    let mut derived = [0u8; DERIVED_LEN];
    derived[..32].copy_from_slice(&media_key);
    media_key.zeroize();

    let blob = std::fs::read(restore_dir.join("blobs/drill-track-1.bin"))
        .map_err(|e| DrillError::Bundle(BundleError::Io(format!("blob read: {e}"))))?;
    let cipher_blocks = blob.len().saturating_sub(32).div_ceil(PLAIN_BLOCK + 16);

    // Ranged reads, article-window shaped: two halves joined, so the
    // drill proves ranged decrypt, not just whole-file decrypt. The
    // plaintext size is discovered from the blob, never trusted from
    // the row: try sizes by block arithmetic from the ciphertext
    // length (last block may be partial).
    let plaintext_size = plaintext_size_from_blob(blob.len() as u64);
    let mut first = vec![0u8; plaintext_size.min(FIRST_READ_LEN) as usize];
    let n1 = nntune_core::decrypt::rclone::read_at(&blob, &derived, plaintext_size, &mut first, 0)
        .map_err(|e| {
            derived.zeroize();
            DrillError::Crypto(e.to_string())
        })?;
    first.truncate(n1);
    let mut rest = vec![0u8; plaintext_size.saturating_sub(n1 as u64) as usize];
    let n2 = nntune_core::decrypt::rclone::read_at(
        &blob,
        &derived,
        plaintext_size,
        &mut rest,
        n1 as u64,
    )
    .map_err(|e| {
        derived.zeroize();
        DrillError::Crypto(e.to_string())
    })?;
    rest.truncate(n2);
    derived.zeroize();
    let mut restored = first;
    restored.extend_from_slice(&rest);

    // The evidence hash covers the manifest as restored into the
    // isolated environment — the same copy every other read above
    // came from — never the backup-store original.
    let manifest_raw = std::fs::read(restore_dir.join(bundle::MANIFEST_FILE))
        .map_err(|e| DrillError::Bundle(BundleError::Io(format!("manifest read: {e}"))))?;
    let finished = now_unix();
    Ok(DrillEvidence {
        tool: tool.to_string(),
        bundle_format_version: manifest.format_version,
        env: "synthetic".to_string(),
        started_at_unix: started,
        finished_at_unix: finished,
        tables: manifest.tables.clone(),
        blob_bytes: blob.len() as u64,
        cipher_blocks: cipher_blocks as u64,
        keyring_versions: manifest.keyring.versions.clone(),
        plaintext_bytes: restored.len() as u64,
        plaintext_sha256: String::new(),
        restored_sha256: bundle::sha256_hex(&restored),
        hash_match: false,
        bundle_manifest_sha256: bundle::sha256_hex(&manifest_raw),
    })
}

fn plaintext_size_from_blob(blob_len: u64) -> u64 {
    if blob_len <= 32 {
        return 0;
    }
    let body = blob_len - 32;
    let full = body / (PLAIN_BLOCK as u64 + 16);
    let rem = body % (PLAIN_BLOCK as u64 + 16);
    // A non-zero remainder is a partial last block (ciphertext bytes
    // minus the 16-byte tag).
    full * PLAIN_BLOCK as u64 + rem.saturating_sub(16)
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rel = dir.strip_prefix(src).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dst.join(rel)).map_err(|e| e.to_string())?;
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            // `file_type` does not follow links (unlike `is_dir` on the
            // path): an offsite bundle containing a symlink refuses the
            // restore instead of traversing outside the bundle boundary.
            let ftype = entry.file_type().map_err(|e| e.to_string())?;
            if ftype.is_symlink() {
                return Err(format!("refuses symlink: {}", entry.path().display()));
            }
            let path = entry.path();
            let rel = path.strip_prefix(src).map_err(|e| e.to_string())?;
            if ftype.is_dir() {
                stack.push(path);
            } else {
                std::fs::copy(&path, dst.join(rel)).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn b64_nonce(s: &str) -> Result<[u8; 12], DrillError> {
    let v = b64_bytes(s, 12)?;
    let mut a = [0u8; 12];
    a.copy_from_slice(&v);
    Ok(a)
}

fn b64_bytes(s: &str, expect: usize) -> Result<Vec<u8>, DrillError> {
    use base64::Engine;
    let v = base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| DrillError::Bundle(BundleError::Json("bad base64".to_string())))?;
    if expect != 0 && v.len() != expect {
        return Err(DrillError::Envelope(EnvelopeError::CorruptStoredData));
    }
    Ok(v)
}

/// Full synthetic drill in isolated temp dirs: backup, wipe, restore,
/// decrypt, compare. `plaintext_sha256` is filled by the caller from
/// the pre-wipe plaintext (the test harness keeps it, never the keys).
/// Process-wide drill counter: parallel tests share a PID and a clock
/// second, so the temp-dir tag needs a uniqueness source beyond both.
static DRILL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn run_synthetic_drill(
    plaintext: &[u8],
    tool: &str,
) -> Result<(DrillEvidence, PathBuf, PathBuf), DrillError> {
    // Fail closed on fixtures that would pass without authenticating a
    // ciphertext block: empty or sub-`FIRST_READ_LEN` plaintexts leave
    // one ranged read empty and never cross a block boundary.
    if (plaintext.len() as u64) < MIN_FIXTURE_BYTES {
        return Err(DrillError::FixtureTooSmall);
    }
    let seq = DRILL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tag = format!("{}-{}-{seq}", std::process::id(), now_unix());
    let base = std::env::temp_dir().join(format!("nntune-drill-{tag}"));
    let bundle_dir = base.join("backup-store");
    let restore_dir = base.join("isolated-restore");
    let _ = std::fs::remove_dir_all(&base);

    let (_manifest, keyring) = backup_fixture(&bundle_dir, plaintext, tool)?;
    // Wipe: the plaintext slice is the caller's; every drill-owned
    // copy (KEK, media key, derived) was zeroized or moved into the
    // bundle/keyring above. Restore sees only bundle_dir + keyring.
    let mut evidence = restore_and_decrypt(&bundle_dir, &restore_dir, &keyring, tool)?;
    evidence.plaintext_sha256 = bundle::sha256_hex(plaintext);
    evidence.hash_match = evidence.plaintext_sha256 == evidence.restored_sha256;
    if !evidence.hash_match {
        return Err(DrillError::HashMismatch);
    }
    Ok((evidence, bundle_dir, restore_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_bytes() -> Vec<u8> {
        // Multi-block deterministic fixture (3 full 64 KiB blocks +
        // partial): proves ranged decrypt across block boundaries.
        (0..(3 * 65536 + 12345)).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn synthetic_drill_hash_matches() {
        let pt = fixture_bytes();
        let (ev, bundle_dir, restore_dir) = run_synthetic_drill(&pt, "test").unwrap();
        assert!(ev.hash_match);
        assert_eq!(ev.plaintext_bytes, pt.len() as u64);
        assert_eq!(
            ev.tables,
            vec![TableCount {
                table: "track_sources".to_string(),
                rows: 1
            }]
        );
        assert_eq!(ev.keyring_versions, vec![DRILL_KEK_VERSION]);
        assert_eq!(ev.bundle_format_version, bundle::BUNDLE_FORMAT_VERSION);
        assert!(ev.finished_at_unix >= ev.started_at_unix);
        assert_eq!(ev.cipher_blocks, 4);
        let _ = std::fs::remove_dir_all(bundle_dir.parent().unwrap());
        let _ = std::fs::remove_dir_all(&restore_dir);
    }

    #[test]
    fn evidence_carries_no_key_material() {
        let pt = fixture_bytes();
        let (ev, bundle_dir, _) = run_synthetic_drill(&pt, "test").unwrap();
        let json = serde_json::to_string(&ev).unwrap();
        // Ciphertext-adjacent field names must not appear: the
        // evidence struct has no such field by construction; this
        // pins it.
        assert!(json.contains(&ev.plaintext_sha256));
        assert!(json.contains(&ev.restored_sha256));
        assert!(!json.contains("dek_wrapped"));
        assert!(!json.contains("nonce"));
        assert!(!json.contains("ciphertext"));
        assert!(!json.contains("kek"));
        let _ = std::fs::remove_dir_all(bundle_dir.parent().unwrap());
    }

    #[test]
    fn tampered_bundle_fails_closed() {
        let pt = fixture_bytes();
        let tag = format!("{}-tamper", std::process::id());
        let base = std::env::temp_dir().join(format!("nntune-drill-{tag}"));
        let bundle_dir = base.join("backup-store");
        let restore_dir = base.join("isolated-restore");
        let _ = std::fs::remove_dir_all(&base);
        let (_m, keyring) = backup_fixture(&bundle_dir, &pt, "test").unwrap();
        std::fs::write(bundle_dir.join("blobs/drill-track-1.bin"), b"tampered").unwrap();
        let err = restore_and_decrypt(&bundle_dir, &restore_dir, &keyring, "test").unwrap_err();
        assert!(matches!(
            err,
            DrillError::Bundle(BundleError::HashMismatch { .. })
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn undersized_fixture_fails_closed() {
        // Empty and sub-`FIRST_READ_LEN` fixtures must not report
        // success: they authenticate no ciphertext block.
        assert_eq!(
            run_synthetic_drill(&[], "test").unwrap_err(),
            DrillError::FixtureTooSmall
        );
        let small = vec![0u8; FIRST_READ_LEN as usize];
        assert_eq!(
            run_synthetic_drill(&small, "test").unwrap_err(),
            DrillError::FixtureTooSmall
        );
        // Exactly one byte over the first-read length passes admission
        // (and crosses a 64 KiB block boundary).
        let pt: Vec<u8> = (0..MIN_FIXTURE_BYTES).map(|i| (i % 251) as u8).collect();
        let (ev, bundle_dir, restore_dir) = run_synthetic_drill(&pt, "test").unwrap();
        assert!(ev.hash_match);
        let _ = std::fs::remove_dir_all(bundle_dir.parent().unwrap());
        let _ = std::fs::remove_dir_all(&restore_dir);
    }

    #[test]
    #[cfg(unix)]
    fn copy_dir_refuses_symlink() {
        let tag = format!("{}-symlink", std::process::id());
        let base = std::env::temp_dir().join(format!("nntune-drill-{tag}"));
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("real.bin"), b"real").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(src.join("real.bin"), src.join("link.bin")).unwrap();
        let err = copy_dir(&src, &dst).unwrap_err();
        assert!(err.contains("symlink"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn wrong_keyring_version_fails_closed() {
        let pt = fixture_bytes();
        let tag = format!("{}-wrongv", std::process::id());
        let base = std::env::temp_dir().join(format!("nntune-drill-{tag}"));
        let bundle_dir = base.join("backup-store");
        let restore_dir = base.join("isolated-restore");
        let _ = std::fs::remove_dir_all(&base);
        let (_m, _keyring) = backup_fixture(&bundle_dir, &pt, "test").unwrap();
        let empty = KeyringRecovery::new(BTreeMap::new());
        let err = restore_and_decrypt(&bundle_dir, &restore_dir, &empty, "test").unwrap_err();
        assert!(matches!(
            err,
            DrillError::KeyringMissingVersion(DRILL_KEK_VERSION)
        ));
        let _ = std::fs::remove_dir_all(&base);
    }
}
