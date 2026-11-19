//! Credential computation only. The caller owns lookup, rate admission and
//! conditional completion; none of these helpers grants a principal.

use nntune_subsonic::auth::{md5_hex, md5_hex_charged};
use nntune_subsonic::construction_budget::ConstructionBudget;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::{
    field_envelope::{self, Context},
    native_rest::credential_hmac,
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

pub(super) fn indexed_digest(
    key: &str,
    pepper: &[u8],
    budget: Option<&mut ConstructionBudget>,
) -> Result<String, AuthError> {
    if let Some(budget) = budget {
        // SHA-256 HMAC state is inline; credential_hmac reserves one exact
        // lowercase-hex String with no per-byte formatting allocations.
        budget
            .charge(64, 1, 1)
            .map_err(|_| AuthError::Unavailable)?;
    }
    Ok(credential_hmac(key, pepper))
}

/// Check a token on an already-admitted record. The caller validates the
/// request and record before rate admission, then completes only on success.
pub(super) fn verify_token(
    selected: &CredentialRecord,
    presented: &str,
    salt: &str,
    keks: &impl KekSource,
    mut budget: Option<&mut ConstructionBudget>,
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
    if let Some(budget) = budget.as_deref_mut() {
        for capacity in field_envelope::open_capacities(&context, &stored.envelope)
            .ok_or(AuthError::Unavailable)?
        {
            budget
                .charge(capacity, 1, 1)
                .map_err(|_| AuthError::Unavailable)?;
        }
    }
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
    let mut salted = Zeroizing::new(match budget.as_deref_mut() {
        Some(budget) => budget
            .allocate_exact(salted_capacity)
            .map_err(|_| AuthError::Unavailable)?,
        None => Vec::with_capacity(salted_capacity),
    });
    salted.extend_from_slice(&secret);
    salted.extend_from_slice(salt.as_bytes());
    let expected = Zeroizing::new(match budget {
        Some(budget) => md5_hex_charged(&salted, budget).map_err(|_| AuthError::Unavailable)?,
        None => md5_hex(&salted),
    });
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

#[cfg(any(test, all(target_arch = "wasm32", feature = "ledger-wasm-test")))]
#[cfg_attr(test, test)]
pub(crate) fn verification_uses_the_carried_charge() {
    use super::{AuthMode, TokenEnvelope};

    const KEY: [u8; 32] = [4; 32];
    struct Keys;
    impl KekSource for Keys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            (version == 1).then_some(&KEY)
        }
    }

    const CARRIED: usize = 24;
    const DIGEST: usize = 80;
    let carried = |limit| {
        let mut budget = ConstructionBudget::new(limit);
        budget.charge(1, 1, 1).unwrap();
        budget
    };
    let mut exact = carried(CARRIED + DIGEST);
    let mut overflow = ConstructionBudget::new(usize::MAX);
    overflow.charge(usize::MAX - 31, 1, 1).unwrap();
    for mut budget in [carried(CARRIED + DIGEST - 1), overflow] {
        let before = budget.charged();
        let result = indexed_digest("fixture-key", &KEY, Some(&mut budget));
        assert_eq!(result, Err(AuthError::Unavailable));
        assert_eq!(budget.charged(), before);
    }
    let digest = indexed_digest("fixture-key", &KEY, Some(&mut exact)).unwrap();
    assert_eq!(digest, credential_hmac("fixture-key", &KEY));
    assert_eq!(digest.capacity(), 64);
    assert_eq!(exact.charged(), CARRIED + DIGEST);

    const PRESENTED: &str = "F1E31E7D653A9EBC711B2890663ACEC6"; // MD5 of x*512 + s*256
    const WRONG: &str = "01E31E7D653A9EBC711B2890663ACEC6";
    let context = token_context("owner", "credential");
    let salt = "s".repeat(256);
    let envelope = field_envelope::seal(&KEY, 1, &context, &[b'x'; 512]).unwrap();
    let decrypted = Zeroizing::new(field_envelope::open(&KEY, 1, &context, &envelope).unwrap());
    let capacities = [
        context.aad("dek", Some(1)).capacity(),
        48,
        context.aad("payload", None).capacity(),
        decrypted.capacity(),
    ];
    assert_eq!(decrypted.capacity(), 528);
    assert_eq!(
        field_envelope::open_capacities(&context, &envelope),
        Some(capacities)
    );
    let selected = CredentialRecord {
        id: context.row.into(),
        user_id: context.scope.into(),
        username: "client".into(),
        scope: "subsonic".into(),
        auth_mode: AuthMode::Token,
        revoked: false,
        owner_active: true,
        api_key_hmac: None,
        pepper_version: None,
        token: Some(TokenEnvelope {
            envelope_version: 2,
            payload_format: 1,
            kek_version: 1,
            envelope,
        }),
    };
    // Count one charge per AEAD buffer, then salted input, padded MD5 input
    // and MD5 hex output. Case normalization uses only inline bytes.
    let exact = CARRIED
        + capacities
            .into_iter()
            .chain([768, 832, 32])
            .map(|capacity| (capacity + 4).next_multiple_of(8).max(16) + 8)
            .sum::<usize>();
    let short = Err(AuthError::Unavailable);
    let hex_charge = 48; // 32-byte hex output plus allocator rounding and overhead.
    for (limit, presented, expected, charged) in [
        (exact - 1, PRESENTED, short, exact - hex_charge),
        (exact, PRESENTED, Ok(()), exact),
        (exact, WRONG, Err(AuthError::InvalidToken), exact),
    ] {
        let mut budget = carried(limit);
        let result = verify_token(&selected, presented, &salt, &Keys, Some(&mut budget));
        assert_eq!((result, budget.charged()), (expected, charged));
    }
}
