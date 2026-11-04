//! Internal pool-channel authentication shared by the API and stream Workers.
//! POST only; exact path, timestamp and bytes are authenticated. These are not
//! user tokens, leases, or replay-proof commands. Operation replay is bounded
//! by fresh account admission; it never transfers or releases a remote lease.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const SECRET_VAR: &str = "POOL_RESOLVER_SECRET";
pub const REGISTRY_PATH: &str = "/internal/v1/pool-providers";
pub const RESOLVE_PATH: &str = "/internal/v1/pool-providers/resolve";
pub const MAX_REQUEST_BYTES: usize = 512;
pub const MAX_CLOCK_SKEW_SECS: u64 = 30;
pub const DIAL_MATERIAL_TTL_SECS: u64 = 5;
pub const ACCOUNT_SECRET_VAR: &str = "POOL_ACCOUNT_SECRET";
pub const CANDIDATE_PATH: &str = "/candidate";
pub const OPERATION_PATH: &str = "/operation";
// Up to 500 validated message IDs, including JSON escaping. Resolver and hint
// requests retain their original 512-byte limit.
pub const MAX_OPERATION_BYTES: usize = 1024 * 1024;
const MIN_SECRET_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    NotConfigured,
    Unauthorized,
}

#[derive(Clone, Copy)]
pub enum Channel {
    Resolver,
    Account,
}

impl Channel {
    fn scheme(self) -> &'static str {
        match self {
            Self::Resolver => "PoolResolver",
            Self::Account => "PoolAccount",
        }
    }

    fn domain(self) -> &'static [u8] {
        match self {
            Self::Resolver => b"nntune-pool-resolver-v1\nPOST\n",
            Self::Account => b"nntune-pool-account-v1\nPOST\n",
        }
    }

    fn allows(self, path: &str) -> bool {
        match self {
            Self::Resolver => matches!(path, REGISTRY_PATH | RESOLVE_PATH),
            Self::Account => matches!(path, CANDIDATE_PATH | OPERATION_PATH),
        }
    }

    fn max_request_bytes(self, path: &str) -> usize {
        match (self, path) {
            (Self::Account, OPERATION_PATH) => MAX_OPERATION_BYTES,
            _ => MAX_REQUEST_BYTES,
        }
    }

    fn signature(self, secret: &[u8], path: &str, timestamp: u64, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
        mac.update(self.domain());
        mac.update(path.as_bytes());
        mac.update(b"\n");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b"\n");
        mac.update(body);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn authorization_header(
        self,
        secret: Option<&[u8]>,
        path: &str,
        timestamp: u64,
        body: &[u8],
    ) -> Option<String> {
        let secret = secret.filter(|s| s.len() >= MIN_SECRET_BYTES)?;
        if !self.allows(path) || body.len() > self.max_request_bytes(path) {
            return None;
        }
        Some(format!(
            "{} {timestamp}.{}",
            self.scheme(),
            self.signature(secret, path, timestamp, body)
        ))
    }

    pub fn verify(
        self,
        secret: Option<&[u8]>,
        path: &str,
        authorization: Option<&str>,
        body: &[u8],
        now: u64,
    ) -> Result<(), AuthError> {
        let secret = secret
            .filter(|s| s.len() >= MIN_SECRET_BYTES)
            .ok_or(AuthError::NotConfigured)?;
        if !self.allows(path) || body.len() > self.max_request_bytes(path) {
            return Err(AuthError::Unauthorized);
        }
        let (scheme, signed) = authorization
            .and_then(|h| h.split_once(' '))
            .ok_or(AuthError::Unauthorized)?;
        if scheme != self.scheme() {
            return Err(AuthError::Unauthorized);
        }
        let (timestamp, supplied) = signed.split_once('.').ok_or(AuthError::Unauthorized)?;
        let timestamp: u64 = timestamp.parse().map_err(|_| AuthError::Unauthorized)?;
        if now.abs_diff(timestamp) > MAX_CLOCK_SKEW_SECS {
            return Err(AuthError::Unauthorized);
        }
        let expected = self.signature(secret, path, timestamp, body);
        if expected.as_bytes().ct_eq(supplied.as_bytes()).into() {
            Ok(())
        } else {
            Err(AuthError::Unauthorized)
        }
    }
}

pub fn authorization_header(
    secret: Option<&[u8]>,
    path: &str,
    timestamp: u64,
    body: &[u8],
) -> Option<String> {
    Channel::Resolver.authorization_header(secret, path, timestamp, body)
}

pub fn verify(
    secret: Option<&[u8]>,
    path: &str,
    authorization: Option<&str>,
    body: &[u8],
    now: u64,
) -> Result<(), AuthError> {
    Channel::Resolver.verify(secret, path, authorization, body, now)
}
