//! Staging verify leg: open every exported envelope row (authentication
//! only) and count.
//!
//! The operator exports production-shaped rows with BYTEA columns
//! hex-encoded (`encode(col, 'hex')`) — see
//! `docs/offsite-backup-runbook.md` §4b — and supplies the KEKs for
//! exactly the backup's versions. This module never prints plaintext,
//! keys, or ciphertext; the [`VerifyReport`] carries counts and version
//! numbers only. Unknown envelope versions and missing KEK versions are
//! failures, not skips: the keyring backup is incomplete.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::envelope::{self, Envelope, ENVELOPE_VERSION_V2, KEK_LEN};

/// Operator-supplied KEK set (version → KEK). No `Debug`: key material
/// must never be printable. Zeroized on drop, so a normal return from
/// the `verify` leg leaves no KEK bytes in memory; callers drop the
/// value *before* any `process::exit`, which would skip destructors.
#[derive(Clone, Default, Zeroize, ZeroizeOnDrop)]
pub struct Keyring {
    keys: Vec<(u32, [u8; KEK_LEN])>,
}

impl Keyring {
    pub fn new(keys: BTreeMap<u32, [u8; KEK_LEN]>) -> Self {
        Keyring {
            keys: keys.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Copy one KEK out for a single row open. The caller zeroizes the
    /// copy as soon as the open returns.
    pub fn get(&self, version: u32) -> Option<[u8; KEK_LEN]> {
        self.keys
            .iter()
            .find(|(v, _)| *v == version)
            .map(|(_, k)| *k)
    }

    pub fn versions(&self) -> Vec<u32> {
        self.keys.iter().map(|(v, _)| *v).collect()
    }
}

/// One production-shaped envelope row, BYTEA columns hex-encoded.
#[derive(Debug, Deserialize)]
pub struct VerifyRow {
    pub user_id: String,
    pub row_key: String,
    pub envelope_version: u16,
    pub payload_format: u16,
    pub kek_version: u32,
    pub dek_wrapped_nonce_hex: String,
    pub dek_wrapped_hex: String,
    pub payload_nonce_hex: String,
    pub ciphertext_hex: String,
}

/// Counts-only report. No row contents, no key material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyReport {
    pub tool: String,
    pub env: String,
    pub rows: u64,
    pub ok: u64,
    pub failed: u64,
    pub keyring_versions: Vec<u32>,
}

/// Per-row failure class. The operator sees the row number and the
/// class — never row contents, keys, or ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// Row JSON malformed, hex malformed, or a stored-shape violation.
    Shape,
    /// Authentication failure, unknown envelope version, or missing
    /// KEK version: the keyring backup is incomplete or the row is not
    /// ours. Deliberately one class — the caller learns nothing about
    /// which step failed.
    Auth,
}

impl core::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VerifyError::Shape => write!(f, "verify shape"),
            VerifyError::Auth => write!(f, "verify auth"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Hex decode for operator-exported columns. `None` on any malformed
/// input; callers map it to their own error class.
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let val = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    for pair in s.as_bytes().chunks(2) {
        out.push((val(pair[0])? << 4) | val(pair[1])?);
    }
    Some(out)
}

fn hex_bytes(s: &str, expect: usize) -> Option<Vec<u8>> {
    let v = decode_hex(s)?;
    if v.len() != expect {
        return None;
    }
    Some(v)
}

/// Authenticate every row in `rows_jsonl` against `keyring`. The
/// per-row KEK copy and the opened DEK are zeroized immediately: this
/// leg proves openability, it never uses the plaintext.
pub fn verify_rows(rows_jsonl: &str, keyring: &Keyring, tool: &str) -> VerifyReport {
    let mut rows = 0u64;
    let mut ok = 0u64;
    let mut failed = 0u64;
    for line in rows_jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        rows += 1;
        let opened = (|| -> Result<(), VerifyError> {
            let row: VerifyRow = serde_json::from_str(line).map_err(|_| VerifyError::Shape)?;
            if row.envelope_version != ENVELOPE_VERSION_V2 {
                return Err(VerifyError::Auth);
            }
            let mut kek = keyring.get(row.kek_version).ok_or(VerifyError::Auth)?;
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(
                &hex_bytes(&row.dek_wrapped_nonce_hex, 12).ok_or(VerifyError::Shape)?,
            );
            let wrapped = hex_bytes(&row.dek_wrapped_hex, envelope::WRAPPED_DEK_LEN)
                .ok_or(VerifyError::Shape)?;
            let mut pnonce = [0u8; 12];
            pnonce
                .copy_from_slice(&hex_bytes(&row.payload_nonce_hex, 12).ok_or(VerifyError::Shape)?);
            let ct = decode_hex(&row.ciphertext_hex).ok_or(VerifyError::Shape)?;
            let env = Envelope {
                dek_wrapped_nonce: nonce,
                dek_wrapped: wrapped,
                payload_nonce: pnonce,
                ciphertext: ct,
            };
            let opened = envelope::open_media_key(
                &kek,
                &row.user_id,
                &row.row_key,
                row.envelope_version,
                row.payload_format,
                row.kek_version,
                &env,
            )
            .map_err(|_| VerifyError::Auth);
            kek.zeroize();
            let mut dek = opened?;
            dek.zeroize();
            Ok(())
        })();
        if opened.is_ok() {
            ok += 1;
        } else {
            failed += 1;
        }
    }
    VerifyReport {
        tool: tool.to_string(),
        env: "staging".to_string(),
        rows,
        ok,
        failed,
        keyring_versions: keyring.versions(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::seal_media_key;

    fn test_kek(fill: u8) -> [u8; KEK_LEN] {
        [fill; KEK_LEN]
    }

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn row_json(kek_version: u32) -> String {
        let env =
            seal_media_key(&test_kek(0x33), kek_version, "u", "u:t", b"media-key").expect("seal");
        serde_json::json!({
            "user_id": "u",
            "row_key": "u:t",
            "envelope_version": ENVELOPE_VERSION_V2,
            "payload_format": 1,
            "kek_version": kek_version,
            "dek_wrapped_nonce_hex": hex_of(&env.dek_wrapped_nonce),
            "dek_wrapped_hex": hex_of(&env.dek_wrapped),
            "payload_nonce_hex": hex_of(&env.payload_nonce),
            "ciphertext_hex": hex_of(&env.ciphertext),
        })
        .to_string()
    }

    fn keyring(versions: &[u32]) -> Keyring {
        Keyring::new(versions.iter().map(|v| (*v, test_kek(0x33))).collect())
    }

    #[test]
    fn verify_ok_counts() {
        let jsonl = format!("{}\n{}\n", row_json(7), row_json(7));
        let rep = verify_rows(&jsonl, &keyring(&[7]), "test");
        assert_eq!((rep.rows, rep.ok, rep.failed), (2, 2, 0));
        assert_eq!(rep.keyring_versions, vec![7]);
    }

    #[test]
    fn missing_kek_version_fails() {
        let rep = verify_rows(&row_json(7), &keyring(&[8]), "test");
        assert_eq!((rep.rows, rep.ok, rep.failed), (1, 0, 1));
    }

    #[test]
    fn tampered_row_fails() {
        let mut v: serde_json::Value = serde_json::from_str(&row_json(7)).unwrap();
        v["ciphertext_hex"] = serde_json::Value::String("00".repeat(64));
        let rep = verify_rows(&v.to_string(), &keyring(&[7]), "test");
        assert_eq!((rep.rows, rep.ok, rep.failed), (1, 0, 1));
    }

    #[test]
    fn bad_hex_fails_closed() {
        let rep = verify_rows("{\"oops\": true}\n", &keyring(&[7]), "test");
        assert_eq!((rep.rows, rep.ok, rep.failed), (1, 0, 1));
    }
}
