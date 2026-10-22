//! `providers` row minting from the BYOP vault flow (migration 0022).
//!
//! Every `PUT /v1/vault` that saves provider credentials also mints one
//! `providers` row: the vault row stays the single-credential source of
//! truth (`provider_vaults`, legacy envelope v1), while the `providers`
//! row is the identity-bearing projection the stream pool and the ingest
//! grant publication join against (`user_cache_grants.provider_id`).
//!
//! Envelope (ADR 0002 rule 1): brand-new v2 field envelope — fresh
//! per-row 32-byte DEK, fresh 12-byte nonces, canonical
//! length-prefixed AAD (`domain="nntune-envelope-v2"`, purpose, table,
//! field, owner scope, row key, versions). The v1 helpers in
//! [`crate::vault`] cannot produce this (their AAD joins are explicitly
//! not the v2 encoding, and byte-copying v1 ciphertext under v2 AAD is
//! forbidden), so this module implements the v2 AAD while reusing the
//! existing envelope machinery: [`crate::random::secure_random_bytes`]
//! for every random fill, `aes-gcm` for the AEAD, the [`crate::vault`]
//! error taxonomy, and `Config::kek_by_version` for KEK selection. No
//! second RNG, no second AEAD pattern, no hand-rolled base64 (v2 binds
//! raw bytes into `BYTEA` columns).
//!
//! Mint shape: one `INSERT` per save (no `ON CONFLICT` — each save mints
//! a fresh credential identity). A re-save (rotation) mints a new row;
//! the pool resolves the latest row per user; historical grants keep
//! pointing at the row whose credentials fetched them (the `RESTRICT`
//! FK is that guarantee). `label` is display-only, derived from the
//! provider host ([`provider_label_for_host`]); identity is the UUID.
//! `last_verified_at` equals `created_at` at mint ("minted, verification
//! pending") until the test-connection-success bump lands (follow-up;
//! no landed code reads the column as a verification signal today).
//!
//! Attribution decision (recorded 2026-10-22): ingest learns
//! `provider_id` from the **pool reply**, never from a vault lookup at
//! fetch time. Ingest cannot look the vault up — the KEK lives only in
//! `nntune-api` (CISO decision: no KEK in stream/ingest) and the rows
//! are envelope-encrypted — and only the pool knows which provider row
//! actually served the bytes once rotations leave several rows per
//! user. Ingest-side recording (stamping `QueueMsg.provider_id` / the
//! checkpoint from the pool reply at the `call_pool_body` site) is a
//! follow-up, not this module.
//!
//! Delete path (explicitly deferred): `DELETE /v1/vault` does not touch
//! `providers` rows yet. Revoke/GC semantics (vault delete must stop
//! the pool from dialling removed credentials; `RESTRICT` grants block
//! naive row deletion) are decided with the pool-reader slice, which is
//! what first consumes these rows. No landed code reads them today.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::vault::VaultError;

/// Value bound into `providers.creds_envelope_version`: the ADR 0002
/// rule-1 v2 field envelope. A SQL literal in
/// [`insert_provider_sql`], never a caller parameter (the schema
/// `CHECK (creds_envelope_version = 2)` backstops it).
pub const CREDS_ENVELOPE_VERSION: i16 = 2;

/// Value bound into `providers.creds_payload_format`: JSON-serialized
/// [`crate::vault::ProviderConfig`]. A SQL literal in
/// [`insert_provider_sql`], never a caller parameter.
pub const CREDS_PAYLOAD_FORMAT_JSON_V1: i16 = 1;

/// Longest display label (`providers.label CHECK (char_length <= 64)`).
pub const MAX_LABEL_CHARS: usize = 64;

/// Latest bindable `created_at`/`last_verified_at`: mirrors the
/// migration `CHECK` (`<= 253402300799`), so a caller clock fault fails
/// here instead of as a 23514 at the database.
pub const MAX_MINT_AT_SECS: i64 = 253402300799;

/// GCM nonce length (schema `CHECK (octet_length(..) = 12)`).
pub const NONCE_LEN: usize = 12;
/// Wrapped-DEK length: 32-byte DEK + 16-byte GCM tag (schema `CHECK
/// (octet_length(creds_dek_wrapped) = 48)`).
pub const WRAPPED_DEK_LEN: usize = 48;

/// AAD domain pinning the v2 envelope family (ADR 0002 rule 1).
const AAD_DOMAIN: &str = "nntune-envelope-v2";
/// AAD table/field pins for this envelope.
const AAD_TABLE: &str = "providers";
const AAD_FIELD: &str = "encrypted_creds";

/// Display label for a minted row: the provider host, truncated to
/// [`MAX_LABEL_CHARS`] at a char boundary. Identity is the row UUID;
/// the label never participates in a lookup, so truncation is display
/// lossy but never ambiguous to the pool.
pub fn provider_label_for_host(host: &str) -> String {
    host.chars().take(MAX_LABEL_CHARS).collect()
}

/// Fresh v4 UUID text for `providers.id`, from
/// [`crate::random::secure_random_bytes`] (version + variant bits set).
/// Randomness failure fails closed as
/// [`VaultError::RandomnessUnavailable`] (HTTP 503), like the envelope
/// fills — never a fallback id, never a reused id.
pub fn mint_provider_id() -> Result<String, VaultError> {
    let bytes = crate::random::secure_random_bytes::<16>()?;
    let mut b = bytes;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12],
        b[13], b[14], b[15]
    ))
}

/// Minted-id shape check: `8-4-4-4-12` hex, mirroring ingest's
/// `validate_provider_id` so every minted id validates downstream at
/// admission. The pool's future attribution format is not ours to
/// constrain beyond the UUID shape (no version/variant pin here).
pub fn validate_provider_mint_id(raw: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = raw.split('-').collect();
    if parts.len() != GROUPS.len() {
        return false;
    }
    parts
        .iter()
        .zip(GROUPS)
        .all(|(p, len)| p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Per-row data-encrypting key, cleared on drop (mirrors the v1
/// holder in [`crate::vault`]: the same zeroizing discipline, not a
/// second crypto design).
#[derive(Zeroize, ZeroizeOnDrop)]
struct Dek([u8; 32]);

/// v2 credential envelope: raw bytes for the `BYTEA` columns. No `Debug`
/// derive: this struct carries key material adjacent to ciphertext, and
/// a derived dump would print both (threat-model §3: no log line may
/// contain credential material).
pub struct ProviderCredsEnvelope {
    pub dek_wrapped_nonce: [u8; NONCE_LEN],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

/// Frame one variable-length AAD segment: `u32` big-endian length +
/// bytes. Fixed field order + fixed int widths make the encoding
/// unambiguous (ADR 0002 rule 1: no delimiter joins).
fn frame(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Canonical v2 AAD: `(domain, purpose, table, field, owner_scope,
/// row_key, envelope_version, payload_format)` plus `kek_version` for
/// the DEK wrap. Built from the row being written/read, never from a
/// stored unauthenticated claim. Payload AAD intentionally excludes
/// `kek_version`: an authorized rewrap changes only the wrap columns.
fn aad_v2(
    purpose: &str,
    user_id: &str,
    provider_id: &str,
    envelope_version: u16,
    payload_format: u16,
    kek_version: Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + user_id.len() + provider_id.len());
    frame(&mut out, AAD_DOMAIN);
    frame(&mut out, purpose);
    frame(&mut out, AAD_TABLE);
    frame(&mut out, AAD_FIELD);
    frame(&mut out, user_id);
    frame(&mut out, provider_id);
    out.extend_from_slice(&envelope_version.to_be_bytes());
    out.extend_from_slice(&payload_format.to_be_bytes());
    if let Some(v) = kek_version {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Encrypt provider credentials into a v2 envelope bound to
/// (`user_id`, `provider_id`).
///
/// Fail-closed: secure-randomness failure (DEK or either nonce)
/// returns [`VaultError::RandomnessUnavailable`] (HTTP 503).
pub fn encrypt_provider_creds(
    kek: &[u8; 32],
    kek_version: u32,
    user_id: &str,
    provider_id: &str,
    plaintext: &[u8],
) -> Result<ProviderCredsEnvelope, VaultError> {
    let mut dek_bytes = crate::random::secure_random_bytes::<32>()?;
    let dek = Dek(dek_bytes);
    // `Dek(dek_bytes)` copies the array: clear the source buffer so the
    // DEK lives only in the zeroizing holder (mirrors `vault.rs`).
    dek_bytes.zeroize();

    // Wrap the DEK with the KEK (AAD binds user + row + KEK version).
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let wrap_nonce = crate::random::secure_random_bytes::<NONCE_LEN>()?;
    let dek_wrapped = kek_cipher
        .encrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &dek.0,
                aad: &aad_v2(
                    "dek",
                    user_id,
                    provider_id,
                    CREDS_ENVELOPE_VERSION as u16,
                    CREDS_PAYLOAD_FORMAT_JSON_V1 as u16,
                    Some(kek_version),
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)?;

    // Encrypt the payload with the DEK (AAD binds user + row; no KEK
    // version, so an authorized rewrap never touches the payload).
    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    let payload_nonce = crate::random::secure_random_bytes::<NONCE_LEN>()?;
    let ciphertext = dek_cipher
        .encrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: plaintext,
                aad: &aad_v2(
                    "payload",
                    user_id,
                    provider_id,
                    CREDS_ENVELOPE_VERSION as u16,
                    CREDS_PAYLOAD_FORMAT_JSON_V1 as u16,
                    None,
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)?;

    Ok(ProviderCredsEnvelope {
        dek_wrapped_nonce: wrap_nonce,
        dek_wrapped,
        payload_nonce,
        ciphertext,
    })
}

/// Decrypt a minted `providers` row. The expected AAD is built from
/// the row being read (ADR 0002 rule 1): the caller passes the row's
/// stored `envelope_version`, `payload_format` and `kek_version`, and
/// the KEK for that stored version (version-pinned decrypt — no
/// fallback loop). Stored-shape failures (negative versions, a
/// short/long wrapped DEK) are [`VaultError::CorruptStoredData`];
/// authentication failures are the generic [`VaultError::Crypto`].
/// No `Debug` on the envelope: nothing here may land in a log line.
pub fn decrypt_provider_creds(
    kek: &[u8; 32],
    user_id: &str,
    provider_id: &str,
    envelope_version: i16,
    payload_format: i16,
    kek_version: u32,
    envelope: &ProviderCredsEnvelope,
) -> Result<Vec<u8>, VaultError> {
    let envelope_version =
        u16::try_from(envelope_version).map_err(|_| VaultError::CorruptStoredData)?;
    let payload_format =
        u16::try_from(payload_format).map_err(|_| VaultError::CorruptStoredData)?;
    if envelope.dek_wrapped.len() != WRAPPED_DEK_LEN {
        return Err(VaultError::CorruptStoredData);
    }
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let mut dek_bytes = kek_cipher
        .decrypt(
            &Nonce::from(envelope.dek_wrapped_nonce),
            Payload {
                msg: &envelope.dek_wrapped,
                aad: &aad_v2(
                    "dek",
                    user_id,
                    provider_id,
                    envelope_version,
                    payload_format,
                    Some(kek_version),
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    if dek_bytes.len() != 32 {
        dek_bytes.zeroize();
        return Err(VaultError::Crypto);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&dek_bytes);
    // Clear the AES-GCM output buffer: the DEK now lives only in `dek`.
    dek_bytes.zeroize();
    let dek = Dek(arr);

    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    dek_cipher
        .decrypt(
            &Nonce::from(envelope.payload_nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &aad_v2(
                    "payload",
                    user_id,
                    provider_id,
                    envelope_version,
                    payload_format,
                    None,
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)
}

/// `$n` parameter order for [`insert_provider_sql`]. The envelope
/// version and payload format are SQL literals (2, 1), not parameters.
pub const PROVIDER_MINT_PARAM_ORDER: [&str; 10] = [
    "id",
    "user_id",
    "label",
    "encrypted_creds",
    "creds_kek_version",
    "creds_dek_wrapped_nonce",
    "creds_dek_wrapped",
    "creds_payload_nonce",
    "created_at",
    "last_verified_at",
];

/// Mint write: one fresh-identity `providers` row per vault save. The
/// `writer_fence` (the owner's `users` row `FOR SHARE` while
/// `deleting_at IS NULL`, ADR 0002 rule 3) makes a fenced mint affect
/// 0 rows; the handler classifies that through `account_deleting`
/// into the typed 409, like the vault upsert. No `ON CONFLICT`: a
/// UUID collision raises 23505 and fails closed, never converging two
/// saves onto one row.
pub fn insert_provider_sql() -> &'static str {
    "INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version, creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce, creds_payload_format, created_at, last_verified_at) SELECT $1, $2, $3, $4, 2, $5, $6, $7, $8, 1, $9, $10 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
}

/// One mintable row: the envelope output plus the identity the vault
/// flow binds it to. Raw bytes throughout (`BYTEA` columns). No
/// `Debug` derive: `encrypted_creds` is ciphertext, but a derived dump
/// next to key-adjacent fields is one refactor from a leak
/// (threat-model §3).
#[derive(Clone)]
pub struct ProviderMintRow {
    pub id: String,
    pub user_id: String,
    pub label: String,
    pub encrypted_creds: Vec<u8>,
    pub kek_version: u32,
    pub dek_wrapped_nonce: [u8; NONCE_LEN],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; NONCE_LEN],
    pub created_at: u64,
    pub last_verified_at: u64,
}

/// Check mint values before they touch the database. Bounds mirror
/// migration 0022 (`CHECK`s on `label`, the nonce/wrapped lengths,
/// `created_at`/`last_verified_at`); the id shape mirrors
/// [`validate_provider_mint_id`] so a minted id always validates
/// downstream at ingest admission.
pub fn validate_mint_row(row: &ProviderMintRow) -> Result<(), &'static str> {
    if !validate_provider_mint_id(&row.id) {
        return Err("provider id must be 8-4-4-4-12 hex");
    }
    if row.user_id.is_empty() || row.user_id.len() > 128 {
        return Err("user id must be 1..=128 chars");
    }
    if row.label.is_empty() || row.label.chars().count() > MAX_LABEL_CHARS {
        return Err("label must be 1..=64 chars");
    }
    if row.encrypted_creds.is_empty() {
        return Err("encrypted creds must not be empty");
    }
    if row.dek_wrapped.len() != WRAPPED_DEK_LEN {
        return Err("wrapped DEK must be exactly 48 bytes");
    }
    if row.created_at > MAX_MINT_AT_SECS as u64 || row.last_verified_at > MAX_MINT_AT_SECS as u64 {
        return Err("mint clock must be 0..=253402300799 unix seconds");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_is_the_host_truncated_to_64_chars() {
        assert_eq!(
            provider_label_for_host("news.example.com"),
            "news.example.com"
        );
        let long = "a".repeat(100);
        assert_eq!(provider_label_for_host(&long), "a".repeat(64));
        // Multibyte truncation stops at a char boundary, never mid-codepoint.
        let wide = "é".repeat(100);
        let label = provider_label_for_host(&wide);
        assert_eq!(label.chars().count(), 64);
        assert!(label.is_char_boundary(label.len()));
    }

    #[test]
    fn minted_ids_are_v4_uuids_that_validate_downstream() {
        for _ in 0..8 {
            let id = mint_provider_id().expect("mint");
            assert!(validate_provider_mint_id(&id), "{id}");
            // v4 version nibble + RFC 4122 variant bits.
            assert_eq!(&id[14..15], "4", "{id}");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        }
        // Uniqueness across mints (a dropped RNG fill would repeat ids).
        let a = mint_provider_id().expect("mint");
        let b = mint_provider_id().expect("mint");
        assert_ne!(a, b);
    }

    #[test]
    fn mint_id_shape_rejects_non_uuids() {
        for bad in [
            "",
            "not-a-uuid",
            "11111111-1111-1111-1111-11111111111",
            "11111111-1111-1111-1111-1111111111111",
            "11111111_1111_1111_1111_111111111111",
            "gggggggg-1111-1111-1111-111111111111",
            "11111111-1111-1111-1111",
        ] {
            assert!(!validate_provider_mint_id(bad), "{bad}");
        }
        assert!(validate_provider_mint_id(
            "11111111-1111-4111-8111-111111111111"
        ));
    }

    #[test]
    fn v2_aad_encoding_is_pinned_byte_exact() {
        let aad = aad_v2(
            "payload",
            "user-1",
            "11111111-1111-4111-8111-111111111111",
            2,
            1,
            None,
        );
        let mut expected = Vec::new();
        for s in [
            "nntune-envelope-v2",
            "payload",
            "providers",
            "encrypted_creds",
            "user-1",
            "11111111-1111-4111-8111-111111111111",
        ] {
            expected.extend_from_slice(&(s.len() as u32).to_be_bytes());
            expected.extend_from_slice(s.as_bytes());
        }
        expected.extend_from_slice(&2u16.to_be_bytes());
        expected.extend_from_slice(&1u16.to_be_bytes());
        assert_eq!(aad, expected);

        // The DEK wrap appends the KEK version; purposes never collide.
        let dek = aad_v2(
            "dek",
            "user-1",
            "11111111-1111-4111-8111-111111111111",
            2,
            1,
            Some(7),
        );
        assert!(dek.starts_with(&expected));
        assert_eq!(&dek[expected.len()..], &7u32.to_be_bytes());
        assert_ne!(
            aad_v2("dek", "user-1", "p", 2, 1, Some(1)),
            aad_v2("payload", "user-1", "p", 2, 1, None)
        );
    }

    #[test]
    fn mint_round_trips_through_encrypt_decrypt() {
        let kek = [0x11u8; 32];
        let plaintext = br#"{"host":"news.example.com","port":563}"#;
        let envelope = encrypt_provider_creds(
            &kek,
            1,
            "user-1",
            "11111111-1111-4111-8111-111111111111",
            plaintext,
        )
        .expect("encrypt");
        assert_eq!(envelope.dek_wrapped.len(), WRAPPED_DEK_LEN);
        assert!(!envelope.ciphertext.is_empty());
        let back = decrypt_provider_creds(
            &kek,
            "user-1",
            "11111111-1111-4111-8111-111111111111",
            2,
            1,
            1,
            &envelope,
        )
        .expect("decrypt");
        assert_eq!(back, plaintext);
    }

    #[test]
    fn envelope_rejects_cross_row_user_field_and_version_substitution() {
        let kek = [0x11u8; 32];
        let id = "11111111-1111-4111-8111-111111111111";
        let envelope = encrypt_provider_creds(&kek, 1, "user-1", id, b"secret").expect("encrypt");
        // Wrong row, wrong user, wrong versions: every substitution fails
        // authentication (generic Crypto, never which check failed).
        for (user, pid, fmt, ver) in [
            ("user-1", "22222222-2222-4222-8222-222222222222", 1, 1),
            ("user-2", id, 1, 1),
            ("user-1", id, 2, 1),
            ("user-1", id, 1, 2),
        ] {
            assert!(
                decrypt_provider_creds(&kek, user, pid, 2, fmt, ver, &envelope).is_err(),
                "substitution accepted: {user} {pid} {fmt} {ver}"
            );
        }
        // A tampered ciphertext byte fails too.
        let mut tampered = ProviderCredsEnvelope {
            dek_wrapped_nonce: envelope.dek_wrapped_nonce,
            dek_wrapped: envelope.dek_wrapped.clone(),
            payload_nonce: envelope.payload_nonce,
            ciphertext: envelope.ciphertext.clone(),
        };
        tampered.ciphertext[0] ^= 0x01;
        assert!(decrypt_provider_creds(&kek, "user-1", id, 2, 1, 1, &tampered).is_err());
    }

    fn mint_row() -> ProviderMintRow {
        ProviderMintRow {
            id: "11111111-1111-4111-8111-111111111111".to_string(),
            user_id: "user-1".to_string(),
            label: "news.example.com".to_string(),
            encrypted_creds: vec![1, 2, 3],
            kek_version: 1,
            dek_wrapped_nonce: [7u8; NONCE_LEN],
            dek_wrapped: vec![8u8; WRAPPED_DEK_LEN],
            payload_nonce: [9u8; NONCE_LEN],
            created_at: 1_700_000_000,
            last_verified_at: 1_700_000_000,
        }
    }

    #[test]
    fn mint_row_params_are_validated_before_they_touch_the_database() {
        assert!(validate_mint_row(&mint_row()).is_ok());
        // Every bound fails closed on its own arm.
        let mut bad_id = mint_row();
        bad_id.id = "not-a-uuid".to_string();
        assert!(validate_mint_row(&bad_id).is_err());
        let mut bad_user = mint_row();
        bad_user.user_id = String::new();
        assert!(validate_mint_row(&bad_user).is_err());
        let mut bad_label = mint_row();
        bad_label.label = "l".repeat(65);
        assert!(validate_mint_row(&bad_label).is_err());
        let mut empty_label = mint_row();
        empty_label.label = String::new();
        assert!(validate_mint_row(&empty_label).is_err());
        let mut empty_ct = mint_row();
        empty_ct.encrypted_creds.clear();
        assert!(validate_mint_row(&empty_ct).is_err());
        let mut short_wrap = mint_row();
        short_wrap.dek_wrapped.pop();
        assert!(validate_mint_row(&short_wrap).is_err());
        let mut bad_clock = mint_row();
        bad_clock.created_at = MAX_MINT_AT_SECS as u64 + 1;
        assert!(validate_mint_row(&bad_clock).is_err());
        let mut bad_verified = mint_row();
        bad_verified.last_verified_at = MAX_MINT_AT_SECS as u64 + 1;
        assert!(validate_mint_row(&bad_verified).is_err());
        // Bounds are inclusive at both ends.
        let mut floor = mint_row();
        floor.created_at = 0;
        floor.last_verified_at = 0;
        assert!(validate_mint_row(&floor).is_ok());
        let mut ceiling = mint_row();
        ceiling.created_at = MAX_MINT_AT_SECS as u64;
        ceiling.last_verified_at = MAX_MINT_AT_SECS as u64;
        assert!(validate_mint_row(&ceiling).is_ok());
    }

    #[test]
    fn decrypt_classifies_stored_shape_failures_as_corrupt() {
        let kek = [0x11u8; 32];
        let id = "11111111-1111-4111-8111-111111111111";
        let envelope = encrypt_provider_creds(&kek, 1, "user-1", id, b"secret").expect("encrypt");
        // Negative stored versions are corrupt storage, never a
        // version the writer could have emitted.
        assert_eq!(
            decrypt_provider_creds(&kek, "user-1", id, -1, 1, 1, &envelope),
            Err(VaultError::CorruptStoredData)
        );
        assert_eq!(
            decrypt_provider_creds(&kek, "user-1", id, 2, -1, 1, &envelope),
            Err(VaultError::CorruptStoredData)
        );
        // A short wrapped DEK is corrupt storage too, distinct from an
        // authentication failure.
        let mut short = ProviderCredsEnvelope {
            dek_wrapped_nonce: envelope.dek_wrapped_nonce,
            dek_wrapped: envelope.dek_wrapped[..47].to_vec(),
            payload_nonce: envelope.payload_nonce,
            ciphertext: envelope.ciphertext.clone(),
        };
        assert_eq!(
            decrypt_provider_creds(&kek, "user-1", id, 2, 1, 1, &short),
            Err(VaultError::CorruptStoredData)
        );
        // ...while a wrong KEK is the generic crypto failure (no oracle
        // for which version or row was wrong).
        short.dek_wrapped = envelope.dek_wrapped.clone();
        assert_eq!(
            decrypt_provider_creds(&[0x22u8; 32], "user-1", id, 2, 1, 1, &short),
            Err(VaultError::Crypto)
        );
    }

    #[test]
    fn encrypt_randomness_differs_per_call() {
        // Pin the secure-randomness fills: a dropped fill would leave a
        // zero DEK and constant AES-GCM nonces (catastrophic nonce reuse)
        // while round-trip tests stayed green.
        let kek = [0x11u8; 32];
        let id = "11111111-1111-4111-8111-111111111111";
        let a = encrypt_provider_creds(&kek, 1, "user-1", id, b"secret").expect("encrypt");
        let b = encrypt_provider_creds(&kek, 1, "user-1", id, b"secret").expect("encrypt");
        assert_ne!(a.dek_wrapped_nonce, b.dek_wrapped_nonce);
        assert_ne!(a.payload_nonce, b.payload_nonce);
        assert_ne!(a.dek_wrapped, b.dek_wrapped);
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_ne!(a.dek_wrapped_nonce, [0u8; NONCE_LEN]);
        assert_ne!(a.payload_nonce, [0u8; NONCE_LEN]);
    }

    #[test]
    fn mint_sql_shape_is_pinned() {
        assert_eq!(
            insert_provider_sql(),
            "INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version, \
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce, \
             creds_payload_format, created_at, last_verified_at) \
             SELECT $1, $2, $3, $4, 2, $5, $6, $7, $8, 1, $9, $10 \
             FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
        );
        assert_eq!(
            PROVIDER_MINT_PARAM_ORDER,
            [
                "id",
                "user_id",
                "label",
                "encrypted_creds",
                "creds_kek_version",
                "creds_dek_wrapped_nonce",
                "creds_dek_wrapped",
                "creds_payload_nonce",
                "created_at",
                "last_verified_at"
            ]
        );
        // The fence binds the owner ($2): dropping the marker check, the
        // lock, or the in-statement join fails here (ADR 0002 rule 3).
        let sql = insert_provider_sql();
        assert!(
            sql.contains(
                "FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
            ),
            "{sql}"
        );
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert!(!sql.contains("NOWAIT"), "{sql}");
        assert!(!sql.contains("SKIP LOCKED"), "{sql}");
        // Envelope constants are literals, never caller parameters.
        assert!(
            sql.contains("SELECT $1, $2, $3, $4, 2, $5, $6, $7, $8, 1, $9, $10"),
            "{sql}"
        );
        // Fresh-identity insert: no ON CONFLICT. A UUID collision raises
        // 23505 and fails closed; it must never converge two saves onto
        // one row.
        assert!(!sql.contains("ON CONFLICT"), "{sql}");
    }
}
