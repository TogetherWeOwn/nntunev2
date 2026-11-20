//! Proven restore drill: synthetic fixture end to end.
//!
//! Pipeline (mirrors `docs/offsite-backup-runbook.md` §4):
//! backup a synthetic posted-file fixture (envelope row + `velmora.media.v1`
//! ciphertext) into a bundle, wipe every live copy, then restore from
//! the bundle into an isolated directory and decrypt the full
//! plaintext from the restored material only, via ranged reads (the
//! same article-window shape the stream path uses). Evidence carries
//! timestamps, object counts and the hash-of-plaintext match — never
//! keys, nonces, ciphertext or article references.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use nntune_core::media::{self, FileKey, Header, Profile, Sealer, VerifiedHeader};
use nntune_core::rng::SystemRng;

use crate::bundle::{self, BundleError, BundleWriter, Manifest, TableCount};
use crate::envelope::{
    self, Envelope, EnvelopeError, ENVELOPE_VERSION_V2, KEK_LEN, PAYLOAD_FORMAT_MEDIA_KEY_V1,
};

/// Drill KEK version: high sentinel, never a production version.
pub const DRILL_KEK_VERSION: u32 = 4294967000;
/// Drill user + track pins (synthetic; never a real identity).
pub const DRILL_USER_ID: &str = "drill-user";
pub const DRILL_TRACK_ID: &str = "drill-track-1";

/// First ranged-read length (article-window shaped). The fixture must
/// exceed it so *both* ranged reads authenticate ciphertext, and it
/// already exceeds one 64 KiB `velmora.media.v1` chunk, so the drill also
/// proves decrypt across a chunk boundary.
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

/// One synthetic posted-file row (JSONL in the bundle): the wrapped media
/// key plus the trusted `velmora.media.v1` record (format, key mode, the
/// 48-byte header and the plaintext size `P`). Field bytes are base64
/// (standard engine) — drill-local encoding; production backups carry the
/// raw `BYTEA` columns via `pg_dump`.
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
    media_format: u8,
    media_mode: u8,
    media_header_b64: String,
    plaintext_size: u64,
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

/// Seal one synthetic posted-file fixture: random media key,
/// `velmora.media.v1` Random-mode ciphertext under the NNTune profile, v2
/// envelope row. Returns the bundle directory and the
/// keyring recovery stand-in (the "offsite keyring backup" side).
pub fn backup_fixture(
    bundle_dir: &Path,
    plaintext: &[u8],
    tool: &str,
) -> Result<(Manifest, KeyringRecovery), DrillError> {
    let mut kek = [0u8; KEK_LEN];
    fill_random(&mut kek)?;
    // `FileKey` is zeroized on drop, so every `?` exit below wipes it.
    let media_key = FileKey::generate(&mut SystemRng).map_err(crypto)?;

    // velmora.media.v1 ciphertext: 48-byte header + sealed 64 KiB chunks.
    let sealer = Sealer::random(&Profile::nntune(), &media_key, &mut SystemRng).map_err(crypto)?;
    let header = sealer.header();
    let blob = sealer.seal_to_vec(plaintext).map_err(crypto)?;

    // Envelope the media key (the "wrapped per-file key") under the KEK.
    let row_key = format!("{DRILL_USER_ID}:{DRILL_TRACK_ID}");
    let env = envelope::seal_media_key(
        &kek,
        DRILL_KEK_VERSION,
        DRILL_USER_ID,
        &row_key,
        media_key.expose_secret(),
    )?;
    drop(media_key);

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
        media_format: media::FORMAT_ID,
        media_mode: media::KeyMode::Random.as_byte(),
        media_header_b64: b64(&header.to_bytes()),
        plaintext_size: plaintext.len() as u64,
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
    // isolated restore environment.
    bundle::verify_bundle(bundle_dir).map_err(DrillError::Bundle)?;
    copy_dir(bundle_dir, restore_dir).map_err(|e| DrillError::Bundle(BundleError::Io(e)))?;
    decrypt_restored_dir(restore_dir, keyring, tool, started)
}

/// Decrypt the fixture from an already-isolated restore directory.
/// Split from [`restore_and_decrypt`] so the evidence hash can be
/// tested with the source and restored manifests diverged: this
/// function takes no `bundle_dir` at all, so the manifest digest it
/// reports can only come from the restored copy. All reads come from
/// `restore_dir`, never the backup store.
fn decrypt_restored_dir(
    restore_dir: &Path,
    keyring: &KeyringRecovery,
    tool: &str,
    started: i64,
) -> Result<DrillEvidence, DrillError> {
    let manifest = bundle::verify_bundle(restore_dir).map_err(DrillError::Bundle)?;

    let row_raw = std::fs::read(restore_dir.join("tables/track_sources.jsonl"))
        .map_err(|e| DrillError::Bundle(BundleError::Io(format!("row read: {e}"))))?;
    let row: DrillRow =
        serde_json::from_slice(row_raw.split(|b| *b == b'\n').next().unwrap_or(&row_raw))
            .map_err(|e| DrillError::Bundle(BundleError::Json(e.to_string())))?;

    // `Zeroizing` so every `?` exit below (malformed row fields,
    // failed open, blob read) drops the KEK zeroized instead of
    // leaving the copy on the stack.
    let mut kek = Zeroizing::new(keyring.get(row.kek_version)?);
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
    // Exactly 32 bytes, or the row is corrupt; `FileKey` owns the only
    // copy from here and is zeroized on drop.
    let key_bytes: Result<[u8; media::KEY_SIZE], _> = media_key.as_slice().try_into();
    media_key.zeroize();
    let media_key = FileKey::from_bytes(
        key_bytes.map_err(|_| DrillError::Crypto("media key length".to_string()))?,
    );

    // The trusted record selects the format; anything but
    // `velmora.media.v1` fails closed before any key use.
    if row.media_format != media::FORMAT_ID {
        return Err(DrillError::Crypto("unknown media format".to_string()));
    }
    let header = Header::from_slice(&b64_bytes(
        &row.media_header_b64,
        media::HEADER_SIZE as usize,
    )?)
    .map_err(crypto)?;
    // Mode allow-list (NNTune: Random only) and the constant-time key
    // commitment run here, before any chunk is touched.
    let verified =
        VerifiedHeader::verify_record(&Profile::nntune(), row.media_mode, &media_key, &header)
            .map_err(crypto)?;
    drop(media_key);
    let reader = verified.reader(row.plaintext_size).map_err(crypto)?;

    let blob = std::fs::read(restore_dir.join("blobs/drill-track-1.bin"))
        .map_err(|e| DrillError::Bundle(BundleError::Io(format!("blob read: {e}"))))?;
    let cipher_blocks = media::chunk_count(reader.plaintext_size());

    // Full verification first (decision §1.5 rule 7): the wire header
    // equals the record, every chunk authenticates with the final flag
    // at `L-1`, and the blob ends right after it, so a `P` that does not
    // match the restored ciphertext fails closed here.
    let verified_sha256 = reader.verify_all(&blob).map_err(crypto)?;

    // Ranged reads, article-window shaped: two halves joined, so the
    // drill proves ranged decrypt, not just whole-file decrypt.
    let plaintext_size = reader.plaintext_size();
    let mut first = vec![0u8; plaintext_size.min(FIRST_READ_LEN) as usize];
    let n1 = reader.read_at(&blob, &mut first, 0).map_err(crypto)?;
    first.truncate(n1);
    let mut rest = vec![0u8; plaintext_size.saturating_sub(n1 as u64) as usize];
    let n2 = reader
        .read_at(&blob, &mut rest, n1 as u64)
        .map_err(crypto)?;
    rest.truncate(n2);
    drop(verified);
    let mut restored = first;
    restored.extend_from_slice(&rest);
    // The joined ranged reads must reproduce the fully verified plaintext.
    if Sha256::digest(&restored)[..] != verified_sha256[..] {
        return Err(DrillError::HashMismatch);
    }

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

fn crypto(e: nntune_core::Error) -> DrillError {
    DrillError::Crypto(e.to_string())
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
    // copy (KEK, media key, payload key) was zeroized or moved into the
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
    fn evidence_hash_covers_restored_manifest() {
        // The evidence digest must come from the manifest as restored
        // into the isolated environment, never the backup-store
        // original: diverge the two sides to different-but-valid bytes
        // (compact vs pretty serialization, same semantics) before the
        // decrypt half runs, and assert the restored digest. Reading
        // the source manifest instead would report the diverged hash.
        let pt = fixture_bytes();
        let tag = format!("{}-manifest-src", std::process::id());
        let base = std::env::temp_dir().join(format!("nntune-drill-{tag}"));
        let bundle_dir = base.join("backup-store");
        let restore_dir = base.join("isolated-restore");
        let _ = std::fs::remove_dir_all(&base);
        let (_m, keyring) = backup_fixture(&bundle_dir, &pt, "test").unwrap();
        copy_dir(&bundle_dir, &restore_dir).unwrap();
        let restored_raw = std::fs::read(restore_dir.join(bundle::MANIFEST_FILE)).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&restored_raw).unwrap();
        let divergent = serde_json::to_vec(&v).unwrap();
        assert_ne!(
            divergent, restored_raw,
            "test needs distinct serializations to diverge the manifests"
        );
        std::fs::write(bundle_dir.join(bundle::MANIFEST_FILE), &divergent).unwrap();
        let started = now_unix();
        let mut ev = decrypt_restored_dir(&restore_dir, &keyring, "test", started).unwrap();
        ev.plaintext_sha256 = bundle::sha256_hex(&pt);
        ev.hash_match = ev.plaintext_sha256 == ev.restored_sha256;
        assert!(ev.hash_match);
        assert_eq!(ev.bundle_manifest_sha256, bundle::sha256_hex(&restored_raw));
        assert_ne!(ev.bundle_manifest_sha256, bundle::sha256_hex(&divergent));
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

    #[test]
    fn fixture_is_media_v1_bound_to_its_record() {
        // The bundled blob is a `velmora.media.v1` object: it starts with
        // the record's 48-byte header (no cleartext magic), has exactly
        // `48 + P + 16·L` bytes, and the record pins format 1 / Random.
        let pt = fixture_bytes();
        let tag = format!("{}-media-v1", std::process::id());
        let base = std::env::temp_dir().join(format!("nntune-drill-{tag}"));
        let bundle_dir = base.join("backup-store");
        let _ = std::fs::remove_dir_all(&base);
        let (_m, _keyring) = backup_fixture(&bundle_dir, &pt, "test").unwrap();
        let raw = std::fs::read(bundle_dir.join("tables/track_sources.jsonl")).unwrap();
        let row: DrillRow =
            serde_json::from_slice(raw.split(|b| *b == b'\n').next().unwrap()).unwrap();
        let blob = std::fs::read(bundle_dir.join("blobs/drill-track-1.bin")).unwrap();
        let header = b64_bytes(&row.media_header_b64, media::HEADER_SIZE as usize).unwrap();
        assert_eq!(row.media_format, media::FORMAT_ID);
        assert_eq!(row.media_mode, media::KeyMode::Random.as_byte());
        assert_eq!(row.plaintext_size, pt.len() as u64);
        assert_eq!(&blob[..media::HEADER_SIZE as usize], &header[..]);
        assert!(!blob.starts_with(b"RCLONE"));
        assert_eq!(
            Some(blob.len() as u64),
            media::ciphertext_size(pt.len() as u64)
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
