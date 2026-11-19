//! Credential computation only. The caller owns lookup, rate admission and
//! conditional completion; none of these helpers grants a principal.

use nntune_subsonic::auth::md5_hex;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::{
    field_envelope::{self, Context},
    rotation::KekSource,
};

use super::{AuthError, CredentialRecord};

pub(super) fn token_context<'a>(user_id: &'a str, credential_id: &'a str) -> Context<'a> {
    Context {
        table: "subsonic_credentials",
        field: "token_secret_encrypted",
        scope: user_id,
        row: credential_id,
        version: 2,
        format: 1,
    }
}

/// Check a token on an already-admitted record. The caller validates the
/// request and record before rate admission, then completes only on success.
pub(super) fn verify_token(
    selected: &CredentialRecord,
    presented: &str,
    salt: &str,
    keks: &impl KekSource,
) -> Result<(), AuthError> {
    let stored = selected.token.as_ref().ok_or(AuthError::InvalidToken)?;
    if stored.envelope_version != 2
        || stored.payload_format != 1
        || stored.envelope.ciphertext.len() < 32 + 16
        || stored.envelope.ciphertext.len() > 512 + 16
    {
        return Err(AuthError::InvalidToken);
    }
    let kek = keks.kek(stored.kek_version).ok_or(AuthError::Unavailable)?;
    let context = token_context(&selected.user_id, &selected.id);
    let secret = Zeroizing::new(
        field_envelope::open(kek, stored.kek_version, &context, &stored.envelope)
            .map_err(|_| AuthError::InvalidToken)?,
    );
    // The provisioning path uses a generated printable secret, not raw
    // arbitrary bytes or a reversible copy of the API key.
    if !(32..=512).contains(&secret.len()) || !secret.iter().all(u8::is_ascii_graphic) {
        return Err(AuthError::InvalidToken);
    }
    let salted_capacity = secret
        .len()
        .checked_add(salt.len())
        .ok_or(AuthError::Unavailable)?;
    let mut salted = Zeroizing::new(Vec::with_capacity(salted_capacity));
    salted.extend_from_slice(&secret);
    salted.extend_from_slice(salt.as_bytes());
    let expected = Zeroizing::new(md5_hex(&salted));
    // MD5 is only the mandated compatibility checksum. Constant-time
    // comparison prevents a prefix oracle; accepting hex case changes no bits.
    let mut normalized = Zeroizing::new(
        <[u8; 32]>::try_from(presented.as_bytes()).map_err(|_| AuthError::InvalidToken)?,
    );
    normalized.make_ascii_lowercase();
    if expected.as_bytes().ct_eq(normalized.as_slice()).unwrap_u8() != 1 {
        return Err(AuthError::InvalidToken);
    }
    Ok(())
}
