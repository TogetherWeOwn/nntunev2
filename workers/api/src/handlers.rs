//! Framework-agnostic HTTP handlers for the accounts/auth/vault API.
//!
//! The Worker entry (`worker_entry`) adapts `worker::Request` into
//! [`Request`] and renders [`Response`]; all security decisions live here
//! and are native-tested against fake stores.
//!
//! Contracts enforced here (threat model + CISO conditions):
//! - One generic error body per surface: auth failures are always
//!   `401 {"error":"check your email and try again"}`, vault failures
//!   always `400 {"error":"vault unavailable"}`, store failures
//!   `500 {"error":"internal error"}`. No credential, host, banner or
//!   existence detail ever reaches a body.
//! - Every authenticated request goes through
//!   [`session::verify_stored_session`] against the current server row
//!   (revoked/missing/stale-counter/mismatched rows reject), and cookie-
//!   authenticated mutations require an exact `Origin` match (CSRF).
//!   `POST /v1/auth/magic/finish` mints a cookie from a pure bearer token,
//!   so it requires the same exact `Origin` match even when unauthenticated
//!   (login-CSRF session planting): browsers always send `Origin` on this
//!   fetch POST, and API-only clients must send `Origin: <configured origin>`.
//! - Vault reads/writes are keyed ONLY by the session's `user_id` — there
//!   is no handler parameter naming a vault, so a non-owner cannot even
//!   address another vault (no existence oracle).
//! - Magic links, challenges and invites are consumed only through the
//!   `*_atomically` helpers (conditional UPDATE/RETURNING, exactly one
//!   row, committed before any response). Session refresh advances the
//!   server row with a conditional counter update before `Set-Cookie`.
//! - Outbound test-connection dials resolve ALL records immediately
//!   before dialing, validate into a [`vault::ValidatedTarget`], and the
//!   [`Dialer`] receives only that vetted address set.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

use serde::Deserialize;

use crate::auth::{
    self, InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential, SingleUseStore,
};
// Single unpadded-base64url codec (F-HARD-01): `crate::b64` over the
// `base64` crate. The aliased names keep wire-format call sites unchanged.
use crate::b64::{decode as b64url_decode, encode as b64url_encode};
use crate::session::{self, SessionClaims, SessionRecord};
use crate::vault::{
    self, AuditEvent, DnsResolver, IndexerConfig, ProviderConfig, ValidatedTarget, VaultBlob,
};
use crate::webauthn;

/// Request bodies above this size reject before routing (not an oracle:
/// size only, same for every route).
pub const MAX_BODY_BYTES: usize = 32 * 1024;
/// Magic links issuable per email inside the link TTL window.
pub const MAGIC_LINKS_PER_WINDOW: u64 = 3;
// Test-connection budget lives in one place: `vault::TEST_CONNECTION_MAX_ATTEMPTS`
// per `vault::TEST_CONNECTION_WINDOW_SECS`. No handler-level copy: the two
// once disagreed (10 vs 5) with both test suites green.
// Expired leases outlive the bounded test-connection probe by five seconds.
const TEST_CONNECTION_LEASE_SECS: u64 = 20;
const AUTH_START_WINDOW_SECS: u64 = 3600;
/// Finish-route attempt budgets (CISO follow-up): each forged assertion
/// costs P-256 ECDSA verify + SHA-256 plus DB reads, while a failed verify
/// does not burn the challenge — so one challenge id would otherwise allow
/// unlimited CPU-heavy attempts. Two layers: a per-IP lookup budget BEFORE
/// any DB read bounds rotating unknown challenge ids (one Pg connect+SELECT
/// each), then one slot per challenge (passkey finishes) or per client IP
/// (magic finish) BEFORE verification; excess attempts answer the same
/// generic auth error as any other failure. Ten per challenge TTL admits
/// legitimate single attempts and typo retries while cutting off
/// bombardment; 100 per 5 min per IP bounds lookup floods while admitting
/// NAT bursts (the Pg slot reservation caps at 100).
const REGISTER_FINISH_ATTEMPTS_PER_CHALLENGE: u64 = 10;
const LOGIN_FINISH_ATTEMPTS_PER_CHALLENGE: u64 = 10;
const MAGIC_FINISH_ATTEMPTS_PER_IP: u64 = 10;
const FINISH_LOOKUP_ATTEMPTS_PER_IP: u64 = 100;
const FINISH_ATTEMPT_WINDOW_SECS: u64 = crate::auth::CHALLENGE_TTL_SECS;
/// Per-IP auth-start admission: 30/hour admits legitimate multi-user NAT
/// while cutting distinct-email loops. Global capacity is reserved only for
/// outbound mail, so exhaustion cannot block auth challenge issuance.
const AUTH_STARTS_PER_IP_PER_HOUR: u64 = 30;
/// Outbound mail abuse cap: register/start and magic/start share one
/// per-IP plus one global issuance budget in front of the per-email budget,
/// so one IP cannot email-bomb unlimited distinct victims (3 mails/15 min
/// per victim, unlimited victims). Any denial sends nothing and stores no
/// magic row. The global cap is 100 per 5 minutes.
const MAIL_PER_IP_PER_HOUR: u64 = 30;
const MAIL_GLOBAL_PER_WINDOW: u64 = 100;
const MAIL_GLOBAL_WINDOW_SECS: u64 = 300;
/// Unverified-signup retention: a user row that still holds no credential
/// and no other account activity (vault, session, unexpired magic link or
/// register challenge) after this is a never-completed signup;
/// `register_start` reaps such rows best-effort (their challenges expire
/// within 5 minutes and are pruned with them). Magic-only accounts (session
/// without a passkey yet) and vault owners are never reaped: their
/// vault/session/magic rows exempt them.
const UNVERIFIED_USER_TTL_SECS: u64 = 24 * 3600;
/// Fixed scope key for the fleet-wide auth-start and mail budgets.
const GLOBAL_SCOPE_KEY: &str = "global";

/// Canonical fleet-wide reap statement shape: the single source of truth
/// for `PgStore::reap_unverified_users` (called from `worker_entry`) and
/// the statement-shape test below. The purpose literal must stay
/// `PURPOSE_REGISTER` (`'register'`): reaping on `'login'` would delete
/// never-completed signups on the wrong ceremony. Exemptions (credentials,
/// unexpired register challenges, vaults, sessions, unexpired magic links)
/// must stay in sync with the `FakeStore` mirror.
pub fn reap_unverified_users_sql() -> String {
    format!(
        "DELETE FROM users WHERE created_at <= $1 \
         AND ($2::TEXT IS NULL OR users.id <> $2) \
         AND NOT EXISTS (SELECT 1 FROM webauthn_credentials \
                         WHERE webauthn_credentials.user_id = users.id) \
         AND NOT EXISTS (SELECT 1 FROM auth_challenges \
                         WHERE auth_challenges.user_id = users.id \
                           AND auth_challenges.purpose = '{PURPOSE_REGISTER}' \
                           AND auth_challenges.expires_at > $3) \
         AND NOT EXISTS (SELECT 1 FROM provider_vaults \
                         WHERE provider_vaults.user_id = users.id) \
         AND NOT EXISTS (SELECT 1 FROM sessions \
                         WHERE sessions.user_id = users.id) \
         AND NOT EXISTS (SELECT 1 FROM magic_link_tokens \
                         WHERE magic_link_tokens.user_id = users.id \
                           AND magic_link_tokens.expires_at > $3)"
    )
}

/// Canonical rate-limit exhausted fast-fail statement shape: the single
/// source of truth for the `PgStore::reserve_rate_limit` O(1) guard
/// (called from `worker_entry`) and the statement-shape test below. The
/// production guard must stay a single `SELECT COUNT(*)` over the live
/// window; without it every denied request pays one DELETE plus `limit`
/// conflicting upserts (~101 statements with 100 row locks for the
/// 100-slot finish-lookup budget). The `FakeStore` mirror exercises the
/// same exhausted-decision behavior and counts slot probes.
pub const RATE_LIMIT_EXHAUSTED_SQL: &str = "SELECT COUNT(*) FROM auth_rate_limit_slots \
     WHERE scope_key = $1 AND action = $2 AND expires_at > $3";

// --- Config ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    Invalid,
}

/// Worker configuration, validated once at startup. Production origins are
/// https-only; an exact loopback http origin is accepted only when the
/// operator explicitly opts in for local development.
pub struct Config {
    pub rp_id: String,
    pub origin: String,
    pub session_secret: Vec<u8>,
    pub kek: [u8; 32],
    pub kek_version: u32,
    pub session_ttl_secs: u64,
    pub invites_required: bool,
    /// Verify-only previous session HMAC secret: `Some` only inside a
    /// recorded rotation window, `None` in steady state. Never mints;
    /// length-checked like the primary (a short previous fails closed).
    pub session_previous_secret: Option<Vec<u8>>,
    /// Rate-limit tunables for the auth-start burst and vault
    /// test-connection budgets. `Config::new` uses the documented
    /// defaults; the Worker entry overlays [vars] via
    /// `RateLimitConfig::from_vars` + [`Config::with_rate_limits`].
    pub rate_limits: crate::rate_limit::RateLimitConfig,
}

impl Config {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rp_id: &str,
        origin: &str,
        session_secret: Vec<u8>,
        kek: [u8; 32],
        kek_version: u32,
        session_ttl_secs: u64,
        invites_required: bool,
        allow_loopback_dev: bool,
        session_previous_secret: Option<Vec<u8>>,
    ) -> Result<Self, ConfigError> {
        // Reuse the core RP/origin validator (canonical serialized origin,
        // RP == exact host), then tighten the scheme for production.
        auth::new_registration_challenge(rp_id, origin, 0).map_err(|_| ConfigError::Invalid)?;
        let loopback_http = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
            .iter()
            .any(|prefix| origin.starts_with(prefix));
        if !(origin.starts_with("https://") || (allow_loopback_dev && loopback_http)) {
            return Err(ConfigError::Invalid);
        }
        if session_secret.len() < 32 || session_ttl_secs == 0 {
            return Err(ConfigError::Invalid);
        }
        if let Some(previous) = &session_previous_secret {
            if previous.len() < 32 {
                return Err(ConfigError::Invalid);
            }
        }
        Ok(Self {
            rp_id: rp_id.to_string(),
            origin: origin.to_string(),
            session_secret,
            kek,
            kek_version,
            session_ttl_secs: session_ttl_secs.min(session::MAX_SESSION_SECS),
            invites_required,
            session_previous_secret,
            rate_limits: crate::rate_limit::RateLimitConfig::default(),
        })
    }

    /// Overlay operator-configured rate limits (from [vars]); the
    /// constructor keeps the documented defaults so existing callers
    /// and tests need no change.
    pub fn with_rate_limits(mut self, rate_limits: crate::rate_limit::RateLimitConfig) -> Self {
        self.rate_limits = rate_limits;
        self
    }

    /// Signing/verification keys for this worker: rotation (the primary
    /// mints, both verify) only when a previous secret is configured,
    /// otherwise single-secret steady state. See
    /// `docs/security/session-secret-rotation.md` §4.
    pub fn session_keys(&self) -> session::SessionKeys<'_> {
        match &self.session_previous_secret {
            Some(previous) => session::SessionKeys::rotation(&self.session_secret, previous),
            None => session::SessionKeys::single(&self.session_secret),
        }
    }

    fn expect<'a>(&'a self, challenge: &'a str) -> webauthn::Expect<'a> {
        webauthn::Expect {
            rp_id: &self.rp_id,
            origin: &self.origin,
            challenge,
        }
    }
}

// --- Store traits -----------------------------------------------------------------

/// Opaque storage failure: detail stays server-side.
#[derive(Debug, Clone, Copy)]
pub struct StoreError;

impl From<auth::AuthError> for StoreError {
    fn from(_: auth::AuthError) -> Self {
        StoreError
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub id: String,
    pub email: String,
    pub created_at: u64,
}

/// Challenge ceremony purposes. A challenge is redeemable only at the
/// finish route matching its purpose — a login challenge presented at
/// registration (which would enroll an attacker's key on the victim's
/// account) rejects before any verification or claim.
pub const PURPOSE_REGISTER: &str = "register";
pub const PURPOSE_LOGIN: &str = "login";
pub const PURPOSE_REAUTH: &str = "reauth";

/// Counter stored for a new credential, whatever the attestation said.
/// `none` attestation is unauthenticated, so its enrollment counter is
/// attacker-chosen: persisting `u32::MAX` would make every later
/// (strictly-increasing) assertion fail. The first assertion's signed
/// counter becomes the baseline; counterless authenticators stay at 0.
const ENROLLMENT_SIGN_COUNT: u32 = 0;

#[derive(Debug, Clone)]
pub struct ChallengeRow {
    pub challenge: PasskeyChallenge,
    pub user_id: Option<String>,
    /// Immutable ceremony purpose recorded at insert (migration 0004).
    pub purpose: String,
}

#[derive(Debug, Clone)]
pub struct VaultRow {
    pub blob: VaultBlob,
    pub indexer_configured: bool,
    pub updated_at: u64,
}

/// One durable rate-limit budget: `limit` slots for
/// `(scope_key, action)` per `window_secs`. See
/// [`Store::reserve_rate_limit_pair`].
#[derive(Debug, Clone, Copy)]
pub struct RateLimitBudget<'a> {
    pub scope_key: &'a str,
    pub action: &'a str,
    pub limit: u64,
    pub window_secs: u64,
}

/// Outcome of [`Store::reserve_rate_limit_pair`]. The denial variants
/// name WHICH budget refused so callers can report that budget's
/// window (a burst denial must not carry the hourly `Retry-After`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairAdmission {
    Allowed,
    DeniedFirst,
    DeniedSecond,
}

/// Persistence over migration-0002 tables. Implementations must use
/// parameterized statements only, and conditional single-row updates for
/// the rotation/sign-count advances. `Store` implementors also implement
/// [`SingleUseStore`] with the `auth::SingleUseClaim::statement` SQL.
#[allow(clippy::too_many_arguments)]
pub trait Store {
    fn user_by_email(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<UserRecord>, StoreError>>;
    fn insert_user(&self, user: &UserRecord) -> impl Future<Output = Result<(), StoreError>>;
    fn insert_challenge(
        &self,
        challenge: &PasskeyChallenge,
        user_id: Option<&str>,
        purpose: &str,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn challenge_by_id(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<ChallengeRow>, StoreError>>;
    fn insert_credential(
        &self,
        credential: &PasskeyCredential,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn credential_by_id(
        &self,
        cred_id_b64url: &str,
    ) -> impl Future<Output = Result<Option<PasskeyCredential>, StoreError>>;
    fn credentials_for_user(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Vec<PasskeyCredential>, StoreError>>;
    /// Conditional `UPDATE ... WHERE cred_id AND sign_count = expected`:
    /// true iff exactly one row advanced.
    fn advance_sign_count(
        &self,
        cred_id_b64url: &str,
        expected: u32,
        new: u32,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn insert_magic(
        &self,
        record: &MagicLinkRecord,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn magic_by_hash(
        &self,
        token_hash: &str,
    ) -> impl Future<Output = Result<Option<MagicLinkRecord>, StoreError>>;
    /// Atomically reserve one of `limit` per-key slots until `now + window_secs`.
    /// Expired slots may be reused; concurrent calls must never exceed the cap.
    fn reserve_rate_limit(
        &self,
        scope_key: &str,
        action: &str,
        limit: u64,
        now: u64,
        window_secs: u64,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    /// Reserve one slot from EACH of two budgets as a single admission
    /// decision: an allowed attempt consumes both, a denied attempt
    /// consumes NEITHER. The two budgets must be distinct
    /// `(scope_key, action)` pairs. Sequential single reserves would let an
    /// account that exhausted its own budget keep burning the shared
    /// per-IP budget (locking out other accounts behind the same
    /// address) on requests that never proceed. Production and the
    /// test fake override this with a truly all-or-none claim; the
    /// default keeps out-of-tree `Store` impls compiling with the
    /// weaker sequential semantics.
    fn reserve_rate_limit_pair(
        &self,
        first: RateLimitBudget<'_>,
        second: RateLimitBudget<'_>,
        now: u64,
    ) -> impl Future<Output = Result<PairAdmission, StoreError>> {
        async move {
            if !self
                .reserve_rate_limit(
                    first.scope_key,
                    first.action,
                    first.limit,
                    now,
                    first.window_secs,
                )
                .await?
            {
                return Ok(PairAdmission::DeniedFirst);
            }
            if self
                .reserve_rate_limit(
                    second.scope_key,
                    second.action,
                    second.limit,
                    now,
                    second.window_secs,
                )
                .await?
            {
                Ok(PairAdmission::Allowed)
            } else {
                Ok(PairAdmission::DeniedSecond)
            }
        }
    }
    /// Remove challenges that can no longer be redeemed.
    fn prune_expired_challenges(&self, now: u64) -> impl Future<Output = Result<(), StoreError>>;
    /// Reap abandoned signups while preserving the current/resumed user,
    /// credentialed accounts, unexpired registration challenges (including
    /// in-flight finishes), and any account activity (vault, session,
    /// unexpired magic link). Default no-op so out-of-tree `Store` impls
    /// keep compiling; production and the test fake override it.
    fn reap_unverified_users(
        &self,
        now: u64,
        ttl_secs: u64,
        protected_user_id: Option<&str>,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async move {
            let _ = (now, ttl_secs, protected_user_id);
            Ok(())
        }
    }
    /// Acquire one of the per-user test-connection leases until `expires_at`.
    fn acquire_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
        lease_secs: u64,
        max_concurrency: u32,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    /// Release only the lease owned by `token`; a stale completion cannot
    /// release a newer lease that reused the slot.
    fn release_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn invite_by_hash(
        &self,
        code_hash: &str,
    ) -> impl Future<Output = Result<Option<InviteRecord>, StoreError>>;
    fn insert_session(
        &self,
        record: &SessionRecord,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn session_by_id(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<SessionRecord>, StoreError>>;
    /// Conditional `UPDATE ... WHERE id AND rotation_counter = expected
    /// AND revoked = FALSE`: true iff exactly one row rotated.
    fn rotate_session_row(
        &self,
        id: &str,
        expected_counter: u32,
        new_counter: u32,
        new_expires_at: u64,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    /// Atomically claim the old session for reauthentication and insert its
    /// replacement. A false result means the conditional revoke lost a race;
    /// errors must leave both rows unchanged.
    fn replace_session_after_reauthentication(
        &self,
        _replacement: &session::ReauthenticatedSession,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async { Err(StoreError) }
    }
    fn revoke_session(&self, id: &str) -> impl Future<Output = Result<(), StoreError>>;
    fn revoke_all_sessions(&self, user_id: &str) -> impl Future<Output = Result<(), StoreError>>;
    fn vault_by_user(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<VaultRow>, StoreError>>;
    fn upsert_vault(
        &self,
        user_id: &str,
        row: &VaultRow,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn delete_vault(&self, user_id: &str) -> impl Future<Output = Result<(), StoreError>>;
    fn insert_audit(&self, event: &AuditEvent) -> impl Future<Output = Result<(), StoreError>>;
}

/// Magic-link delivery. The token is passed only to the mailer; it must
/// never be logged by implementations.
pub trait Mailer {
    fn send_magic_link(
        &self,
        email: &str,
        token: &str,
    ) -> impl Future<Output = Result<(), StoreError>>;
}

/// Bounded outbound probe for `test connection`. Implementations dial
/// ONLY addresses from `target` (never re-resolve `target.host()`),
/// keep the hostname for TLS SNI/certificate checks where the runtime
/// supports it, apply the per-address and total `vault` timeout constants,
/// attempt no more than `vault::MAX_DIAL_ADDRESSES`, follow no redirects, and return a
/// bare boolean — banners/errors never leave the dialer.
pub trait Dialer {
    fn probe(&self, target: &ValidatedTarget, port: u16, tls: bool) -> impl Future<Output = bool>;
}

/// Async DNS for handler-time resolution (the sync core guard consumes
/// the result via a static adapter, so exactly the resolved set is what
/// gets vetted and dialed).
pub trait AsyncDns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>>;
}

struct StaticDns {
    addresses: Vec<IpAddr>,
}

impl DnsResolver for StaticDns {
    fn resolve(&self, _host: &str) -> Vec<IpAddr> {
        self.addresses.clone()
    }
}

// --- HTTP shapes --------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Raw `Cookie` header value, if any.
    pub cookie: Option<String>,
    /// Raw `Origin` header value, if any.
    pub origin: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub set_cookie: Option<String>,
    /// `Retry-After` seconds for a 429; the entry renders the header.
    pub retry_after: Option<u64>,
}

fn json_response(status: u16, body: serde_json::Value) -> Response {
    Response {
        status,
        body: body.to_string(),
        set_cookie: None,
        retry_after: None,
    }
}

fn generic_auth() -> Response {
    json_response(
        401,
        serde_json::json!({"error": "check your email and try again"}),
    )
}

fn generic_vault() -> Response {
    json_response(400, serde_json::json!({"error": "vault unavailable"}))
}

fn internal() -> Response {
    json_response(500, serde_json::json!({"error": "internal error"}))
}

fn session_error_response(error: session::SessionError) -> Response {
    match error {
        session::SessionError::Invalid => generic_auth(),
        error => {
            let error = crate::api_error::ApiError::from(error);
            Response {
                status: error.status,
                body: error.body_json(),
                set_cookie: None,
                retry_after: None,
            }
        }
    }
}

pub(crate) fn invalid_request() -> Response {
    json_response(400, serde_json::json!({"error": "invalid request"}))
}

pub struct Ctx<'a, S, M, D, N>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    pub cfg: &'a Config,
    pub store: &'a S,
    pub mailer: &'a M,
    pub dialer: &'a D,
    pub dns: &'a N,
    pub now: u64,
    pub source_ip: String,
}

// --- Helpers ---------------------------------------------------------------------------

fn random_id(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("secure randomness unavailable");
    b64url_encode(&buf)
}

/// Deterministic stand-in ids for auth-start response shapes: stable
/// per email across calls (keyed by the session secret), so probing the
/// same address twice cannot distinguish stand-ins from real ids.
fn fake_id(cfg: &Config, label: &str, email: &str, index: u32, bytes: usize) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&cfg.session_secret);
    hasher.update(b"|");
    hasher.update(label.as_bytes());
    hasher.update(b"|");
    hasher.update(email.as_bytes());
    hasher.update(b"|");
    hasher.update(index.to_be_bytes());
    b64url_encode(&hasher.finalize()[..bytes])
}

/// Opaque deterministic email scope for rate-limit rows; the address itself
/// is not copied into the reservation table.
fn email_rate_limit_key(cfg: &Config, email: &str) -> String {
    fake_id(cfg, "email-rate-limit", email, 0, 32)
}

/// Stand-in user id for register/start taken-email shapes, shaped like
/// the real 16-byte random user ids and stable per email.
fn valid_challenge_id(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn fake_user_id(cfg: &Config, email: &str) -> String {
    fake_id(cfg, "fake-user", email, 0, 16)
}

/// Canonical per-IP rate-limit key: IPv6 clients sharing a /64 share one
/// budget, IPv4 keeps the full address. Audit logs keep the full `source_ip`;
/// only budget scopes use this key, so rotation inside one prefix cannot mint
/// fresh budgets.
fn rate_limit_ip_key(source_ip: &str) -> String {
    let trimmed = source_ip.trim();
    let Ok(addr) = trimmed.parse::<IpAddr>() else {
        return trimmed.to_string();
    };
    match addr {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.to_string();
            }
            let segments = v6.segments();
            std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            )
            .to_string()
        }
    }
}

/// Uniform per-IP throttle response: fixed 429 body independent of the
/// request email, because the budget is keyed only on the client prefix.
/// Callers must not return email-derived stand-ins on this path (that would
/// be an account-existence oracle across the denial boundary).
/// `retry_after_secs` is the budget window: slot expiries are staggered, so
/// the next slot frees no later than one full window out.
fn throttled(retry_after_secs: u64) -> Response {
    let mut resp = json_response(
        crate::rate_limit::RATE_LIMIT_STATUS,
        serde_json::json!({"error":"rate_limited"}),
    );
    resp.retry_after = Some(retry_after_secs);
    resp
}

/// Register/login challenge inserts allowed per account per hour.
/// Excess starts keep the usual response shape but store no challenge.
const AUTH_STARTS_PER_HOUR: u64 = 10;

fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    if email.len() < 3 || email.len() > 254 {
        return None;
    }
    if email.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') {
        return None;
    }
    Some(email)
}

fn parse_body<T: for<'de> Deserialize<'de>>(req: &Request) -> Option<T> {
    serde_json::from_slice(&req.body).ok()
}

fn session_cookie_value(req: &Request) -> Option<String> {
    let header = req.cookie.as_deref()?;
    for part in header.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if name.trim() == session::COOKIE_NAME {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

impl<'a, S, M, D, N> Ctx<'a, S, M, D, N>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    async fn audit(&self, user_id: &str, action: &str, ok: bool) {
        // Best-effort: an audit-store failure must not turn a completed
        // auth decision into a distinguishable response. Rate budgets and
        // connection leases use separate atomic store operations and fail
        // closed when those reservations fail.
        let _ = self
            .store
            .insert_audit(&vault::audit_event(
                user_id,
                action,
                ok,
                &self.source_ip,
                self.now,
            ))
            .await;
    }

    /// Issue + persist a new session row; the cookie is only returned
    /// after the row insert succeeds.
    async fn start_session(&self, user_id: &str) -> Result<Response, Response> {
        let token = session::issue_session(
            &self.cfg.session_secret,
            user_id,
            self.now,
            self.cfg.session_ttl_secs,
        )
        .map_err(|_| internal())?;
        let claims = session::verify_session(&self.cfg.session_secret, &token, self.now)
            .map_err(|_| internal())?;
        self.store
            .insert_session(&SessionRecord {
                id: claims.session_id.clone(),
                user_id: claims.user_id.clone(),
                created_at: claims.created_at,
                expires_at: claims.expires_at,
                rotation_counter: claims.rotation_counter,
                revoked: false,
            })
            .await
            .map_err(|_| internal())?;
        let mut resp = json_response(200, serde_json::json!({"ok": true}));
        resp.set_cookie = Some(session::set_cookie_header(
            &token,
            claims.expires_at.saturating_sub(self.now),
        ));
        Ok(resp)
    }

    /// Full request authentication: signed token + current server row via
    /// `verify_stored_session`, plus the CSRF origin pin for mutations.
    async fn authenticate(
        &self,
        req: &Request,
        mutation: bool,
    ) -> Result<(SessionClaims, String), Response> {
        if mutation {
            match req.origin.as_deref() {
                Some(origin) if origin == self.cfg.origin => {}
                _ => return Err(generic_auth()),
            }
        }
        let token = session_cookie_value(req).ok_or_else(generic_auth)?;
        // Dual path first: a pre-cutover token must locate its row. The row
        // check below still enforces revocation, counter and expiry, and
        // minting always uses the primary.
        let keys = self.cfg.session_keys();
        let claims = session::verify_session_dual(&keys, &token, self.now)
            .map_err(session_error_response)?;
        let record = self
            .store
            .session_by_id(&claims.session_id)
            .await
            .map_err(|_| internal())?
            .ok_or_else(generic_auth)?;
        let claims = session::verify_stored_session(&keys, &token, self.now, &record)
            .map_err(session_error_response)?;
        Ok((claims, token))
    }

    /// Authenticate a privilege-changing request and require a fresh session.
    /// Ordinary authentication remains valid for non-privileged operations.
    async fn authenticate_privileged(
        &self,
        req: &Request,
    ) -> Result<(SessionClaims, String), Response> {
        match req.origin.as_deref() {
            Some(origin) if origin == self.cfg.origin => {}
            _ => return Err(generic_auth()),
        }
        let token = session_cookie_value(req).ok_or_else(generic_auth)?;
        let keys = self.cfg.session_keys();
        let claims =
            session::verify_session_dual(&keys, &token, self.now).map_err(|_| generic_auth())?;
        let record = self
            .store
            .session_by_id(&claims.session_id)
            .await
            .map_err(|_| internal())?
            .ok_or_else(generic_auth)?;
        let claims = session::require_fresh_session(&keys, &token, self.now, &record)
            .map_err(session_error_response)?;
        Ok((claims, token))
    }

    /// Per-IP admission shared by register, login and magic start routes.
    /// The global budget is reserved only for outbound mail, so exhaustion
    /// cannot prevent users from obtaining an authentication challenge.
    /// The scope key is the client /64 (IPv6) or full address (IPv4), so
    /// rotation inside one prefix cannot mint fresh budgets; audit logs keep
    /// the full `source_ip`. Denial is `Err(retry_after_secs)` carrying
    /// the DENIED budget's window, so a burst denial back-off hint is
    /// the burst window, never the hourly one.
    async fn admit_auth_start(&self) -> Result<(), u64> {
        // Hourly budget AND the configured short-window burst budget
        // (`AUTH_RATE_LIMIT_*`, default 10/60s), claimed as one
        // all-or-none decision: a burst-denied start must not burn an
        // hourly slot, and vice versa.
        let ip_key = rate_limit_ip_key(&self.source_ip);
        match self
            .store
            .reserve_rate_limit_pair(
                RateLimitBudget {
                    scope_key: &ip_key,
                    action: "auth.start_ip",
                    limit: AUTH_STARTS_PER_IP_PER_HOUR,
                    window_secs: AUTH_START_WINDOW_SECS,
                },
                RateLimitBudget {
                    scope_key: &ip_key,
                    action: "auth.start_ip_burst",
                    limit: u64::from(self.cfg.rate_limits.auth_max_attempts),
                    window_secs: self.cfg.rate_limits.auth_window_secs,
                },
                self.now,
            )
            .await
        {
            Ok(PairAdmission::Allowed) => Ok(()),
            Ok(PairAdmission::DeniedFirst) => Err(AUTH_START_WINDOW_SECS),
            Ok(PairAdmission::DeniedSecond) => Err(self.cfg.rate_limits.auth_window_secs),
            // Store failure fails closed with the longer window: the
            // denying budget is unknown, and understating the wait
            // would invite a hot retry loop against a sick store.
            Err(_) => Err(AUTH_START_WINDOW_SECS),
        }
    }

    async fn send_magic_link_if_allowed(&self, user_id: &str, email: &str) {
        // Outbound mail abuse cap BEFORE the per-email budget: one IP must
        // not email-bomb unlimited distinct victims. Any denial sends
        // nothing and stores no magic row.
        let mail_ip_ok = self
            .store
            .reserve_rate_limit(
                &rate_limit_ip_key(&self.source_ip),
                "auth.mail_ip",
                MAIL_PER_IP_PER_HOUR,
                self.now,
                AUTH_START_WINDOW_SECS,
            )
            .await
            .unwrap_or(false);
        if !mail_ip_ok {
            return;
        }
        let mail_global_ok = self
            .store
            .reserve_rate_limit(
                GLOBAL_SCOPE_KEY,
                "auth.mail_global",
                MAIL_GLOBAL_PER_WINDOW,
                self.now,
                MAIL_GLOBAL_WINDOW_SECS,
            )
            .await
            .unwrap_or(false);
        if !mail_global_ok {
            // Alert without including request/user data; this signals fleet-wide
            // outbound-mail pressure while keeping auth starts available.
            #[cfg(target_arch = "wasm32")]
            worker::console_log!("api global mail budget exhausted");
            return;
        }
        let scope_key = email_rate_limit_key(self.cfg, email);
        let admitted = self
            .store
            .reserve_rate_limit(
                &scope_key,
                "auth.magic_link",
                MAGIC_LINKS_PER_WINDOW,
                self.now,
                auth::MAGIC_LINK_TTL_SECS,
            )
            .await
            .unwrap_or(false);
        if !admitted {
            return;
        }
        let (token, record) = auth::issue_magic_link(user_id, email, self.now);
        if self.store.insert_magic(&record).await.is_ok() {
            let _ = self.mailer.send_magic_link(email, &token).await;
        }
    }

    async fn resolve_all(&self, host: &str) -> StaticDns {
        StaticDns {
            addresses: self.dns.resolve(host).await,
        }
    }
}

// --- Request bodies ----------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailBody {
    email: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterFinishBody {
    challenge: String,
    credential_id: String,
    attestation_object: String,
    client_data_json: String,
    /// The magic-link token emailed at register/start: enrolling a
    /// passkey requires proof of inbox control for the account email,
    /// so an unregistered address cannot be claimed by a stranger.
    magic_token: String,
    invite_code: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginFinishBody {
    challenge: String,
    credential_id: String,
    authenticator_data: String,
    client_data_json: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MagicFinishBody {
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultPutBody {
    provider: ProviderConfig,
    indexer: Option<IndexerConfig>,
}

#[derive(serde::Serialize, Deserialize)]
struct VaultPayload {
    provider: ProviderConfig,
    indexer: Option<IndexerConfig>,
}

// --- Route handlers ------------------------------------------------------------------------

async fn register_start<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some(body) = parse_body::<EmailBody>(req) else {
        return generic_auth();
    };
    let Some(email) = normalize_email(&body.email) else {
        return generic_auth();
    };
    // Identical 200 shape for every admitted outcome below (new email,
    // abandoned signup, already-registered account): this route must not be
    // an account-existence oracle. A taken email gets an unbound challenge
    // whose finish fails generically, and no mail is sent for it. Per-IP
    // throttle denial is a fixed email-independent 429 (never an
    // email-derived stand-in): the budget is keyed only on the client
    // prefix, so the denied body is identical for taken and fresh emails.
    let challenge = match auth::new_registration_challenge(&ctx.cfg.rp_id, &ctx.cfg.origin, ctx.now)
    {
        Ok(challenge) => challenge,
        Err(_) => return internal(),
    };
    let shape = |user_id: String| {
        json_response(
            200,
            serde_json::json!({
                "challenge": challenge.id.clone(),
                "user_id": user_id,
                "rp_id": ctx.cfg.rp_id,
            }),
        )
    };
    // Per-IP admission before user lookup or maintenance. The global budget
    // applies to outbound mail only, never to auth challenge issuance.
    if let Err(retry_after) = ctx.admit_auth_start().await {
        return throttled(retry_after);
    }
    let existing = match ctx.store.user_by_email(&email).await {
        Ok(user) => user,
        Err(_) => return internal(),
    };
    // Preserve a resumed signup through this cleanup pass. Other active
    // registration attempts are protected by their unexpired challenges.
    // Same-request only: a concurrent `register_start` for another email
    // can reap this resuming user between our lookup above and our
    // challenge insert below (it holds no unexpired register challenge
    // precisely because it is resuming). The post-insert revalidation
    // below heals that race by recreating the user under the same id, so
    // a resumed signup survives concurrent cleanup (client retries also
    // recreate it; see migration 0006 FK note).
    if ctx
        .store
        .reap_unverified_users(
            ctx.now,
            UNVERIFIED_USER_TTL_SECS,
            existing.as_ref().map(|user| user.id.as_str()),
        )
        .await
        .is_err()
    {
        return internal();
    }
    if ctx.store.prune_expired_challenges(ctx.now).await.is_err() {
        return internal();
    }
    let user = match existing {
        Some(user) => {
            // Resume an abandoned signup only when no credential exists yet.
            match ctx.store.credentials_for_user(&user.id).await {
                Ok(creds) if creds.is_empty() => user,
                Ok(_) => {
                    // Unstored challenge + deterministic stand-in user id:
                    // same shape on every call, no row growth, finish
                    // fails generically at lookup.
                    return shape(fake_user_id(ctx.cfg, &email));
                }
                Err(_) => return internal(),
            }
        }
        None => {
            let user = UserRecord {
                id: random_id(16),
                email: email.clone(),
                created_at: ctx.now,
            };
            match ctx.store.insert_user(&user).await {
                Ok(()) => user,
                // Unique-email race: another request just claimed the row,
                // indistinguishable from a taken account by design.
                Err(_) => return shape(fake_user_id(ctx.cfg, &email)),
            }
        }
    };
    // Reserve atomically before storing the challenge. The response shape
    // stays identical when storage fails or the per-account cap is reached.
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &user.id,
            "auth.register_start",
            AUTH_STARTS_PER_HOUR,
            ctx.now,
            AUTH_START_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    ctx.audit(&user.id, "auth.register_start", admitted).await;
    if !admitted {
        return shape(user.id.clone());
    }
    // Challenge insert plus concurrent-reap healing. With the migration
    // 0006 FK (`auth_challenges.user_id -> users(id)`) a concurrent reap
    // of our user fails here; without it, an insert can succeed just before
    // the reap and leave an orphaned challenge. Revalidation below heals the
    // user and preserves that challenge, so either ordering can recover.
    if ctx
        .store
        .insert_challenge(&challenge, Some(&user.id), PURPOSE_REGISTER)
        .await
        .is_err()
    {
        match ctx.store.user_by_email(&email).await {
            Ok(None) => {
                let healed = UserRecord {
                    id: user.id.clone(),
                    email: email.clone(),
                    created_at: ctx.now,
                };
                if ctx.store.insert_user(&healed).await.is_err() {
                    return shape(fake_user_id(ctx.cfg, &email));
                }
                if ctx
                    .store
                    .insert_challenge(&challenge, Some(&user.id), PURPOSE_REGISTER)
                    .await
                    .is_err()
                {
                    return internal();
                }
            }
            _ => return internal(),
        }
    } else {
        match ctx.store.user_by_email(&email).await {
            Ok(Some(current)) if current.id == user.id => {}
            Ok(None) => {
                let healed = UserRecord {
                    id: user.id.clone(),
                    email: email.clone(),
                    created_at: ctx.now,
                };
                if ctx.store.insert_user(&healed).await.is_err() {
                    return shape(fake_user_id(ctx.cfg, &email));
                }
                match ctx.store.challenge_by_id(&challenge.id).await {
                    Ok(Some(row))
                        if row.challenge.id == challenge.id
                            && row.challenge.rp_id == challenge.rp_id
                            && row.challenge.origin == challenge.origin
                            && row.challenge.expires_at == challenge.expires_at
                            && !row.challenge.consumed
                            && row.user_id.as_deref() == Some(user.id.as_str())
                            && row.purpose == PURPOSE_REGISTER => {}
                    Ok(None) => {
                        if ctx
                            .store
                            .insert_challenge(&challenge, Some(&user.id), PURPOSE_REGISTER)
                            .await
                            .is_err()
                        {
                            return internal();
                        }
                    }
                    _ => return internal(),
                }
            }
            Ok(Some(_)) => {
                return shape(fake_user_id(ctx.cfg, &email));
            }
            Err(_) => return internal(),
        }
    }
    // Email-ownership proof and magic login share one atomic per-email
    // issuance budget, so parallel requests cannot oversend.
    ctx.send_magic_link_if_allowed(&user.id, &email).await;
    shape(user.id.clone())
}

async fn register_finish<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some(body) = parse_body::<RegisterFinishBody>(req) else {
        return generic_auth();
    };
    if !valid_challenge_id(&body.challenge) {
        return generic_auth();
    }
    // Per-IP lookup budget BEFORE any DB read: rotating well-formed unknown
    // challenge ids costs one Pg connect+SELECT each, so they must share one
    // per-client pool (keyed on the client /64, not the raw address).
    // Denial is the generic error (no throttle oracle).
    let lookup_admitted = ctx
        .store
        .reserve_rate_limit(
            &rate_limit_ip_key(&ctx.source_ip),
            "auth.register_finish_ip",
            FINISH_LOOKUP_ATTEMPTS_PER_IP,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !lookup_admitted {
        return generic_auth();
    }
    let row = match ctx.store.challenge_by_id(&body.challenge).await {
        Ok(Some(row)) if row.purpose == PURPOSE_REGISTER => row,
        Ok(_) => return generic_auth(),
        Err(_) => return internal(),
    };
    let (Ok(attestation_object), Ok(client_data_json)) = (
        b64url_decode(&body.attestation_object),
        b64url_decode(&body.client_data_json),
    ) else {
        return generic_auth();
    };
    // Per-challenge attempt budget BEFORE CPU-heavy verification: a failed
    // attestation verify must not grant unlimited retries on one challenge.
    // Same atomic slot pattern as register/start; denial is the generic
    // error so throttled and invalid attempts are indistinguishable.
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &body.challenge,
            "auth.register_finish",
            REGISTER_FINISH_ATTEMPTS_PER_CHALLENGE,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !admitted {
        return generic_auth();
    }
    let Some(user_id) = row.user_id.clone() else {
        return generic_auth();
    };
    // Defense in depth beside the purpose binding: registration is only
    // for accounts with no credential yet.
    match ctx.store.credentials_for_user(&user_id).await {
        Ok(creds) if creds.is_empty() => {}
        Ok(_) => return generic_auth(),
        Err(_) => return internal(),
    }
    let credential = match webauthn::verify_attestation(
        &attestation_object,
        &client_data_json,
        &ctx.cfg.expect(&row.challenge.id),
    ) {
        Ok(credential) => credential,
        Err(_) => return generic_auth(),
    };
    if b64url_encode(&credential.cred_id) != body.credential_id {
        return generic_auth();
    }
    // Email-ownership proof, pre-validated BEFORE either single-use claim:
    // a mistyped emailed code must not burn the ceremony challenge, so the
    // user can simply re-enter the code (the pure snapshot check grants
    // nothing — both atomic claims below still gate enrollment).
    if body.magic_token.is_empty() || body.magic_token.len() > 256 {
        return generic_auth();
    }
    let magic = match ctx
        .store
        .magic_by_hash(&auth::hash_magic_token(&body.magic_token))
        .await
    {
        Ok(Some(record)) if record.user_id == user_id => record,
        Ok(_) => return generic_auth(),
        Err(_) => return internal(),
    };
    if auth::consume_magic_link(magic.clone(), &body.magic_token, ctx.now).is_err() {
        return generic_auth();
    }
    // Invite typo-class failures are likewise rejected BEFORE either
    // claim: the throttle reservation, lookup and pure snapshot check run
    // here, so a mistyped invite leaves the challenge and the emailed
    // token intact for a corrected retry (the signup page stays on the
    // code step). The atomic invite consume still runs after the claims
    // below — single-use is enforced by that store claim, not this
    // snapshot.
    let invite_to_consume = if ctx.cfg.invites_required || body.invite_code.is_some() {
        // Atomic per-client redemption budget BEFORE the lookup: parallel
        // guesses share one reservation (no check-then-act race), and the
        // denial is the generic error (no throttle oracle). Keyed by
        // client prefix and account, per `auth::invite_attempt_allowed`.
        let attempt_key = format!("invite:{}:{}", rate_limit_ip_key(&ctx.source_ip), user_id);
        let allowed = ctx
            .store
            .reserve_rate_limit(
                &attempt_key,
                "auth.invite_redeem",
                u64::from(auth::INVITE_REDEMPTION_MAX_ATTEMPTS),
                ctx.now,
                auth::INVITE_REDEMPTION_WINDOW_SECS,
            )
            .await
            .unwrap_or(false);
        if !allowed {
            return generic_auth();
        }
        let Some(code) = body.invite_code.clone() else {
            return generic_auth();
        };
        let invite = match ctx
            .store
            .invite_by_hash(&auth::hash_invite_code(&code))
            .await
        {
            Ok(Some(invite)) => invite,
            Ok(None) => return generic_auth(),
            Err(_) => return internal(),
        };
        if auth::consume_invite(invite.clone(), &code).is_err() {
            return generic_auth();
        }
        Some((invite, code))
    } else {
        None
    };
    // Single-use claim (conditional UPDATE, exactly one row) before any
    // signup side effect is allowed to stand.
    if auth::consume_challenge_atomically(
        row.challenge,
        &ctx.cfg.rp_id,
        &ctx.cfg.origin,
        ctx.now,
        ctx.store,
    )
    .await
    .is_err()
    {
        return generic_auth();
    }
    // The same proof, now consumed atomically before enrollment.
    if auth::consume_magic_link_atomically(magic, &body.magic_token, ctx.now, ctx.store)
        .await
        .is_err()
    {
        return generic_auth();
    }
    if let Some((invite, code)) = invite_to_consume {
        if auth::consume_invite_atomically(invite, &code, ctx.store)
            .await
            .is_err()
        {
            return generic_auth();
        }
    }
    let stored = PasskeyCredential {
        cred_id_b64url: body.credential_id.clone(),
        user_id: user_id.clone(),
        public_key: credential.public_key_cose.clone(),
        sign_count: ENROLLMENT_SIGN_COUNT,
        registered_at: ctx.now,
    };
    match ctx.store.insert_credential(&stored).await {
        Ok(()) => {}
        // Duplicate credential id (replay under a fresh challenge) stays generic.
        Err(_) => return generic_auth(),
    }
    ctx.audit(&user_id, "auth.register", true).await;
    match ctx.start_session(&user_id).await {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

async fn login_start<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some(body) = parse_body::<EmailBody>(req) else {
        return generic_auth();
    };
    let Some(email) = normalize_email(&body.email) else {
        return generic_auth();
    };
    let challenge = match auth::new_registration_challenge(&ctx.cfg.rp_id, &ctx.cfg.origin, ctx.now)
    {
        Ok(challenge) => challenge,
        Err(_) => return internal(),
    };
    // Per-IP admission before maintenance or lookup: unknown emails would
    // otherwise mint unbounded unbound challenge rows. Denial returns the
    // same fixed-shape 200 with an unstored challenge.
    if ctx.admit_auth_start().await.is_err() {
        return json_response(
            200,
            serde_json::json!({
                "challenge": challenge.id,
                "allow_credentials": [],
                "rp_id": ctx.cfg.rp_id,
            }),
        );
    }
    if ctx.store.prune_expired_challenges(ctx.now).await.is_err() {
        return internal();
    }
    let user = match ctx.store.user_by_email(&email).await {
        Ok(user) => user,
        Err(_) => return internal(),
    };
    let scope_key = user
        .as_ref()
        .map(|user| user.id.clone())
        .unwrap_or_else(|| email_rate_limit_key(ctx.cfg, &email));
    // Use a discoverable/usernameless passkey flow: returning credential ids
    // here leaks their variable lengths even when the list is padded.
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &scope_key,
            "auth.login_start",
            AUTH_STARTS_PER_HOUR,
            ctx.now,
            AUTH_START_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if admitted
        && ctx
            .store
            .insert_challenge(
                &challenge,
                user.as_ref().map(|user| user.id.as_str()),
                PURPOSE_LOGIN,
            )
            .await
            .is_err()
    {
        return internal();
    }
    json_response(
        200,
        serde_json::json!({
            "challenge": challenge.id,
            "allow_credentials": [],
            "rp_id": ctx.cfg.rp_id,
        }),
    )
}

async fn session_reauth_start<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let challenge = match auth::new_registration_challenge(&ctx.cfg.rp_id, &ctx.cfg.origin, ctx.now)
    {
        Ok(challenge) => challenge,
        Err(_) => return internal(),
    };
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &claims.user_id,
            "auth.reauth_start",
            AUTH_STARTS_PER_HOUR,
            ctx.now,
            AUTH_START_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !admitted {
        return generic_auth();
    }
    if ctx.store.prune_expired_challenges(ctx.now).await.is_err()
        || ctx
            .store
            .insert_challenge(&challenge, Some(&claims.user_id), PURPOSE_REAUTH)
            .await
            .is_err()
    {
        return internal();
    }
    json_response(
        200,
        serde_json::json!({
            "challenge": challenge.id,
            "allow_credentials": [],
            "rp_id": ctx.cfg.rp_id,
        }),
    )
}

async fn login_finish<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some(body) = parse_body::<LoginFinishBody>(req) else {
        return generic_auth();
    };
    if !valid_challenge_id(&body.challenge) {
        return generic_auth();
    }
    // Per-IP lookup budget BEFORE any DB read, same as register/finish:
    // rotating well-formed unknown challenge ids must share one pool
    // (keyed on the client /64, not the raw address).
    let lookup_admitted = ctx
        .store
        .reserve_rate_limit(
            &rate_limit_ip_key(&ctx.source_ip),
            "auth.login_finish_ip",
            FINISH_LOOKUP_ATTEMPTS_PER_IP,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !lookup_admitted {
        return generic_auth();
    }
    let row = match ctx.store.challenge_by_id(&body.challenge).await {
        Ok(Some(row)) if row.purpose == PURPOSE_LOGIN => row,
        Ok(_) => return generic_auth(),
        Err(_) => return internal(),
    };
    let (Ok(authenticator_data), Ok(client_data_json), Ok(signature)) = (
        b64url_decode(&body.authenticator_data),
        b64url_decode(&body.client_data_json),
        b64url_decode(&body.signature),
    ) else {
        return generic_auth();
    };
    // Per-challenge attempt budget BEFORE CPU-heavy verification, same as
    // register/finish: one challenge id cannot fund unlimited assertions.
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &body.challenge,
            "auth.login_finish",
            LOGIN_FINISH_ATTEMPTS_PER_CHALLENGE,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !admitted {
        return generic_auth();
    }
    let Some(user_id) = row.user_id.clone() else {
        return generic_auth();
    };
    let credential = match ctx.store.credential_by_id(&body.credential_id).await {
        Ok(Some(credential)) => credential,
        Ok(None) => return generic_auth(),
        Err(_) => return internal(),
    };
    if credential.user_id != user_id {
        return generic_auth();
    }
    let new_sign_count = match webauthn::verify_assertion(
        &credential.public_key,
        credential.sign_count,
        &authenticator_data,
        &client_data_json,
        &signature,
        &ctx.cfg.expect(&row.challenge.id),
    ) {
        Ok(count) => count,
        Err(_) => return generic_auth(),
    };
    if auth::consume_challenge_atomically(
        row.challenge,
        &ctx.cfg.rp_id,
        &ctx.cfg.origin,
        ctx.now,
        ctx.store,
    )
    .await
    .is_err()
    {
        return generic_auth();
    }
    // Conditional counter advance: a concurrent assertion replay that
    // already moved the counter loses here even if both snapshots passed.
    match ctx
        .store
        .advance_sign_count(
            &credential.cred_id_b64url,
            credential.sign_count,
            new_sign_count,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return generic_auth(),
        Err(_) => return internal(),
    }
    ctx.audit(&user_id, "auth.login", true).await;
    match ctx.start_session(&user_id).await {
        Ok(resp) => resp,
        Err(resp) => resp,
    }
}

async fn session_reauth_finish<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, old_token) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let Some(body) = parse_body::<LoginFinishBody>(req) else {
        return generic_auth();
    };
    if !valid_challenge_id(&body.challenge) {
        return generic_auth();
    }
    let lookup_admitted = ctx
        .store
        .reserve_rate_limit(
            &rate_limit_ip_key(&ctx.source_ip),
            "auth.reauth_finish_ip",
            FINISH_LOOKUP_ATTEMPTS_PER_IP,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !lookup_admitted {
        return generic_auth();
    }
    let row = match ctx.store.challenge_by_id(&body.challenge).await {
        Ok(Some(row))
            if row.purpose == PURPOSE_REAUTH
                && row.user_id.as_deref() == Some(claims.user_id.as_str()) =>
        {
            row
        }
        Ok(_) => return generic_auth(),
        Err(_) => return internal(),
    };
    let (Ok(authenticator_data), Ok(client_data_json), Ok(signature)) = (
        b64url_decode(&body.authenticator_data),
        b64url_decode(&body.client_data_json),
        b64url_decode(&body.signature),
    ) else {
        return generic_auth();
    };
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &body.challenge,
            "auth.reauth_finish",
            LOGIN_FINISH_ATTEMPTS_PER_CHALLENGE,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !admitted {
        return generic_auth();
    }
    let credential = match ctx.store.credential_by_id(&body.credential_id).await {
        Ok(Some(credential)) if credential.user_id == claims.user_id => credential,
        Ok(_) => return generic_auth(),
        Err(_) => return internal(),
    };
    let new_sign_count = match webauthn::verify_assertion(
        &credential.public_key,
        credential.sign_count,
        &authenticator_data,
        &client_data_json,
        &signature,
        &ctx.cfg.expect(&row.challenge.id),
    ) {
        Ok(count) => count,
        Err(_) => return generic_auth(),
    };
    if auth::consume_challenge_atomically(
        row.challenge,
        &ctx.cfg.rp_id,
        &ctx.cfg.origin,
        ctx.now,
        ctx.store,
    )
    .await
    .is_err()
    {
        return generic_auth();
    }
    match ctx
        .store
        .advance_sign_count(
            &credential.cred_id_b64url,
            credential.sign_count,
            new_sign_count,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return generic_auth(),
        Err(_) => return internal(),
    }
    let keys = ctx.cfg.session_keys();
    let replacement = match session::reauthenticate_session(
        &keys,
        &old_token,
        ctx.now,
        &SessionRecord {
            id: claims.session_id.clone(),
            user_id: claims.user_id.clone(),
            created_at: claims.created_at,
            expires_at: claims.expires_at,
            rotation_counter: claims.rotation_counter,
            revoked: false,
        },
        &credential.user_id,
        ctx.cfg.session_ttl_secs,
    ) {
        Ok(replacement) => replacement,
        Err(error) => return session_error_response(error),
    };
    match ctx
        .store
        .replace_session_after_reauthentication(&replacement)
        .await
    {
        Ok(true) => {}
        Ok(false) => return generic_auth(),
        Err(_) => return internal(),
    }
    ctx.audit(&claims.user_id, "auth.reauthenticate", true)
        .await;
    let mut response = json_response(200, serde_json::json!({"ok": true}));
    response.set_cookie = Some(session::set_cookie_header(
        &replacement.token,
        replacement.new_record.expires_at.saturating_sub(ctx.now),
    ));
    response
}

async fn magic_start<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some(body) = parse_body::<EmailBody>(req) else {
        return generic_auth();
    };
    let Some(email) = normalize_email(&body.email) else {
        return generic_auth();
    };
    // Per-IP/global budget BEFORE the lookup: rotating addresses must share
    // one pool, or magic starts fund unbounded reads. Denial keeps the
    // identical response (the mail-budget denial inside
    // `send_magic_link_if_allowed` likewise sends nothing and stores no
    // magic row on the same shape).
    if ctx.admit_auth_start().await.is_err() {
        return json_response(200, serde_json::json!({"ok": true}));
    }
    // Identical response whether or not the account exists or the atomic
    // per-email issuance budget has been exhausted.
    if let Ok(Some(user)) = ctx.store.user_by_email(&email).await {
        ctx.send_magic_link_if_allowed(&user.id, &email).await;
    }
    json_response(200, serde_json::json!({"ok": true}))
}

async fn magic_finish<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Login-CSRF session-planting pin: this route mints a cookie from a pure
    // bearer token, so a cross-site POST would plant the attacker's session
    // in the victim's browser — response `Set-Cookie` is not stopped by
    // `SameSite=Lax`, and CORS alone does not stop form/nav POSTs. Require
    // the same exact-`Origin` match as cookie-authenticated mutations,
    // BEFORE any token lookup or consume (so a rejected cross-origin
    // attempt leaves the token unspent). Browsers always send `Origin` on
    // this fetch POST; API-only clients must send `Origin: <configured origin>`.
    if req.origin.as_deref() != Some(ctx.cfg.origin.as_str()) {
        return generic_auth();
    }
    let Some(body) = parse_body::<MagicFinishBody>(req) else {
        return generic_auth();
    };
    if body.token.is_empty() || body.token.len() > 256 {
        return generic_auth();
    }
    // Attempt budget AFTER the exact-Origin pin and BEFORE the hash lookup:
    // rotating tokens must share one per-client budget (keyed on the client
    // /64, not the raw address), or bogus finishes are an unbounded DB read.
    // Cross-origin rejects above never reach this reservation, so an attacker
    // cannot burn a victim IP's budget.
    let admitted = ctx
        .store
        .reserve_rate_limit(
            &rate_limit_ip_key(&ctx.source_ip),
            "auth.magic_finish",
            MAGIC_FINISH_ATTEMPTS_PER_IP,
            ctx.now,
            FINISH_ATTEMPT_WINDOW_SECS,
        )
        .await
        .unwrap_or(false);
    if !admitted {
        return generic_auth();
    }
    let record = match ctx
        .store
        .magic_by_hash(&auth::hash_magic_token(&body.token))
        .await
    {
        Ok(Some(record)) => record,
        Ok(None) => return generic_auth(),
        Err(_) => return internal(),
    };
    // Login-CSRF guard (second layer after the exact-`Origin` pin above):
    // an emailed link must never silently REPLACE an existing session for
    // a different account. If the request already authenticates as someone
    // else, fail generically WITHOUT consuming the token (its owner can
    // still redeem it after signing out). An absent or invalid session is
    // the ordinary sign-in path. The web landing page adds the third layer:
    // no auto-redeem, an explicit confirm, and the signed-in identity shown
    // before any credential is typed.
    if session_cookie_value(req).is_some() {
        if let Ok((claims, _)) = ctx.authenticate(req, false).await {
            if claims.user_id != record.user_id {
                return generic_auth();
            }
        }
    }
    let email = record.email.clone();
    let consumed =
        match auth::consume_magic_link_atomically(record, &body.token, ctx.now, ctx.store).await {
            Ok(consumed) => consumed,
            Err(_) => return generic_auth(),
        };
    ctx.audit(&consumed.user_id, "auth.magic", true).await;
    match ctx.start_session(&consumed.user_id).await {
        Ok(mut resp) => {
            // The token holder is now signed in as this account; echoing
            // its own email back lets the web app show "Signed in as …"
            // before any provider credential is entered.
            resp.body = serde_json::json!({"ok": true, "email": email}).to_string();
            resp
        }
        Err(resp) => resp,
    }
}

async fn session_refresh<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, token) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    // Same keys as authentication: a pre-cutover token verifies through
    // the dual path and is re-signed with the primary, migrating the
    // session onto the new secret.
    let keys = ctx.cfg.session_keys();
    let new_token = match session::rotate_session(&keys, &token, ctx.now, ctx.cfg.session_ttl_secs)
    {
        Ok(new_token) => new_token,
        Err(error) => return session_error_response(error),
    };
    let new_claims = match session::verify_session(&ctx.cfg.session_secret, &new_token, ctx.now) {
        Ok(new_claims) => new_claims,
        Err(_) => return internal(),
    };
    // Row first (conditional counter update), cookie only after commit:
    // the previous token stops verifying against the advanced row.
    match ctx
        .store
        .rotate_session_row(
            &claims.session_id,
            claims.rotation_counter,
            new_claims.rotation_counter,
            new_claims.expires_at,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return generic_auth(),
        Err(_) => return internal(),
    }
    let mut resp = json_response(200, serde_json::json!({"ok": true}));
    resp.set_cookie = Some(session::set_cookie_header(
        &new_token,
        new_claims.expires_at.saturating_sub(ctx.now),
    ));
    resp
}

async fn session_logout<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    if ctx.store.revoke_session(&claims.session_id).await.is_err() {
        return internal();
    }
    let mut resp = json_response(200, serde_json::json!({"ok": true}));
    resp.set_cookie = Some(session::clear_cookie_header());
    resp
}

async fn session_revoke_all<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    if ctx
        .store
        .revoke_all_sessions(&claims.user_id)
        .await
        .is_err()
    {
        return internal();
    }
    ctx.audit(&claims.user_id, "session.revoke_all", true).await;
    let mut resp = json_response(200, serde_json::json!({"ok": true}));
    resp.set_cookie = Some(session::clear_cookie_header());
    resp
}

fn summary_response(row: Option<&VaultRow>) -> Response {
    let (provider_configured, indexer_configured, updated_at) = match row {
        Some(row) => (true, row.indexer_configured, row.updated_at),
        None => (false, false, 0),
    };
    let summary = vault::redacted_summary(provider_configured, indexer_configured, updated_at);
    json_response(
        200,
        serde_json::json!({
            "provider_configured": summary.provider_configured,
            "indexer_configured": summary.indexer_configured,
            "updated_at": summary.updated_at,
        }),
    )
}

async fn vault_get<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate(req, false).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match ctx.store.vault_by_user(&claims.user_id).await {
        Ok(row) => summary_response(row.as_ref()),
        Err(_) => generic_vault(),
    }
}

async fn vault_put<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let Some(body) = parse_body::<VaultPutBody>(req) else {
        return generic_vault();
    };
    // Save-time SSRF validation over freshly resolved records (this does
    // not authorize any later fetch; dial time revalidates).
    let provider_dns = ctx.resolve_all(&body.provider.host).await;
    if body.provider.validate(&provider_dns).is_err() {
        return generic_vault();
    }
    if let Some(indexer) = &body.indexer {
        let host = match url::Url::parse(&indexer.url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()))
        {
            Some(host) => host,
            None => return generic_vault(),
        };
        let indexer_dns = ctx.resolve_all(&host).await;
        if indexer.validate(&indexer_dns).is_err() {
            return generic_vault();
        }
    }
    // Repudiation-safe audit action from the core contract
    // (`vault::ProviderConfig::audit_action`), taken BEFORE the move
    // below: a save carrying the explicit cleartext opt-in must not look
    // like a normal save in the audit log.
    let audit_action = body.provider.audit_action();
    let indexer_configured = body.indexer.is_some();
    let payload = VaultPayload {
        provider: body.provider,
        indexer: body.indexer,
    };
    let plaintext = match serde_json::to_vec(&payload) {
        Ok(bytes) => bytes,
        Err(_) => return generic_vault(),
    };
    let blob = match vault::encrypt_vault(
        &ctx.cfg.kek,
        ctx.cfg.kek_version,
        &claims.user_id,
        &plaintext,
    ) {
        Ok(blob) => blob,
        Err(_) => return generic_vault(),
    };
    let row = VaultRow {
        blob,
        indexer_configured,
        updated_at: ctx.now,
    };
    if ctx.store.upsert_vault(&claims.user_id, &row).await.is_err() {
        return generic_vault();
    }
    ctx.audit(&claims.user_id, audit_action, true).await;
    summary_response(Some(&row))
}

async fn vault_delete<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    if ctx.store.delete_vault(&claims.user_id).await.is_err() {
        return generic_vault();
    }
    ctx.audit(&claims.user_id, "vault.delete", true).await;
    summary_response(None)
}

async fn vault_test_connection<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    // Every failure below collapses to the same boolean (never a banner,
    // host verdict or vault-existence signal).
    let fail = || json_response(200, serde_json::json!({"ok": false}));
    // Dual budget (per-IP AND per-account; roadmap R6): the limits come
    // from `ctx.cfg.rate_limits` (`VAULT_TEST_*_RATE_LIMIT_*` [vars]
    // overlaid by the Worker entry), whose per-account default IS
    // `vault::TEST_CONNECTION_MAX_ATTEMPTS` per
    // `vault::TEST_CONNECTION_WINDOW_SECS` — one source, see
    // rate_limit.rs. The per-IP budget has its own larger default
    // (5x the account cap): NAT-shared addresses serve many accounts,
    // so one account's allowed probes must not consume the whole IP
    // budget and lock out its neighbours. Both slots are claimed as
    // one all-or-none pair before any lookup or dial, so one host
    // cannot burn probes across accounts, one account cannot burn
    // them across hosts, and a denied attempt consumes neither
    // budget. Denial keeps the same boolean collapse as every other
    // failure on this route (never a 429 oracle here).
    let limits = ctx.cfg.rate_limits;
    let ip_key = rate_limit_ip_key(&ctx.source_ip);
    let admitted = matches!(
        ctx.store
            .reserve_rate_limit_pair(
                RateLimitBudget {
                    scope_key: &ip_key,
                    action: "vault.test_ip",
                    limit: u64::from(limits.vault_test_ip_max_attempts),
                    window_secs: limits.vault_test_window_secs,
                },
                RateLimitBudget {
                    scope_key: &claims.user_id,
                    action: "vault.test",
                    limit: u64::from(limits.vault_test_max_attempts),
                    window_secs: limits.vault_test_window_secs,
                },
                ctx.now,
            )
            .await,
        Ok(PairAdmission::Allowed)
    );
    if !admitted {
        return fail();
    }
    // What to dial: the stored vault by default (empty body), or a
    // candidate envelope for test-before-save. A candidate runs the exact
    // save-time guards (`validate` over freshly resolved records) without
    // persisting anything, so the wizard can test a config the server has
    // never stored. This widens no SSRF boundary: saving the same
    // candidate then testing the stored row reaches the identical dial,
    // and every failure below still collapses to the same boolean. A
    // non-empty body that is not the save envelope fails closed instead
    // of silently testing something else.
    let (provider, indexer): (ProviderConfig, Option<IndexerConfig>) = if req.body.is_empty() {
        let row = match ctx.store.vault_by_user(&claims.user_id).await {
            Ok(Some(row)) => row,
            _ => return fail(),
        };
        match vault::decrypt_vault(&ctx.cfg.kek, &claims.user_id, &row.blob)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<VaultPayload>(&bytes).ok())
        {
            Some(payload) => (payload.provider, payload.indexer),
            None => return fail(),
        }
    } else {
        match serde_json::from_slice::<VaultPutBody>(&req.body).ok() {
            Some(envelope) => (envelope.provider, envelope.indexer),
            None => return generic_vault(),
        }
    };
    // The attempt slot is reserved before any potentially slow lookup or
    // dial, so concurrent requests cannot pass a count-then-act check.
    let result = async {
        // Resolve all records immediately before the dial and hand the
        // dialer only the vetted, bounded address set.
        let dns = ctx.resolve_all(&provider.host).await;
        let target: ValidatedTarget = match provider.validate(&dns) {
            Ok(target) => target,
            Err(_) => return false,
        };
        // A candidate that would not save fails the test instead of
        // testing half a config (the stored path was validated at save).
        if let Some(indexer) = &indexer {
            let host = match url::Url::parse(&indexer.url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()))
            {
                Some(host) => host,
                None => return false,
            };
            let indexer_dns = ctx.resolve_all(&host).await;
            if indexer.validate(&indexer_dns).is_err() {
                return false;
            }
        }
        let lease_token = random_id(16);
        let lease_acquired = ctx
            .store
            .acquire_test_connection_lease(
                &claims.user_id,
                &lease_token,
                TEST_CONNECTION_LEASE_SECS,
                vault::MAX_TEST_CONCURRENCY,
            )
            .await
            .unwrap_or(false);
        if !lease_acquired {
            return false;
        }
        let ok = ctx.dialer.probe(&target, provider.port, provider.tls).await;
        let _ = ctx
            .store
            .release_test_connection_lease(&claims.user_id, &lease_token)
            .await;
        ok
    }
    .await;
    ctx.audit(&claims.user_id, "vault.test", result).await;
    json_response(200, serde_json::json!({"ok": result}))
}

// --- Dispatch ------------------------------------------------------------------------------

/// Every path the router serves. The Worker answers `OPTIONS` on exactly
/// these paths with an empty 204 (the entry renders credentialed CORS
/// headers only for an exact `Origin` match); anything else 404s, so a
/// preflight never maps the API surface.
pub fn is_known_path(path: &str) -> bool {
    matches!(
        path,
        "/healthz"
            | "/v1/auth/passkey/register/start"
            | "/v1/auth/passkey/register/finish"
            | "/v1/auth/passkey/login/start"
            | "/v1/auth/passkey/login/finish"
            | "/v1/session/reauthenticate/start"
            | "/v1/session/reauthenticate/finish"
            | "/v1/auth/magic/start"
            | "/v1/auth/magic/finish"
            | "/v1/session/refresh"
            | "/v1/session/logout"
            | "/v1/session/revoke-all"
            | "/v1/vault"
            | "/v1/vault/test"
            | "/v1/vault/test-connection"
    )
}

/// Empty 204 for a preflight against a known path. Carries no ACA headers
/// itself: the entry appends them exactly when the request `Origin` equals
/// the configured origin (never reflected, never `*`).
fn preflight_response() -> Response {
    Response {
        status: 204,
        body: String::new(),
        set_cookie: None,
        retry_after: None,
    }
}

/// Route one request. Unknown routes are a plain 404; known routes map
/// every domain failure through their surface's single generic body.
pub async fn handle<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.body.len() > MAX_BODY_BYTES {
        return invalid_request();
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/healthz") => json_response(200, serde_json::json!({"ok": true})),
        ("POST", "/v1/auth/passkey/register/start") => register_start(ctx, req).await,
        ("POST", "/v1/auth/passkey/register/finish") => register_finish(ctx, req).await,
        ("POST", "/v1/auth/passkey/login/start") => login_start(ctx, req).await,
        ("POST", "/v1/auth/passkey/login/finish") => login_finish(ctx, req).await,
        ("POST", "/v1/session/reauthenticate/start") => session_reauth_start(ctx, req).await,
        ("POST", "/v1/session/reauthenticate/finish") => session_reauth_finish(ctx, req).await,
        ("POST", "/v1/auth/magic/start") => magic_start(ctx, req).await,
        ("POST", "/v1/auth/magic/finish") => magic_finish(ctx, req).await,
        ("POST", "/v1/session/refresh") => session_refresh(ctx, req).await,
        ("POST", "/v1/session/logout") => session_logout(ctx, req).await,
        ("POST", "/v1/session/revoke-all") => session_revoke_all(ctx, req).await,
        ("GET", "/v1/vault") => vault_get(ctx, req).await,
        ("PUT", "/v1/vault") => vault_put(ctx, req).await,
        ("DELETE", "/v1/vault") => vault_delete(ctx, req).await,
        ("POST", "/v1/vault/test") | ("POST", "/v1/vault/test-connection") => {
            vault_test_connection(ctx, req).await
        }
        ("OPTIONS", path) if is_known_path(path) => preflight_response(),
        _ => json_response(404, serde_json::json!({"error": "not found"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use std::task::{Context as TaskContext, Poll, Waker};

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = TaskContext::from_waker(waker);
        let mut future = Box::pin(future);
        for _ in 0..1_000 {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
        panic!("future did not complete");
    }

    /// Strip `//` line comments (outside string literals) so shape/wiring
    /// pins cannot be satisfied by an explanatory comment naming the
    /// constant. Full-line doc comments and trailing remarks are removed;
    /// `https://` inside a string literal is preserved.
    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .map(|line| {
                let bytes = line.as_bytes();
                let mut in_str = false;
                let mut escaped = false;
                let mut cut = line.len();
                let mut i = 0;
                while i + 1 < line.len() {
                    let c = bytes[i] as char;
                    if in_str {
                        if escaped {
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c == '"' {
                            in_str = false;
                        }
                    } else if c == '"' {
                        in_str = true;
                    } else if c == '/' && bytes[i + 1] == b'/' {
                        cut = i;
                        break;
                    }
                    i += 1;
                }
                line[..cut].to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    const NOW: u64 = 1_700_000_000;
    const ORIGIN: &str = "https://example.com";

    #[test]
    fn session_misconfiguration_is_retryable_server_error() {
        let response = session_error_response(session::SessionError::Misconfigured);
        assert_eq!(response.status, 500);
        assert_eq!(
            response.body,
            r#"{"code":"internal_error","message":"unexpected error","retryable":true}"#
        );

        let response = session_error_response(session::SessionError::Invalid);
        assert_eq!(response.status, 401);
        assert_eq!(
            response.body,
            r#"{"error":"check your email and try again"}"#
        );
    }

    fn cfg() -> Config {
        // The hourly per-IP budget pins predate the configured burst
        // budget: raise the burst to the durable-store cap so the
        // hourly cap stays the binding constraint in those loops. The
        // burst at its documented default is pinned separately by
        // `auth_start_burst_budget_binds_before_hourly`.
        Config::new(
            "example.com",
            ORIGIN,
            b"test-session-secret-32-bytes-long!!".to_vec(),
            [9u8; 32],
            1,
            3600,
            false,
            false,
            None,
        )
        .unwrap()
        .with_rate_limits(crate::rate_limit::RateLimitConfig {
            auth_max_attempts: 100,
            ..crate::rate_limit::RateLimitConfig::default()
        })
    }

    #[derive(Default)]
    struct FakeStore {
        users: RefCell<HashMap<String, UserRecord>>,
        challenges: RefCell<HashMap<String, ChallengeRow>>,
        credentials: RefCell<HashMap<String, PasskeyCredential>>,
        magic: RefCell<HashMap<String, MagicLinkRecord>>,
        invites: RefCell<HashMap<String, InviteRecord>>,
        sessions: RefCell<HashMap<String, SessionRecord>>,
        vaults: RefCell<HashMap<String, VaultRow>>,
        audits: RefCell<Vec<AuditEvent>>,
        rate_limit_slots: RefCell<HashMap<(String, String, u32), u64>>,
        test_connection_leases: RefCell<HashMap<(String, u32), (String, u64)>>,
        delete_user_before_next_challenge: RefCell<Option<String>>,
        delete_user_after_next_challenge: RefCell<Option<String>>,
        orphan_user_after_next_challenge: RefCell<Option<String>>,
        challenge_reads: RefCell<usize>,
        rate_limit_slot_probes: RefCell<usize>,
        fail_reauth_revocation: RefCell<bool>,
        fail_reauth_insert: RefCell<bool>,
    }

    impl Store for FakeStore {
        async fn user_by_email(&self, email: &str) -> Result<Option<UserRecord>, StoreError> {
            Ok(self
                .users
                .borrow()
                .values()
                .find(|u| u.email == email)
                .cloned())
        }
        async fn insert_user(&self, user: &UserRecord) -> Result<(), StoreError> {
            let mut users = self.users.borrow_mut();
            if users.values().any(|u| u.email == user.email) {
                return Err(StoreError);
            }
            users.insert(user.id.clone(), user.clone());
            Ok(())
        }
        async fn insert_challenge(
            &self,
            challenge: &PasskeyChallenge,
            user_id: Option<&str>,
            purpose: &str,
        ) -> Result<(), StoreError> {
            if let Some(id) = self.delete_user_before_next_challenge.borrow_mut().take() {
                self.users.borrow_mut().remove(&id);
            }
            // Mirror migration 0006 FK (`auth_challenges.user_id ->
            // users(id)`): bound challenges fail when the user is gone, so a
            // concurrent reap surfaces here instead of orphaning the row.
            if let Some(uid) = user_id {
                if !self.users.borrow().contains_key(uid) {
                    return Err(StoreError);
                }
            }
            if self.challenges.borrow().contains_key(&challenge.id) {
                return Err(StoreError);
            }
            self.challenges.borrow_mut().insert(
                challenge.id.clone(),
                ChallengeRow {
                    challenge: challenge.clone(),
                    user_id: user_id.map(str::to_string),
                    purpose: purpose.to_string(),
                },
            );
            if let Some(id) = self.delete_user_after_next_challenge.borrow_mut().take() {
                self.users.borrow_mut().remove(&id);
                self.challenges
                    .borrow_mut()
                    .retain(|_, row| row.user_id.as_deref() != Some(id.as_str()));
            }
            if let Some(id) = self.orphan_user_after_next_challenge.borrow_mut().take() {
                self.users.borrow_mut().remove(&id);
            }
            Ok(())
        }
        async fn challenge_by_id(&self, id: &str) -> Result<Option<ChallengeRow>, StoreError> {
            *self.challenge_reads.borrow_mut() += 1;
            Ok(self.challenges.borrow().get(id).cloned())
        }
        async fn insert_credential(
            &self,
            credential: &PasskeyCredential,
        ) -> Result<(), StoreError> {
            let mut creds = self.credentials.borrow_mut();
            if creds.contains_key(&credential.cred_id_b64url) {
                return Err(StoreError);
            }
            creds.insert(credential.cred_id_b64url.clone(), credential.clone());
            Ok(())
        }
        async fn credential_by_id(
            &self,
            cred_id_b64url: &str,
        ) -> Result<Option<PasskeyCredential>, StoreError> {
            Ok(self.credentials.borrow().get(cred_id_b64url).cloned())
        }
        async fn credentials_for_user(
            &self,
            user_id: &str,
        ) -> Result<Vec<PasskeyCredential>, StoreError> {
            Ok(self
                .credentials
                .borrow()
                .values()
                .filter(|c| c.user_id == user_id)
                .cloned()
                .collect())
        }
        async fn advance_sign_count(
            &self,
            cred_id_b64url: &str,
            expected: u32,
            new: u32,
        ) -> Result<bool, StoreError> {
            let mut creds = self.credentials.borrow_mut();
            match creds.get_mut(cred_id_b64url) {
                Some(cred) if cred.sign_count == expected => {
                    cred.sign_count = new;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn insert_magic(&self, record: &MagicLinkRecord) -> Result<(), StoreError> {
            self.magic
                .borrow_mut()
                .insert(record.token_hash.clone(), record.clone());
            Ok(())
        }
        async fn magic_by_hash(
            &self,
            token_hash: &str,
        ) -> Result<Option<MagicLinkRecord>, StoreError> {
            Ok(self.magic.borrow().get(token_hash).cloned())
        }
        async fn reserve_rate_limit(
            &self,
            scope_key: &str,
            action: &str,
            limit: u64,
            now: u64,
            window_secs: u64,
        ) -> Result<bool, StoreError> {
            if limit == 0 || window_secs == 0 {
                return Ok(false);
            }
            let expires_at = now.saturating_add(window_secs);
            let mut slots = self.rate_limit_slots.borrow_mut();
            slots.retain(|_, expires| *expires > now);
            // O(1) fast-fail mirrors the production `SELECT count(*)` guard:
            // an exhausted budget returns without scanning all slots, so a
            // sustained flood costs one lookup, not `limit` upserts.
            let active = slots
                .iter()
                .filter(|((scope, act, _), expires)| {
                    scope == scope_key && act == action && **expires > now
                })
                .count() as u64;
            if active >= limit {
                return Ok(false);
            }
            for slot in 0..limit {
                *self.rate_limit_slot_probes.borrow_mut() += 1;
                let slot = u32::try_from(slot).map_err(|_| StoreError)?;
                let key = (scope_key.to_string(), action.to_string(), slot);
                if let std::collections::hash_map::Entry::Vacant(entry) = slots.entry(key) {
                    entry.insert(expires_at);
                    return Ok(true);
                }
            }
            Ok(false)
        }
        async fn reserve_rate_limit_pair(
            &self,
            first: RateLimitBudget<'_>,
            second: RateLimitBudget<'_>,
            now: u64,
        ) -> Result<PairAdmission, StoreError> {
            // All-or-none under the single slot-map borrow: count and
            // locate a free slot in BOTH budgets first, insert only
            // when both admit.
            if first.limit == 0 || first.window_secs == 0 {
                return Ok(PairAdmission::DeniedFirst);
            }
            if second.limit == 0 || second.window_secs == 0 {
                return Ok(PairAdmission::DeniedSecond);
            }
            let mut slots = self.rate_limit_slots.borrow_mut();
            slots.retain(|_, expires| *expires > now);
            let free_slot = |budget: &RateLimitBudget<'_>,
                             slots: &std::collections::HashMap<(String, String, u32), u64>|
             -> Option<u32> {
                let active = slots
                    .iter()
                    .filter(|((scope, act, _), expires)| {
                        scope == budget.scope_key && act == budget.action && **expires > now
                    })
                    .count() as u64;
                if active >= budget.limit {
                    return None;
                }
                (0..budget.limit).find_map(|slot| {
                    let slot = u32::try_from(slot).ok()?;
                    let key = (
                        budget.scope_key.to_string(),
                        budget.action.to_string(),
                        slot,
                    );
                    (!slots.contains_key(&key)).then_some(slot)
                })
            };
            let (first_slot, second_slot) = (free_slot(&first, &slots), free_slot(&second, &slots));
            let (first_slot, second_slot) = match (first_slot, second_slot) {
                (Some(first_slot), Some(second_slot)) => (first_slot, second_slot),
                (None, _) => return Ok(PairAdmission::DeniedFirst),
                (_, None) => return Ok(PairAdmission::DeniedSecond),
            };
            *self.rate_limit_slot_probes.borrow_mut() += 2;
            slots.insert(
                (
                    first.scope_key.to_string(),
                    first.action.to_string(),
                    first_slot,
                ),
                now.saturating_add(first.window_secs),
            );
            slots.insert(
                (
                    second.scope_key.to_string(),
                    second.action.to_string(),
                    second_slot,
                ),
                now.saturating_add(second.window_secs),
            );
            Ok(PairAdmission::Allowed)
        }
        async fn prune_expired_challenges(&self, now: u64) -> Result<(), StoreError> {
            self.challenges
                .borrow_mut()
                .retain(|_, row| row.challenge.expires_at > now);
            Ok(())
        }
        async fn reap_unverified_users(
            &self,
            now: u64,
            ttl_secs: u64,
            protected_user_id: Option<&str>,
        ) -> Result<(), StoreError> {
            // Mirror `reap_unverified_users_sql`: magic-only accounts (session
            // without a passkey yet) and vault owners are never reaped.
            let cutoff = now.saturating_sub(ttl_secs);
            let credentialed: HashSet<String> = self
                .credentials
                .borrow()
                .values()
                .map(|cred| cred.user_id.clone())
                .collect();
            let active_registrations: HashSet<String> = self
                .challenges
                .borrow()
                .values()
                .filter(|row| row.purpose == PURPOSE_REGISTER && row.challenge.expires_at > now)
                .filter_map(|row| row.user_id.clone())
                .collect();
            let vault_owners: HashSet<String> = self.vaults.borrow().keys().cloned().collect();
            let session_owners: HashSet<String> = self
                .sessions
                .borrow()
                .values()
                .map(|row| row.user_id.clone())
                .collect();
            let magic_owners: HashSet<String> = self
                .magic
                .borrow()
                .values()
                .filter(|record| record.expires_at > now)
                .map(|record| record.user_id.clone())
                .collect();
            self.users.borrow_mut().retain(|_, user| {
                user.created_at > cutoff
                    || credentialed.contains(&user.id)
                    || active_registrations.contains(&user.id)
                    || vault_owners.contains(&user.id)
                    || session_owners.contains(&user.id)
                    || magic_owners.contains(&user.id)
                    || protected_user_id == Some(user.id.as_str())
            });
            Ok(())
        }
        async fn acquire_test_connection_lease(
            &self,
            user_id: &str,
            token: &str,
            lease_secs: u64,
            max_concurrency: u32,
        ) -> Result<bool, StoreError> {
            let expires_at = NOW.saturating_add(lease_secs);
            let mut leases = self.test_connection_leases.borrow_mut();
            leases.retain(|_, (_, expires)| *expires > NOW);
            for slot in 0..max_concurrency {
                let key = (user_id.to_string(), slot);
                if let std::collections::hash_map::Entry::Vacant(entry) = leases.entry(key) {
                    entry.insert((token.to_string(), expires_at));
                    return Ok(true);
                }
            }
            Ok(false)
        }
        async fn release_test_connection_lease(
            &self,
            user_id: &str,
            token: &str,
        ) -> Result<(), StoreError> {
            self.test_connection_leases
                .borrow_mut()
                .retain(|(owner, _), (lease_token, _)| owner != user_id || lease_token != token);
            Ok(())
        }
        async fn invite_by_hash(
            &self,
            code_hash: &str,
        ) -> Result<Option<InviteRecord>, StoreError> {
            Ok(self.invites.borrow().get(code_hash).cloned())
        }
        async fn insert_session(&self, record: &SessionRecord) -> Result<(), StoreError> {
            self.sessions
                .borrow_mut()
                .insert(record.id.clone(), record.clone());
            Ok(())
        }
        async fn session_by_id(&self, id: &str) -> Result<Option<SessionRecord>, StoreError> {
            Ok(self.sessions.borrow().get(id).cloned())
        }
        async fn rotate_session_row(
            &self,
            id: &str,
            expected_counter: u32,
            new_counter: u32,
            new_expires_at: u64,
        ) -> Result<bool, StoreError> {
            let mut sessions = self.sessions.borrow_mut();
            match sessions.get_mut(id) {
                Some(row) if row.rotation_counter == expected_counter && !row.revoked => {
                    row.rotation_counter = new_counter;
                    row.expires_at = new_expires_at;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn replace_session_after_reauthentication(
            &self,
            replacement: &session::ReauthenticatedSession,
        ) -> Result<bool, StoreError> {
            if *self.fail_reauth_revocation.borrow() {
                return Ok(false);
            }
            let mut sessions = self.sessions.borrow_mut();
            let old_id = &replacement.revoked_session_id;
            let Some(old) = sessions.get(old_id) else {
                return Ok(false);
            };
            if old.user_id != replacement.revoked_user_id
                || u64::from(old.rotation_counter) != replacement.revoked_rotation_counter
                || old.revoked
                || sessions.contains_key(&replacement.new_record.id)
            {
                return Ok(false);
            }
            sessions.get_mut(old_id).expect("row checked").revoked = true;
            if *self.fail_reauth_insert.borrow() {
                sessions.get_mut(old_id).expect("row checked").revoked = false;
                return Err(StoreError);
            }
            sessions.insert(
                replacement.new_record.id.clone(),
                replacement.new_record.clone(),
            );
            Ok(true)
        }
        async fn revoke_session(&self, id: &str) -> Result<(), StoreError> {
            if let Some(row) = self.sessions.borrow_mut().get_mut(id) {
                row.revoked = true;
            }
            Ok(())
        }
        async fn revoke_all_sessions(&self, user_id: &str) -> Result<(), StoreError> {
            for row in self.sessions.borrow_mut().values_mut() {
                if row.user_id == user_id {
                    row.revoked = true;
                }
            }
            Ok(())
        }
        async fn vault_by_user(&self, user_id: &str) -> Result<Option<VaultRow>, StoreError> {
            Ok(self.vaults.borrow().get(user_id).cloned())
        }
        async fn upsert_vault(&self, user_id: &str, row: &VaultRow) -> Result<(), StoreError> {
            self.vaults
                .borrow_mut()
                .insert(user_id.to_string(), row.clone());
            Ok(())
        }
        async fn delete_vault(&self, user_id: &str) -> Result<(), StoreError> {
            self.vaults.borrow_mut().remove(user_id);
            Ok(())
        }
        async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
            self.audits.borrow_mut().push(event.clone());
            Ok(())
        }
    }

    /// Mirrors the conditional UPDATE/RETURNING semantics: one winner.
    impl SingleUseStore for FakeStore {
        async fn claim(&self, candidate: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
            match candidate {
                auth::SingleUseClaim::MagicLink { record, now } => {
                    let mut magic = self.magic.borrow_mut();
                    match magic.get_mut(&record.token_hash) {
                        Some(row)
                            if row.user_id == record.user_id
                                && row.email == record.email
                                && row.expires_at == record.expires_at
                                && row.expires_at > now
                                && !row.used =>
                        {
                            row.used = true;
                            Ok(1)
                        }
                        _ => Ok(0),
                    }
                }
                auth::SingleUseClaim::Challenge { record, now } => {
                    let mut challenges = self.challenges.borrow_mut();
                    match challenges.get_mut(&record.id) {
                        Some(row)
                            if row.challenge.rp_id == record.rp_id
                                && row.challenge.origin == record.origin
                                && row.challenge.expires_at == record.expires_at
                                && row.challenge.expires_at > now
                                && !row.challenge.consumed =>
                        {
                            row.challenge.consumed = true;
                            Ok(1)
                        }
                        _ => Ok(0),
                    }
                }
                auth::SingleUseClaim::Invite { record } => {
                    let mut invites = self.invites.borrow_mut();
                    match invites.get_mut(&record.code_hash) {
                        Some(row) if !row.used => {
                            row.used = true;
                            Ok(1)
                        }
                        _ => Ok(0),
                    }
                }
            }
        }
    }

    #[derive(Default)]
    struct FakeMailer {
        sent: RefCell<Vec<(String, String)>>,
    }

    impl Mailer for FakeMailer {
        async fn send_magic_link(&self, email: &str, token: &str) -> Result<(), StoreError> {
            self.sent
                .borrow_mut()
                .push((email.to_string(), token.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDialer {
        ok: bool,
        seen: RefCell<Vec<Vec<IpAddr>>>,
    }

    impl Dialer for FakeDialer {
        async fn probe(&self, target: &ValidatedTarget, _port: u16, _tls: bool) -> bool {
            self.seen.borrow_mut().push(target.addresses().to_vec());
            self.ok
        }
    }

    struct FakeDns {
        map: HashMap<String, Vec<IpAddr>>,
    }

    impl AsyncDns for FakeDns {
        fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
            let addrs = self.map.get(host).cloned().unwrap_or_default();
            Box::pin(async move { addrs })
        }
    }

    struct Harness {
        config: Config,
        store: FakeStore,
        mailer: FakeMailer,
        dialer: FakeDialer,
        dns: FakeDns,
    }

    fn harness() -> Harness {
        Harness {
            config: cfg(),
            store: FakeStore::default(),
            mailer: FakeMailer::default(),
            dialer: FakeDialer {
                ok: true,
                seen: RefCell::new(Vec::new()),
            },
            dns: FakeDns {
                map: HashMap::from([(
                    "news.example.com".to_string(),
                    vec!["93.184.216.34".parse().unwrap()],
                )]),
            },
        }
    }

    impl Harness {
        fn ctx(&self) -> Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns> {
            Ctx {
                cfg: &self.config,
                store: &self.store,
                mailer: &self.mailer,
                dialer: &self.dialer,
                dns: &self.dns,
                now: NOW,
                source_ip: "198.51.100.9".to_string(),
            }
        }

        fn call(&self, method: &str, path: &str, body: serde_json::Value) -> Response {
            self.call_full(method, path, Some(body.to_string()), None, Some(ORIGIN))
        }

        fn call_with_ip(
            &self,
            source_ip: &str,
            method: &str,
            path: &str,
            body: serde_json::Value,
        ) -> Response {
            let ctx = Ctx {
                cfg: &self.config,
                store: &self.store,
                mailer: &self.mailer,
                dialer: &self.dialer,
                dns: &self.dns,
                now: NOW,
                source_ip: source_ip.to_string(),
            };
            let req = Request {
                method: method.to_string(),
                path: path.to_string(),
                cookie: None,
                origin: Some(ORIGIN.to_string()),
                body: body.to_string().into_bytes(),
            };
            block_on(handle(&ctx, &req))
        }

        fn call_full(
            &self,
            method: &str,
            path: &str,
            body: Option<String>,
            cookie: Option<String>,
            origin: Option<&str>,
        ) -> Response {
            let req = Request {
                method: method.to_string(),
                path: path.to_string(),
                cookie,
                origin: origin.map(str::to_string),
                body: body.map(|b| b.into_bytes()).unwrap_or_default(),
            };
            block_on(handle(&self.ctx(), &req))
        }

        fn call_full_with_ip(
            &self,
            source_ip: &str,
            method: &str,
            path: &str,
            body: Option<String>,
            cookie: Option<String>,
            origin: Option<&str>,
        ) -> Response {
            let ctx = Ctx {
                cfg: &self.config,
                store: &self.store,
                mailer: &self.mailer,
                dialer: &self.dialer,
                dns: &self.dns,
                now: NOW,
                source_ip: source_ip.to_string(),
            };
            let req = Request {
                method: method.to_string(),
                path: path.to_string(),
                cookie,
                origin: origin.map(str::to_string),
                body: body.map(|b| b.into_bytes()).unwrap_or_default(),
            };
            block_on(handle(&ctx, &req))
        }
    }

    fn cookie_of(resp: &Response) -> String {
        let header = resp.set_cookie.clone().expect("set-cookie");
        header.split(';').next().unwrap().to_string()
    }

    fn seed_user(h: &Harness, id: &str, email: &str) {
        block_on(h.store.insert_user(&UserRecord {
            id: id.to_string(),
            email: email.to_string(),
            created_at: NOW,
        }))
        .unwrap();
    }

    fn magic_login(h: &Harness, email: &str) -> String {
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": email}),
        );
        assert_eq!(resp.status, 200);
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        cookie_of(&resp)
    }

    #[test]
    fn config_rejects_http_origin_in_production_mode() {
        assert!(Config::new(
            "example.com",
            "http://example.com",
            vec![1u8; 32],
            [0u8; 32],
            1,
            60,
            false,
            false,
            None,
        )
        .is_err());
        assert!(Config::new(
            "localhost",
            "http://localhost",
            vec![1u8; 32],
            [0u8; 32],
            1,
            60,
            false,
            true,
            None,
        )
        .is_ok());
    }

    #[test]
    fn magic_link_flow_issues_session_once() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        // Unknown email: same 200, nothing mailed.
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "ghost@example.com"}),
        );
        assert_eq!(resp.status, 200);
        assert!(h.mailer.sent.borrow().is_empty());
        // Real flow + replay rejection.
        let _cookie = magic_login(&h, "a@example.com");
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let replay = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(replay.status, 401);
        assert_eq!(replay.body, r#"{"error":"check your email and try again"}"#);
        // Bad token is indistinguishable.
        let bad = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": "nope"}),
        );
        assert_eq!(bad.status, 401);
        assert_eq!(bad.body, replay.body);
    }

    #[test]
    fn magic_finish_never_swaps_an_existing_session_for_another_account() {
        // Login CSRF: a signed-in victim lured into redeeming someone
        // else's emailed link must keep their own session, and the
        // rejected attempt must not burn the other account's token.
        let h = harness();
        seed_user(&h, "victim", "victim@example.com");
        seed_user(&h, "other", "other@example.com");
        let victim_cookie = magic_login(&h, "victim@example.com");
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "other@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let other_token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let resp = h.call_full(
            "POST",
            "/v1/auth/magic/finish",
            Some(serde_json::json!({"token": other_token}).to_string()),
            Some(victim_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 401);
        assert_eq!(resp.body, r#"{"error":"check your email and try again"}"#);
        assert!(resp.set_cookie.is_none(), "no swapped-session cookie");
        // The same account redeeming its OWN fresh link mid-session works.
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "victim@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let own_token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let resp = h.call_full(
            "POST",
            "/v1/auth/magic/finish",
            Some(serde_json::json!({"token": own_token}).to_string()),
            Some(victim_cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        // The other account's token survived the rejected swap, and the
        // success body carries that account's own email for display.
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": other_token}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(
            resp.body.contains("\"other@example.com\""),
            "signed-in identity must be in the body: {}",
            resp.body
        );
    }

    #[test]
    fn magic_finish_requires_exact_origin_and_leaves_token_unspent() {
        // Login-CSRF session planting: an unauthenticated cross-origin POST
        // carrying the attacker's token must not mint a cookie (response
        // `Set-Cookie` is not stopped by `SameSite=Lax`), and the rejected
        // attempt must not spend the token — the same-origin explicit-confirm
        // flow still redeems afterwards.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "a@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        // Cross-origin: no cookie, generic body, token survives.
        for origin in [Some("https://evil.test"), None] {
            let resp = h.call_full(
                "POST",
                "/v1/auth/magic/finish",
                Some(serde_json::json!({"token": token}).to_string()),
                None,
                origin,
            );
            assert_eq!(resp.status, 401, "origin={origin:?}: {}", resp.body);
            assert_eq!(resp.body, r#"{"error":"check your email and try again"}"#);
            assert!(
                resp.set_cookie.is_none(),
                "no planted cookie for {origin:?}"
            );
        }
        // Same-origin explicit confirm still redeems and spends the token.
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some(), "same-origin must set a cookie");
        let replay = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(replay.status, 401);
        assert!(replay.set_cookie.is_none());
    }

    #[test]
    fn magic_link_reservations_are_shared_by_magic_login_and_register_proof() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        for _ in 0..2 {
            let resp = h.call(
                "POST",
                "/v1/auth/magic/start",
                serde_json::json!({"email": "a@example.com"}),
            );
            assert_eq!(resp.status, 200);
        }
        assert_eq!(h.mailer.sent.borrow().len(), 2);

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "a@example.com"}),
        );
        assert_eq!(resp.status, 200);
        assert_eq!(
            h.mailer.sent.borrow().len(),
            MAGIC_LINKS_PER_WINDOW as usize
        );

        for _ in 0..2 {
            let resp = h.call(
                "POST",
                "/v1/auth/magic/start",
                serde_json::json!({"email": "a@example.com"}),
            );
            assert_eq!(resp.status, 200);
        }
        assert_eq!(
            h.mailer.sent.borrow().len(),
            MAGIC_LINKS_PER_WINDOW as usize
        );
        assert_eq!(
            h.store.magic.borrow().len(),
            MAGIC_LINKS_PER_WINDOW as usize
        );
    }

    #[test]
    fn login_start_is_usernameless_budgeted_and_prunes_expired_challenges() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(&[0x5a; 32]),
            user_id: "u1".to_string(),
            public_key: fx_es256_cose(17),
            sign_count: 1,
            registered_at: NOW,
        }))
        .unwrap();

        for email in ["a@example.com", "unknown@example.com"] {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/login/start",
                serde_json::json!({"email": email}),
            );
            assert_eq!(resp.status, 200);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(body["allow_credentials"], serde_json::json!([]));
        }

        for _ in 0..AUTH_STARTS_PER_HOUR + 2 {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/login/start",
                serde_json::json!({"email": "unknown@example.com"}),
            );
            assert_eq!(resp.status, 200);
        }
        let expired = PasskeyChallenge {
            id: "expired-challenge".to_string(),
            rp_id: "example.com".to_string(),
            origin: ORIGIN.to_string(),
            issued_at: NOW.saturating_sub(auth::CHALLENGE_TTL_SECS + 1),
            expires_at: NOW.saturating_sub(1),
            consumed: false,
        };
        block_on(h.store.insert_challenge(&expired, None, PURPOSE_LOGIN)).unwrap();
        let _ = h.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": "unknown@example.com"}),
        );
        assert!(block_on(h.store.challenge_by_id("expired-challenge"))
            .unwrap()
            .is_none());
        let unknown_challenges = h
            .store
            .challenges
            .borrow()
            .values()
            .filter(|row| row.user_id.is_none())
            .count();
        assert_eq!(unknown_challenges, AUTH_STARTS_PER_HOUR as usize);
    }

    #[test]
    fn malformed_cookie_parts_do_not_hide_the_session_cookie() {
        let req = Request {
            method: "GET".to_string(),
            path: "/v1/vault".to_string(),
            cookie: Some(format!("nameless; {}=token-value", session::COOKIE_NAME)),
            origin: None,
            body: Vec::new(),
        };
        assert_eq!(session_cookie_value(&req).as_deref(), Some("token-value"));
    }

    #[test]
    fn test_connection_leases_enforce_cap_and_token_scoped_release() {
        let store = FakeStore::default();
        assert!(block_on(store.acquire_test_connection_lease("u1", "lease-a", 20, 2)).unwrap());
        assert!(block_on(store.acquire_test_connection_lease("u1", "lease-b", 20, 2)).unwrap());
        assert!(!block_on(store.acquire_test_connection_lease("u1", "lease-c", 20, 2)).unwrap());

        block_on(store.release_test_connection_lease("u1", "lease-a")).unwrap();
        assert!(block_on(store.acquire_test_connection_lease("u1", "lease-c", 20, 2)).unwrap());
        // A stale token cannot release a slot that was reused by lease-c.
        block_on(store.release_test_connection_lease("u1", "lease-a")).unwrap();
        assert!(!block_on(store.acquire_test_connection_lease("u1", "lease-d", 20, 2)).unwrap());
        block_on(store.release_test_connection_lease("u1", "lease-c")).unwrap();
        assert!(block_on(store.acquire_test_connection_lease("u1", "lease-d", 20, 2)).unwrap());
    }

    #[test]
    fn rate_limit_slots_cap_calls_and_reuse_expired_slots() {
        let store = FakeStore::default();
        let mut admitted = 0;
        for _ in 0..20 {
            if block_on(store.reserve_rate_limit("scope", "action", 10, NOW, 60)).unwrap() {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 10);
        assert!(block_on(store.reserve_rate_limit("scope", "action", 10, NOW + 60, 60)).unwrap());
    }

    #[test]
    fn two_concurrent_magic_finishes_cannot_both_win() {
        // Mutex store standing in for the conditional UPDATE/RETURNING:
        // the claim transition itself is the race boundary.
        struct ClaimStore {
            claimed: Mutex<HashSet<String>>,
        }
        impl SingleUseStore for ClaimStore {
            async fn claim(
                &self,
                candidate: auth::SingleUseClaim<'_>,
            ) -> Result<u64, auth::AuthError> {
                let key = match candidate {
                    auth::SingleUseClaim::MagicLink { record, .. } => record.token_hash.clone(),
                    _ => return Ok(0),
                };
                let mut set = self.claimed.lock().unwrap();
                Ok(if set.insert(key) { 1 } else { 0 })
            }
        }
        let (token, record) = auth::issue_magic_link("u1", "a@example.com", NOW);
        let store = Arc::new(ClaimStore {
            claimed: Mutex::new(HashSet::new()),
        });
        let mut joins = Vec::new();
        for _ in 0..2 {
            let store = store.clone();
            let record = record.clone();
            let token = token.clone();
            joins.push(std::thread::spawn(move || {
                block_on(auth::consume_magic_link_atomically(
                    record,
                    &token,
                    NOW,
                    store.as_ref(),
                ))
                .is_ok()
            }));
        }
        let wins = joins
            .into_iter()
            .map(|j| j.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(wins, 1);
    }

    #[test]
    fn refresh_rotates_server_row_and_revoke_all_kills_sessions() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let resp = h.call_full(
            "POST",
            "/v1/session/refresh",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200);
        let cookie2 = cookie_of(&resp);
        assert_ne!(cookie, cookie2);
        // The pre-rotation cookie is now stale against the row.
        let stale = h.call_full(
            "POST",
            "/v1/session/refresh",
            None,
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(stale.status, 401);
        // Revoke-all kills the rotated cookie too.
        let resp = h.call_full(
            "POST",
            "/v1/session/revoke-all",
            None,
            Some(cookie2.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200);
        let dead = h.call_full("GET", "/v1/vault", None, Some(cookie2), None);
        assert_eq!(dead.status, 401);
    }

    #[test]
    fn vault_crud_is_booleans_only_and_owner_scoped() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        seed_user(&h, "u2", "b@example.com");
        let cookie_a = magic_login(&h, "a@example.com");
        let cookie_b = magic_login(&h, "b@example.com");
        let put_body = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 563, "tls": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        });
        let resp = h.call_full(
            "PUT",
            "/v1/vault",
            Some(put_body.to_string()),
            Some(cookie_a.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(
            resp.body.contains("\"provider_configured\":true"),
            "{}",
            resp.body
        );
        for leak in ["news.example.com", "synthetic-user", "synthetic-pass"] {
            assert!(!resp.body.contains(leak), "leaked {leak}");
        }
        // Another user sees an empty vault, never the owner's existence.
        let resp = h.call_full("GET", "/v1/vault", None, Some(cookie_b), None);
        assert!(
            resp.body.contains("\"provider_configured\":false"),
            "{}",
            resp.body
        );
        // Unauthenticated and CSRF-origin mismatch both reject generically.
        let resp = h.call_full("GET", "/v1/vault", None, None, None);
        assert_eq!(resp.status, 401);
        let resp = h.call_full(
            "PUT",
            "/v1/vault",
            Some(put_body.to_string()),
            Some(cookie_a.clone()),
            Some("https://evil.example.com"),
        );
        assert_eq!(resp.status, 401);
        // Private targets collapse to the one generic vault error.
        let mut dns_map = HashMap::new();
        dns_map.insert(
            "news.example.com".to_string(),
            vec!["10.0.0.5".parse().unwrap()],
        );
        let h2_dns = FakeDns { map: dns_map };
        let ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h2_dns,
            now: NOW,
            source_ip: "198.51.100.9".to_string(),
        };
        let req = Request {
            method: "PUT".to_string(),
            path: "/v1/vault".to_string(),
            cookie: Some(cookie_a.clone()),
            origin: Some(ORIGIN.to_string()),
            body: put_body.to_string().into_bytes(),
        };
        let resp = block_on(handle(&ctx, &req));
        assert_eq!(resp.status, 400);
        assert_eq!(resp.body, r#"{"error":"vault unavailable"}"#);
        // Delete returns to booleans-false.
        let resp = h.call_full("DELETE", "/v1/vault", None, Some(cookie_a), Some(ORIGIN));
        assert!(
            resp.body.contains("\"provider_configured\":false"),
            "{}",
            resp.body
        );
    }

    #[test]
    fn vault_mutations_require_fresh_session_but_reads_do_not() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let put_body = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 563, "tls": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        });

        let fresh_put = h.call_full(
            "PUT",
            "/v1/vault",
            Some(put_body.to_string()),
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(fresh_put.status, 200, "{}", fresh_put.body);

        let stale_ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            now: NOW + session::PRIVILEGE_REAUTH_WINDOW_SECS + 1,
            source_ip: "198.51.100.9".to_string(),
        };
        let stale_put_req = Request {
            method: "PUT".to_string(),
            path: "/v1/vault".to_string(),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            body: put_body.to_string().into_bytes(),
        };
        let stale_put = block_on(handle(&stale_ctx, &stale_put_req));
        assert_eq!(stale_put.status, 401);
        assert_eq!(
            stale_put.body,
            r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
        );

        let stale_get_req = Request {
            method: "GET".to_string(),
            path: "/v1/vault".to_string(),
            cookie: Some(cookie.clone()),
            origin: None,
            body: Vec::new(),
        };
        let stale_get = block_on(handle(&stale_ctx, &stale_get_req));
        assert_eq!(stale_get.status, 200, "{}", stale_get.body);

        let stale_delete_req = Request {
            method: "DELETE".to_string(),
            path: "/v1/vault".to_string(),
            cookie: Some(cookie),
            origin: Some(ORIGIN.to_string()),
            body: Vec::new(),
        };
        let stale_delete = block_on(handle(&stale_ctx, &stale_delete_req));
        assert_eq!(stale_delete.status, 401);
        assert_eq!(
            stale_delete.body,
            r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
        );
        assert!(h.store.vaults.borrow().contains_key("u1"));
    }

    #[test]
    fn test_connection_dials_only_vetted_addresses_and_rate_limits() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let put_body = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 563, "tls": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        });
        let resp = h.call_full(
            "PUT",
            "/v1/vault",
            Some(put_body.to_string()),
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200);
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":true}"#);
        assert_eq!(
            h.dialer.seen.borrow().as_slice(),
            [vec!["93.184.216.34".parse::<IpAddr>().unwrap()]]
        );
        // Exhaust the per-user budget (shared with the vault module: 5 per
        // 60s): further probes answer false without reaching the dialer.
        for _ in 1..u64::from(vault::TEST_CONNECTION_MAX_ATTEMPTS) {
            assert!(block_on(h.store.reserve_rate_limit(
                "u1",
                "vault.test",
                u64::from(vault::TEST_CONNECTION_MAX_ATTEMPTS),
                NOW,
                vault::TEST_CONNECTION_WINDOW_SECS,
            ))
            .unwrap());
        }
        let before = h.dialer.seen.borrow().len();
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert_eq!(h.dialer.seen.borrow().len(), before);
    }

    // Shared synthetic-authenticator builders for the passkey tests below.
    fn fx_head(major: u8, v: u64, out: &mut Vec<u8>) {
        let m = major << 5;
        if v < 24 {
            out.push(m | v as u8);
        } else if v <= u8::MAX as u64 {
            out.push(m | 24);
            out.push(v as u8);
        } else if v <= u16::MAX as u64 {
            out.push(m | 25);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        } else {
            out.push(m | 26);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        }
    }
    fn fx_int(v: i64, out: &mut Vec<u8>) {
        if v >= 0 {
            fx_head(0, v as u64, out)
        } else {
            fx_head(1, (-1 - v) as u64, out)
        }
    }
    fn fx_bytes(b: &[u8], out: &mut Vec<u8>) {
        fx_head(2, b.len() as u64, out);
        out.extend_from_slice(b);
    }
    fn fx_text(t: &str, out: &mut Vec<u8>) {
        fx_head(3, t.len() as u64, out);
        out.extend_from_slice(t.as_bytes());
    }
    fn fx_es256_cose(secret: u8) -> Vec<u8> {
        use p256::ecdsa::SigningKey;
        let sk = SigningKey::from_slice(&[secret; 32]).unwrap();
        let point = sk.verifying_key().to_sec1_point(false);
        let mut cose = Vec::new();
        fx_head(5, 5, &mut cose);
        fx_int(1, &mut cose);
        fx_int(2, &mut cose);
        fx_int(3, &mut cose);
        fx_int(-7, &mut cose);
        fx_int(-1, &mut cose);
        fx_int(1, &mut cose);
        fx_int(-2, &mut cose);
        fx_bytes(point.x().unwrap(), &mut cose);
        fx_int(-3, &mut cose);
        fx_bytes(point.y().unwrap(), &mut cose);
        cose
    }
    fn fx_none_attestation(cred_id: &[u8], cose: &[u8]) -> Vec<u8> {
        fx_none_attestation_with_count(cred_id, cose, 1)
    }
    fn fx_none_attestation_with_count(cred_id: &[u8], cose: &[u8], sign_count: u32) -> Vec<u8> {
        use sha2::Digest;
        let mut auth = Vec::new();
        auth.extend_from_slice(&sha2::Sha256::digest(b"example.com"));
        auth.push(0x45);
        auth.extend_from_slice(&sign_count.to_be_bytes());
        auth.extend_from_slice(&[0u8; 16]);
        auth.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
        auth.extend_from_slice(cred_id);
        auth.extend_from_slice(cose);
        let mut att = Vec::new();
        fx_head(5, 3, &mut att);
        fx_text("fmt", &mut att);
        fx_text("none", &mut att);
        fx_text("authData", &mut att);
        fx_bytes(&auth, &mut att);
        fx_text("attStmt", &mut att);
        fx_head(5, 0, &mut att);
        att
    }
    fn fx_client_json(type_: &str, challenge: &str) -> String {
        format!(
            r#"{{"type":"{}","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
            type_, challenge, ORIGIN
        )
    }

    fn fx_reauth_assertion(
        challenge: &str,
        credential_id: &[u8],
        secret: u8,
        sign_count: u32,
    ) -> serde_json::Value {
        use p256::ecdsa::{signature::Signer, Signature as EsSignature, SigningKey};
        use sha2::Digest;

        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&sha2::Sha256::digest(b"example.com"));
        auth_data.push(0x05); // User presence and user verification are required.
        auth_data.extend_from_slice(&sign_count.to_be_bytes());
        let client_data = fx_client_json("webauthn.get", challenge);
        let mut message = auth_data.clone();
        message.extend_from_slice(&sha2::Sha256::digest(client_data.as_bytes()));
        let signing_key = SigningKey::from_slice(&[secret; 32]).unwrap();
        let signature: EsSignature = signing_key.sign(&message);
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64url_encode(credential_id),
            "authenticator_data": b64url_encode(&auth_data),
            "client_data_json": b64url_encode(client_data.as_bytes()),
            "signature": b64url_encode(signature.to_der().as_bytes()),
        })
    }

    fn start_reauthentication(h: &Harness, cookie: &str) -> String {
        let response = h.call_full(
            "POST",
            "/v1/session/reauthenticate/start",
            None,
            Some(cookie.to_string()),
            Some(ORIGIN),
        );
        assert_eq!(response.status, 200, "{}", response.body);
        let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        body["challenge"].as_str().unwrap().to_string()
    }

    fn add_test_passkey(h: &Harness, user_id: &str, credential_id: &[u8], secret: u8) {
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(credential_id),
            user_id: user_id.to_string(),
            public_key: fx_es256_cose(secret),
            sign_count: 0,
            registered_at: NOW,
        }))
        .unwrap();
    }

    #[test]
    fn passkey_reauthentication_replaces_only_the_authenticated_session() {
        let h = harness();
        seed_user(&h, "reauth-user", "reauth@example.com");
        let old_cookie = magic_login(&h, "reauth@example.com");
        let old_token = old_cookie.split_once('=').unwrap().1;
        let old_claims = session::verify_session(&h.config.session_secret, old_token, NOW).unwrap();
        let old_record = h
            .store
            .sessions
            .borrow()
            .get(&old_claims.session_id)
            .unwrap()
            .clone();
        let credential_id = b"reauth-credential";
        add_test_passkey(&h, &old_claims.user_id, credential_id, 73);

        let challenge = start_reauthentication(&h, &old_cookie);
        let response = h.call_full(
            "POST",
            "/v1/session/reauthenticate/finish",
            Some(fx_reauth_assertion(&challenge, credential_id, 73, 1).to_string()),
            Some(old_cookie),
            Some(ORIGIN),
        );
        assert_eq!(response.status, 200, "{}", response.body);
        let replacement_cookie = cookie_of(&response);
        let replacement_token = replacement_cookie.split_once('=').unwrap().1;
        let replacement_claims =
            session::verify_session(&h.config.session_secret, replacement_token, NOW).unwrap();
        assert_ne!(replacement_claims.session_id, old_record.id);
        assert_eq!(replacement_claims.user_id, old_record.user_id);
        let sessions = h.store.sessions.borrow();
        assert!(sessions.get(&old_record.id).unwrap().revoked);
        assert!(session::verify_stored_session(
            &h.config.session_keys(),
            replacement_token,
            NOW,
            sessions.get(&replacement_claims.session_id).unwrap(),
        )
        .is_ok());
    }

    #[test]
    fn reauthentication_rejects_a_credential_for_another_account() {
        let h = harness();
        seed_user(&h, "reauth-owner", "reauth-owner@example.com");
        let old_cookie = magic_login(&h, "reauth-owner@example.com");
        let old_token = old_cookie.split_once('=').unwrap().1;
        let old_claims = session::verify_session(&h.config.session_secret, old_token, NOW).unwrap();
        let old_record = h
            .store
            .sessions
            .borrow()
            .get(&old_claims.session_id)
            .unwrap()
            .clone();
        seed_user(&h, "other-account", "other@example.com");
        let credential_id = b"other-account-credential";
        add_test_passkey(&h, "other-account", credential_id, 74);

        let challenge = start_reauthentication(&h, &old_cookie);
        let response = h.call_full(
            "POST",
            "/v1/session/reauthenticate/finish",
            Some(fx_reauth_assertion(&challenge, credential_id, 74, 1).to_string()),
            Some(old_cookie),
            Some(ORIGIN),
        );
        assert_eq!(response.status, 401);
        assert!(response.set_cookie.is_none());
        assert!(
            !h.store
                .sessions
                .borrow()
                .get(&old_record.id)
                .unwrap()
                .revoked
        );
        assert!(
            !h.store
                .challenges
                .borrow()
                .get(&challenge)
                .unwrap()
                .challenge
                .consumed
        );
    }

    #[test]
    fn atomic_reauthentication_race_and_failures_leave_no_partial_replacement() {
        let h = harness();
        seed_user(&h, "atomic-reauth", "atomic-reauth@example.com");
        let old_cookie = magic_login(&h, "atomic-reauth@example.com");
        let old_token = old_cookie.split_once('=').unwrap().1;
        let old_claims = session::verify_session(&h.config.session_secret, old_token, NOW).unwrap();
        let old_record = h
            .store
            .sessions
            .borrow()
            .get(&old_claims.session_id)
            .unwrap()
            .clone();
        let keys = h.config.session_keys();
        let prepared = session::reauthenticate_session(
            &keys,
            old_token,
            NOW + 1200,
            &old_record,
            &old_claims.user_id,
            h.config.session_ttl_secs,
        )
        .unwrap();
        assert!(block_on(h.store.replace_session_after_reauthentication(&prepared)).unwrap());
        let loser = session::reauthenticate_session(
            &keys,
            old_token,
            NOW + 1200,
            &old_record,
            &old_claims.user_id,
            h.config.session_ttl_secs,
        )
        .unwrap();
        assert!(!block_on(h.store.replace_session_after_reauthentication(&loser)).unwrap());
        assert_eq!(h.store.sessions.borrow().len(), 2);

        for fail_insert in [false, true] {
            let h = harness();
            seed_user(&h, "rollback-reauth", "rollback-reauth@example.com");
            let old_cookie = magic_login(&h, "rollback-reauth@example.com");
            let old_token = old_cookie.split_once('=').unwrap().1;
            let old_claims =
                session::verify_session(&h.config.session_secret, old_token, NOW).unwrap();
            let old_record = h
                .store
                .sessions
                .borrow()
                .get(&old_claims.session_id)
                .unwrap()
                .clone();
            let credential_id = b"rollback-credential";
            add_test_passkey(&h, &old_claims.user_id, credential_id, 75);
            if fail_insert {
                *h.store.fail_reauth_insert.borrow_mut() = true;
            } else {
                *h.store.fail_reauth_revocation.borrow_mut() = true;
            }
            let challenge = start_reauthentication(&h, &old_cookie);
            let response = h.call_full(
                "POST",
                "/v1/session/reauthenticate/finish",
                Some(fx_reauth_assertion(&challenge, credential_id, 75, 1).to_string()),
                Some(old_cookie),
                Some(ORIGIN),
            );
            assert!(response.status >= 400);
            assert!(response.set_cookie.is_none());
            let sessions = h.store.sessions.borrow();
            assert_eq!(sessions.len(), 1, "failed replacement must insert no row");
            assert!(!sessions.get(&old_record.id).unwrap().revoked);
        }
    }

    #[test]
    fn ordinary_magic_login_does_not_revoke_existing_sessions() {
        let h = harness();
        seed_user(&h, "ordinary-login", "ordinary-login@example.com");
        let existing_cookie = magic_login(&h, "ordinary-login@example.com");
        let existing_token = existing_cookie.split_once('=').unwrap().1;
        let existing_claims =
            session::verify_session(&h.config.session_secret, existing_token, NOW).unwrap();
        let existing_record = h
            .store
            .sessions
            .borrow()
            .get(&existing_claims.session_id)
            .unwrap()
            .clone();

        let start = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "ordinary-login@example.com"}),
        );
        assert_eq!(start.status, 200);
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let finish = h.call_full(
            "POST",
            "/v1/auth/magic/finish",
            Some(serde_json::json!({"token": token}).to_string()),
            Some(existing_cookie),
            Some(ORIGIN),
        );
        assert_eq!(finish.status, 200, "{}", finish.body);
        assert!(
            !h.store
                .sessions
                .borrow()
                .get(&existing_record.id)
                .unwrap()
                .revoked
        );
    }

    #[test]
    fn passkey_register_then_login_roundtrip() {
        use p256::ecdsa::{signature::Signer, Signature as EsSignature, SigningKey};
        use sha2::Digest;

        // Fixture builders (synthetic authenticator).
        fn head(major: u8, v: u64, out: &mut Vec<u8>) {
            let m = major << 5;
            if v < 24 {
                out.push(m | v as u8);
            } else if v <= u8::MAX as u64 {
                out.push(m | 24);
                out.push(v as u8);
            } else if v <= u16::MAX as u64 {
                out.push(m | 25);
                out.extend_from_slice(&(v as u16).to_be_bytes());
            } else {
                out.push(m | 26);
                out.extend_from_slice(&(v as u32).to_be_bytes());
            }
        }
        fn eint(v: i64, out: &mut Vec<u8>) {
            if v >= 0 {
                head(0, v as u64, out)
            } else {
                head(1, (-1 - v) as u64, out)
            }
        }
        fn ebytes(b: &[u8], out: &mut Vec<u8>) {
            head(2, b.len() as u64, out);
            out.extend_from_slice(b);
        }
        fn etext(t: &str, out: &mut Vec<u8>) {
            head(3, t.len() as u64, out);
            out.extend_from_slice(t.as_bytes());
        }

        let h = harness();
        let sk = SigningKey::from_slice(&[11u8; 32]).unwrap();
        let point = sk.verifying_key().to_sec1_point(false);
        let mut cose = Vec::new();
        head(5, 5, &mut cose);
        eint(1, &mut cose);
        eint(2, &mut cose);
        eint(3, &mut cose);
        eint(-7, &mut cose);
        eint(-1, &mut cose);
        eint(1, &mut cose);
        eint(-2, &mut cose);
        ebytes(point.x().unwrap(), &mut cose);
        eint(-3, &mut cose);
        ebytes(point.y().unwrap(), &mut cose);

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "new@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();

        let cred_id = b"synthetic-cred";
        let mut auth = Vec::new();
        auth.extend_from_slice(&sha2::Sha256::digest(b"example.com"));
        auth.push(0x45);
        auth.extend_from_slice(&1u32.to_be_bytes());
        auth.extend_from_slice(&[0u8; 16]);
        auth.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
        auth.extend_from_slice(cred_id);
        auth.extend_from_slice(&cose);
        let mut att = Vec::new();
        head(5, 3, &mut att);
        etext("fmt", &mut att);
        etext("none", &mut att);
        etext("authData", &mut att);
        ebytes(&auth, &mut att);
        etext("attStmt", &mut att);
        head(5, 0, &mut att);
        let cd_create = format!(
            r#"{{"type":"webauthn.create","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
            challenge, ORIGIN
        );
        let finish_body = |magic_token: &str| {
            serde_json::json!({
                "challenge": challenge,
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(cd_create.as_bytes()),
                "magic_token": magic_token,
                "invite_code": null,
            })
        };
        // register/start mailed the inbox-proof token for this signup.
        let magic_token = h.mailer.sent.borrow().last().unwrap().1.clone();
        assert_eq!(h.mailer.sent.borrow().last().unwrap().0, "new@example.com");
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            finish_body(&magic_token),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some());

        // Login with an assertion (sign_count 2 > stored 1).
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": "new@example.com"}),
        );
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        assert_eq!(start["allow_credentials"], serde_json::json!([]));
        let mut auth = Vec::new();
        auth.extend_from_slice(&sha2::Sha256::digest(b"example.com"));
        auth.push(0x05);
        auth.extend_from_slice(&2u32.to_be_bytes());
        let cd_get = format!(
            r#"{{"type":"webauthn.get","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
            challenge, ORIGIN
        );
        let mut msg = auth.clone();
        msg.extend_from_slice(&sha2::Sha256::digest(cd_get.as_bytes()));
        let sig: EsSignature = sk.sign(&msg);
        let finish = serde_json::json!({
            "challenge": challenge,
            "credential_id": b64url_encode(cred_id),
            "authenticator_data": b64url_encode(&auth),
            "client_data_json": b64url_encode(cd_get.as_bytes()),
            "signature": b64url_encode(sig.to_der().as_bytes()),
        });
        let resp = h.call("POST", "/v1/auth/passkey/login/finish", finish.clone());
        assert_eq!(resp.status, 200, "{}", resp.body);
        // Replaying the same assertion/challenge fails generically.
        let resp = h.call("POST", "/v1/auth/passkey/login/finish", finish);
        assert_eq!(resp.status, 401);
    }

    /// The database CHECK (migration 0008) must list exactly the purposes
    /// the handlers mint. Adding a purpose constant without updating the
    /// migration would make every such insert fail at runtime.
    #[test]
    fn purpose_constants_match_the_migration_check() {
        let sql = include_str!("../../../migrations/0008_auth_challenge_purpose_guard.sql");
        let expected = format!("CHECK (purpose IN ('', '{PURPOSE_REGISTER}', '{PURPOSE_LOGIN}'))");
        assert!(
            sql.contains(&expected),
            "0008 CHECK must be exactly: {expected}"
        );
    }

    /// `none` attestation is unauthenticated, so
    /// the enrollment counter is attacker-chosen: a crafted attestation
    /// carrying `u32::MAX` used to be stored verbatim and then rejected
    /// every legitimate assertion (strict-increase rule) forever. The
    /// enrollment counter must never be persisted; the first assertion's
    /// signed counter establishes the baseline.
    #[test]
    fn register_ignores_attestation_supplied_sign_count() {
        use p256::ecdsa::{signature::Signer, Signature as EsSignature, SigningKey};
        use sha2::Digest;

        let h = harness();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "pinned@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let magic_token = h.mailer.sent.borrow().last().unwrap().1.clone();

        let cred_id = b"pinned-counter-cred";
        let att = fx_none_attestation_with_count(cred_id, &fx_es256_cose(11), u32::MAX);
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge,
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", &challenge).as_bytes()
                ),
                "magic_token": magic_token,
                "invite_code": null,
            }),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);

        let user = block_on(h.store.user_by_email("pinned@example.com"))
            .unwrap()
            .unwrap();
        let creds = block_on(h.store.credentials_for_user(&user.id)).unwrap();
        assert_eq!(creds.len(), 1);
        assert_eq!(
            creds[0].sign_count, 0,
            "attestation-supplied counter must not be stored"
        );

        // A legitimate first assertion (counter 1) still logs in.
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": "pinned@example.com"}),
        );
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let mut auth = Vec::new();
        auth.extend_from_slice(&sha2::Sha256::digest(b"example.com"));
        auth.push(0x05);
        auth.extend_from_slice(&1u32.to_be_bytes());
        let cd_get = fx_client_json("webauthn.get", &challenge);
        let mut msg = auth.clone();
        msg.extend_from_slice(&sha2::Sha256::digest(cd_get.as_bytes()));
        let sig: EsSignature = SigningKey::from_slice(&[11u8; 32]).unwrap().sign(&msg);
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/login/finish",
            serde_json::json!({
                "challenge": challenge,
                "credential_id": b64url_encode(cred_id),
                "authenticator_data": b64url_encode(&auth),
                "client_data_json": b64url_encode(cd_get.as_bytes()),
                "signature": b64url_encode(sig.to_der().as_bytes()),
            }),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let creds = block_on(h.store.credentials_for_user(&user.id)).unwrap();
        assert_eq!(creds[0].sign_count, 1);
    }

    #[test]
    fn login_challenge_cannot_enroll_a_new_passkey() {
        // Regression: challenges are purpose-bound. Without the binding,
        // anyone could mint a login challenge for a victim's email and
        // redeem it at register/finish to enroll their own passkey.
        let h = harness();
        seed_user(&h, "victim", "victim@example.com");
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(b"victim-cred"),
            user_id: "victim".to_string(),
            public_key: fx_es256_cose(21),
            sign_count: 1,
            registered_at: NOW,
        }))
        .unwrap();

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": "victim@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();

        let attacker_cred = b"attacker-cred";
        let att = fx_none_attestation(attacker_cred, &fx_es256_cose(33));
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge,
                "credential_id": b64url_encode(attacker_cred),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", start["challenge"].as_str().unwrap()).as_bytes()
                ),
                "magic_token": "irrelevant",
                "invite_code": null,
            }),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert_eq!(
            block_on(h.store.credentials_for_user("victim"))
                .unwrap()
                .len(),
            1,
            "no attacker credential may be enrolled"
        );
    }

    #[test]
    fn login_purpose_challenge_bound_to_credential_less_user_cannot_enroll() {
        // Pins the register-direction purpose binding ALONE: the user
        // exists with no credential yet, the login-purpose challenge is
        // bound to Some(user_id), the attestation is valid and the inbox
        // proof is that user's real token — so every later
        // register_finish gate passes and only
        // `row.purpose != PURPOSE_REGISTER` can reject. Disabling that
        // check enrolls the attacker's key (200) and fails this test.
        let h = harness();
        seed_user(&h, "victim", "victim@example.com");

        let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        let challenge_id = challenge.id.clone();
        block_on(
            h.store
                .insert_challenge(&challenge, Some("victim"), PURPOSE_LOGIN),
        )
        .unwrap();
        let (token, record) = auth::issue_magic_link("victim", "victim@example.com", NOW);
        block_on(h.store.insert_magic(&record)).unwrap();

        let attacker_cred = b"attacker-cred";
        let att = fx_none_attestation(attacker_cred, &fx_es256_cose(33));
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge_id,
                "credential_id": b64url_encode(attacker_cred),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", &challenge_id).as_bytes()
                ),
                "magic_token": token,
                "invite_code": null,
            }),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert!(
            block_on(h.store.credentials_for_user("victim"))
                .unwrap()
                .is_empty(),
            "login-purpose challenge must enroll nothing"
        );
    }

    #[test]
    fn register_finish_requires_the_emailed_token() {
        // Regression: passkey enrollment requires inbox proof, so an
        // unregistered email cannot be claimed by a stranger.
        let h = harness();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "fresh@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let cred_id = b"fresh-cred";
        let att = fx_none_attestation(cred_id, &fx_es256_cose(44));
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge,
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", start["challenge"].as_str().unwrap()).as_bytes()
                ),
                "magic_token": "not-the-emailed-token",
                "invite_code": null,
            }),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        let user = block_on(h.store.user_by_email("fresh@example.com"))
            .unwrap()
            .unwrap();
        assert!(
            block_on(h.store.credentials_for_user(&user.id))
                .unwrap()
                .is_empty(),
            "no credential may be enrolled without inbox proof"
        );
    }

    #[test]
    fn register_finish_typoed_code_does_not_burn_the_challenge() {
        // A mistyped emailed code is rejected before either single-use
        // claim, so re-entering the correct code against the SAME
        // challenge still enrolls — no second register/start email needed.
        let h = harness();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "typo@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let cred_id = b"typo-cred";
        let att = fx_none_attestation(cred_id, &fx_es256_cose(45));
        let finish = |magic_token: String| {
            serde_json::json!({
                "challenge": challenge.clone(),
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", &challenge).as_bytes()
                ),
                "magic_token": magic_token,
                "invite_code": null,
            })
        };
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            finish("typoed-code".to_string()),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        let emailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        let resp = h.call("POST", "/v1/auth/passkey/register/finish", finish(emailed));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some());
    }

    #[test]
    fn register_finish_wrong_invite_does_not_burn_challenge_or_token() {
        // A mistyped invite is pre-validated before either single-use
        // claim (like a mistyped emailed code), so correcting it against
        // the SAME challenge and token still enrolls — the signup page's
        // stay-on-the-code-step retry must not be a dead end.
        let h = harness();
        let good_hash = auth::hash_invite_code("right-invite");
        h.store.invites.borrow_mut().insert(
            good_hash.clone(),
            InviteRecord {
                code_hash: good_hash.clone(),
                used: false,
                created_at: NOW,
            },
        );
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "invited@example.com"}),
        );
        assert_eq!(resp.status, 200);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let cred_id = b"invited-cred";
        let att = fx_none_attestation(cred_id, &fx_es256_cose(46));
        let emailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        let finish = |invite_code: &str| {
            serde_json::json!({
                "challenge": challenge.clone(),
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", &challenge).as_bytes()
                ),
                "magic_token": emailed.clone(),
                "invite_code": invite_code,
            })
        };
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            finish("wrong-invite"),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert!(
            !h.store.invites.borrow().get(&good_hash).unwrap().used,
            "rejected attempt must not consume the invite"
        );
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            finish("right-invite"),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some());
        assert!(
            h.store.invites.borrow().get(&good_hash).unwrap().used,
            "corrected retry consumes the invite exactly once"
        );
    }

    #[test]
    fn auth_starts_do_not_reveal_account_existence() {
        let h = harness();
        seed_user(&h, "u1", "known@example.com");
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(b"known-cred"),
            user_id: "u1".to_string(),
            public_key: fx_es256_cose(55),
            sign_count: 0,
            registered_at: NOW,
        }))
        .unwrap();
        // register/start: same 200 shape for taken and fresh emails,
        // no mail for the taken one, and the taken email's stand-in
        // user_id is stable across calls (a fresh random per call would
        // itself reveal that the account is taken).
        let mut taken_user_ids = Vec::new();
        for email in [
            "known@example.com",
            "fresh@example.com",
            "known@example.com",
        ] {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": email}),
            );
            assert_eq!(resp.status, 200, "{email}: {}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert!(body["challenge"].is_string());
            if email == "known@example.com" {
                taken_user_ids.push(body["user_id"].clone());
            } else {
                assert!(body["user_id"].is_string());
            }
        }
        assert_eq!(taken_user_ids[0], taken_user_ids[1]);
        assert_eq!(h.mailer.sent.borrow().len(), 1);
        assert_eq!(h.mailer.sent.borrow()[0].0, "fresh@example.com");
        // login/start uses a usernameless flow, so neither account's
        // variable-length credential ids are returned.
        for email in ["ghost@example.com", "known@example.com"] {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/login/start",
                serde_json::json!({"email": email}),
            );
            assert_eq!(resp.status, 200);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(body["allow_credentials"], serde_json::json!([]), "{email}");
        }
    }

    fn put_test_vault(h: &Harness, cookie: &str) {
        let put_body = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 563, "tls": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        });
        let resp = h.call_full(
            "PUT",
            "/v1/vault",
            Some(put_body.to_string()),
            Some(cookie.to_string()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
    }

    fn call_at(h: &Harness, now: u64, method: &str, path: &str, cookie: String) -> Response {
        let ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            now,
            source_ip: "198.51.100.9".to_string(),
        };
        let req = Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie: Some(cookie),
            origin: Some(ORIGIN.to_string()),
            body: Vec::new(),
        };
        block_on(handle(&ctx, &req))
    }

    #[test]
    fn vault_test_connection_lease_cap_and_release_are_handler_enforced() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        put_test_vault(&h, &cookie);

        // A normal probe succeeds and releases its lease afterwards.
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":true}"#);
        assert_eq!(h.dialer.seen.borrow().len(), 1);
        assert!(
            h.store.test_connection_leases.borrow().is_empty(),
            "successful probe must release its lease"
        );

        // Fill the per-user lease cap directly (MAX_TEST_CONCURRENCY slots).
        assert!(block_on(h.store.acquire_test_connection_lease(
            "u1",
            "lease-a",
            20,
            vault::MAX_TEST_CONCURRENCY
        ))
        .unwrap());
        assert!(block_on(h.store.acquire_test_connection_lease(
            "u1",
            "lease-b",
            20,
            vault::MAX_TEST_CONCURRENCY
        ))
        .unwrap());

        // The handler must refuse without reaching the dialer.
        let before = h.dialer.seen.borrow().len();
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert_eq!(
            h.dialer.seen.borrow().len(),
            before,
            "capped probe must not dial"
        );
        assert_eq!(h.store.test_connection_leases.borrow().len(), 2);

        // Free both slots: the next probe is admitted and releases again.
        block_on(h.store.release_test_connection_lease("u1", "lease-a")).unwrap();
        block_on(h.store.release_test_connection_lease("u1", "lease-b")).unwrap();
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":true}"#);
        assert_eq!(h.dialer.seen.borrow().len(), before + 1);
        assert!(
            h.store.test_connection_leases.borrow().is_empty(),
            "probe must release its lease"
        );
    }

    #[test]
    fn register_start_budget_stores_no_challenge_past_cap() {
        let h = harness();
        let email = "budget@example.com";
        let mut user_id = String::new();
        for i in 0..AUTH_STARTS_PER_HOUR {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": email}),
            );
            assert_eq!(resp.status, 200, "{}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            let got = body["user_id"].as_str().unwrap().to_string();
            if i == 0 {
                user_id = got;
            } else {
                assert_eq!(got, user_id, "abandoned signup keeps one user id");
            }
        }
        assert_eq!(
            h.store.challenges.borrow().len(),
            AUTH_STARTS_PER_HOUR as usize
        );
        let mails_before = h.mailer.sent.borrow().len();

        // The 11th start keeps the 200 shape but stores nothing and mails nothing.
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": email}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["user_id"].as_str().unwrap(), user_id);
        assert_eq!(
            h.store.challenges.borrow().len(),
            AUTH_STARTS_PER_HOUR as usize,
            "over-budget register/start must not store a challenge"
        );
        assert_eq!(
            h.mailer.sent.borrow().len(),
            mails_before,
            "over-budget register/start must not send mail"
        );
    }

    #[test]
    fn global_mail_budget_exhaustion_does_not_block_registration_challenge() {
        let h = harness();
        for _ in 0..MAIL_GLOBAL_PER_WINDOW {
            assert!(block_on(h.store.reserve_rate_limit(
                GLOBAL_SCOPE_KEY,
                "auth.mail_global",
                MAIL_GLOBAL_PER_WINDOW,
                NOW,
                MAIL_GLOBAL_WINDOW_SECS,
            ))
            .unwrap());
        }

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "budget-global@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert!(body["challenge"].is_string());
        assert_eq!(h.store.challenges.borrow().len(), 1);
        assert!(h.mailer.sent.borrow().is_empty());
    }

    #[test]
    fn register_start_distinct_email_loop_bounded_by_ip_budget() {
        // Distinct-email loop from one IP: per-account caps never bind on
        // fresh addresses, so the shared per-IP budget must cut the loop
        // off with a fixed email-independent 429 (never an email-derived
        // stand-in, which would be an oracle across the denial boundary)
        // and no further user rows or mail.
        let h = harness();
        let total = AUTH_STARTS_PER_IP_PER_HOUR + 10;
        for i in 0..total {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("victim{i}@example.com")}),
            );
            if i < AUTH_STARTS_PER_IP_PER_HOUR {
                assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
                let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
                assert!(
                    body["challenge"].is_string(),
                    "iteration {i}: {}",
                    resp.body
                );
                assert!(body["user_id"].is_string(), "iteration {i}: {}", resp.body);
            } else {
                assert_eq!(resp.status, 429, "iteration {i}: {}", resp.body);
                assert_eq!(resp.body, r#"{"error":"rate_limited"}"#, "iteration {i}");
            }
        }
        assert_eq!(
            h.store.users.borrow().len(),
            AUTH_STARTS_PER_IP_PER_HOUR as usize,
            "no user row past the per-IP budget"
        );
        assert_eq!(
            h.mailer.sent.borrow().len(),
            AUTH_STARTS_PER_IP_PER_HOUR as usize,
            "no mail queued past the per-IP budget"
        );
        assert_eq!(
            h.store.challenges.borrow().len(),
            AUTH_STARTS_PER_IP_PER_HOUR as usize,
            "no challenge stored past the per-IP budget"
        );
    }

    #[test]
    fn register_start_ip_throttle_is_email_independent() {
        // Account-existence oracle pin: while admitted, a taken email returns
        // the stable stand-in and a fresh email returns a fresh random id;
        // once the per-IP budget is exhausted every email must get the same
        // fixed 429, so pre/post comparison cannot distinguish taken/fresh.
        let h = harness();
        seed_user(&h, "u-taken", "taken@example.com");
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(b"taken-cred-oracle"),
            user_id: "u-taken".to_string(),
            public_key: fx_es256_cose(61),
            sign_count: 0,
            registered_at: NOW,
        }))
        .unwrap();
        let taken_admitted = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "taken@example.com"}),
        );
        assert_eq!(taken_admitted.status, 200, "{}", taken_admitted.body);
        let fresh_admitted = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "fresh-oracle@example.com"}),
        );
        assert_eq!(fresh_admitted.status, 200, "{}", fresh_admitted.body);
        for i in 0..(AUTH_STARTS_PER_IP_PER_HOUR - 2) {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("filler{i}@example.com")}),
            );
            assert_eq!(resp.status, 200, "filler {i}: {}", resp.body);
        }
        let taken_denied = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "taken@example.com"}),
        );
        let fresh_denied = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "fresh-oracle@example.com"}),
        );
        assert_eq!(taken_denied.status, 429, "{}", taken_denied.body);
        assert_eq!(fresh_denied.status, 429, "{}", fresh_denied.body);
        // This harness raises the burst cap (see `cfg()`), so the
        // HOURLY budget is the denier here and Retry-After must report
        // its window — the mirror of the burst-denial pin in
        // `auth_start_burst_budget_binds_before_hourly`.
        assert_eq!(taken_denied.retry_after, Some(AUTH_START_WINDOW_SECS));
        assert_eq!(fresh_denied.retry_after, Some(AUTH_START_WINDOW_SECS));
        assert_eq!(
            taken_denied.body, fresh_denied.body,
            "throttled bodies must be identical across emails"
        );
        assert_eq!(taken_denied.body, r#"{"error":"rate_limited"}"#);
    }

    #[test]
    fn auth_start_burst_budget_binds_before_hourly() {
        // The configured burst (documented default 10/60s from
        // `RateLimitConfig`) is live on the auth-start path: the 11th
        // start inside the window is denied long before the hourly
        // per-IP budget (30) would bind. `cfg()` raises the burst for
        // the hourly pins, so restore the default here.
        let mut h = harness();
        h.config.rate_limits = crate::rate_limit::RateLimitConfig::default();
        let burst = h.config.rate_limits.auth_max_attempts as usize;
        assert!((burst as u64) < AUTH_STARTS_PER_IP_PER_HOUR);
        for i in 0..burst {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("burst{i}@example.com")}),
            );
            assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
        }
        let denied = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "burst-over@example.com"}),
        );
        assert_eq!(denied.status, 429, "{}", denied.body);
        assert_eq!(denied.body, r#"{"error":"rate_limited"}"#);
        // Handler-level Retry-After pin: a BURST denial reports the
        // burst window (60s default), not the hourly window — clients
        // must not back off for an hour over a one-minute budget. The
        // hourly-denial case reports AUTH_START_WINDOW_SECS and is
        // pinned in `register_start_ip_throttle_is_email_independent`
        // (where cfg() raises the burst so the hourly cap binds).
        assert_eq!(
            denied.retry_after,
            Some(h.config.rate_limits.auth_window_secs)
        );
        assert_ne!(denied.retry_after, Some(AUTH_START_WINDOW_SECS));
        assert_eq!(
            h.store.users.borrow().len(),
            burst,
            "no user row past the burst budget"
        );
    }

    #[test]
    fn rate_limit_pair_denial_consumes_neither_budget() {
        // All-or-none pin for the dual claim: with the second budget
        // exhausted, denied pair claims must leave the first budget
        // fully intact (a sequential reserve would burn one first
        // slot per denial).
        let store = FakeStore::default();
        let first = RateLimitBudget {
            scope_key: "ip-scope",
            action: "pair.first",
            limit: 2,
            window_secs: 60,
        };
        let second = RateLimitBudget {
            scope_key: "account-scope",
            action: "pair.second",
            limit: 1,
            window_secs: 60,
        };
        // Exhaust the second budget on its own.
        assert!(block_on(store.reserve_rate_limit(
            second.scope_key,
            second.action,
            second.limit,
            NOW,
            second.window_secs
        ))
        .unwrap());
        for _ in 0..3 {
            assert_eq!(
                block_on(store.reserve_rate_limit_pair(first, second, NOW)).unwrap(),
                PairAdmission::DeniedSecond,
                "the exhausted second budget must be named as the denier"
            );
        }
        // Both first-budget slots are still available to single claims.
        assert!(block_on(store.reserve_rate_limit(
            first.scope_key,
            first.action,
            first.limit,
            NOW,
            first.window_secs
        ))
        .unwrap());
        assert!(block_on(store.reserve_rate_limit(
            first.scope_key,
            first.action,
            first.limit,
            NOW,
            first.window_secs
        ))
        .unwrap());
        assert!(!block_on(store.reserve_rate_limit(
            first.scope_key,
            first.action,
            first.limit,
            NOW,
            first.window_secs
        ))
        .unwrap());
    }

    #[test]
    fn rate_limit_exhausted_path_skips_slot_scan() {
        // DoS-amplification pin: an exhausted 100-slot finish-lookup budget
        // must fast-fail without scanning every slot (production runs one
        // `RATE_LIMIT_EXHAUSTED_SQL` count then returns; the fake mirrors it
        // and counts slot probes). Removing the guard drives 100 probes here.
        let store = FakeStore::default();
        for _ in 0..FINISH_LOOKUP_ATTEMPTS_PER_IP {
            assert!(block_on(store.reserve_rate_limit(
                "ip",
                "auth.register_finish_ip",
                FINISH_LOOKUP_ATTEMPTS_PER_IP,
                NOW,
                300
            ))
            .unwrap());
        }
        *store.rate_limit_slot_probes.borrow_mut() = 0;
        assert!(!block_on(store.reserve_rate_limit(
            "ip",
            "auth.register_finish_ip",
            FINISH_LOOKUP_ATTEMPTS_PER_IP,
            NOW,
            300
        ))
        .unwrap());
        assert_eq!(
            *store.rate_limit_slot_probes.borrow(),
            0,
            "exhausted budget must fast-fail without scanning slots"
        );
    }

    #[test]
    fn rate_limit_exhausted_sql_pins_production_fast_fail() {
        // Pin the production `PgStore::reserve_rate_limit` fast-fail itself:
        // the guard must stay a single `SELECT COUNT(*)` over the live
        // window (`worker_entry` executes this shared constant). Deleting or
        // retargeting the Pg guard leaves the fake-mirror probe test green,
        // so this shape test observes the production path directly.
        let sql = RATE_LIMIT_EXHAUSTED_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            sql.starts_with("SELECT COUNT(*) FROM auth_rate_limit_slots"),
            "fast-fail must count live slots in one statement, got: {sql}"
        );
        for predicate in ["scope_key = $1", "action = $2", "expires_at > $3"] {
            assert!(
                sql.contains(predicate),
                "fast-fail must include `{predicate}`"
            );
        }
        assert!(
            !sql.contains("expires_at <= $3"),
            "fast-fail must count live slots, never expired ones"
        );
        // Wiring pin: production must execute this shared constant via
        // `query_one`, not a divergent literal or no guard at all. Deleting
        // the Pg fast-fail block (or inlining a hand-written literal) leaves
        // the FakeStore probe test green, so this assertion observes the
        // production path directly at compile time. Comments are stripped
        // first: the explanatory comment naming the constant must not
        // satisfy the pin on its own.
        let entry = include_str!("worker_entry.rs");
        let code = strip_line_comments(entry);
        assert!(
            code.contains("RATE_LIMIT_EXHAUSTED_SQL"),
            "Pg reserve_rate_limit must execute the shared exhausted SQL"
        );
        assert!(
            code.contains("query_one("),
            "Pg fast-fail must run the shared SQL via query_one"
        );
        let query_pos = code.find("query_one(").expect("query_one call");
        assert!(
            code[query_pos..]
                .chars()
                .take(300)
                .collect::<String>()
                .contains("RATE_LIMIT_EXHAUSTED_SQL"),
            "Pg query_one call must pass the shared exhausted SQL"
        );
        assert!(
            !code.contains("SELECT COUNT(*) FROM auth_rate_limit_slots"),
            "Pg must not carry a divergent fast-fail literal"
        );
    }

    #[test]
    fn rate_limit_ip_key_groups_ipv6_slash64() {
        // IPv6 rotation pin: two addresses in one /64 share one budget key,
        // different /64s do not; IPv4 keeps the full address and unparsable
        // values pass through (e.g. the "unknown" fallback).
        assert_eq!(
            rate_limit_ip_key("2001:db8:abcd:12::1"),
            rate_limit_ip_key("2001:db8:abcd:12::2")
        );
        assert_ne!(
            rate_limit_ip_key("2001:db8:abcd:12::1"),
            rate_limit_ip_key("2001:db8:abcd:13::1")
        );
        assert_eq!(rate_limit_ip_key("198.51.100.9"), "198.51.100.9");
        assert_eq!(rate_limit_ip_key("unknown"), "unknown");
        let store = FakeStore::default();
        let k1 = rate_limit_ip_key("2001:db8:abcd:12::1");
        let k2 = rate_limit_ip_key("2001:db8:abcd:12::9");
        for _ in 0..AUTH_STARTS_PER_IP_PER_HOUR {
            assert!(block_on(store.reserve_rate_limit(
                &k1,
                "auth.start_ip",
                AUTH_STARTS_PER_IP_PER_HOUR,
                NOW,
                3600
            ))
            .unwrap());
        }
        assert!(
            !block_on(store.reserve_rate_limit(
                &k2,
                "auth.start_ip",
                AUTH_STARTS_PER_IP_PER_HOUR,
                NOW,
                3600
            ))
            .unwrap(),
            "same /64 must share one budget"
        );
    }

    #[test]
    fn register_start_ipv6_slash64_shares_handler_budget() {
        // Handler wiring pin: two IPv6 addresses in one /64 share one
        // `admit_auth_start` budget through the real `register_start`
        // handler — not just the `rate_limit_ip_key` unit above (which
        // pre-keys via the function itself) and not the IPv4-only harness
        // default. Reverting `admit_auth_start` to the raw `source_ip`
        // leaves every other test green but fails here: the second address
        // would get a fresh budget instead of the shared refusal.
        let h = harness();
        let ip_a = "2001:db8:abcd:12::1";
        let ip_b = "2001:db8:abcd:12::9";
        let ip_other = "2001:db8:abcd:13::1";
        let from_a = AUTH_STARTS_PER_IP_PER_HOUR - 10;
        for i in 0..from_a {
            let resp = h.call_with_ip(
                ip_a,
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("v6a{i}@example.com")}),
            );
            assert_eq!(resp.status, 200, "ip_a iteration {i}: {}", resp.body);
        }
        for i in 0..10 {
            let resp = h.call_with_ip(
                ip_b,
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("v6b{i}@example.com")}),
            );
            assert_eq!(
                resp.status, 200,
                "ip_b iteration {i} must share the remaining /64 budget: {}",
                resp.body
            );
        }
        let denied = h.call_with_ip(
            ip_b,
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "v6b-final@example.com"}),
        );
        assert_eq!(
            denied.status, 429,
            "second address in the same /64 must hit the shared budget: {}",
            denied.body
        );
        assert_eq!(denied.body, r#"{"error":"rate_limited"}"#);
        let other = h.call_with_ip(
            ip_other,
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "v6other@example.com"}),
        );
        assert_eq!(
            other.status, 200,
            "a different /64 must keep its own budget: {}",
            other.body
        );
    }

    #[test]
    fn ipv6_slash64_wiring_pins_all_rate_limit_scopes() {
        // All-sites wiring pin: every per-IP scope must key on
        // `rate_limit_ip_key`, not the raw `source_ip`. The behavioral test
        // above covers `auth.start_ip` only; the other scopes revert to
        // raw IP with all handler tests green (the harness default
        // `198.51.100.9` makes key == raw), so this shape test observes all
        // call sites directly at compile time. Slice ONLY the pre-test
        // production source first: the unquoted `let ip_key = ...` needles
        // below appear verbatim in this test's own string literals, so
        // searching the whole file would match the test itself and pass
        // even with a call site reverted to the raw address. Comments are
        // stripped next so prose mentioning a scope cannot satisfy its pin.
        let source = include_str!("handlers.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let code = strip_line_comments(production);
        let norm = code.split_whitespace().collect::<Vec<_>>().join(" ");
        // `admit_auth_start` claims its two budgets as a pair keyed on
        // one shared `ip_key`; pin both the derivation and the scopes.
        assert!(
            norm.contains("let ip_key = rate_limit_ip_key(&self.source_ip);"),
            "auth-start pair key must derive from rate_limit_ip_key, not raw source_ip",
        );
        assert!(
            norm.contains("let ip_key = rate_limit_ip_key(&ctx.source_ip);"),
            "vault-test pair key must derive from rate_limit_ip_key, not raw source_ip",
        );
        for (scope, keyed) in [
            (
                "auth.start_ip",
                "scope_key: &ip_key, action: \"auth.start_ip\",",
            ),
            (
                "auth.start_ip_burst",
                "scope_key: &ip_key, action: \"auth.start_ip_burst\",",
            ),
            (
                "vault.test_ip",
                "scope_key: &ip_key, action: \"vault.test_ip\",",
            ),
            (
                "auth.mail_ip",
                "&rate_limit_ip_key(&self.source_ip), \"auth.mail_ip\",",
            ),
            (
                "auth.register_finish_ip",
                "&rate_limit_ip_key(&ctx.source_ip), \"auth.register_finish_ip\",",
            ),
            (
                "auth.login_finish_ip",
                "&rate_limit_ip_key(&ctx.source_ip), \"auth.login_finish_ip\",",
            ),
            (
                "auth.magic_finish",
                "&rate_limit_ip_key(&ctx.source_ip), \"auth.magic_finish\",",
            ),
        ] {
            assert!(
                norm.contains(keyed),
                "scope {scope} must key on rate_limit_ip_key, not raw source_ip"
            );
        }
        assert!(
            norm.contains("format!(\"invite:{}:{}\", rate_limit_ip_key(&ctx.source_ip), user_id)"),
            "scope auth.invite_redeem must key on rate_limit_ip_key, not raw source_ip",
        );
    }

    #[test]
    fn login_start_distinct_email_loop_bounded_by_ip_budget() {
        // Unknown-email login starts mint unbound challenge rows per fresh
        // address; the per-IP budget must cap them with the same 200 shape.
        let h = harness();
        let total = AUTH_STARTS_PER_IP_PER_HOUR + 10;
        for i in 0..total {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/login/start",
                serde_json::json!({"email": format!("ghost{i}@example.com")}),
            );
            assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(body["allow_credentials"], serde_json::json!([]));
        }
        assert_eq!(
            h.store.challenges.borrow().len(),
            AUTH_STARTS_PER_IP_PER_HOUR as usize,
            "no challenge row past the per-IP budget"
        );
    }

    #[test]
    fn magic_start_mail_capped_per_ip_across_victims() {
        // Third-party email-bombing shape: one IP working down a victim
        // list (3 mails/15 min per victim, unlimited victims) must hit the
        // shared per-IP mail cap with the identical {"ok":true} response.
        let h = harness();
        let victims = MAIL_PER_IP_PER_HOUR + 10;
        for i in 0..victims {
            seed_user(&h, &format!("u{i}"), &format!("victim{i}@example.com"));
        }
        for i in 0..victims {
            let resp = h.call(
                "POST",
                "/v1/auth/magic/start",
                serde_json::json!({"email": format!("victim{i}@example.com")}),
            );
            assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
            assert_eq!(resp.body, r#"{"ok":true}"#);
        }
        assert_eq!(
            h.mailer.sent.borrow().len(),
            MAIL_PER_IP_PER_HOUR as usize,
            "no mail queued past the per-IP mail cap"
        );
        assert_eq!(
            h.store.magic.borrow().len(),
            MAIL_PER_IP_PER_HOUR as usize,
            "no magic row stored past the per-IP mail cap"
        );
    }

    #[test]
    fn magic_start_mail_cap_binds_independently_of_start_budget() {
        // Pins the per-IP mail cap itself: `admit_auth_start` (`auth.start_ip`)
        // and the mail cap (`auth.mail_ip`) share the same 30/hour shape, so
        // the victim-loop test above stays green even with the `!mail_ip_ok`
        // check removed (the start budget cuts the loop first). Pre-seed only
        // the mail slots, leaving the start budget fresh: the next start must
        // admit but send/store nothing. Removing the mail check sends one mail
        // and fails this test.
        let h = harness();
        seed_user(&h, "u1", "pinned@example.com");
        for _ in 0..MAIL_PER_IP_PER_HOUR {
            assert!(block_on(h.store.reserve_rate_limit(
                "198.51.100.9",
                "auth.mail_ip",
                MAIL_PER_IP_PER_HOUR,
                NOW,
                3600,
            ))
            .unwrap());
        }
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "pinned@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_eq!(resp.body, r#"{"ok":true}"#);
        assert!(
            h.mailer.sent.borrow().is_empty(),
            "exhausted mail_ip budget must send nothing"
        );
        assert!(
            h.store.magic.borrow().is_empty(),
            "exhausted mail_ip budget must store no magic row"
        );
    }

    #[test]
    fn register_start_reaps_expired_unverified_users_and_prunes_challenges() {
        // Register-only traffic must not grow tables monotonically: a
        // never-completed signup past the TTL is reaped (credentialed
        // accounts survive), and expired challenges are pruned here too.
        let h = harness();
        let stale_at = NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1);
        block_on(h.store.insert_user(&UserRecord {
            id: "stale-unverified".to_string(),
            email: "stale@example.com".to_string(),
            created_at: stale_at,
        }))
        .unwrap();
        block_on(h.store.insert_user(&UserRecord {
            id: "old-verified".to_string(),
            email: "old@example.com".to_string(),
            created_at: stale_at,
        }))
        .unwrap();
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(&[0x7e; 32]),
            user_id: "old-verified".to_string(),
            public_key: fx_es256_cose(77),
            sign_count: 1,
            registered_at: stale_at,
        }))
        .unwrap();
        let expired = PasskeyChallenge {
            id: "expired-challenge".to_string(),
            rp_id: "example.com".to_string(),
            origin: ORIGIN.to_string(),
            issued_at: NOW.saturating_sub(auth::CHALLENGE_TTL_SECS + 1),
            expires_at: NOW.saturating_sub(1),
            consumed: false,
        };
        block_on(h.store.insert_challenge(&expired, None, PURPOSE_LOGIN)).unwrap();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "fresh@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(
            block_on(h.store.user_by_email("stale@example.com"))
                .unwrap()
                .is_none(),
            "expired unverified signup must be reaped"
        );
        assert!(
            block_on(h.store.user_by_email("old@example.com"))
                .unwrap()
                .is_some(),
            "credentialed account must survive reaping"
        );
        assert!(
            block_on(h.store.user_by_email("fresh@example.com"))
                .unwrap()
                .is_some(),
            "the current signup still proceeds"
        );
        assert!(block_on(h.store.challenge_by_id("expired-challenge"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn register_start_reaper_preserves_resumed_and_active_signups() {
        let h = harness();
        let stale_at = NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1);
        for (id, email) in [
            ("resumed", "resume@example.com"),
            ("active", "active@example.com"),
            ("abandoned", "abandoned@example.com"),
        ] {
            block_on(h.store.insert_user(&UserRecord {
                id: id.to_string(),
                email: email.to_string(),
                created_at: stale_at,
            }))
            .unwrap();
        }
        let mut finishing_challenge =
            auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        // Finish consumes the challenge before credential insertion; cleanup
        // must still protect the user during that in-flight interval.
        finishing_challenge.consumed = true;
        block_on(
            h.store
                .insert_challenge(&finishing_challenge, Some("active"), PURPOSE_REGISTER),
        )
        .unwrap();

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "resume@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(block_on(h.store.user_by_email("resume@example.com"))
            .unwrap()
            .is_some());
        assert!(block_on(h.store.user_by_email("active@example.com"))
            .unwrap()
            .is_some());
        assert!(block_on(h.store.user_by_email("abandoned@example.com"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn reap_sql_pins_register_purpose_and_activity_exemptions() {
        // Pin the destructive SQL predicates themselves: the FakeStore
        // exercises an independent mirror and cannot establish Pg semantics.
        let sql = reap_unverified_users_sql();
        let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            sql.contains("created_at <= $1"),
            "reap must enforce the TTL"
        );
        assert!(
            sql.contains("users.id <> $2"),
            "reap must preserve the current user"
        );
        assert!(sql.contains("auth_challenges.purpose = 'register'"));
        assert!(sql.contains("auth_challenges.expires_at > $3"));
        assert!(sql.contains("magic_link_tokens.expires_at > $3"));
        for predicate in [
            "NOT EXISTS (SELECT 1 FROM webauthn_credentials",
            "NOT EXISTS (SELECT 1 FROM auth_challenges",
            "NOT EXISTS (SELECT 1 FROM provider_vaults",
            "NOT EXISTS (SELECT 1 FROM sessions",
            "NOT EXISTS (SELECT 1 FROM magic_link_tokens",
        ] {
            assert!(sql.contains(predicate), "reap must include `{predicate}`");
        }
        assert!(!sql.contains("auth_challenges.expires_at <= $3"));
        assert!(!sql.contains("magic_link_tokens.expires_at <= $3"));
    }

    #[test]
    fn insert_challenge_enforces_user_fk_like_migration_0006() {
        // Mirrors `migrations/0006_auth_challenge_user_fk.sql`: a bound
        // challenge for a missing user fails instead of orphaning the row,
        // so a concurrent reap surfaces at insert (healed by retry).
        let h = harness();
        let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        assert!(
            block_on(
                h.store
                    .insert_challenge(&challenge, Some("ghost"), PURPOSE_REGISTER)
            )
            .is_err(),
            "bound challenge for a missing user must fail"
        );
        assert!(
            block_on(h.store.insert_challenge(&challenge, None, PURPOSE_LOGIN)).is_ok(),
            "unbound login challenges stay allowed"
        );
    }

    #[test]
    fn register_start_reaper_preserves_magic_only_vault_and_inbox() {
        // CRITICAL pin: `magic_finish` issues a session with no credential
        // check and the register inbox email carries a redeemable magic link,
        // while `provider_vaults.user_id ... ON DELETE CASCADE` destroys a
        // vault saved under that session. A stale magic-only account (session
        // + vault, no passkey) and a stale inbox-only account (unexpired
        // magic, no session yet) must both survive a later `register_start`
        // for another email; a truly abandoned signup must still be reaped.
        let h = harness();
        let stale_at = NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1);
        for (id, email) in [
            ("magic-only", "magic-only@example.com"),
            ("inbox-only", "inbox-only@example.com"),
            ("abandoned", "abandoned2@example.com"),
        ] {
            block_on(h.store.insert_user(&UserRecord {
                id: id.to_string(),
                email: email.to_string(),
                created_at: stale_at,
            }))
            .unwrap();
        }
        let (_, magic_only_record) =
            auth::issue_magic_link("magic-only", "magic-only@example.com", NOW);
        block_on(h.store.insert_magic(&magic_only_record)).unwrap();
        block_on(h.store.insert_session(&SessionRecord {
            id: "sess-magic-only".to_string(),
            user_id: "magic-only".to_string(),
            created_at: NOW,
            expires_at: NOW + 3600,
            rotation_counter: 0,
            revoked: false,
        }))
        .unwrap();
        h.store.vaults.borrow_mut().insert(
            "magic-only".to_string(),
            VaultRow {
                blob: crate::vault::VaultBlob {
                    kek_version: 1,
                    dek_wrapped_nonce_b64url: "AAAAAAAAAAAAAAAAAAAAAA".to_string(),
                    dek_wrapped_b64url: "AA".to_string(),
                    payload_nonce_b64url: "AAAAAAAAAAAAAAAAAAAAAA".to_string(),
                    payload_b64url: "AA".to_string(),
                },
                indexer_configured: false,
                updated_at: NOW,
            },
        );
        let (_, inbox_record) = auth::issue_magic_link("inbox-only", "inbox-only@example.com", NOW);
        block_on(h.store.insert_magic(&inbox_record)).unwrap();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "fresh2@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(
            block_on(h.store.user_by_email("magic-only@example.com"))
                .unwrap()
                .is_some(),
            "magic-only account with session/vault must survive reaping"
        );
        assert!(
            block_on(h.store.user_by_email("inbox-only@example.com"))
                .unwrap()
                .is_some(),
            "inbox-only account with redeemable magic must survive reaping"
        );
        assert!(
            block_on(h.store.user_by_email("abandoned2@example.com"))
                .unwrap()
                .is_none(),
            "truly abandoned signup must still be reaped"
        );
        assert!(
            h.store.vaults.borrow().contains_key("magic-only"),
            "surviving vault must not cascade-delete"
        );
    }

    #[test]
    fn register_start_heals_concurrently_reaped_resumed_signup() {
        // Delete the resumed user at the handler's challenge insert to model
        // another register_start reaping it after lookup; the handler must
        // recreate the row and retry the challenge insert.
        let h = harness();
        let stale_at = NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1);
        block_on(h.store.insert_user(&UserRecord {
            id: "resumed-race".to_string(),
            email: "race@example.com".to_string(),
            created_at: stale_at,
        }))
        .unwrap();
        *h.store.delete_user_before_next_challenge.borrow_mut() = Some("resumed-race".to_string());

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "race@example.com"}),
        );

        assert!(
            h.store.delete_user_before_next_challenge.borrow().is_none(),
            "race hook must fire during register_start"
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let user = block_on(h.store.user_by_email("race@example.com"))
            .unwrap()
            .expect("handler heals the user deleted during challenge insert");
        assert_eq!(
            user.id, "resumed-race",
            "healing retains the original user id"
        );
        assert!(
            h.store.challenges.borrow().values().any(|row| {
                row.user_id.as_deref() == Some("resumed-race") && row.purpose == PURPOSE_REGISTER
            }),
            "handler retries and stores the registration challenge"
        );
    }

    #[test]
    fn register_start_reinserts_challenge_deleted_by_concurrent_reap() {
        // Model a reaper deleting the user after the registration challenge
        // insert; ON DELETE CASCADE removes that challenge before revalidation.
        let h = harness();
        let stale_at = NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1);
        block_on(h.store.insert_user(&UserRecord {
            id: "cascade-race".to_string(),
            email: "cascade@example.com".to_string(),
            created_at: stale_at,
        }))
        .unwrap();
        *h.store.delete_user_after_next_challenge.borrow_mut() = Some("cascade-race".to_string());

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "cascade@example.com"}),
        );

        assert!(h.store.delete_user_after_next_challenge.borrow().is_none());
        assert_eq!(resp.status, 200, "{}", resp.body);
        let user = block_on(h.store.user_by_email("cascade@example.com"))
            .unwrap()
            .expect("handler recreates the concurrently reaped user");
        assert_eq!(user.id, "cascade-race");
        assert!(
            h.store.challenges.borrow().values().any(|row| {
                row.user_id.as_deref() == Some("cascade-race") && row.purpose == PURPOSE_REGISTER
            }),
            "handler must recreate the challenge removed by the user cascade"
        );
    }

    #[test]
    fn register_start_preserves_orphaned_challenge_without_user_fk() {
        // Older schemas without the user FK can leave the successful insert
        // behind when a concurrent reap deletes only the user row.
        let h = harness();
        let stale_at = NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1);
        block_on(h.store.insert_user(&UserRecord {
            id: "orphan-race".to_string(),
            email: "orphan@example.com".to_string(),
            created_at: stale_at,
        }))
        .unwrap();
        *h.store.orphan_user_after_next_challenge.borrow_mut() = Some("orphan-race".to_string());

        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "orphan@example.com"}),
        );

        assert!(h.store.orphan_user_after_next_challenge.borrow().is_none());
        assert_eq!(resp.status, 200, "{}", resp.body);
        let user = block_on(h.store.user_by_email("orphan@example.com"))
            .unwrap()
            .expect("handler recreates the concurrently reaped user");
        assert_eq!(user.id, "orphan-race");
        assert!(
            h.store.challenges.borrow().values().any(|row| {
                row.user_id.as_deref() == Some("orphan-race") && row.purpose == PURPOSE_REGISTER
            }),
            "handler must preserve the challenge inserted before the user was reaped"
        );
    }

    #[test]
    fn register_finish_lookup_budget_bounds_rotating_ids_before_db() {
        // Pins the per-IP lookup budget BEFORE any DB read: pre-seed the
        // `auth.register_finish_ip` pool, then a VALID finish (built via
        // register/start) must fail generically without enrolling.
        // Removing the pre-lookup reservation would let it succeed (200).
        let h = harness();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "lookup@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let start: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let cred_id = b"lookup-cred";
        let att = fx_none_attestation(cred_id, &fx_es256_cose(48));
        let emailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        for _ in 0..FINISH_LOOKUP_ATTEMPTS_PER_IP {
            assert!(block_on(h.store.reserve_rate_limit(
                "198.51.100.9",
                "auth.register_finish_ip",
                FINISH_LOOKUP_ATTEMPTS_PER_IP,
                NOW,
                FINISH_ATTEMPT_WINDOW_SECS,
            ))
            .unwrap());
        }
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge.clone(),
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", &challenge).as_bytes()
                ),
                "magic_token": emailed,
                "invite_code": null,
            }),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        let user = block_on(h.store.user_by_email("lookup@example.com"))
            .unwrap()
            .unwrap();
        assert!(
            block_on(h.store.credentials_for_user(&user.id))
                .unwrap()
                .is_empty(),
            "lookup-blocked finish must enroll nothing"
        );
    }

    #[test]
    fn login_finish_lookup_budget_bounds_rotating_ids_before_db() {
        // Exhaust the per-IP lookup budget while providing a real stored
        // challenge. Rejection must happen before challenge_by_id, rather
        // than passing vacuously because a bogus challenge misses lookup.
        let h = harness();
        let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        let challenge_id = challenge.id.clone();
        block_on(h.store.insert_challenge(&challenge, None, PURPOSE_LOGIN)).unwrap();
        for _ in 0..FINISH_LOOKUP_ATTEMPTS_PER_IP {
            assert!(block_on(h.store.reserve_rate_limit(
                "198.51.100.9",
                "auth.login_finish_ip",
                FINISH_LOOKUP_ATTEMPTS_PER_IP,
                NOW,
                FINISH_ATTEMPT_WINDOW_SECS,
            ))
            .unwrap());
        }
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/login/finish",
            serde_json::json!({
                "challenge": challenge_id.clone(),
                "credential_id": b64url_encode(b"nope"),
                "authenticator_data": b64url_encode(b"nope"),
                "client_data_json": b64url_encode(b"nope"),
                "signature": b64url_encode(b"nope"),
            }),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert_eq!(
            *h.store.challenge_reads.borrow(),
            0,
            "budget denial must precede challenge lookup"
        );
        assert!(h.store.challenges.borrow().contains_key(&challenge_id));
    }

    #[test]
    fn vault_test_connection_window_reopens_after_60s_at_handler() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        put_test_vault(&h, &cookie);

        for _ in 0..vault::TEST_CONNECTION_MAX_ATTEMPTS {
            let resp = h.call_full(
                "POST",
                "/v1/vault/test-connection",
                None,
                Some(cookie.clone()),
                Some(ORIGIN),
            );
            assert_eq!(resp.body, r#"{"ok":true}"#);
        }
        assert_eq!(
            h.dialer.seen.borrow().len(),
            vault::TEST_CONNECTION_MAX_ATTEMPTS as usize
        );

        // Budget exhausted: blocked without a dial.
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert_eq!(
            h.dialer.seen.borrow().len(),
            vault::TEST_CONNECTION_MAX_ATTEMPTS as usize
        );

        // 2s later the 60s window is still closed (pins the window length).
        let resp = call_at(
            &h,
            NOW + 2,
            "POST",
            "/v1/vault/test-connection",
            cookie.clone(),
        );
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert_eq!(
            h.dialer.seen.borrow().len(),
            vault::TEST_CONNECTION_MAX_ATTEMPTS as usize
        );

        // After the window the same caller is admitted again and releases.
        let resp = call_at(&h, NOW + 61, "POST", "/v1/vault/test-connection", cookie);
        assert_eq!(resp.body, r#"{"ok":true}"#);
        assert_eq!(
            h.dialer.seen.borrow().len(),
            vault::TEST_CONNECTION_MAX_ATTEMPTS as usize + 1
        );
        assert!(
            h.store.test_connection_leases.borrow().is_empty(),
            "re-admitted probe must release its lease"
        );
    }

    #[test]
    fn vault_test_ip_budget_blocks_other_account_at_handler() {
        // Per-IP gate isolated at the handler, NAT-sized: the per-IP
        // cap sits ABOVE the per-account cap, so account A exhausting
        // its own budget must NOT lock out neighbour B on the same
        // address (first probe still admitted). Only once enough
        // accounts together exhaust the larger per-IP budget is a
        // fresh account denied on its FIRST probe with the usual
        // boolean collapse and no dial. Shrinking the IP cap back to
        // the account cap fails the neighbour probe; removing the
        // handler's admission check fails the final denial.
        let h = harness();
        let account_cap = h.config.rate_limits.vault_test_max_attempts;
        let ip_cap = h.config.rate_limits.vault_test_ip_max_attempts;
        assert!(
            ip_cap > account_cap,
            "per-IP cap must exceed the per-account cap"
        );
        assert_eq!(
            ip_cap % account_cap,
            0,
            "test arithmetic assumes whole account budgets fill the IP budget"
        );
        let fillers = (ip_cap / account_cap) as usize;
        // fillers accounts to exhaust the IP budget, plus one victim.
        let mut cookies = Vec::new();
        for i in 0..=fillers {
            let user_id = format!("u{i}");
            let email = format!("nat{i}@example.com");
            seed_user(&h, &user_id, &email);
            let cookie = magic_login(&h, &email);
            put_test_vault(&h, &cookie);
            cookies.push(cookie);
        }
        let probe = |cookie: &str| {
            h.call_full(
                "POST",
                "/v1/vault/test-connection",
                None,
                Some(cookie.to_string()),
                Some(ORIGIN),
            )
        };

        // A exhausts its own per-account budget...
        for i in 0..account_cap {
            assert_eq!(probe(&cookies[0]).body, r#"{"ok":true}"#, "probe {i} by A");
        }
        // ...and neighbour B's first probe is still admitted: one
        // account alone cannot consume the NAT-shared IP budget.
        assert_eq!(
            probe(&cookies[1]).body,
            r#"{"ok":true}"#,
            "neighbour behind the same IP must not be locked out by A alone"
        );
        // Fill the remaining per-IP slots across the other accounts.
        let mut dialled = usize::try_from(account_cap).unwrap() + 1;
        for (idx, cookie) in cookies.iter().enumerate().take(fillers).skip(1) {
            let probes = if idx == 1 {
                // B already used one of its account slots above.
                account_cap - 1
            } else {
                account_cap
            };
            for i in 0..probes {
                assert_eq!(probe(cookie).body, r#"{"ok":true}"#, "probe {i} by u{idx}");
                dialled += 1;
            }
        }
        assert_eq!(dialled, ip_cap as usize);
        let dials = h.dialer.seen.borrow().len();
        assert_eq!(dials, ip_cap as usize);

        // The victim's per-account budget is untouched, but the shared
        // per-IP budget is exhausted: denied before any dial.
        let resp = probe(&cookies[fillers]);
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert_eq!(
            h.dialer.seen.borrow().len(),
            dials,
            "per-IP denial must happen before any dial"
        );
    }

    #[test]
    fn vault_test_ipv6_slash64_shares_ip_budget_at_handler() {
        // Handler wiring pin for the vault per-IP key: probes spread
        // across two addresses in one IPv6 /64 must share the single
        // `vault.test_ip` budget through the real `vault_test_connection`
        // handler. Each address alone stays well under the per-IP cap,
        // so keying on the raw `source_ip` (instead of
        // `rate_limit_ip_key`) would admit the victim's first probe and
        // fail here — while the IPv4 harness default, where key == raw,
        // keeps every other vault test green. A different /64 keeps its
        // own budget.
        let h = harness();
        let account_cap = h.config.rate_limits.vault_test_max_attempts;
        let ip_cap = h.config.rate_limits.vault_test_ip_max_attempts;
        assert_eq!(
            ip_cap % account_cap,
            0,
            "test arithmetic assumes whole account budgets fill the IP budget"
        );
        let fillers = (ip_cap / account_cap) as usize;
        let ip_a = "2001:db8:abcd:12::1";
        let ip_b = "2001:db8:abcd:12::9";
        let ip_other = "2001:db8:abcd:13::1";
        // fillers accounts to exhaust the shared /64 budget, plus one victim.
        let mut cookies = Vec::new();
        for i in 0..=fillers {
            let user_id = format!("v6u{i}");
            let email = format!("v6nat{i}@example.com");
            seed_user(&h, &user_id, &email);
            let cookie = magic_login(&h, &email);
            put_test_vault(&h, &cookie);
            cookies.push(cookie);
        }
        let probe = |cookie: &str, ip: &str| {
            h.call_full_with_ip(
                ip,
                "POST",
                "/v1/vault/test-connection",
                None,
                Some(cookie.to_string()),
                Some(ORIGIN),
            )
        };

        // Fill the per-IP budget, alternating the two addresses in the
        // /64 so neither raw address ever reaches the cap on its own.
        let mut dialled = 0usize;
        for (idx, cookie) in cookies.iter().enumerate().take(fillers) {
            for i in 0..account_cap {
                let ip = if dialled.is_multiple_of(2) {
                    ip_a
                } else {
                    ip_b
                };
                assert_eq!(
                    probe(cookie, ip).body,
                    r#"{"ok":true}"#,
                    "probe {i} by v6u{idx} from {ip}"
                );
                dialled += 1;
            }
        }
        assert_eq!(dialled, ip_cap as usize);
        let dials = h.dialer.seen.borrow().len();
        assert_eq!(dials, ip_cap as usize);

        // The victim's per-account budget is untouched, but the /64's
        // shared per-IP budget is exhausted: denied before any dial.
        let resp = probe(&cookies[fillers], ip_b);
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert_eq!(
            h.dialer.seen.borrow().len(),
            dials,
            "shared-/64 denial must happen before any dial"
        );

        // A different /64 keeps its own budget: the same victim is
        // admitted from there (the denial above consumed neither budget).
        let resp = probe(&cookies[fillers], ip_other);
        assert_eq!(
            resp.body, r#"{"ok":true}"#,
            "a different /64 must keep its own budget"
        );
        assert_eq!(h.dialer.seen.borrow().len(), dials + 1);
    }

    #[test]
    fn previous_secret_authenticates_only_inside_rotation_window() {
        // Runbook §4: the Worker honors a staged previous secret only as
        // part of a recorded rotation, through the dual path. A pre-cutover
        // token authenticates and refreshes onto the primary; once the
        // previous secret is dropped the old token fails.
        let primary = vec![7u8; 32];
        let previous = vec![9u8; 32];
        let rotation_cfg = Config::new(
            "example.com",
            ORIGIN,
            primary.clone(),
            [9u8; 32],
            1,
            3600,
            false,
            false,
            Some(previous.clone()),
        )
        .unwrap();
        let steady_cfg = Config::new(
            "example.com",
            ORIGIN,
            primary.clone(),
            [9u8; 32],
            1,
            3600,
            false,
            false,
            None,
        )
        .unwrap();
        // A set-but-short previous fails closed at startup, never silently
        // narrowing the window.
        assert!(Config::new(
            "example.com",
            ORIGIN,
            primary.clone(),
            [9u8; 32],
            1,
            3600,
            false,
            false,
            Some(vec![1u8; 16]),
        )
        .is_err());

        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        // Pre-cutover session minted under the previous secret.
        let old_token =
            session::issue_session(&previous, "u1", NOW, 3600).expect("issue under previous");
        let old_claims =
            session::verify_session(&previous, &old_token, NOW).expect("previous verifies");
        block_on(h.store.insert_session(&SessionRecord {
            id: old_claims.session_id.clone(),
            user_id: "u1".to_string(),
            created_at: old_claims.created_at,
            expires_at: old_claims.expires_at,
            rotation_counter: old_claims.rotation_counter,
            revoked: false,
        }))
        .unwrap();
        let cookie = format!("{}={}", session::COOKIE_NAME, old_token);
        let authed = |cfg: &Config| {
            let ctx = Ctx {
                cfg,
                store: &h.store,
                mailer: &h.mailer,
                dialer: &h.dialer,
                dns: &h.dns,
                now: NOW,
                source_ip: "198.51.100.9".to_string(),
            };
            let req = Request {
                method: "GET".to_string(),
                path: "/v1/vault".to_string(),
                cookie: Some(cookie.clone()),
                origin: None,
                body: Vec::new(),
            };
            block_on(handle(&ctx, &req))
        };
        // Inside the window the pre-cutover cookie authenticates ...
        assert_eq!(authed(&rotation_cfg).status, 200);
        // ... refresh migrates it onto the primary signature ...
        let ctx = Ctx {
            cfg: &rotation_cfg,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            now: NOW,
            source_ip: "198.51.100.9".to_string(),
        };
        let req = Request {
            method: "POST".to_string(),
            path: "/v1/session/refresh".to_string(),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            body: Vec::new(),
        };
        let resp = block_on(handle(&ctx, &req));
        assert_eq!(resp.status, 200);
        let migrated = resp.set_cookie.expect("migrated cookie");
        let migrated_token = migrated
            .split(';')
            .next()
            .unwrap()
            .trim_start_matches(&format!("{}=", session::COOKIE_NAME));
        assert!(session::verify_session(&primary, migrated_token, NOW).is_ok());
        // ... and once the previous secret is dropped the old token fails.
        assert_eq!(authed(&steady_cfg).status, 401);
    }

    #[test]
    fn plaintext_opt_in_save_uses_insecure_audit_action() {
        // The cleartext opt-in must be repudiation-safe in the audit log: a
        // save with `allow_insecure_plaintext` records the insecure action,
        // a normal save keeps the plain one.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        put_test_vault(&h, &cookie);
        assert_eq!(
            h.store.audits.borrow().last().unwrap().action,
            "vault.update"
        );
        let put_body = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 119, "tls": false,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4, "allow_insecure_plaintext": true
            },
            "indexer": null
        });
        let resp = h.call_full(
            "PUT",
            "/v1/vault",
            Some(put_body.to_string()),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_eq!(
            h.store.audits.borrow().last().unwrap().action,
            vault::INSECURE_PLAINTEXT_AUDIT_ACTION
        );
    }

    #[test]
    fn invite_redemption_throttles_per_client_past_ten() {
        // `auth.rs` documents the throttle as a MUST for the HTTP slice:
        // the 11th redemption attempt inside the window denies generically,
        // while another account from the same client still proceeds.
        let h = harness();
        let finish_with_code = |user_id: &str, email: &str, cred_id: &[u8], code: &str| {
            // Fresh single-use challenge + inbox proof per attempt, without
            // touching the register/start budget.
            let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
            let challenge_id = challenge.id.clone();
            block_on(
                h.store
                    .insert_challenge(&challenge, Some(user_id), PURPOSE_REGISTER),
            )
            .unwrap();
            let (token, record) = auth::issue_magic_link(user_id, email, NOW);
            block_on(h.store.insert_magic(&record)).unwrap();
            let att = fx_none_attestation(cred_id, &fx_es256_cose(77));
            h.call(
                "POST",
                "/v1/auth/passkey/register/finish",
                serde_json::json!({
                    "challenge": challenge_id,
                    "credential_id": b64url_encode(cred_id),
                    "attestation_object": b64url_encode(&att),
                    "client_data_json": b64url_encode(
                        fx_client_json("webauthn.create", &challenge_id).as_bytes()
                    ),
                    "magic_token": token,
                    "invite_code": code,
                }),
            )
        };

        seed_user(&h, "u1", "a@example.com");
        let good_hash = auth::hash_invite_code("correct-code-1");
        h.store.invites.borrow_mut().insert(
            good_hash.clone(),
            InviteRecord {
                code_hash: good_hash.clone(),
                used: false,
                created_at: NOW,
            },
        );
        // Ten wrong guesses: admitted by the throttle, denied on the code.
        for _ in 0..auth::INVITE_REDEMPTION_MAX_ATTEMPTS {
            let resp = finish_with_code("u1", "a@example.com", b"throttle-cred", "wrong-code");
            assert_eq!(resp.status, 401);
            assert!(resp.set_cookie.is_none());
        }
        // 11th attempt carries the CORRECT code but the budget is spent.
        let resp = finish_with_code("u1", "a@example.com", b"throttle-cred", "correct-code-1");
        assert_eq!(resp.status, 401);
        assert!(resp.set_cookie.is_none());
        assert!(
            block_on(h.store.credentials_for_user("u1"))
                .unwrap()
                .is_empty(),
            "throttled attempt must not enroll a credential"
        );
        assert!(
            !h.store.invites.borrow().get(&good_hash).unwrap().used,
            "throttled attempt must not consume the invite"
        );
        // Same client, another account: its own budget still admits.
        seed_user(&h, "u2", "b@example.com");
        let good_hash_2 = auth::hash_invite_code("correct-code-2");
        h.store.invites.borrow_mut().insert(
            good_hash_2.clone(),
            InviteRecord {
                code_hash: good_hash_2,
                used: false,
                created_at: NOW,
            },
        );
        let resp = finish_with_code("u2", "b@example.com", b"throttle-cred-2", "correct-code-2");
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some());
    }

    #[test]
    fn register_finish_attempt_budget_cuts_off_forged_attestations() {
        // One challenge id funds at most 10 finish attempts: 10 forged
        // attestations exhaust it, and the 11th attempt with a VALID
        // attestation + inbox proof still fails generically without
        // enrolling. A fresh challenge is unaffected (proves the budget is
        // per-challenge, and legitimate single attempts still pass).
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        let challenge_id = challenge.id.clone();
        block_on(
            h.store
                .insert_challenge(&challenge, Some("u1"), PURPOSE_REGISTER),
        )
        .unwrap();
        let (token, record) = auth::issue_magic_link("u1", "a@example.com", NOW);
        block_on(h.store.insert_magic(&record)).unwrap();
        let cred_id = b"budget-cred";
        let att = fx_none_attestation(cred_id, &fx_es256_cose(61));
        let client_json = fx_client_json("webauthn.create", &challenge_id);
        let forged = serde_json::json!({
            "challenge": challenge_id.clone(),
            "credential_id": b64url_encode(cred_id),
            "attestation_object": b64url_encode(b"forged-attestation"),
            "client_data_json": b64url_encode(client_json.as_bytes()),
            "magic_token": token.clone(),
            "invite_code": null,
        });
        for _ in 0..REGISTER_FINISH_ATTEMPTS_PER_CHALLENGE {
            let resp = h.call("POST", "/v1/auth/passkey/register/finish", forged.clone());
            assert_eq!(resp.status, 401);
            assert!(resp.set_cookie.is_none());
        }
        // Valid assertion on the spent challenge is now cut off by budget.
        let valid = serde_json::json!({
            "challenge": challenge_id.clone(),
            "credential_id": b64url_encode(cred_id),
            "attestation_object": b64url_encode(&att),
            "client_data_json": b64url_encode(client_json.as_bytes()),
            "magic_token": token.clone(),
            "invite_code": null,
        });
        let resp = h.call("POST", "/v1/auth/passkey/register/finish", valid);
        assert_eq!(resp.status, 401);
        assert!(resp.set_cookie.is_none());
        assert!(
            block_on(h.store.credentials_for_user("u1"))
                .unwrap()
                .is_empty(),
            "budget-cutoff attempt must not enroll"
        );
        // A fresh challenge for the same account still admits one attempt
        // shape (fails only on the forged bytes, not the budget).
        let challenge2 = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        let challenge_id2 = challenge2.id.clone();
        block_on(
            h.store
                .insert_challenge(&challenge2, Some("u1"), PURPOSE_REGISTER),
        )
        .unwrap();
        let (token2, record2) = auth::issue_magic_link("u1", "a@example.com", NOW);
        block_on(h.store.insert_magic(&record2)).unwrap();
        let client_json2 = fx_client_json("webauthn.create", &challenge_id2);
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge_id2,
                "credential_id": b64url_encode(cred_id),
                "attestation_object": b64url_encode(b"forged-attestation"),
                "client_data_json": b64url_encode(client_json2.as_bytes()),
                "magic_token": token2,
                "invite_code": null,
            }),
        );
        assert_eq!(resp.status, 401);
    }

    #[test]
    fn login_finish_attempt_budget_cuts_off_forged_assertions() {
        // Same per-challenge budget on the login finish: 10 forged
        // assertions exhaust the challenge, and the 11th valid assertion
        // still fails without a session.
        use p256::ecdsa::{signature::Signer, Signature as EsSignature, SigningKey};
        use sha2::Digest;
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let sk = SigningKey::from_slice(&[62u8; 32]).unwrap();
        let point = sk.verifying_key().to_sec1_point(false);
        let mut cose = Vec::new();
        fx_head(5, 5, &mut cose);
        fx_int(1, &mut cose);
        fx_int(2, &mut cose);
        fx_int(3, &mut cose);
        fx_int(-7, &mut cose);
        fx_int(-1, &mut cose);
        fx_int(1, &mut cose);
        fx_int(-2, &mut cose);
        fx_bytes(point.x().unwrap(), &mut cose);
        fx_int(-3, &mut cose);
        fx_bytes(point.y().unwrap(), &mut cose);
        let cred_id = b"budget-login-cred";
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(cred_id),
            user_id: "u1".to_string(),
            public_key: cose,
            sign_count: 1,
            registered_at: NOW,
        }))
        .unwrap();
        let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
        let challenge_id = challenge.id.clone();
        block_on(
            h.store
                .insert_challenge(&challenge, Some("u1"), PURPOSE_LOGIN),
        )
        .unwrap();
        let forged = serde_json::json!({
            "challenge": challenge_id.clone(),
            "credential_id": b64url_encode(cred_id),
            "authenticator_data": b64url_encode(b"forged-auth"),
            "client_data_json": b64url_encode(b"forged-client"),
            "signature": b64url_encode(b"forged-sig"),
        });
        for _ in 0..LOGIN_FINISH_ATTEMPTS_PER_CHALLENGE {
            let resp = h.call("POST", "/v1/auth/passkey/login/finish", forged.clone());
            assert_eq!(resp.status, 401);
            assert!(resp.set_cookie.is_none());
        }
        // Build the valid assertion (sign_count 2 > stored 1) — still cut off.
        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&sha2::Sha256::digest(b"example.com"));
        auth_data.push(0x01);
        auth_data.extend_from_slice(&2u32.to_be_bytes());
        let cd_get = format!(
            r#"{{"type":"webauthn.get","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
            challenge_id, ORIGIN
        );
        let mut msg = auth_data.clone();
        msg.extend_from_slice(&sha2::Sha256::digest(cd_get.as_bytes()));
        let sig: EsSignature = sk.sign(&msg);
        let valid = serde_json::json!({
            "challenge": challenge_id,
            "credential_id": b64url_encode(cred_id),
            "authenticator_data": b64url_encode(&auth_data),
            "client_data_json": b64url_encode(cd_get.as_bytes()),
            "signature": b64url_encode(sig.to_der().as_bytes()),
        });
        let resp = h.call("POST", "/v1/auth/passkey/login/finish", valid);
        assert_eq!(resp.status, 401);
        assert!(resp.set_cookie.is_none());
    }

    #[test]
    fn magic_finish_attempt_budget_cuts_off_per_ip() {
        // Rotating tokens must share one per-client budget: 10 bogus
        // finishes exhaust the IP, and the valid token is then cut off
        // without a session. All calls carry the exact Origin (required by
        // the login-CSRF pin) so rejections prove the budget, not the pin.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let (token, record) = auth::issue_magic_link("u1", "a@example.com", NOW);
        block_on(h.store.insert_magic(&record)).unwrap();
        for i in 0..MAGIC_FINISH_ATTEMPTS_PER_IP {
            let resp = h.call(
                "POST",
                "/v1/auth/magic/finish",
                serde_json::json!({"token": format!("bogus-token-{i}")}),
            );
            assert_eq!(resp.status, 401, "{i}");
        }
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(resp.status, 401);
        assert!(resp.set_cookie.is_none());
    }

    #[test]
    fn options_preflight_only_for_known_routes() {
        let h = harness();
        // Every served path answers the preflight with an empty 204 (the
        // entry adds credentialed CORS headers on exact-origin match).
        for path in [
            "/healthz",
            "/v1/auth/passkey/register/start",
            "/v1/auth/passkey/register/finish",
            "/v1/auth/passkey/login/start",
            "/v1/auth/passkey/login/finish",
            "/v1/auth/magic/start",
            "/v1/auth/magic/finish",
            "/v1/session/refresh",
            "/v1/session/logout",
            "/v1/session/revoke-all",
            "/v1/vault",
            "/v1/vault/test-connection",
        ] {
            assert!(is_known_path(path), "{path}");
            let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 204, "{path}");
            assert!(resp.body.is_empty(), "{path}");
            assert!(resp.set_cookie.is_none(), "{path}");
        }
        // Unknown paths still 404 on preflight: no surface mapping.
        assert!(!is_known_path("/v1/auth/magic-link"));
        let resp = h.call_full("OPTIONS", "/v1/auth/magic-link", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404);
        let resp = h.call_full("OPTIONS", "/nope", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn test_connection_accepts_candidate_envelope_without_stored_vault() {
        // Test-before-save: an authenticated caller can dial a candidate
        // config the server has never stored, under the same budget, lease
        // and dial guards, without creating a vault row.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let candidate = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 563, "tls": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        });
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            Some(candidate.to_string()),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":true}"#);
        assert_eq!(
            h.dialer.seen.borrow().as_slice(),
            [vec!["93.184.216.34".parse::<IpAddr>().unwrap()]]
        );
        assert!(
            h.store.vaults.borrow().is_empty(),
            "candidate test must not persist a vault row"
        );
    }

    #[test]
    fn test_connection_rejects_unparsable_body_closed() {
        // A non-empty body that is not the save envelope fails closed
        // instead of silently testing something else.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            Some(r#"{"provider": {"host": 7}}"#.to_string()),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 400);
        assert!(h.dialer.seen.borrow().is_empty());
    }

    #[test]
    fn test_connection_candidate_with_forbidden_host_fails_boolean() {
        // A candidate pointing at a forbidden address collapses to the
        // same boolean as a bad stored vault (never a banner or verdict).
        let mut h = harness();
        h.dns.map.insert(
            "blocked.example.com".to_string(),
            vec!["10.0.0.5".parse().unwrap()],
        );
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let candidate = serde_json::json!({
            "provider": {
                "host": "blocked.example.com", "port": 119, "tls": false,
                "allow_insecure_plaintext": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        });
        let resp = h.call_full(
            "POST",
            "/v1/vault/test-connection",
            Some(candidate.to_string()),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.body, r#"{"ok":false}"#);
        assert!(h.dialer.seen.borrow().is_empty());
    }

    #[test]
    fn api_web_contract_fixture_routes_are_served() {
        // The canonical web/Worker contract fixture: every route the web
        // client may call must be served (never 404), with or without a
        // session. Unauthenticated authed routes answer 401/400, unknown
        // challenges 401 — all non-404 by construction.
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/api-web-contract.json"))
                .expect("contract fixture parses");
        let routes = fixture["routes"].as_array().expect("routes array");
        assert!(!routes.is_empty());
        let h = harness();
        for route in routes {
            let method = route["method"].as_str().expect("method");
            let path = route["path"].as_str().expect("path");
            assert!(is_known_path(path), "{method} {path}");
            let body = route.get("wire").map(|v| v.to_string());
            let resp = h.call_full(method, path, body, None, Some(ORIGIN));
            assert_ne!(resp.status, 404, "{method} {path}");
        }
    }

    #[test]
    fn api_web_contract_fixture_bodies_parse_as_handler_bodies() {
        // Route-served is not enough: every fixture wire body must
        // deserialize into the exact `deny_unknown_fields` body struct its
        // handler parses, so a renamed wire key (in the fixture, the web
        // builders, or the structs) fails here instead of in production.
        // A wire-bearing route id with no mapping below also fails, so a
        // new body-bearing route cannot be added unpinned.
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/api-web-contract.json"))
                .expect("contract fixture parses");
        let routes = fixture["routes"].as_array().expect("routes array");
        let mut pinned = 0;
        for route in routes {
            let Some(wire) = route.get("wire") else {
                continue;
            };
            let id = route["id"].as_str().expect("route id");
            let parsed: Result<(), String> = (match id {
                "register-start" | "login-start" | "magic-start" => {
                    serde_json::from_value::<EmailBody>(wire.clone()).map(|_| ())
                }
                "register-finish" | "register-finish-invite" => {
                    serde_json::from_value::<RegisterFinishBody>(wire.clone()).map(|_| ())
                }
                "login-finish" | "session-reauthenticate-finish" => {
                    serde_json::from_value::<LoginFinishBody>(wire.clone()).map(|_| ())
                }
                "magic-finish" => {
                    serde_json::from_value::<MagicFinishBody>(wire.clone()).map(|_| ())
                }
                "vault-save" | "vault-test" => {
                    serde_json::from_value::<VaultPutBody>(wire.clone()).map(|_| ())
                }
                other => panic!("no handler body mapping for route {other}"),
            })
            .map_err(|e| e.to_string());
            assert!(parsed.is_ok(), "route {id}: {parsed:?}");
            // Every body-bearing route must also be pinned from the web
            // side (a `web` builder entry the vitest suite drives).
            assert!(
                route.get("web").is_some(),
                "route {id} has a wire body but no web builder entry"
            );
            pinned += 1;
        }
        assert_eq!(
            pinned, 10,
            "every body-bearing route (incl. the invite variant) is pinned"
        );
    }
}
