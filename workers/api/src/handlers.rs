//! Framework-agnostic HTTP handlers for the accounts/auth API.
//!
//! The Worker entry (`worker_entry`) adapts `worker::Request` into
//! [`Request`] and renders [`Response`]; all security decisions live here
//! and are native-tested against fake stores.
//!
//! Contracts enforced here (threat model + CISO conditions):
//! - One generic error body per surface: auth failures are always
//!   `401 {"error":"check your email and try again"}`, store failures
//!   `500 {"error":"internal error"}`. No credential, host, banner or
//!   existence detail ever reaches a body.
//! - Every authenticated request goes through
//!   [`session::verify_stored_session`] against the current server row
//!   (revoked/missing/stale-counter/mismatched rows reject), and cookie-
//!   authenticated mutations require an exact `Origin` match (CSRF).
//!   The only stale-cookie operation is opt-in refresh recovery: it requires
//!   a separate client secret and an atomic current-row claim, and can only
//!   reissue the immediate successor, never authorize an application action.
//!   `POST /v1/auth/magic/finish` mints a cookie from a pure bearer token,
//!   so it requires the same exact `Origin` match even when unauthenticated
//!   (login-CSRF session planting): browsers always send `Origin` on this
//!   fetch POST, and API-only clients must send `Origin: <configured origin>`.
//! - Magic links, challenges and invites are consumed only through the
//!   `*_atomically` helpers (conditional UPDATE/RETURNING, exactly one
//!   row, committed before any response). Session refresh advances the
//!   server row with a conditional counter update before `Set-Cookie`.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::auth::{
    self, InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential, SingleUseStore,
};
// Single unpadded-base64url codec (F-HARD-01): `crate::b64` over the
// `base64` crate. The aliased names keep wire-format call sites unchanged.
use crate::admin_ingest::{
    self, AdminIngestAttempt, AdminIngestDeleteOutcome, AdminIngestUploadOutcome,
    StoredAdminIngestJob,
};
use crate::b64::{decode as b64url_decode, encode as b64url_encode};
use crate::session::{self, SessionClaims, SessionRecord};
use crate::vault::{self, AuditEvent, ValidatedTarget};
use crate::webauthn;

/// Request bodies above this size reject before routing (not an oracle:
/// size only, same for every route).
pub const MAX_BODY_BYTES: usize = 32 * 1024;
/// Intake-route body cap: `POST /api/ingest` carries an NZB document, not
/// API JSON, so it gets its own bound. The value parities the NZB intake
/// ceiling (`workers/ingest/src/nzb_intake.rs::MAX_NZB_BYTES`, the threat
/// model's documented 2 MiB direct-XML-upload cap): the guard must accept
/// every document the intake path itself accepts, or a lawful upload
/// would 400 before the cap check runs. Kept as a cited literal rather
/// than a cross-crate dependency (the ingest worker's dependency graph
/// must not ship in this worker); bump both together.
pub const INGEST_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Per-route request-body bound: the NZB intake route and the admin NZB
/// upload route accept document uploads, every other route takes
/// API-sized bodies. Both the entry's collection gate and the pre-routing
/// size check call this, so the two caps cannot drift apart.
pub fn max_body_bytes(method: &str, path: &str) -> usize {
    if method == "POST" && path == "/api/ingest" {
        INGEST_BODY_LIMIT_BYTES
    } else if method == "POST" && path == "/v1/admin/ingest" {
        admin_ingest::ADMIN_INGEST_BODY_LIMIT_BYTES
    } else {
        MAX_BODY_BYTES
    }
}
/// Magic links issuable per email inside the link TTL window.
pub const MAGIC_LINKS_PER_WINDOW: u64 = 3;
/// Admitted inline auth-mail send budget (client-visible auth-start contract:
/// registration and known-user magic-start race the mail send against this
/// deadline, then follow the ordinary ignored delivery-error path with the
/// unchanged generic envelope). Drift must be a deliberate slice with README
/// and caller updates, not a silent edit.
pub const AUTH_MAIL_SEND_DEADLINE_SECS: u64 = 5;
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
/// NAT bursts (the Pg slot reservation caps at 3000, well above this pool).
const REGISTER_FINISH_ATTEMPTS_PER_CHALLENGE: u64 = 10;
const LOGIN_FINISH_ATTEMPTS_PER_CHALLENGE: u64 = 10;
const MAGIC_FINISH_ATTEMPTS_PER_IP: u64 = 10;
const FINISH_LOOKUP_ATTEMPTS_PER_IP: u64 = 100;
const FINISH_ATTEMPT_WINDOW_SECS: u64 = crate::auth::CHALLENGE_TTL_SECS;
/// Bearer-lookup budgets (ADR 0010 DoS): a well-formed random `ntk_` token
/// still costs an HMAC plus up to two Postgres lookups (one per pepper),
/// so bearer attempts share one per-client pool keyed on the client /64
/// (IPv6) or full address (IPv4) BEFORE any credential DB work — the same
/// two-layer shape as the finish-route lookup budget above. Denial answers
/// the same plain 404 as any other bearer failure (no throttle oracle).
/// 300 per minute admits legitimate API polling while capping a single
/// client's DB amplification; 3000 per hour bounds sustained floods while
/// admitting NAT-shared bursts. Both slots are claimed as one all-or-none
/// pair, so a burst-denied attempt burns neither budget. Per-credential
/// rate limits and plan entitlements (ADR 0010) follow with the data
/// routes; this pool only bounds unauthenticated lookup work.
pub(crate) const BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_MIN: u64 = 300;
pub(crate) const BEARER_LOOKUP_WINDOW_SECS: u64 = 60;
pub(crate) const BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_HOUR: u64 = 3000;
pub(crate) const BEARER_LOOKUP_HOUR_WINDOW_SECS: u64 = 3600;
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

/// Canonical fleet-wide reap eligibility: shared by the candidate lock and
/// fresh-snapshot delete in `PgStore::reap_unverified_users` (called from
/// `worker_entry`) and the statement-shape test below. The purpose literal must stay
/// `PURPOSE_REGISTER` (`'register'`): reaping on `'login'` would delete
/// never-completed signups on the wrong ceremony. Exemptions (credentials,
/// unexpired register challenges, vaults, sessions, unexpired magic links)
/// must stay in sync with the `FakeStore` mirror. Accounts with a
/// scheduled deletion (`deleting_at IS NOT NULL`) are also exempt: they
/// are owned by the deletion executor, and their `delete_operations`
/// row's `ON DELETE RESTRICT` FK would otherwise abort the whole reap
/// (`FakeStore` mirrors this through `deleting_users`). Admin accounts
/// are exempt too: a promoted stub (the operator bootstrap promotes an
/// existing account, which may hold no credential, session, vault or
/// live proof yet) is never unverified-signup garbage, and reaping it
/// would silently delete an admin (`FakeStore` mirrors this through
/// `user_roles`).
fn unverified_user_filter() -> String {
    format!(
        "created_at <= $1 \
         AND deleting_at IS NULL \
         AND users.role = '{member}' \
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
                           AND magic_link_tokens.expires_at > $3)",
        member = crate::admin::ROLE_MEMBER
    )
}

/// Lock at most 128 eligible parents without waiting on enrollment's lock.
pub fn lock_unverified_users_sql() -> String {
    format!(
        "SELECT users.id FROM users WHERE {} LIMIT 128 FOR UPDATE SKIP LOCKED",
        unverified_user_filter()
    )
}

/// Recheck eligibility on a fresh snapshot, restricted to the locked parents.
pub fn reap_unverified_users_sql() -> String {
    format!(
        "DELETE FROM users WHERE {} AND users.id = ANY($4)",
        unverified_user_filter()
    )
}

/// Canonical rate-limit exhausted fast-fail statement shape: the single
/// source of truth for the `PgStore::reserve_rate_limit` O(1) guard
/// (called from `worker_entry`) and the statement-shape test below. The
/// production guard must stay a single `SELECT COUNT(*)` over the live
/// window; without it every denied request pays one DELETE plus a
/// full-pool anti-join claim that scans `limit` rows and finds nothing.
/// The `FakeStore` mirror exercises the same exhausted-decision behavior
/// and counts slot probes.
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
    /// Retained prior KEKs for mixed-version reads: `(version, key)` pairs
    /// from the `VAULT_KEK_V<N>` per-version secrets. Order is irrelevant —
    /// lookup is by exact version, and the current key always wins over a
    /// retained entry for the same version (see [`Config::kek_by_version`]).
    /// Empty in legacy single-key mode, where [`Config::new`] leaves it
    /// untouched. Provisioned only by [`Config::with_prior_keks`]; the
    /// Worker entry builds it from env, never from request input.
    pub prior_keks: Vec<(u32, [u8; 32])>,
    pub session_ttl_secs: u64,
    pub invites_required: bool,
    /// Verify-only previous session HMAC secret: `Some` only inside a
    /// recorded rotation window, `None` in steady state. Never mints;
    /// length-checked like the primary (a short previous fails closed).
    pub session_previous_secret: Option<Vec<u8>>,
    /// Dedicated native-REST bearer keyring (ADR 0010): the HMAC pepper
    /// bytes plus their version. `None` until the operator provisions
    /// `NATIVE_REST_PEPPER_BASE64` + `NATIVE_REST_PEPPER_VERSION` (see the
    /// Worker entry and `wrangler.toml.example`). Unprovisioned, issuance
    /// endpoints fail closed per-request (generic 500) and bearer routes
    /// fail closed (plain 404, like header-absent callers); every other
    /// route keeps serving, so
    /// provisioning the keyring is never a flag-day.
    pub native_rest_pepper: Option<Vec<u8>>,
    /// Version bound to [`Config::native_rest_pepper`]; stored per row and
    /// looked up by exact version (composite `UNIQUE (pepper_version,
    /// key_hmac)`, migration 0019).
    pub native_rest_pepper_version: u32,
    /// Dedicated compatibility keyring, loaded only from the Worker secret.
    /// Absent or invalid configuration disables issuance, not native routes.
    pub subsonic_index: Option<crate::subsonic_config::SubsonicIndexConfig>,
    /// Stripe bearer secret (`STRIPE_SECRET_KEY` Worker secret), test mode
    /// only. `None` until the operator provisions it (see the Worker entry
    /// and `wrangler.toml.example`); unprovisioned, every Stripe-touching
    /// path fails closed per-request (generic 500, so Stripe retries the
    /// webhook) while every other route keeps serving. Never hardcoded.
    pub stripe_secret_key: Option<String>,
    /// Stripe webhook signing secret (`STRIPE_WEBHOOK_SECRET` Worker
    /// secret). `None` until provisioned; unprovisioned, the webhook route
    /// fails closed per-request (generic 500) before signature verification
    /// — an unverifiable webhook is never parsed, never acked.
    pub stripe_webhook_secret: Option<String>,
    /// Play-grant HMAC secret shared with the stream Worker
    /// (`PLAY_GRANT_SECRET`, [`crate::play_grant`]): the RAW string
    /// bytes, never base64-decoded — the stream side keys its verifier
    /// with the secret string's bytes, so both sides must agree byte
    /// for byte. `None` until the operator provisions it; unprovisioned
    /// (or under the 32-byte minimum, enforced at mint time) the play
    /// routes fail closed per-request with a generic 500 while every
    /// other route keeps serving.
    pub play_grant_secret: Option<Vec<u8>>,
    /// Meter-sink HMAC secret shared with the stream Worker
    /// (`METER_SINK_SECRET`, [`crate::meter_sink`]): the RAW string
    /// bytes, never base64-decoded, for the same reason as the
    /// play-grant secret. `None` until provisioned; unprovisioned
    /// (or under the 32-byte minimum) the internal sink route fails
    /// closed per-request with a generic 500 while every other
    /// route keeps serving.
    pub meter_sink_secret: Option<Vec<u8>>,
    /// Rate-limit tunables for the auth-start burst and vault
    /// test-connection budgets. `Config::new` uses the documented
    /// defaults; the Worker entry overlays [vars] via
    /// `RateLimitConfig::from_vars` + [`Config::with_rate_limits`].
    pub rate_limits: crate::rate_limit::RateLimitConfig,
    /// Staging-only emailed-code retrieval seam (`POST /api/test-codes`):
    /// false unless the operator explicitly sets the staging flag. The
    /// constructor keeps it off so existing callers — and production
    /// builds, which never set the flag — serve the seam path as a plain
    /// 404. The Worker entry enables it via
    /// [`Config::with_qa_code_seam`] + [`Config::with_qa_auth_token`].
    pub qa_code_seam: bool,
    /// Bearer expected on the seam route (`QA_AUTH_TOKEN` Worker secret).
    /// `None` (the constructor default) fails the seam closed even when
    /// the flag is on: with no expected value there is nothing to compare
    /// against, so every seam request 404s.
    pub qa_auth_token: Option<String>,
    /// Deployed build identity reported by `GET /healthz`: a 40-char
    /// lowercase hex commit or the explicit `"unknown"` marker. The
    /// constructor defaults to `"unknown"`; the Worker entry stages the
    /// public `BUILD_SHA` var via [`Config::with_build_sha`], which
    /// normalizes through the shared validator so nothing else can leak.
    pub build_sha: String,
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
            prior_keks: Vec::new(),
            session_ttl_secs: session_ttl_secs.min(session::MAX_SESSION_SECS),
            invites_required,
            session_previous_secret,
            native_rest_pepper: None,
            native_rest_pepper_version: 0,
            subsonic_index: None,
            stripe_secret_key: None,
            stripe_webhook_secret: None,
            play_grant_secret: None,
            meter_sink_secret: None,
            rate_limits: crate::rate_limit::RateLimitConfig::default(),
            qa_code_seam: false,
            qa_auth_token: None,
            build_sha: crate::logging::BUILD_SHA_UNKNOWN.to_string(),
        })
    }

    /// Provision retained prior KEKs for mixed-version reads during a
    /// rotation window (`docs/security/kek-rotation.md` §3): each
    /// `(version, key)` pair comes from one `VAULT_KEK_V<N>` per-version
    /// secret. The constructor leaves the set empty (legacy single-key
    /// mode), so existing callers need no change; the Worker entry calls
    /// this only after strict validation (unpadded-base64url decode plus
    /// the 32-byte length, version parses as `u32`), like the native
    /// keyring setter below. A retained entry for the current version
    /// can never shadow the write key: [`Config::kek_by_version`]
    /// checks the current key first.
    pub fn with_prior_keks(mut self, prior_keks: Vec<(u32, [u8; 32])>) -> Self {
        self.prior_keks = prior_keks;
        self
    }

    /// Version-pinned KEK selector: exactly one candidate for `version` —
    /// the current key when `version` is the configured write version,
    /// else the retained prior for that version, else `None`. There is no
    /// fallback loop and no fallthrough to another version's key: callers
    /// map `None` (and any downstream AEAD failure) to the same generic
    /// vault error, so an unknown version is indistinguishable from a bad
    /// tag or a missing row. The current key wins over a same-version
    /// retained entry by construction (checked first), so a stale prior
    /// can neither shadow writes nor resurrect retired reads.
    pub fn kek_by_version(&self, version: u32) -> Option<&[u8; 32]> {
        if version == self.kek_version {
            return Some(&self.kek);
        }
        self.prior_keks
            .iter()
            .find(|(v, _)| *v == version)
            .map(|(_, key)| key)
    }

    /// Overlay operator-configured rate limits (from [vars]); the
    /// constructor keeps the documented defaults so existing callers
    /// and tests need no change.
    pub fn with_rate_limits(mut self, rate_limits: crate::rate_limit::RateLimitConfig) -> Self {
        self.rate_limits = rate_limits;
        self
    }

    /// Provision the native-REST bearer keyring. The Worker entry calls
    /// this only after strict validation (unpadded-base64url decode plus
    /// the 32-byte minimum, version parses as `u32`); a half-configured
    /// keyring refuses to start there, so this setter never sees bad
    /// input in production. Native tests pass synthetic peppers directly.
    pub fn with_native_rest_pepper(mut self, pepper: Vec<u8>, version: u32) -> Self {
        self.native_rest_pepper = Some(pepper);
        self.native_rest_pepper_version = version;
        self
    }

    /// Provision the Stripe secrets. The Worker entry calls this only
    /// after strict validation (the bearer must be a test-mode key —
    /// `sk_live_` refuses the whole startup — and neither value may be
    /// blank); unprovisioned (`None`) paths fail closed per-request, so
    /// this setter never sees bad input in production. Native tests stage
    /// synthetic secrets directly.
    pub fn with_stripe(
        mut self,
        secret_key: Option<String>,
        webhook_secret: Option<String>,
    ) -> Self {
        self.stripe_secret_key = secret_key;
        self.stripe_webhook_secret = webhook_secret;
        self
    }

    /// Provision the play-grant HMAC secret (raw bytes of the
    /// `PLAY_GRANT_SECRET` Worker secret). The 32-byte minimum is
    /// enforced by the minter per request (fail closed), so this
    /// setter accepts whatever the entry staged; native tests pass
    /// synthetic secrets directly.
    pub fn with_play_grant_secret(mut self, secret: Vec<u8>) -> Self {
        self.play_grant_secret = Some(secret);
        self
    }

    /// Provision the meter-sink HMAC secret (raw bytes of the
    /// `METER_SINK_SECRET` Worker secret). The 32-byte minimum is
    /// enforced by [`crate::meter_sink::verify`] per request (fail
    /// closed), so this setter accepts whatever the entry staged.
    pub fn with_meter_sink_secret(mut self, secret: Vec<u8>) -> Self {
        self.meter_sink_secret = Some(secret);
        self
    }

    /// Authorizing pepper set for bearer verification: exactly the
    /// configured version, or empty when unprovisioned (empty fails
    /// closed inside `authorize`). A rotation adds the previous entry
    /// here without touching the handlers: dual-version lookup already
    /// tries every listed version.
    pub fn native_rest_peppers(&self) -> Vec<crate::native_rest::Pepper<'_>> {
        self.native_rest_pepper
            .as_deref()
            .map(|bytes| crate::native_rest::Pepper {
                version: self.native_rest_pepper_version,
                bytes,
            })
            .into_iter()
            .collect()
    }

    /// Arm the staging-only emailed-code retrieval seam. The constructor
    /// keeps it off so production builds serve the seam path as a plain
    /// 404; only an explicit staging flag enables capture + retrieval.
    pub fn with_qa_code_seam(mut self, enabled: bool) -> Self {
        self.qa_code_seam = enabled;
        self
    }

    /// Stage the bearer the seam route expects (`QA_AUTH_TOKEN`). Without
    /// it the seam stays fail-closed even when armed: there is no expected
    /// value to compare against, so every seam request 404s.
    pub fn with_qa_auth_token(mut self, token: &str) -> Self {
        self.qa_auth_token = Some(token.to_string());
        self
    }

    /// Stage the deployed build identity for `GET /healthz`. Anything
    /// that is not a 40-char lowercase hex commit becomes `"unknown"`, so
    /// the health body never echoes an arbitrary var value.
    pub fn with_build_sha(mut self, raw: Option<&str>) -> Self {
        self.build_sha = crate::logging::parse_build_sha(raw);
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

/// Outcome of one admin write transaction (migration 0028). The
/// transaction serializes every admin write and re-reads the actor's
/// role under its locks, so a write authorized by the guard can still
/// lose the race to a demotion that committed first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminWriteOutcome {
    /// The write and its audit row committed; carries the value the
    /// write replaced (`None` = no prior stored value).
    Applied(Option<String>),
    /// The subject user does not exist (role writes only); nothing was
    /// written or audited.
    SubjectMissing,
    /// The actor was no longer an admin when the write transaction
    /// re-read their role; nothing was written or audited. The handler
    /// answers the plain 404, as for any non-admin caller.
    ActorNotAdmin,
    /// The subject's account is scheduled for deletion (`deleting_at`
    /// set; role writes only). ADR 0002 rule 3 fences every writer
    /// from that moment, so nothing was written or audited; the
    /// handler answers the typed `account_deleting` 409.
    AccountDeleting,
}

/// Outcome of an admin invite issue (migration 0033): the insert and
/// its `invite.issued` audit row commit together under the same
/// admin-write serialization as [`AdminWriteOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminInviteIssueOutcome {
    /// Issued; carries the surrogate row id the audit row references.
    Issued(i64),
    /// The actor was no longer an admin at write time; nothing was
    /// written or audited. The handler answers the plain 404.
    ActorNotAdmin,
}

/// Outcome of an admin invite revoke (migration 0033). The revocation
/// stamps and its `invite.revoked` audit row commit together, and the
/// conditional write makes revocation win any race against a
/// redemption claim executing concurrently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminInviteRevokeOutcome {
    /// This call revoked the invite.
    Revoked,
    /// The invite was already revoked; nothing changed and no second
    /// audit row was written (idempotent retry).
    AlreadyRevoked,
    /// The invite was already redeemed; revocation never reaches back
    /// into a created account.
    AlreadyRedeemed,
    /// No invite with that id exists.
    Missing,
    /// The actor was no longer an admin at write time; nothing was
    /// written or audited. The handler answers the plain 404.
    ActorNotAdmin,
}

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

/// One `plans` row (migration 0021 as reseeded by 0026). Plan prices and
/// quotas are data: the `GET /v1/plans` handler renders these rows, never
/// a second hard-coded copy (the `PlanId` enum stays the parity mirror).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    pub id: String,
    pub price_cents: u64,
    pub stream_hours_month: u64,
    pub max_concurrent_streams: u64,
}

/// Owned live-slot subscription row for the billing display reads; feeds
/// [`crate::billing::SubscriptionView`] by reference. Timestamps are Unix
/// seconds, `None` where the DB column is NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRow {
    pub plan_id: String,
    pub status: String,
    pub cancel_at_period_end: bool,
    pub paid_through_at_unix: Option<i64>,
    pub grace_until_unix: Option<i64>,
    pub current_period_end_unix: Option<i64>,
}

/// Stored native REST credential (ADR 0010, migration 0019): the core
/// [`crate::native_rest::NativeRestRecord`] (HMAC-only, never the secret)
/// plus HTTP-layer metadata. `Clone`; manual `Debug` reuses the record's
/// redacted form and names the label, but never the client IP or any key
/// material — diagnostics name names, never values or addresses.
#[derive(Clone)]
pub struct NativeCredentialRow {
    pub record: crate::native_rest::NativeRestRecord,
    pub label: String,
    pub created_from_ip: String,
    pub last_used_at: Option<u64>,
}

impl std::fmt::Debug for NativeCredentialRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCredentialRow")
            .field("record", &self.record)
            .field("label", &self.label)
            .field("last_used_at", &self.last_used_at)
            .finish()
    }
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
    /// Atomically claim all registration proofs and insert the credential.
    /// Every claim must return exactly one row using `SingleUseClaim` SQL.
    /// Rejection, storage failure, or cancellation must never leave partial
    /// writes. Return success only after commit; no sequential fallback.
    ///
    /// When an invite is claimed, `invite_audit` is its `invite.redeemed`
    /// row and MUST commit in the same transaction (an audit failure
    /// leaves the invite unspent and the signup retryable); the store
    /// also stamps `users.invited_by` / `users.invite_code_hash` from
    /// the claimed invite in that transaction. `invite` and
    /// `invite_audit` are always both present or both absent.
    fn complete_registration(
        &self,
        challenge: &PasskeyChallenge,
        magic: &MagicLinkRecord,
        invite: Option<&InviteRecord>,
        invite_audit: Option<&AuditEvent>,
        credential: &PasskeyCredential,
        now: u64,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async move {
            // Stores without a transactional implementation must refuse
            // enrollment, never fall back to separately committed claims.
            let _ = (challenge, magic, invite, invite_audit, credential, now);
            Err(StoreError)
        }
    }
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
    /// Staging-only emailed-code outbox (migration 0018): remember the raw
    /// issued token for a synthetic mailbox, replacing any earlier row for
    /// it. Default no-op keeps out-of-tree `Store` impls compiling;
    /// production and the test fake override it. Called only when the seam
    /// flag is armed and the recipient is an `example.test` mailbox, and
    /// only after the magic row itself was stored — capture can never
    /// break issuance (callers ignore its result).
    fn record_qa_issued_code(
        &self,
        email: &str,
        token: &str,
        now: u64,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async move {
            let _ = (email, token, now);
            Ok(())
        }
    }
    /// Latest captured token for a synthetic mailbox, if any. Default
    /// `Ok(None)` keeps out-of-tree `Store` impls compiling (their seam
    /// reads 404); production and the test fake override it. Read-only:
    /// implementations must not create users, challenges, or magic rows.
    fn qa_issued_code(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<QaIssuedCode>, StoreError>> {
        async move {
            let _ = email;
            Ok(None)
        }
    }
    /// Ingest jobs consumed today (UTC), counted from accepted
    /// `ingest_jobs` rows: 0 when none exist yet. The intake
    /// cap guard feeds this to `plan_limits::check_ingest_write`; the
    /// staging-only over-limit mint seeds it. Default 0 keeps out-of-tree
    /// `Store` impls compiling (their guard allows until the write path
    /// lands); production and the test fake override it.
    fn ingest_jobs_today(
        &self,
        user_id: &str,
        now: u64,
    ) -> impl Future<Output = Result<u64, StoreError>> {
        async move {
            let _ = (user_id, now);
            Ok(0)
        }
    }
    /// Seed today's ingest count to at least `ingest_jobs`
    /// (an idempotent reseed never lowers the count): the staging-only
    /// over-limit mint pre-seeds a capped account this way, so the intake
    /// guard refuses it without hammering a real path. Positive counts
    /// only; implementations must not create users or sessions. Default
    /// no-op keeps out-of-tree `Store` impls compiling; production and
    /// the test fake override it.
    fn seed_qa_ingest_usage(
        &self,
        user_id: &str,
        now: u64,
        ingest_jobs: u64,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async move {
            let _ = (user_id, now, ingest_jobs);
            Ok(())
        }
    }
    /// Every plan row, cheapest first (`billing::plans_select_statement`).
    /// Default `Err` keeps out-of-tree `Store` impls compiling while
    /// failing closed: a billing display must never invent plan data.
    /// Production and the test fake override it.
    fn plans(&self) -> impl Future<Output = Result<Vec<PlanRow>, StoreError>> {
        async move { Err(StoreError) }
    }
    /// The live-slot subscription row plus whether any subscription row
    /// ever existed (`billing::subscription_live_select_statement` +
    /// `billing::subscription_any_select_statement`): the two reads
    /// `billing::account_state` needs. Default `Err` fails closed like
    /// [`Store::plans`]; production and the test fake override it.
    fn subscription_snapshot(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<(Option<SubscriptionRow>, bool), StoreError>> {
        async move {
            let _ = user_id;
            Err(StoreError)
        }
    }
    /// The live-slot subscription row (`billing::subscription_live_
    /// select_statement`) feeding `resolve_effective_plan` at the
    /// playback gate. Default `Err` fails closed: a store that cannot
    /// read subscriptions must refuse grant issuance (generic 500),
    /// never invent entitlement. Production and the test fake override.
    fn live_subscription(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<crate::playback::SubscriptionRow>, StoreError>> {
        async move {
            let _ = user_id;
            Err(StoreError)
        }
    }
    /// Stream seconds metered for `(user_id, period_month)` — the monthly
    /// `SUM` over `stream_play_ledger`
    /// (`ledger::ledger_monthly_total_statement`), 0 when no plays are
    /// metered. Default `Err` fails closed like [`Store::plans`];
    /// production and the test fake override it.
    fn stream_seconds_month(
        &self,
        user_id: &str,
        period_month: &str,
    ) -> impl Future<Output = Result<u64, StoreError>> {
        async move {
            let _ = (user_id, period_month);
            Err(StoreError)
        }
    }
    /// Whether any subscription row ever reached paid coverage
    /// (`billing::subscription_ever_paid_statement`): the
    /// `lapsed` vs `never_subscribed` discriminator. Default `Err`
    /// fails closed like [`Store::live_subscription`].
    fn ever_subscribed(&self, user_id: &str) -> impl Future<Output = Result<bool, StoreError>> {
        async move {
            let _ = user_id;
            Err(StoreError)
        }
    }
    /// Runtime caps from the `plans` row
    /// (`plan_limits::plan_caps_select_statement`): `None` when the id
    /// has no row (the gate fails closed to the reserved `free` caps).
    /// Default `Err` fails closed like [`Store::live_subscription`].
    fn plan_caps(
        &self,
        plan_id: &str,
    ) -> impl Future<Output = Result<Option<crate::plan_limits::PlanCaps>, StoreError>> {
        async move {
            let _ = plan_id;
            Err(StoreError)
        }
    }
    /// Billed stream seconds for (`user_id`, `period_month`) — the
    /// single monthly-usage authority
    /// (`ledger::ledger_monthly_total_statement`). Default `Err` fails
    /// closed: an unreadable meter refuses new playback rather than
    /// reading as zero usage.
    fn monthly_stream_seconds(
        &self,
        user_id: &str,
        period_month: &str,
    ) -> impl Future<Output = Result<u64, StoreError>> {
        async move {
            let _ = (user_id, period_month);
            Err(StoreError)
        }
    }
    /// The user's `playback_leases` row, if any. Default `Err` fails
    /// closed like [`Store::live_subscription`].
    fn playback_lease(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<crate::playback::LeaseRow>, StoreError>> {
        async move {
            let _ = user_id;
            Err(StoreError)
        }
    }
    /// Atomically acquire/replace the user's lease for a NEW play
    /// (`playback::playback_lease_acquire_statement`): succeeds when
    /// no row exists, the stored lease has expired at `now`, the same
    /// client session re-acquires, or `takeover` is set; the epoch
    /// increments in the store, never from a caller value. Otherwise
    /// `Held` with the live lease's remaining TTL. Default `Err` fails
    /// closed like [`Store::live_subscription`].
    fn acquire_playback_lease(
        &self,
        user_id: &str,
        session_id: &str,
        track_id: &str,
        play_id: &str,
        now: u64,
        expires_at: u64,
        takeover: bool,
    ) -> impl Future<Output = Result<crate::playback::LeaseAcquire, StoreError>> {
        async move {
            let _ = (
                user_id, session_id, track_id, play_id, now, expires_at, takeover,
            );
            Err(StoreError)
        }
    }
    /// Extend the exact holder's lease (`playback::playback_lease_
    /// touch_statement`): true iff the (`session_id`, `play_id`,
    /// `epoch`) triple still names the stored row. Default `Err` fails
    /// closed like [`Store::live_subscription`].
    fn touch_playback_lease(
        &self,
        user_id: &str,
        session_id: &str,
        play_id: &str,
        epoch: u64,
        now: u64,
        expires_at: u64,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async move {
            let _ = (user_id, session_id, play_id, epoch, now, expires_at);
            Err(StoreError)
        }
    }
    /// Derivation metadata for one user and local track ID or recording MBID
    /// (app and mirror metadata statements in `meter_sink`). Exact local IDs
    /// take precedence; ambiguous recording matches suppress artifact fields.
    /// A recording without an artifact can still resolve in the mirror. Unknown
    /// or non-UUID IDs resolve to all-`None`; store failures still error. Default
    /// `Err` fails closed like [`Store::live_subscription`] (the sink
    /// answers 500 and the DO retries; nothing meters on a guess).
    fn track_meter_meta(
        &self,
        user_id: &str,
        track_id: &str,
    ) -> impl Future<Output = Result<crate::meter_sink::TrackMeterMeta, StoreError>> {
        async move {
            let _ = (user_id, track_id);
            Err(StoreError)
        }
    }
    /// Write one cumulative play window through the monotonic upsert
    /// (`ledger::ledger_play_window_upsert_statement`): which
    /// disposition the write converged to is reported for the sink
    /// ack. Default `Err` fails closed like
    /// [`Store::live_subscription`].
    fn record_play_window(
        &self,
        window: &crate::ledger::PlayWindow,
    ) -> impl Future<Output = Result<crate::meter_sink::PlayWindowOutcome, StoreError>> {
        async move {
            let _ = window;
            Err(StoreError)
        }
    }
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
    fn invite_by_hash(
        &self,
        code_hash: &str,
    ) -> impl Future<Output = Result<Option<InviteRecord>, StoreError>>;
    fn insert_session(
        &self,
        record: &SessionRecord,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Claim a validated magic link and insert its session in one transaction.
    /// `Ok(false)` means the conditional single-use claim lost (replay/expiry).
    /// Both writes must commit together or neither may commit; never emulate
    /// this with autocommitted writes or a compensating reset of `used`. The default
    /// fails closed for stores without transactional redemption support.
    /// `_now` is the request-admission timestamp: database implementations
    /// MUST re-evaluate expiry against their own clock when the claim
    /// executes (the production store does, inside the claim statement), so a
    /// request admitted just before expiry cannot commit after it. In-memory
    /// test stores have no second clock and use `_now`.
    fn redeem_magic_link(
        &self,
        _magic: &MagicLinkRecord,
        _now: u64,
        _session: &SessionRecord,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async { Err(StoreError) }
    }
    /// Redemption plus its success audit, for handlers whose response
    /// carries the freshly committed credential: no store operation may run
    /// between the redemption commit and that response, or a slow audit
    /// insert could eat the client's remaining deadline after the link is
    /// already spent. The production store writes the audit inside the
    /// redemption transaction, so all three writes commit together and an
    /// audit failure leaves the link unspent and retryable. The default
    /// keeps stores without that seam on the sequential shape (redeem, then
    /// best-effort audit on success), preserving observable test behavior.
    fn redeem_magic_link_and_audit(
        &self,
        magic: &MagicLinkRecord,
        now: u64,
        session: &SessionRecord,
        audit: &AuditEvent,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async move {
            let redeemed = self.redeem_magic_link(magic, now, session).await?;
            if redeemed {
                let _ = self.insert_audit(audit).await;
            }
            Ok(redeemed)
        }
    }
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
    /// Atomically rotate a current session OR recover its immediate successor
    /// using a digest bound to the old cookie and a separate client retry secret.
    /// Lock the row BEFORE sampling a fresh database claim clock, then execute
    /// `session::ROTATE_SESSION_WITH_RETRY_SQL` in that transaction (production:
    /// `session_retry_store::rotate_with_retry`). Recheck both expiries and the
    /// receipt window at the claim, not at request arrival. A missing/revoked/
    /// stale row or mismatched/expired recovery must return None. The default
    /// fails closed for stores that have not implemented the new protocol.
    fn rotate_session_with_retry(
        &self,
        _previous: &SessionClaims,
        _next: &SessionClaims,
        _retry_hash: &str,
        _now: u64,
    ) -> impl Future<Output = Result<Option<SessionClaims>, StoreError>> {
        async { Err(StoreError) }
    }
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
    /// Conditional single-session revoke matching
    /// [`session::SessionRevokeClaim::Single`]: `UPDATE ... WHERE id = $1
    /// AND user_id = $2 AND revoked = FALSE`. Returns true iff exactly one
    /// row transitioned, so concurrent logouts cannot double-count. The
    /// default preserves legacy behavior for out-of-tree impls by delegating
    /// to [`Store::revoke_session`]; production and updated fakes override
    /// with the conditional write.
    fn revoke_single_session(
        &self,
        id: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async move {
            let _ = user_id;
            self.revoke_session(id).await.map(|_| true)
        }
    }
    /// Atomically revoke all sessions, native and compatibility credentials for
    /// one owner. Returns the number of newly revoked native credentials,
    /// preserving timestamps on already revoked rows. An empty owner revokes
    /// nothing. Must serialize with issuance on both surfaces; composing
    /// independent writes is not valid.
    fn revoke_sessions_and_native_credentials(
        &self,
        user_id: &str,
        now: u64,
    ) -> impl Future<Output = Result<u64, StoreError>>;
    /// Whether the user's account is fenced for deletion
    /// (`users.deleting_at IS NOT NULL`, ADR 0002 rule 3). Handlers use
    /// it only to classify a fenced write's failure as the typed 409
    /// instead of the generic storage error; it is never the fence
    /// itself (production writes carry the fence in-statement).
    /// Default `Ok(false)` keeps out-of-tree `Store` impls compiling;
    /// production and the test fake override it.
    fn account_deleting(&self, _user_id: &str) -> impl Future<Output = Result<bool, StoreError>> {
        async { Ok(false) }
    }
    /// Whether the account was ever admitted past signup: it holds a
    /// passkey credential, a session row (current or revoked — a
    /// completed sign-in leaves one), or a provider vault, or it
    /// carries a non-member role (promotion is an admission act; the
    /// reaper's `unverified_user_filter` protects the same states).
    /// The magic-finish invite gate refuses only accounts for which
    /// this is false — a never-admitted signup in progress — so
    /// established magic-only members keep signing in after
    /// `INVITES_REQUIRED` flips. The default checks credentials only;
    /// production and the test fake override it with the full legacy
    /// predicate (sessions, role and remaining encrypted rows included).
    fn account_admitted(&self, user_id: &str) -> impl Future<Output = Result<bool, StoreError>> {
        async move { Ok(!self.credentials_for_user(user_id).await?.is_empty()) }
    }
    fn insert_audit(&self, event: &AuditEvent) -> impl Future<Output = Result<(), StoreError>>;
    /// The stored `users.role` for the admin guard, read fresh per
    /// request (migration 0028). Fail-closed default is absent — and
    /// absent is never admin — so an unprovisioned store grants nothing;
    /// production and the test fake override it.
    fn user_role(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> {
        async { Ok(None) }
    }
    /// Set a user's role and write the extended `user.role_changed`
    /// audit row (actor, subject, old-to-new) in one transaction:
    /// the role must never change without its audit row. The
    /// transaction re-reads the actor's role under its locks (see
    /// `AdminWriteOutcome`), so two admins passing the guard
    /// concurrently cannot demote each other to zero admins.
    /// Fail-closed default; production and the test fake override it.
    fn admin_set_user_role(
        &self,
        _actor_user_id: &str,
        _subject_user_id: &str,
        _new_role: &str,
        _source_ip: &str,
        _now: u64,
    ) -> impl Future<Output = Result<AdminWriteOutcome, StoreError>> {
        async { Err(StoreError) }
    }
    /// Every `app_settings` row as `(key, value)` pairs (migration
    /// 0028). Fail-closed default is empty — the typed accessors then
    /// resolve their built-in defaults; production and the test fake
    /// override it.
    fn app_settings(&self) -> impl Future<Output = Result<Vec<(String, String)>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Upsert one `app_settings` row and write the extended
    /// `settings.changed` audit row (actor, old-to-new) in one
    /// transaction, revalidating the actor's role under the write
    /// serialization lock (see `AdminWriteOutcome`): a settings write
    /// can never commit after its actor's demotion. Fail-closed
    /// default; production and the test fake override it.
    fn admin_set_app_setting(
        &self,
        _actor_user_id: &str,
        _key: &str,
        _new_value: &str,
        _source_ip: &str,
        _now: u64,
    ) -> impl Future<Output = Result<AdminWriteOutcome, StoreError>> {
        async { Err(StoreError) }
    }
    // --- Stripe billing reconciliation (migrations 0021/0029) ------------------
    //
    // The webhook and the inline/API paths share one reconciliation
    // function (`billing_reconcile::reconcile_subscription`); these are
    // its store seams. Reads fail closed (`Err`, except where noted) so
    // an unprovisioned store can never mint or extend entitlement; the
    // reconcile write runs as one transaction under the per-user advisory
    // lock (see the statements in `billing_reconcile`). Production
    // (`PgStore`) and the test fake override all of them.
    /// First claim of a verified webhook event id wins (`true`);
    /// duplicates report `false` so retried deliveries ack 200 with no
    /// state change. Default `Ok(true)` keeps out-of-tree `Store` impls
    /// compiling (their webhooks reprocess, never double-advance: the
    /// reconcile write itself is idempotent on the invoice id).
    fn claim_webhook_event(
        &self,
        _event_id: &str,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async move { Ok(true) }
    }
    /// Release a claim whose handling failed mid-reconcile (Stripe 5xx,
    /// store outage): the next delivery reprocesses instead of acking 200
    /// on a consumed claim. Default no-op keeps out-of-tree `Store` impls
    /// compiling; production and the test fake override it.
    fn release_webhook_event(
        &self,
        _event_id: &str,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async move { Ok(()) }
    }
    /// The reconciliation input for one user: bound Stripe customer, live
    /// or explicitly targeted subscription row, and the full `plan_prices`
    /// map. Default `Err` (fail closed): without local state nothing advances.
    fn load_reconcile_input(
        &self,
        _user_id: &str,
        _target_subscription_id: Option<&str>,
    ) -> impl Future<Output = Result<crate::billing_reconcile::ReconcileInput, StoreError>> {
        async move { Err(StoreError) }
    }
    /// Reverse lookup for webhooks that carry no operation binding:
    /// the user bound to a Stripe customer id, if any. Default `Ok(None)`
    /// (unknown customer: ack without state).
    fn user_for_stripe_customer(
        &self,
        _customer_id: &str,
    ) -> impl Future<Output = Result<Option<String>, StoreError>> {
        async move { Ok(None) }
    }
    /// Persist one reconcile pass: the subscription row upsert plus the
    /// idempotent history inserts, as a single transaction under the
    /// per-user advisory lock. Default `Err` (fail closed): unconfirmed
    /// state is never half-written.
    fn store_reconcile_outcome(
        &self,
        _user_id: &str,
        _outcome: &crate::billing_reconcile::ReconcileOutcome,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async move { Err(StoreError) }
    }
    /// The user's unresolved single-flight checkout operation, if any (at
    /// most one: `uq_billing_operations_single_flight`). Default `Ok(None)`
    /// (no flight: the create path inserts, failing closed on `Err`).
    fn open_checkout_operation(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Option<crate::billing_reconcile::StoredCheckoutOp>, StoreError>>
    {
        async move { Ok(None) }
    }
    /// Single-flight checkout-operation insert: concurrent first attempts
    /// converge (the partial unique admits exactly one; the loser reads
    /// the winner's row). Binds the Stripe customer to the account, but
    /// only when unbound — an existing binding is never overwritten by a
    /// racing second customer create. Default `Err` (fail closed).
    fn create_checkout_operation(
        &self,
        _op_id: &str,
        _user_id: &str,
        _price_id: &str,
        _customer_id: Option<&str>,
    ) -> impl Future<Output = Result<crate::billing_reconcile::CreateOpOutcome, StoreError>> {
        async move { Err(StoreError) }
    }
    /// Operation lookup by id (the webhook's `client_reference_id`
    /// binding). Default `Ok(None)` (unknown operation: ack without state).
    fn checkout_operation_by_id(
        &self,
        _op_id: &str,
    ) -> impl Future<Output = Result<Option<crate::billing_reconcile::StoredCheckoutOp>, StoreError>>
    {
        async move { Ok(None) }
    }
    /// Record the created session (and later terminal marks) on the
    /// operation row — but only while the row still holds the single
    /// flight. Reports whether the mark landed: a mark that arrives after
    /// a deliberate supersede must not resurrect the retired row, so the
    /// caller retries into the replacement instead. Default `Err` (fail
    /// closed).
    fn update_checkout_operation(
        &self,
        _op_id: &str,
        _state: &str,
        _price_id: &str,
        _session_id: Option<&str>,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async move { Err(StoreError) }
    }
    /// One guarded admin NZB upload attempt (migration 0034): validate
    /// the attempt, converge duplicates on the content-derived id, and
    /// write the job row, the encrypted original (accepted attempts
    /// only) and the `ingest.uploaded` audit row in one transaction,
    /// revalidating the actor's role under the write serialization lock
    /// (see `AdminWriteOutcome`). Fail-closed default; production and
    /// the test fake override it.
    fn admin_ingest_upload(
        &self,
        _attempt: &AdminIngestAttempt,
    ) -> impl Future<Output = Result<AdminIngestUploadOutcome, StoreError>> {
        async { Err(StoreError) }
    }
    /// One stored admin job row by id, or `None` for unknown ids and
    /// pipeline rows (the admin reads never expose the pipeline
    /// ledger). Never carries NZB bytes or parse detail: the row type
    /// is redacted by construction. Fail-closed default is absent;
    /// production and the test fake override it.
    fn admin_ingest_get(
        &self,
        _job_id: &str,
    ) -> impl Future<Output = Result<Option<StoredAdminIngestJob>, StoreError>> {
        async { Ok(None) }
    }
    /// Stored admin job rows, newest first: `status` admits exactly one
    /// view status (`accepted`/`failed`) or `None` for every admin job;
    /// `limit`/`offset` are already bounded by the handler. Pipeline
    /// rows never appear. Fail-closed default is empty; production and
    /// the test fake override it.
    fn admin_ingest_list(
        &self,
        _status: Option<&str>,
        _limit: u64,
        _offset: u64,
    ) -> impl Future<Output = Result<Vec<StoredAdminIngestJob>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Store-level delete primitive (no route serves it): the original
    /// goes first, then the job, under the actor re-check. A deleted
    /// actor's attempt reports `ActorNotAdmin`; an unknown id (or a
    /// pipeline row) reports `Missing`. No audit row: deletion writes
    /// no new observable state. Fail-closed default; production and the
    /// test fake override it.
    fn admin_ingest_delete(
        &self,
        _actor_user_id: &str,
        _job_id: &str,
    ) -> impl Future<Output = Result<AdminIngestDeleteOutcome, StoreError>> {
        async { Err(StoreError) }
    }
    /// Issue one invite and write its `invite.issued` audit row in one
    /// transaction, revalidating the actor's role under the write
    /// serialization lock (see [`AdminInviteIssueOutcome`]). The audit
    /// row references the invite by the returned row id only — the
    /// code and its hash never appear in it. Fail-closed default;
    /// production and the test fake override it.
    fn admin_issue_invite(
        &self,
        _actor_user_id: &str,
        _record: &InviteRecord,
        _source_ip: &str,
        _now: u64,
    ) -> impl Future<Output = Result<AdminInviteIssueOutcome, StoreError>> {
        async { Err(StoreError) }
    }
    /// Every invite row, newest first, for the admin list. `issuer`
    /// filters by exact `issued_by`; `status` filters by the derived
    /// [`InviteRecord::status`] at `now` (`active`, `expired`,
    /// `redeemed`, `revoked`); both may combine. The store caps the
    /// result; pagination is post-M1. Fail-closed default is empty;
    /// production and the test fake override it.
    fn admin_list_invites(
        &self,
        _issuer: Option<&str>,
        _status: Option<&str>,
        _now: u64,
        _limit: usize,
    ) -> impl Future<Output = Result<Vec<InviteRecord>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Revoke one unredeemed invite and write its `invite.revoked`
    /// audit row in one transaction, revalidating the actor's role
    /// under the write serialization lock (see
    /// [`AdminInviteRevokeOutcome`]). Fail-closed default; production
    /// and the test fake override it.
    fn admin_revoke_invite(
        &self,
        _actor_user_id: &str,
        _invite_id: i64,
        _source_ip: &str,
        _now: u64,
    ) -> impl Future<Output = Result<AdminInviteRevokeOutcome, StoreError>> {
        async { Err(StoreError) }
    }
    /// Compatibility lifecycle is native-session-only. Implementations must
    /// fence the live owner and commit before returning success; false means
    /// no insert (deleting owner or active-credential limit). The default
    /// cannot mint, list invented data, or claim a successful revocation.
    fn insert_subsonic_credential(
        &self,
        _row: &crate::subsonic_lifecycle::PreparedCredential,
        _session: &SessionClaims,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async { Err(StoreError) }
    }
    fn subsonic_credentials_for_user(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Vec<crate::subsonic_lifecycle::ListedCredential>, StoreError>>
    {
        async { Err(StoreError) }
    }
    fn revoke_subsonic_credential(
        &self,
        _id: &str,
        _user_id: &str,
        _now: u64,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async { Err(StoreError) }
    }
    /// Stores that enable compatibility issuance must include it in recovery.
    /// The default is safe only because default issuance always refuses.
    fn revoke_all_subsonic_credentials(
        &self,
        _user_id: &str,
        _now: u64,
    ) -> impl Future<Output = Result<u64, StoreError>> {
        async { Ok(0) }
    }
    /// Persist a freshly issued native REST credential row (migration
    /// 0019) only while the issuing session is still live and belongs to the
    /// credential owner. The check and insert must serialize with session
    /// revocation and retain the account-deletion fence through commit.
    /// Fail-closed default: an unprovisioned store never reports a mint.
    fn insert_native_credential(
        &self,
        _row: &NativeCredentialRow,
        _session_id: &str,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async { Err(StoreError) }
    }
    /// Every credential row owned by `user_id`, for the
    /// session-authenticated listing (never carries secret material: the
    /// row holds the HMAC only). Fail-closed default is empty; production
    /// and the test fake override it.
    fn native_credentials_for_user(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Vec<NativeCredentialRow>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Bearer lookup by the composite identity `(pepper_version,
    /// key_hmac)`: the gate HMACs the presented secret under each listed
    /// pepper and looks the digest up here. Fail-closed default is
    /// absent; production and the test fake override it.
    fn native_credential_by_hmac(
        &self,
        _pepper_version: u32,
        _key_hmac: &str,
    ) -> impl Future<Output = Result<Option<NativeCredentialRow>, StoreError>> {
        async { Ok(None) }
    }
    /// Owner-scoped single revoke: marks the row `revoked_at = now` when
    /// it belongs to `user_id` and is still live (sticky — an
    /// already-revoked row keeps its original timestamp), and reports
    /// whether such a row exists at all (unknown or foreign ids report
    /// `false`, which the handler answers 404). Fail-closed default is
    /// `false`; production and the test fake override it.
    fn revoke_native_credential(
        &self,
        _id: &str,
        _user_id: &str,
        _now: u64,
    ) -> impl Future<Output = Result<bool, StoreError>> {
        async { Ok(false) }
    }
    /// Best-effort successful-use accounting (`last_used_at`): called by
    /// the bearer gate after authorization, its outcome never changes the
    /// decision (like `audit`). Default no-op keeps out-of-tree `Store`
    /// impls compiling; production and the test fake override it.
    fn record_native_credential_use(
        &self,
        _id: &str,
        _now: u64,
    ) -> impl Future<Output = Result<(), StoreError>> {
        async { Ok(()) }
    }
    /// Catalogue slice-1 reads: the per-user ingest library plus
    /// the shared mirror collections. Read-only: implementations must not
    /// create users, ledger rows, or mirror entries. Owner scoping lives in
    /// the SQL: the library page filters `WHERE ... user_id = $1`, and the
    /// browse lists plus the release detail resolve only through the
    /// caller's ledger membership (ADR 0006 §6). No owner id is ever
    /// returned. Fail-open-empty defaults keep out-of-tree `Store` impls
    /// compiling (their catalogue routes serve empty pages); production
    /// and the test fake override every method below.
    /// Per-user library page (joined ledger rows, matched-first order).
    fn catalogue_library(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Vec<crate::catalogue::LibraryDbRow>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Caller-member artist collection page (only artists of the caller's
    /// member releases).
    fn catalogue_artists(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Vec<crate::catalogue::ArtistDbRow>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Caller-member release collection page (artist-joined).
    fn catalogue_releases(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Vec<crate::catalogue::ReleaseDbRow>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// Caller-member track-occurrence collection page (exact pairs only).
    fn catalogue_tracks(
        &self,
        _user_id: &str,
    ) -> impl Future<Output = Result<Vec<crate::catalogue::TrackDbRow>, StoreError>> {
        async { Ok(Vec::new()) }
    }
    /// One release by MBID (`None` when unknown or non-member — the detail
    /// handler answers 404 for both, indistinguishably).
    fn catalogue_release(
        &self,
        _user_id: &str,
        _mbid: &str,
    ) -> impl Future<Output = Result<Option<crate::catalogue::ReleaseDbRow>, StoreError>> {
        async { Ok(None) }
    }
    /// One member release's tracks in album order (empty when the release
    /// has no mirror occurrences yet — still a 200 with no tracks — and
    /// empty for a non-member release, which the handler never reaches).
    fn catalogue_release_tracks(
        &self,
        _user_id: &str,
        _mbid: &str,
    ) -> impl Future<Output = Result<Vec<crate::catalogue::ReleaseTrackDbRow>, StoreError>> {
        async { Ok(Vec::new()) }
    }
}

/// Magic-link delivery. The token is passed only to the mailer; it must
/// never be logged by implementations. Dropping the returned future must
/// cancel the underlying I/O, not just stop polling it.
pub trait Mailer {
    fn send_magic_link(
        &self,
        email: &str,
        token: &str,
    ) -> impl Future<Output = Result<(), StoreError>>;
}

// Only the admitted send is timed: issuance and staging capture have already
// completed. A timeout follows the ordinary ignored delivery-error path.
pub(crate) async fn deliver_mail_with_deadline(
    send: impl Future<Output = Result<(), StoreError>>,
) -> Result<(), StoreError> {
    let duration = std::time::Duration::from_secs(AUTH_MAIL_SEND_DEADLINE_SECS);
    #[cfg(target_arch = "wasm32")]
    let deadline = worker::Delay::from(duration);
    #[cfg(not(target_arch = "wasm32"))]
    let deadline = futures_timer::Delay::new(duration);
    let mut deadline = std::pin::pin!(deadline);
    let mut send = std::pin::pin!(send);
    std::future::poll_fn(|cx| {
        // Worker Delay arms on first poll, so poll it before starting I/O.
        // Expiry wins if both become ready together; there is no late retry.
        if deadline.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(StoreError));
        }
        send.as_mut().poll(cx)
    })
    .await
    // Both futures are dropped here, before the public response is built.
}

/// Bounded outbound probe for `test connection`. Implementations dial
/// `target.host()` (the runtime resolves it for routing and uses it for
/// TLS SNI/certificate checks — an IP-literal dial would verify the
/// certificate against the address and fail every hostname-only
/// certificate), at exactly `target.port()` (the allowlisted port the
/// target was vetted for — callers pass no port, so a substitute can
/// never be smuggled in). There is no connected-peer check: workerd
/// reports the dial string itself as `SocketInfo.remote_address`, so no
/// peer IP exists to vet — containment is the handler's fresh dial-time
/// validation plus the timeouts below. Apply the per-attempt and total
/// `vault` timeout constants, attempt no more than
/// `vault::MAX_DIAL_ADDRESSES`, follow no redirects, and return a bare
/// boolean — banners/errors never leave the dialer.
pub trait Dialer {
    fn probe(&self, target: &ValidatedTarget, tls: bool) -> impl Future<Output = bool>;
}

/// Async DNS for handler-time resolution (the sync core guard consumes
/// the result via a static adapter, so exactly the resolved set is what
/// gets vetted and dialed). The caller bounds the ENTIRE resolution using
/// `vault::RESOLUTION_TIMEOUT_MS`, separately from the socket budgets.
/// Implementations must cancel underlying I/O when their future is dropped;
/// dropping a Rust future alone does not abort a JavaScript fetch/body.
pub trait AsyncDns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>>;
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
    /// Raw `Authorization` header value, if any. Carried so the
    /// staging-only QA seams and the staging QA rate-limit exemption
    /// (`qa_rate_limit_exempt`) can check their bearer, and for the
    /// native-REST bearer gate; account authentication rides the session
    /// cookie, except the NZB intake route, which takes the session as a
    /// Bearer (the programmatic ingest caller holds no cookie jar).
    pub authorization: Option<String>,
    /// Raw `Stripe-Signature` header value, if any. Carried only for the
    /// billing webhook route (the signature is that route's authentication;
    /// every other route ignores it). The entry lowercases header names, so
    /// this arrives normalized.
    pub stripe_signature: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub set_cookie: Option<String>,
    /// `Retry-After` seconds for a 429; the entry renders the header.
    pub retry_after: Option<u64>,
    /// Extra response headers the entry renders verbatim (plan-limit
    /// `x-plan-limit` / `x-plan-reset` today). Values must be names-only
    /// by construction — see `plan_limits::Denial::headers`; never put
    /// account data here.
    pub extra_headers: Vec<(String, String)>,
}

fn json_response(status: u16, body: serde_json::Value) -> Response {
    Response {
        status,
        body: body.to_string(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

fn generic_auth() -> Response {
    json_response(
        401,
        serde_json::json!({"error": "check your email and try again"}),
    )
}

/// The typed account-deletion fence refusal (ADR 0002 rule 3): HTTP 409
/// with the registered `account_deleting` envelope. The blocking
/// operation id and retry hint join this body with the deletion routes,
/// which own the operation lookup.
fn account_deleting_response() -> Response {
    let error = crate::api_error::ApiError::account_deleting("account is being deleted");
    Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

fn internal() -> Response {
    json_response(500, serde_json::json!({"error": "internal error"}))
}

/// Fail-closed secure-randomness response: the existing 503
/// `service_unavailable` envelope (retryable, sealed public text).
fn unavailable() -> Response {
    let error = crate::api_error::ApiError::service_unavailable("service unavailable");
    Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
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
                extra_headers: Vec::new(),
            }
        }
    }
}

pub(crate) fn invalid_request() -> Response {
    let error = crate::api_error::ApiError::invalid_request("invalid request");
    Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

// --- Staging-only QA code seam ----------------------------------------------------------
//
// `POST /api/test-codes` returns the issued signup code for an
// `example.test` mailbox without minting an account, so the staging E2E can
// drive the real `/signup` form to the BYOP wizard. Armed ONLY by the
// explicit staging flag (`Config::qa_code_seam`, from `QA_CODE_SEAM`) plus
// the staged bearer (`Config::qa_auth_token`, from the `QA_AUTH_TOKEN`
// secret); every other posture answers the seam path with the same plain
// 404 as an unknown route. See `docs/security/threat-model.md`.

/// Seam path, served by the API Worker on staging. The E2E contract pins
/// the same path (`testCodes` in `e2e/helpers/contract.ts`); a rename must
/// change both sides together.
pub const QA_TEST_CODES_PATH: &str = "/api/test-codes";

/// Staging-only test-account mint seam, armed by the same flag + bearer as
/// [`QA_TEST_CODES_PATH`]. The E2E contract pins the same path
/// (`testAccounts` in `e2e/helpers/contract.ts`).
pub const QA_TEST_ACCOUNTS_PATH: &str = "/api/test-accounts";

/// True iff the address is a synthetic test mailbox: exactly one `@` and
/// the reserved `example.test` domain (RFC 2606, never routable), matched
/// case-insensitively. Subdomains and suffix lookalikes
/// (`example.test.evil.com`) do not match.
fn is_example_test_mailbox(email: &str) -> bool {
    let mut parts = email.trim().split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) if !local.is_empty() => {
            domain.eq_ignore_ascii_case("example.test")
        }
        _ => false,
    }
}

/// Constant-time bearer comparison for the seam route. Length mismatch
/// rejects without comparing (only the length leaks, never content).
fn qa_bearer_ok(presented: Option<&str>, expected: &str) -> bool {
    match presented {
        Some(candidate) => {
            let a = candidate.as_bytes();
            let b = expected.as_bytes();
            a.len() == b.len() && a.ct_eq(b).into()
        }
        None => false,
    }
}

/// True iff the request names a synthetic `example.test` mailbox and
/// carries the staged QA bearer on an armed staging posture
/// (`QA_CODE_SEAM` + `QA_AUTH_TOKEN`, the same gate as the seam routes).
/// Such requests skip the per-IP auth-start and per-IP mail budgets: every
/// journey spec shares one CI runner IP, and the bearer already unlocks
/// minting synthetic accounts, so the exemption grants nothing it does
/// not. Routable mailboxes always spend both budgets, so even a leaked
/// staging token cannot create unbounded real-address rows or mail. Every
/// other posture (production included) is false whatever header is
/// presented.
fn qa_rate_limit_exempt(cfg: &Config, req: &Request, email: &str) -> bool {
    if !is_example_test_mailbox(email) {
        return false;
    }
    let Some(expected) = cfg.qa_auth_token.as_deref().filter(|_| cfg.qa_code_seam) else {
        return false;
    };
    let presented = req
        .authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "));
    qa_bearer_ok(presented, expected)
}

/// One captured issuance for a synthetic mailbox (migration 0018).
#[derive(Debug, Clone)]
pub struct QaIssuedCode {
    pub token: String,
    pub issued_at: u64,
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
    /// Stripe HTTPS transport (Worker `Fetch` in production, fixture fakes
    /// in tests). A shared reference — not a fifth generic — so the ~40
    /// existing `Ctx` signatures keep their shape; billing handlers are the
    /// only readers. Unprovisioned postures supply
    /// [`crate::stripe::NoStripeTransport`], whose every call fails closed.
    pub stripe: &'a dyn crate::stripe::StripeTransport,
    pub now: u64,
    pub source_ip: String,
}

// --- Helpers ---------------------------------------------------------------------------

/// Random opaque id of `N` entropy bytes (unpadded base64url). Fail-closed:
/// secure-randomness failure propagates [`crate::random::CryptoError`]
/// (HTTP 503 `service_unavailable`) so the caller fails the request closed.
/// The success path is byte-identical: the same `N` entropy bytes through
/// the same `b64url_encode`.
fn random_id<const N: usize>() -> Result<String, crate::random::CryptoError> {
    let bytes = crate::random::secure_random_bytes::<N>()?;
    Ok(b64url_encode(&bytes))
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

fn valid_challenge_id(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Public register/start user handle, stable per normalized email and
/// independent of stored account state. Internal user ids stay server-side;
/// register/finish resolves enrollment through the stored challenge instead.
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

/// Uniform per-IP throttle response: the standard `rate_limited` error
/// envelope (`docs/api-errors.md`) with a fixed message independent of the
/// request email, because the budget is keyed only on the client prefix.
/// Callers must not return email-derived stand-ins on this path (that would
/// be an account-existence oracle across the denial boundary).
/// `retry_after_secs` is the budget window: slot expiries are staggered, so
/// the next slot frees no later than one full window out.
fn throttled(retry_after_secs: u64) -> Response {
    let error = crate::api_error::ApiError::rate_limited("too many requests");
    let mut resp = Response {
        status: crate::rate_limit::RATE_LIMIT_STATUS,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    };
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

    /// Prepare a signed token and its row without committing any auth state.
    fn prepare_session(&self, user_id: &str) -> Result<(String, SessionRecord), Response> {
        let token = session::issue_session(
            &self.cfg.session_secret,
            user_id,
            self.now,
            self.cfg.session_ttl_secs,
        )
        .map_err(|e| match e {
            session::SessionError::RandomnessUnavailable => unavailable(),
            _ => internal(),
        })?;
        let claims = session::verify_session(&self.cfg.session_secret, &token, self.now)
            .map_err(|_| internal())?;
        Ok((
            token,
            SessionRecord {
                id: claims.session_id,
                user_id: claims.user_id,
                created_at: claims.created_at,
                expires_at: claims.expires_at,
                rotation_counter: claims.rotation_counter,
                revoked: false,
            },
        ))
    }

    /// Issue + persist a new session row; the cookie is only returned
    /// after the row insert succeeds.
    async fn start_session(&self, user_id: &str) -> Result<Response, Response> {
        let (token, record) = self.prepare_session(user_id)?;
        self.store
            .insert_session(&record)
            .await
            .map_err(|_| internal())?;
        let mut resp = json_response(200, serde_json::json!({"ok": true}));
        resp.set_cookie = Some(session::set_cookie_header(
            &token,
            record.expires_at.saturating_sub(self.now),
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

    /// Ordinary session authentication for the NZB intake route, which
    /// takes the session as an `Authorization: Bearer` header (the
    /// programmatic ingest caller holds no cookie jar) instead of the
    /// session cookie. Same checks as `authenticate` with `mutation:
    /// false` — signature, row, revocation, counter, expiry — with the
    /// transport swapped and no Origin gate (a Bearer-carrying POST is
    /// never a simple request, so browsers preflight it; CSRF needs no
    /// Origin pin here). A presented native-REST (`ntk_`) credential is
    /// not a session and fails closed with the generic 401; scoped
    /// ingest submission uses `POST /v1/ingest*` with `ingest:submit`.
    async fn authenticate_bearer_session(
        &self,
        req: &Request,
    ) -> Result<(SessionClaims, String), Response> {
        let token = req
            .authorization
            .as_deref()
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|token| !token.is_empty())
            .ok_or_else(generic_auth)?;
        let keys = self.cfg.session_keys();
        let claims =
            session::verify_session_dual(&keys, token, self.now).map_err(session_error_response)?;
        let record = self
            .store
            .session_by_id(&claims.session_id)
            .await
            .map_err(|_| internal())?
            .ok_or_else(generic_auth)?;
        let claims = session::verify_stored_session(&keys, token, self.now, &record)
            .map_err(session_error_response)?;
        Ok((claims, token.to_string()))
    }

    /// The admin guard (migration 0028): a cookie session whose
    /// `users.role` — read fresh from the store on every call, never a
    /// session claim — is exactly `admin`. Mutations additionally
    /// require the fresh-session step-up, like credential minting.
    ///
    /// The admin surface is non-enumerable (TWO-70 A-AC1): every caller
    /// who is not a confirmed admin gets the plain 404 an unknown path
    /// gives, whatever went wrong — a presented `Authorization` header
    /// (admin routes are `SessionOnly` in the scope map, and
    /// native/API-key credentials can never call them, not even
    /// alongside a valid admin cookie), a missing, invalid or non-admin
    /// session, or a stale session on a mutation. Only a caller already
    /// known to be an admin can hear anything else (the step-up prompt
    /// below, validation, the generic 500) — so no response a non-admin
    /// can elicit distinguishes an admin path from a nonexistent one.
    ///
    /// Role is checked before freshness: a non-admin must hear the
    /// plain 404, never a step-up prompt that reauthenticating could
    /// not satisfy. Only a confirmed admin can be asked to
    /// reauthenticate for a mutation.
    async fn authenticate_admin(
        &self,
        req: &Request,
        mutation: bool,
    ) -> Result<SessionClaims, Response> {
        if req.authorization.is_some() {
            return Err(not_found());
        }
        // `authenticate`'s own failures (a bad or missing cookie, and
        // for mutations a missing or foreign Origin) collapse to the
        // same plain 404: the caller is not yet known to be an admin.
        let (claims, token) = self
            .authenticate(req, mutation)
            .await
            .map_err(|_| not_found())?;
        // A role-read failure collapses to the same 404: until the role
        // is confirmed the caller is not known to be an admin, and a
        // 500 here (which an unknown path never produces) would
        // enumerate the surface. Production logs the failed statement
        // server-side, so the outage stays visible to operators.
        let role = self
            .store
            .user_role(&claims.user_id)
            .await
            .map_err(|_| not_found())?;
        if !crate::admin::is_admin_role(role.as_deref()) {
            return Err(not_found());
        }
        if mutation {
            let record = self
                .store
                .session_by_id(&claims.session_id)
                .await
                .map_err(|_| internal())?
                .ok_or_else(generic_auth)?;
            let keys = self.cfg.session_keys();
            session::require_fresh_session(&keys, &token, self.now, &record)
                .map_err(session_error_response)?;
        }
        Ok(claims)
    }

    /// Per-IP admission shared by register, login and magic start routes.
    /// The global budget is reserved only for outbound mail, so exhaustion
    /// cannot prevent users from obtaining an authentication challenge.
    /// The scope key is the client /64 (IPv6) or full address (IPv4), so
    /// rotation inside one prefix cannot mint fresh budgets; audit logs keep
    /// the full `source_ip`. Denial is `Err(retry_after_secs)` carrying
    /// the DENIED budget's window, so a burst denial back-off hint is
    /// the burst window, never the hourly one. A staged QA bearer on an
    /// armed staging posture, for an `example.test` mailbox, is admitted
    /// without spending either budget (`qa_rate_limit_exempt`).
    async fn admit_auth_start(&self, req: &Request, email: &str) -> Result<(), u64> {
        if qa_rate_limit_exempt(self.cfg, req, email) {
            return Ok(());
        }
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

    async fn send_magic_link_if_allowed(&self, req: &Request, user_id: &str, email: &str) {
        // Outbound mail abuse cap BEFORE the per-email budget: one IP must
        // not email-bomb unlimited distinct victims. Any denial sends
        // nothing and stores no magic row. The staged QA bearer skips only
        // this per-IP cap and only for unroutable `example.test` mailboxes
        // (`qa_rate_limit_exempt`); the global and per-email caps below
        // still apply.
        let mail_ip_ok = qa_rate_limit_exempt(self.cfg, req, email)
            || self
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
            // Still a plain console line: emitting it through the structured
            // logger needs the request trace threaded through `Request`
            // first (follow-up slice), and a fresh ID here would break
            // correlation, so it stays as-is until then.
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
        // Fail closed on randomness: mint nothing, send nothing. The
        // caller already returned the identical admitted 200, so skipping
        // issuance here preserves the no-oracle shape.
        let Ok((token, record)) = auth::issue_magic_link(user_id, email, self.now) else {
            return;
        };
        if self.store.insert_magic(&record).await.is_ok() {
            // Staging-only code seam: remember the raw token for synthetic
            // mailboxes so `POST /api/test-codes` can return the issued
            // code. Best-effort and capture-only — a failed capture must
            // never break issuance or delivery, and non-synthetic mailboxes
            // never reach the outbox.
            if self.cfg.qa_code_seam && is_example_test_mailbox(email) {
                let _ = self
                    .store
                    .record_qa_issued_code(email, &token, self.now)
                    .await;
            }
            let _ = deliver_mail_with_deadline(self.mailer.send_magic_link(email, &token)).await;
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
struct RegisterStartBody {
    email: String,
    /// Optional invite pre-check: when presented, the code is
    /// validated (never consumed) before any signup row is minted, so
    /// a bad code fails at the code step. Consumption happens only
    /// inside `complete_registration`.
    invite_code: Option<String>,
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

// --- Route handlers ------------------------------------------------------------------------

async fn register_start<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let Some(body) = parse_body::<RegisterStartBody>(req) else {
        return generic_auth();
    };
    let Some(email) = normalize_email(&body.email) else {
        return generic_auth();
    };
    // Invite pre-check (spec §6.3): when the signup page presents a
    // code, validate it — never consume it — before any signup row is
    // minted, so a bad code fails at the code step with the one
    // generic error and leaves no account behind. The per-client and
    // per-email redemption budgets are spent here exactly as at
    // finish, so this check is no cheaper an oracle than finishing.
    // Consumption still happens only inside `complete_registration`;
    // a valid pre-check leaves the code redeemable.
    if let Some(code) = &body.invite_code {
        let attempt_key = format!("invite:{}:start", rate_limit_ip_key(&ctx.source_ip));
        let email_key = email_rate_limit_key(ctx.cfg, &email);
        let allowed = matches!(
            ctx.store
                .reserve_rate_limit_pair(
                    RateLimitBudget {
                        scope_key: &attempt_key,
                        action: "auth.invite_redeem",
                        limit: u64::from(auth::INVITE_REDEMPTION_MAX_ATTEMPTS),
                        window_secs: auth::INVITE_REDEMPTION_WINDOW_SECS,
                    },
                    RateLimitBudget {
                        scope_key: &email_key,
                        action: "auth.invite_redeem_email",
                        limit: u64::from(auth::INVITE_EMAIL_MAX_ATTEMPTS),
                        window_secs: auth::INVITE_EMAIL_WINDOW_SECS,
                    },
                    ctx.now,
                )
                .await,
            Ok(PairAdmission::Allowed)
        );
        if !allowed {
            return generic_auth();
        }
        let invite = match ctx
            .store
            .invite_by_hash(&auth::hash_invite_code(code))
            .await
        {
            Ok(Some(invite)) => invite,
            Ok(None) => return generic_auth(),
            Err(_) => return internal(),
        };
        // No user row exists yet: validate against a sentinel id that
        // can never equal an issuer, so the hash/spend/revoked/expiry/
        // email-bind/self-invite (by email) predicates all run; the
        // prepared record is discarded.
        let issuer_email = invite.issuer_email.clone();
        let redemption = auth::InviteRedemption {
            user_id: "",
            email: &email,
            issuer_email: issuer_email.as_deref(),
            now: ctx.now,
        };
        if auth::prepare_invite_redemption(invite, code, &redemption).is_err() {
            return generic_auth();
        }
    }
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
        Err(auth::AuthError::RandomnessUnavailable) => return unavailable(),
        Err(_) => return internal(),
    };
    let shape = || {
        json_response(
            200,
            serde_json::json!({
                "challenge": challenge.id.clone(),
                "user_id": fake_user_id(ctx.cfg, &email),
                "rp_id": ctx.cfg.rp_id,
            }),
        )
    };
    // Per-IP admission before user lookup or maintenance. The global budget
    // applies to outbound mail only, never to auth challenge issuance.
    if let Err(retry_after) = ctx.admit_auth_start(req, &email).await {
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
                    return shape();
                }
                Err(_) => return internal(),
            }
        }
        None => {
            // Fail closed on randomness: no id means no user row, no
            // challenge insert, no mail. A 503 (not a stand-in 200) is
            // correct here — the request minted nothing.
            let Ok(user_id) = random_id::<16>() else {
                return unavailable();
            };
            let user = UserRecord {
                id: user_id,
                email: email.clone(),
                created_at: ctx.now,
            };
            match ctx.store.insert_user(&user).await {
                Ok(()) => user,
                // Unique-email race: another request just claimed the row,
                // indistinguishable from a taken account by design.
                Err(_) => return shape(),
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
        return shape();
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
                    return shape();
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
                    return shape();
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
                return shape();
            }
            Err(_) => return internal(),
        }
    }
    // Email-ownership proof and magic login share one atomic per-email
    // issuance budget, so parallel requests cannot oversend.
    ctx.send_magic_link_if_allowed(req, &user.id, &email).await;
    shape()
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
    // nothing — the transactional claims below still gate enrollment).
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
    // code step). Single-use is enforced by the conditional store claim,
    // not this snapshot. All claims and enrollment commit together.
    let invite_redemption = if ctx.cfg.invites_required || body.invite_code.is_some() {
        // Atomic redemption budgets BEFORE the lookup, claimed as one
        // all-or-none pair: the per-client key (10/300s, keyed by client
        // prefix and account, per `auth::invite_attempt_allowed`) and
        // the per-email key (5/hour), so rotating IPs or pre-created
        // rows cannot multiply guesses against one invited address.
        // Denial is the generic error — it never reveals which limit
        // fired (no throttle oracle).
        let attempt_key = format!("invite:{}:{}", rate_limit_ip_key(&ctx.source_ip), user_id);
        let email_key = email_rate_limit_key(ctx.cfg, &magic.email);
        let allowed = matches!(
            ctx.store
                .reserve_rate_limit_pair(
                    RateLimitBudget {
                        scope_key: &attempt_key,
                        action: "auth.invite_redeem",
                        limit: u64::from(auth::INVITE_REDEMPTION_MAX_ATTEMPTS),
                        window_secs: auth::INVITE_REDEMPTION_WINDOW_SECS,
                    },
                    RateLimitBudget {
                        scope_key: &email_key,
                        action: "auth.invite_redeem_email",
                        limit: u64::from(auth::INVITE_EMAIL_MAX_ATTEMPTS),
                        window_secs: auth::INVITE_EMAIL_WINDOW_SECS,
                    },
                    ctx.now,
                )
                .await,
            Ok(PairAdmission::Allowed)
        );
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
        // The snapshot prepare validates every refusal class with the
        // one generic error (unknown/expired/revoked/email-mismatch/
        // self-invite are indistinguishable) and stamps the spend the
        // conditional claim will bind. It grants nothing on its own.
        let issuer_email = invite.issuer_email.clone();
        let redemption = auth::InviteRedemption {
            user_id: &user_id,
            email: &magic.email,
            issuer_email: issuer_email.as_deref(),
            now: ctx.now,
        };
        let prepared = match auth::prepare_invite_redemption(invite, &code, &redemption) {
            Ok(prepared) => prepared,
            Err(_) => return generic_auth(),
        };
        // The `invite.redeemed` audit rides the enrollment transaction
        // inside `complete_registration`, so it can never be separated
        // from the claim it records. It references the invite by row
        // id only — no code or hash material.
        let audit = vault::audit_event_extended(
            prepared.issued_by.as_deref().unwrap_or("system"),
            auth::INVITE_REDEEMED_AUDIT_ACTION,
            true,
            &ctx.source_ip,
            ctx.now,
            Some(&user_id),
            serde_json::json!({
                "invite_id": if prepared.id > 0 {
                    serde_json::json!(prepared.id)
                } else {
                    serde_json::Value::Null
                },
            }),
        );
        Some((prepared, audit))
    } else {
        None
    };
    let (invite_to_consume, invite_audit) = match invite_redemption.as_ref() {
        Some((invite, audit)) => (Some(invite), Some(audit)),
        None => (None, None),
    };
    // Preserve the RP/origin/expiry snapshot validation; the transaction
    // below must still win every conditional claim before enrollment.
    let challenge = match auth::verify_and_consume_challenge(
        row.challenge,
        &ctx.cfg.rp_id,
        &ctx.cfg.origin,
        ctx.now,
    ) {
        Ok(challenge) => challenge,
        Err(_) => return generic_auth(),
    };
    let stored = PasskeyCredential {
        cred_id_b64url: body.credential_id.clone(),
        user_id: user_id.clone(),
        public_key: credential.public_key_cose.clone(),
        sign_count: ENROLLMENT_SIGN_COUNT,
        registered_at: ctx.now,
    };
    // Claims and credential insertion stand or fall together. Throttle
    // reservations above intentionally survive failed attempts.
    match ctx
        .store
        .complete_registration(
            &challenge,
            &magic,
            invite_to_consume,
            invite_audit,
            &stored,
            ctx.now,
        )
        .await
    {
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
        Err(auth::AuthError::RandomnessUnavailable) => return unavailable(),
        Err(_) => return internal(),
    };
    // Per-IP admission before maintenance or lookup: unknown emails would
    // otherwise mint unbounded unbound challenge rows. Denial returns the
    // same fixed-shape 200 with an unstored challenge.
    if ctx.admit_auth_start(req, &email).await.is_err() {
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
        Err(auth::AuthError::RandomnessUnavailable) => return unavailable(),
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
    if ctx.admit_auth_start(req, &email).await.is_err() {
        return json_response(200, serde_json::json!({"ok": true}));
    }
    // Identical response whether or not the account exists or the atomic
    // per-email issuance budget has been exhausted.
    if let Ok(Some(user)) = ctx.store.user_by_email(&email).await {
        ctx.send_magic_link_if_allowed(req, &user.id, &email).await;
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
        match ctx.authenticate(req, false).await {
            Ok((claims, _)) if claims.user_id != record.user_id => return generic_auth(),
            Ok(_) => {}
            // A storage failure inside `authenticate` (its 5xx results, e.g.
            // the `session_by_id` lookup) is NOT an absent or invalid
            // cookie: the existing session's owner is unproven, so fail
            // closed without consuming the token instead of letting a
            // transient lookup error silently replace that session.
            Err(resp) if resp.status >= 500 => return resp,
            Err(_) => {}
        }
    }
    // Invite-required posture: a never-admitted account is a signup
    // in progress, and its emailed token (sent by register/start as
    // the inbox proof) must not mint a session on this route — that
    // would complete a signup with no invite consumed. Refuse
    // generically WITHOUT spending the token, so the legitimate
    // enrollment at register/finish can still use it. "Never
    // admitted" is the full reaper-protected predicate (no
    // credential AND no prior session AND no vault AND member role),
    // NOT mere credential-lessness: magic-only accounts are an
    // established state, and gating on credentials alone would lock
    // those members out the moment invites flip on, with
    // register/finish demanding an invite they never needed.
    if ctx.cfg.invites_required {
        match ctx.store.account_admitted(&record.user_id).await {
            Ok(false) => return generic_auth(),
            Ok(true) => {}
            Err(_) => return internal(),
        }
    }
    // Pure validation is not authentication: prepare everything that can fail
    // before the database claim, then commit the claim AND session together.
    // No cookie or success audit may escape before that transaction commits.
    let prepared = match auth::consume_magic_link(record, &body.token, ctx.now) {
        Ok(prepared) => prepared,
        Err(_) => return generic_auth(),
    };
    let (token, session) = match ctx.prepare_session(&prepared.user_id) {
        Ok(prepared) => prepared,
        Err(resp) => return resp,
    };
    // `ctx.now` is the request-admission timestamp; the production store
    // re-evaluates expiry against the database clock when its claim
    // executes (documented on the trait), so time spent in admission and
    // lookup cannot push a redemption past the link's stated expiry. The
    // success audit rides the same call (inside the production
    // transaction), so nothing but response construction follows the
    // commit that spends the link.
    let audit = vault::audit_event(
        &prepared.user_id,
        "auth.magic",
        true,
        &ctx.source_ip,
        ctx.now,
    );
    match ctx
        .store
        .redeem_magic_link_and_audit(&prepared.used_record, ctx.now, &session, &audit)
        .await
    {
        Ok(true) => {}
        Ok(false) => return generic_auth(),
        Err(_) => return internal(),
    }
    // The token holder is now signed in as this account; echo only its email.
    let mut resp = json_response(
        200,
        serde_json::json!({"ok": true, "email": prepared.used_record.email}),
    );
    resp.set_cookie = Some(session::set_cookie_header(
        &token,
        session.expires_at.saturating_sub(ctx.now),
    ));
    resp
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRefreshBody {
    retry_key: Option<String>,
}

/// Body-independent opt-in marker. A lengthless upload hidden as EOF must not
/// downgrade a bound refresh to the ordinary empty-body rotation protocol.
pub const BOUND_REFRESH_CONTENT_TYPE: &str = "application/vnd.nntune.session-refresh-bound+json";

/// Runtime-only transport guard, before `handle` and all session mutations.
/// Keep metadata outside Request so existing transport-free callers retain the
/// core API while HTTP clients must explicitly mark the bound protocol.
pub fn refresh_transport_rejection(req: &Request, content_type: Option<&str>) -> Option<Response> {
    if req.method != "POST" || req.path != "/v1/session/refresh" {
        return None;
    }
    let marked = content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| {
            media_type
                .trim()
                .eq_ignore_ascii_case(BOUND_REFRESH_CONTENT_TYPE)
        });
    let has_key =
        parse_body::<SessionRefreshBody>(req).is_some_and(|body| body.retry_key.is_some());
    if marked != has_key {
        return Some(generic_auth());
    }
    None
}

/// Optional lost-response recovery protocol. This is NOT ordinary request
/// authentication: the stale cookie plus the separately retained client secret
/// can ONLY retrieve the already committed successor via the atomic DB claim.
async fn session_refresh_with_retry<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    retry_key: String,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.origin.as_deref() != Some(ctx.cfg.origin.as_str()) {
        return generic_auth();
    }
    let Some(token) = session_cookie_value(req) else {
        return generic_auth();
    };
    let keys = ctx.cfg.session_keys();
    let previous = match session::verify_session_dual(&keys, &token, ctx.now) {
        Ok(claims) => claims,
        Err(error) => return session_error_response(error),
    };
    let retry_hash = match session::refresh_retry_hash(&token, &retry_key) {
        Ok(hash) => hash,
        Err(_) => return generic_auth(),
    };
    let candidate = match session::rotate_session(&keys, &token, ctx.now, ctx.cfg.session_ttl_secs)
        .and_then(|next| session::verify_session(&ctx.cfg.session_secret, &next, ctx.now))
    {
        Ok(claims) => claims,
        Err(error) => return session_error_response(error),
    };
    let committed = match ctx
        .store
        .rotate_session_with_retry(&previous, &candidate, &retry_hash, ctx.now)
        .await
    {
        Ok(Some(claims)) => claims,
        Ok(None) => return generic_auth(),
        Err(_) => return internal(), // Keep the original cookie and retry key.
    };
    // Defense in depth against an adapter returning unrelated row metadata.
    if committed.session_id != previous.session_id
        || committed.user_id != previous.user_id
        || committed.created_at != previous.created_at
        || committed.rotation_counter != candidate.rotation_counter
    {
        return internal();
    }
    let new_token = match session::sign_refreshed_claims(&ctx.cfg.session_secret, &committed) {
        Ok(token) => token,
        Err(error) => return session_error_response(error),
    };
    if session::verify_session(&ctx.cfg.session_secret, &new_token, ctx.now).is_err() {
        return internal();
    }
    let mut response = json_response(200, serde_json::json!({"ok": true}));
    response.set_cookie = Some(session::set_cookie_header(
        &new_token,
        committed.expires_at.saturating_sub(ctx.now),
    ));
    response
}

async fn session_refresh<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Empty bodies and `{}` preserve the strict one-shot rotation protocol.
    // Recovery requires an explicit, separately bound retry secret.
    if !req.body.is_empty() {
        let Some(body) = parse_body::<SessionRefreshBody>(req) else {
            return generic_auth();
        };
        if let Some(retry_key) = body.retry_key {
            return session_refresh_with_retry(ctx, req, retry_key).await;
        }
    }
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
    // Conditional revoke: concurrent logouts of the same session race here
    // after both authenticating on the pre-revoke snapshot. Only the winner
    // transitions the row and audits; the loser still clears its (now dead)
    // cookie but writes no second `session.logout` row.
    match ctx
        .store
        .revoke_single_session(&claims.session_id, &claims.user_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            let mut resp = json_response(200, serde_json::json!({"ok": true}));
            resp.set_cookie = Some(session::clear_cookie_header());
            return resp;
        }
        Err(_) => return internal(),
    }
    ctx.audit(&claims.user_id, session::LOGOUT_AUDIT_ACTION, true)
        .await;
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
    // ADR 0010: one committed event covers both sessions and credentials.
    // On failure neither sweep commits; retain the cookie so the caller can
    // retry. Audit success only after the store reports the committed result.
    let revoked_native = match ctx
        .store
        .revoke_sessions_and_native_credentials(&claims.user_id, ctx.now)
        .await
    {
        Ok(revoked) => revoked,
        Err(_) => return internal(),
    };
    ctx.audit(&claims.user_id, "session.revoke_all", true).await;
    if revoked_native > 0 {
        ctx.audit(&claims.user_id, "native_credentials_revoked_all", true)
            .await;
    }
    let mut resp = json_response(200, serde_json::json!({"ok": true}));
    resp.set_cookie = Some(session::clear_cookie_header());
    resp
}

/// `GET /v1/plans`: every plan row for pricing display. Prices/quotas are
/// plan *data* read from the `plans` table (the one runtime source, ADR
/// 0004); the reserved `free` row (and any unrecognised id, fail-closed)
/// is marked non-selectable so no client offers it at checkout. Session
/// only: the signed-out pricing page renders its static fallback mirror.
async fn plans_list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (_claims, _token) = match ctx.authenticate(req, false).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match ctx.store.plans().await {
        Ok(rows) => {
            let plans: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    let selectable = crate::plan_limits::PlanId::parse_id(&row.id)
                        .is_some_and(|plan| plan != crate::plan_limits::PlanId::Free);
                    serde_json::json!({
                        "id": row.id,
                        "priceCents": row.price_cents,
                        "streamHoursMonth": row.stream_hours_month,
                        "maxConcurrentStreams": row.max_concurrent_streams,
                        "selectable": selectable,
                    })
                })
                .collect();
            json_response(200, serde_json::json!({ "plans": plans }))
        }
        Err(_) => internal(),
    }
}

/// Resolve the catalogue caller: a gate-authorized bearer wins, otherwise
/// the session cookie, otherwise the generic 401. The gate already 404s
/// presented-but-invalid bearers, so reaching this function with
/// `bearer = None` means no credential was presented at all — the 401
/// discloses nothing (every catalogue path is a known, scope-mapped route).
async fn catalogue_caller<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Result<String, Response>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if let Some(user) = bearer {
        if !user.is_empty() {
            return Ok(user.to_string());
        }
    }
    match ctx.authenticate(req, false).await {
        Ok((claims, _token)) => Ok(claims.user_id),
        Err(resp) => Err(resp),
    }
}

/// Serialize a catalogue page, failing closed on the (in practice
/// unreachable) encoding error rather than panicking.
fn catalogue_response<T: serde::Serialize>(entries: &T) -> Response {
    match serde_json::to_value(entries) {
        Ok(body) => json_response(200, body),
        Err(_) => internal(),
    }
}

/// `GET /v1/library` and the legacy `GET /api/library` alias: the caller's
/// joined ingest ledger as a bare array (the documented `/api/library`
/// shape the M1 journey pins — bare array or `{ items }` with string-`id`
/// rows). Store failure is the generic 500.
async fn catalogue_library<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let user_id = match catalogue_caller(ctx, req, bearer).await {
        Ok(user) => user,
        Err(resp) => return resp,
    };
    catalogue_library_entries(ctx, &user_id).await
}

/// `GET /api/library`: the legacy alias serving the same entries as
/// `GET /v1/library` (the documented M1 shape — bare array or `{ items }`
/// with string-`id` rows). Outside the bearer scope map, so this handler
/// owns authentication exactly like the `/api/ingest` cap guard: a
/// presented `Authorization` header is verified as a session bearer with
/// no session-cookie rescue (a native-REST `ntk_` credential is not a
/// session and fails closed here with the generic 401, never the gate's
/// 404), while a header-absent call falls back to the session cookie.
/// Store failure is the generic 500.
async fn catalogue_library_alias<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let user_id = if req.authorization.is_some() {
        match ctx.authenticate_bearer_session(req).await {
            Ok((claims, _token)) => claims.user_id,
            Err(resp) => return resp,
        }
    } else {
        match ctx.authenticate(req, false).await {
            Ok((claims, _token)) => claims.user_id,
            Err(resp) => return resp,
        }
    };
    catalogue_library_entries(ctx, &user_id).await
}

/// Shared store read plus serialization for the two library routes: the
/// caller arrives authenticated, this only assembles the entries.
async fn catalogue_library_entries<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, user_id: &str) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    match ctx.store.catalogue_library(user_id).await {
        Ok(rows) => {
            let entries: Vec<crate::catalogue::LibraryEntry> =
                rows.iter().map(crate::catalogue::library_entry).collect();
            catalogue_response(&entries)
        }
        Err(_) => internal(),
    }
}

/// `GET /v1/me/usage`: this UTC calendar month's metered stream seconds
/// (the ledger `SUM`, the single authority) plus the reset instant the
/// UI must show wherever usage appears (spec §3.2). Display only —
/// enforcement reads the same fold server-side at grant issuance.
async fn usage_get<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
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
    let period = crate::ledger::period_month_for_timestamp(ctx.now);
    match ctx
        .store
        .stream_seconds_month(&claims.user_id, &period)
        .await
    {
        Ok(stream_seconds) => json_response(
            200,
            serde_json::json!({
                "period": period,
                "streamSecondsMonth": stream_seconds,
                "resetsAt": crate::ledger::iso8601_utc(
                    crate::ledger::next_month_start_unix(ctx.now) as i64
                ),
            }),
        ),
        Err(_) => internal(),
    }
}

/// `GET /v1/artists`: the caller's member artist collection as a bare
/// array (only artists of the caller's member releases).
/// Authenticated callers only: anonymous callers answer 401 before any
/// store read.
async fn catalogue_artists<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let user_id = match catalogue_caller(ctx, req, bearer).await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };
    match ctx.store.catalogue_artists(&user_id).await {
        Ok(rows) => {
            let entries: Vec<crate::catalogue::ArtistResponse> =
                rows.iter().map(crate::catalogue::artist_response).collect();
            catalogue_response(&entries)
        }
        Err(_) => unavailable(),
    }
}

/// `GET /v1/me/subscription`: the caller's subscription display state.
/// Always 200 for a session (a missing row is the `no_subscription`
/// state, not a 404): `state` is derived server-side from the ADR 0005
/// effective-plan predicate, `status` is the stored provider status
/// verbatim (null when no live row), and no Stripe ids ever leave the
/// server. Display only — the gate re-derives entitlement at grant
/// issuance and never trusts this DTO.
async fn subscription_get<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
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
    let (row, ever_subscribed) = match ctx.store.subscription_snapshot(&claims.user_id).await {
        Ok(snapshot) => snapshot,
        Err(_) => return internal(),
    };
    let view = row.as_ref().map(|row| crate::billing::SubscriptionView {
        plan_id: &row.plan_id,
        status: &row.status,
        cancel_at_period_end: row.cancel_at_period_end,
        paid_through_at_unix: row.paid_through_at_unix,
        grace_until_unix: row.grace_until_unix,
        current_period_end_unix: row.current_period_end_unix,
    });
    let now_unix = i64::try_from(ctx.now).unwrap_or(i64::MAX);
    let state = crate::billing::account_state(view, ever_subscribed, now_unix);
    json_response(
        200,
        serde_json::json!({
            "state": state.as_str(),
            "planId": row.as_ref().map(|row| row.plan_id.as_str()),
            "status": row.as_ref().map(|row| row.status.as_str()),
            "currentPeriodEnd": row
                .as_ref()
                .and_then(|row| row.current_period_end_unix)
                .map(crate::ledger::iso8601_utc),
            "graceUntil": row
                .as_ref()
                .and_then(|row| row.grace_until_unix)
                .map(crate::ledger::iso8601_utc),
            "cancelAtPeriodEnd": row.as_ref().is_some_and(|row| row.cancel_at_period_end),
        }),
    )
}

/// `GET /v1/releases`: the caller's member release collection as a bare
/// array. Authentication posture matches [`catalogue_artists`].
async fn catalogue_releases<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let user_id = match catalogue_caller(ctx, req, bearer).await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };
    match ctx.store.catalogue_releases(&user_id).await {
        Ok(rows) => {
            let entries: Vec<crate::catalogue::ReleaseResponse> = rows
                .iter()
                .map(crate::catalogue::release_response)
                .collect();
            catalogue_response(&entries)
        }
        Err(_) => unavailable(),
    }
}

/// `GET /v1/tracks`: the caller's member track-occurrence collection as a
/// bare array (exact member pairs only). Authentication posture matches
/// [`catalogue_artists`].
async fn catalogue_tracks<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let user_id = match catalogue_caller(ctx, req, bearer).await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };
    match ctx.store.catalogue_tracks(&user_id).await {
        Ok(rows) => {
            let entries: Vec<crate::catalogue::TrackResponse> =
                rows.iter().map(crate::catalogue::track_response).collect();
            catalogue_response(&entries)
        }
        Err(_) => unavailable(),
    }
}

/// `GET /v1/releases/{mbid}`: one member release plus its tracks in album
/// order. Only single-segment, well-formed MBIDs reach the store (anything
/// else — empty, nested, malformed — is the plain 404, indistinguishable
/// from unknown); unknown and non-member releases are 404; store failure is
/// the opaque 503 service-unavailable envelope.
async fn catalogue_release<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let user_id = match catalogue_caller(ctx, req, bearer).await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };
    let mbid = match req.path.strip_prefix("/v1/releases/") {
        Some(rest)
            if !rest.is_empty()
                && !rest.contains('/')
                && crate::catalogue::is_catalogue_mbid(rest) =>
        {
            rest
        }
        _ => return not_found(),
    };
    let release = match ctx.store.catalogue_release(&user_id, mbid).await {
        Ok(release) => release,
        Err(_) => return unavailable(),
    };
    let Some(release) = release else {
        return not_found();
    };
    match ctx.store.catalogue_release_tracks(&user_id, mbid).await {
        Ok(tracks) => catalogue_response(&crate::catalogue::assemble_release_detail(
            &release, &tracks,
        )),
        Err(_) => unavailable(),
    }
}

#[path = "subsonic_handlers.rs"]
mod subsonic_handlers;

// --- Native REST credentials (ADR 0010) ------------------------------------------------------
//
// Session-authenticated issuance surface only: `POST /v1/credentials`
// (fresh session, 600 s privilege gate — the step-up for this credential
// class), `GET /v1/credentials` (ordinary session), `DELETE
// /v1/credentials/{id}` (fresh session: revocation is privilege-changing).
// The route map marks all three `SessionOnly`, so bearer keys can never
// reach them; the `Authorization` header is ignored here exactly like
// cookies are ignored on bearer routes. The plaintext secret is rendered
// into the mint response once and never stored, listed, logged, or
// audited — audit rows carry the credential id only.

/// Mint request: human label for listing, exact ADR scope strings, and a
/// TTL in seconds (clamped fail-safe to the 365-day cap at issue, so even
/// `u64::MAX` can never mint a forever key).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsMintBody {
    label: Option<String>,
    scopes: Vec<String>,
    ttl_secs: u64,
}

/// Listing label: trimmed, at most 64 chars (mirrors the migration 0019
/// `CHECK`), no control characters. Empty means "no label". `None`
/// rejects the request.
fn normalize_credential_label(raw: Option<&str>) -> Option<String> {
    let label = raw.unwrap_or("").trim();
    if label.len() > 64 || label.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(label.to_string())
}

fn validation_failed(message: &str) -> Response {
    let error = crate::api_error::ApiError::validation_failed(message);
    Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

fn scope_forbidden() -> Response {
    // Names the outcome only: authenticated, wrong scope.
    let error = crate::api_error::ApiError::by_code("forbidden", "insufficient scope");
    Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

fn not_found() -> Response {
    let error = crate::api_error::ApiError::not_found("not found");
    Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

async fn credentials_mint<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Issuance-time freshness IS the step-up (ADR 0010): a stale session
    // is denied with the registered `reauth_required` response before any
    // key material exists. The origin pin inside `authenticate_privileged`
    // is the CSRF guard for this mutation.
    let (claims, _token) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let Some(body) = parse_body::<CredentialsMintBody>(req) else {
        return validation_failed("invalid request");
    };
    let Some(label) = normalize_credential_label(body.label.as_deref()) else {
        return validation_failed("invalid label");
    };
    // Zero TTL would mint a credential already expired at issuance
    // (`expires_at == now`) while still returning a one-time secret (ADR
    // 0010 requires meaningful expiry): reject before any row, audit entry,
    // or secret exists.
    if body.ttl_secs == 0 {
        return validation_failed("invalid ttl");
    }
    // Exact scope strings only: unknown, cross-surface (`"subsonic"`),
    // case or whitespace variants reject — the allowlist never trims or
    // folds.
    let mut scopes = Vec::with_capacity(body.scopes.len());
    for name in &body.scopes {
        match crate::native_rest::Scope::parse(name) {
            Some(scope) => scopes.push(scope),
            None => return validation_failed("unknown scope"),
        }
    }
    // Raw length past the defined vocabulary can only be duplicates (only
    // `ALL_SCOPES.len()` distinct scopes exist): reject before any row,
    // audit entry, or secret exists, so an arbitrarily long duplicate list
    // can never mint.
    if scopes.len() > crate::native_rest::ALL_SCOPES.len() {
        return validation_failed("too many scopes");
    }
    // Duplicates within the cap collapse to first-seen order, so the 201
    // body and the stored row carry the same deduplicated set.
    {
        let mut deduped = Vec::with_capacity(scopes.len());
        for scope in scopes {
            if !deduped.contains(&scope) {
                deduped.push(scope);
            }
        }
        scopes = deduped;
    }
    // Unprovisioned keyring fails closed per-request (generic 500, never
    // which secret is missing): without a pepper there is nothing to HMAC
    // with, so no credential may be minted.
    let peppers = ctx.cfg.native_rest_peppers();
    let Some(pepper) = peppers.first() else {
        return internal();
    };
    // The only mint failure is unavailable randomness: fail closed 503.
    // No credential row is persisted and no secret is rendered.
    let issued = match crate::native_rest::issue_credential(
        &claims.user_id,
        &scopes,
        body.ttl_secs,
        ctx.now,
        pepper,
    ) {
        Ok(issued) => issued,
        Err(_) => return unavailable(),
    };
    let id = issued.record.id.clone();
    let row = NativeCredentialRow {
        record: issued.record,
        label,
        created_from_ip: ctx.source_ip.clone(),
        last_used_at: None,
    };
    if ctx
        .store
        .insert_native_credential(&row, &claims.session_id)
        .await
        .is_err()
    {
        // A fenced insert (account being deleted) is the typed 409,
        // never the generic 500; anything else stays generic so a probe
        // failure cannot leak a new shape. Mirrors `vault_put` (ADR 0002
        // rule 3): the production statement only lands while the owner's
        // `deleting_at` is NULL, so a fresh session during the 24 h grace
        // cannot mint a new long-lived bearer path behind the marker.
        if matches!(ctx.store.account_deleting(&claims.user_id).await, Ok(true)) {
            return account_deleting_response();
        }
        return internal();
    }
    // Audit names the id only — never the secret, the HMAC, or the IP.
    ctx.audit(
        &claims.user_id,
        &format!("native_credential_created:{id}"),
        true,
    )
    .await;
    // The one and only time the plaintext exists outside the client's
    // hands: this 201 body. Nothing below stores, logs, or re-renders it.
    Response {
        status: 201,
        body: serde_json::json!({
            "id": id,
            "secret": issued.display_once,
            "key_prefix": row.record.key_prefix,
            "scopes": scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "expires_at": row.record.expires_at,
        })
        .to_string(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

/// One listed credential: routing and lifecycle facts only. There is no
/// `secret` (or HMAC) key on this shape by construction — a serializer
/// cannot leak what the struct does not carry.
fn listed_credential(row: &NativeCredentialRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.record.id,
        "label": row.label,
        "key_prefix": row.record.key_prefix,
        "scopes": row.record.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "created_at": row.record.created_at,
        "expires_at": row.record.expires_at,
        "revoked_at": row.record.revoked_at,
        "last_used_at": row.last_used_at,
    })
}

async fn credentials_list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
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
    match ctx.store.native_credentials_for_user(&claims.user_id).await {
        Ok(rows) => json_response(
            200,
            serde_json::json!({
                "credentials": rows.iter().map(listed_credential).collect::<Vec<_>>(),
            }),
        ),
        Err(_) => internal(),
    }
}

async fn credentials_revoke<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Fresh session like issuance: revocation is privilege-changing, and
    // the origin pin is the CSRF guard.
    let (claims, _token) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    // Single-segment id only: deeper shapes are not credential ids and
    // stay 404, so no id parser can be tricked by a path.
    let id = req.path.strip_prefix("/v1/credentials/").unwrap_or("");
    if id.is_empty() || id.contains('/') {
        return not_found();
    }
    match ctx
        .store
        .revoke_native_credential(id, &claims.user_id, ctx.now)
        .await
    {
        // Owner-scoped and sticky: unknown or foreign ids report `false`
        // and answer the same 404 (no existence oracle across users).
        // Revoking an already-revoked row still answers `ok: true`
        // (idempotent), keeping the original timestamp in the store.
        Ok(true) => {
            ctx.audit(
                &claims.user_id,
                &format!("native_credential_revoked:{id}"),
                true,
            )
            .await;
            json_response(200, serde_json::json!({"ok": true}))
        }
        Ok(false) => not_found(),
        Err(_) => internal(),
    }
}

/// Bearer gate for `RouteDecision::Bearer` routes: header-only,
/// cookie-blind. The `Authorization` header is the ONLY input — no
/// session cookie is consulted here (no session-cookie fallback), and
/// there is intentionally no query-string credential reader (query
/// strings land in logs).
///
/// Gates run in order: header shape, display-form shape, per-IP lookup
/// budget, then the HMAC/DB lookup. A malformed shape fails before any
/// HMAC or database work (unauthenticated bearer routes were plain 404s
/// with no DB work before this credential class, so junk tokens must not
/// buy Postgres connections). The lookup budget is claimed before the
/// first query and its denial collapses to the same plain 404 as any
/// other bearer failure (no throttle oracle), mirroring the finish-route
/// lookup budget.
///
/// Every unauthenticated outcome — malformed, unknown, tampered,
/// revoked, expired, or an unprovisioned keyring — is the same plain 404
/// a header-absent caller receives (uniform posture: a presented bearer
/// must not disclose route existence, and is never rescued by a cookie).
/// Only a valid credential lacking the route's scope is distinct: the
/// names-only 403. Successful use records `last_used_at` best-effort —
/// its outcome never changes the decision, like audit writes.
async fn authorize_bearer<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    authorization: Option<&str>,
    required: crate::native_rest::Scope,
) -> Result<NativeCredentialRow, Response>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    use crate::native_rest::{
        authorize, credential_hmac, extract_bearer_key, is_plausible_credential_shape,
        CredentialError,
    };
    let token = match extract_bearer_key(authorization) {
        Ok(token) => token,
        Err(_) => return Err(not_found()),
    };
    // Shape before HMAC/DB: random Authorization values cost no crypto
    // beyond the parse and no store work at all.
    if !is_plausible_credential_shape(token) {
        return Err(not_found());
    }
    // Per-IP lookup budget BEFORE any credential DB work: a flood of
    // well-formed random `ntk_` tokens shares one pool (client /64 or
    // full address), claimed as one all-or-none pair so a burst-denied
    // attempt burns neither budget. Denial is the plain 404.
    let ip_key = rate_limit_ip_key(&ctx.source_ip);
    let admitted = matches!(
        ctx.store
            .reserve_rate_limit_pair(
                RateLimitBudget {
                    scope_key: &ip_key,
                    action: "native_rest.bearer_lookup_ip_min",
                    limit: BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_MIN,
                    window_secs: BEARER_LOOKUP_WINDOW_SECS,
                },
                RateLimitBudget {
                    scope_key: &ip_key,
                    action: "native_rest.bearer_lookup_ip_hour",
                    limit: BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_HOUR,
                    window_secs: BEARER_LOOKUP_HOUR_WINDOW_SECS,
                },
                ctx.now,
            )
            .await,
        Ok(PairAdmission::Allowed)
    );
    if !admitted {
        return Err(not_found());
    }
    let peppers = ctx.cfg.native_rest_peppers();
    // Dual-pepper lookup: the presented secret is HMACed under each listed
    // version and looked up by the composite identity, so a transitioning
    // record authorizes while both versions are listed.
    let mut found = None;
    for pepper in &peppers {
        let hmac = credential_hmac(token, pepper.bytes);
        match ctx
            .store
            .native_credential_by_hmac(pepper.version, &hmac)
            .await
        {
            Ok(Some(row)) => {
                found = Some(row);
                break;
            }
            Ok(None) => {}
            Err(_) => return Err(not_found()),
        }
    }
    let Some(row) = found else {
        return Err(not_found());
    };
    // The credential IS the identity on bearer routes: the owner check
    // runs against the row's own owner, and every downstream path
    // re-checks ownership (defense in depth, ADR 0010 §6).
    match authorize(
        &row.record,
        token,
        required,
        ctx.now,
        &row.record.user_id,
        &peppers,
    ) {
        Ok(()) => {}
        Err(CredentialError::Unauthenticated) => return Err(not_found()),
        Err(CredentialError::Forbidden) => return Err(scope_forbidden()),
        // Unreachable: `authorize` is pure verification and never mints.
        // Fail closed 503 if that ever changes.
        Err(CredentialError::RandomnessUnavailable) => return Err(unavailable()),
    }
    let _ = ctx
        .store
        .record_native_credential_use(&row.record.id, ctx.now)
        .await;
    Ok(row)
}

/// Staging-only emailed-code retrieval seam (`POST /api/test-codes`).
/// Returns the issued code for an `example.test` mailbox without minting
/// an account (read-only: no users, challenges, or magic rows are created
/// here). Gates run in order — flag armed plus bearer staged (else the
/// plain unknown-route 404, so the seam's existence is not enumerable
/// from its posture), then the `Authorization: Bearer <QA_AUTH_TOKEN>`
/// header present and constant-time equal (else a names-only 401; the
/// bearer is a secret request header rather than an ambient credential,
/// so no `Origin` pin is needed as a cross-site forgery cannot present
/// it), then a well-formed `example.test` mailbox and a captured
/// unexpired code (else 404). Every failure names only the gate, never
/// the mailbox, the token, or which check refused.
async fn qa_test_codes<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let not_found = || {
        let error = crate::api_error::ApiError::not_found("not found");
        Response {
            status: error.status,
            body: error.body_json(),
            set_cookie: None,
            retry_after: None,
            extra_headers: Vec::new(),
        }
    };
    let Some(expected) = ctx
        .cfg
        .qa_auth_token
        .as_deref()
        .filter(|_| ctx.cfg.qa_code_seam)
    else {
        return not_found();
    };
    let presented = req
        .authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "));
    if !qa_bearer_ok(presented, expected) {
        return json_response(401, serde_json::json!({"error": "unauthorized"}));
    }
    let Some(body) = parse_body::<EmailBody>(req) else {
        return not_found();
    };
    let Some(email) = normalize_email(&body.email) else {
        return not_found();
    };
    if !is_example_test_mailbox(&email) {
        return not_found();
    }
    let stored = match ctx.store.qa_issued_code(&email).await {
        Ok(stored) => stored,
        Err(_) => return internal(),
    };
    match stored {
        Some(code) if code.issued_at.saturating_add(auth::MAGIC_LINK_TTL_SECS) > ctx.now => {
            json_response(200, serde_json::json!({"magic_token": code.token}))
        }
        _ => not_found(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QaTestAccountBody {
    email: String,
    plan: Option<String>,
}

/// Staging-only test-account mint seam (`POST /api/test-accounts`).
/// Creates a fresh `example.test` account and returns its email plus a
/// stored session (the `__Host-nntune-session` cookie value) so the
/// staging E2E can act as that account without the passkey ceremony.
/// Same gates as [`qa_test_codes`] and in the same order: flag armed plus
/// bearer staged (else the unknown-route 404), constant-time bearer (else
/// a names-only 401), then a well-formed synthetic mailbox (else 404).
/// Two plans mint: `plan` absent or `"free"` provisions a plain account,
/// while `"overlimit"` additionally seeds the account's daily ingest usage
/// at the free cap so the `POST /api/ingest` plan-cap guard (see
/// [`ingest_cap_guard`]) honestly refuses it — the plan string itself is
/// never stored, and a capped account is never faked. Any other plan is a
/// fixed 422. An address that already has an account is a fixed 422 too,
/// never handed a new session (mint, not sign-in). Nothing here echoes the
/// mailbox or token.
async fn qa_test_accounts<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let error_response = |error: crate::api_error::ApiError| Response {
        status: error.status,
        body: error.body_json(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    };
    let not_found = || error_response(crate::api_error::ApiError::not_found("not found"));
    let refused =
        |message: &str| error_response(crate::api_error::ApiError::validation_failed(message));
    let Some(expected) = ctx
        .cfg
        .qa_auth_token
        .as_deref()
        .filter(|_| ctx.cfg.qa_code_seam)
    else {
        return not_found();
    };
    let presented = req
        .authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "));
    if !qa_bearer_ok(presented, expected) {
        return json_response(401, serde_json::json!({"error": "unauthorized"}));
    }
    let Some(body) = parse_body::<QaTestAccountBody>(req) else {
        return not_found();
    };
    let Some(email) = normalize_email(&body.email) else {
        return not_found();
    };
    if !is_example_test_mailbox(&email) {
        return not_found();
    }
    if !matches!(
        body.plan.as_deref(),
        None | Some("free") | Some("overlimit")
    ) {
        return refused("unsupported plan");
    }
    let overlimit = matches!(body.plan.as_deref(), Some("overlimit"));
    match ctx.store.user_by_email(&email).await {
        Ok(None) => {}
        Ok(Some(_)) => return refused("account exists"),
        Err(_) => return internal(),
    }
    let Ok(user_id) = random_id::<16>() else {
        return unavailable();
    };
    // Everything that can fail without touching the store runs first, so
    // only a failed session or usage-seed write can leave a user row
    // behind; that row holds no session, credential or vault and is
    // removed by the unverified-signup reaper (`unverified_user_filter`),
    // and the E2E never retries the same mailbox. The stored session row
    // is also what exempts a minted account from that reaper.
    let (token, record) = match ctx.prepare_session(&user_id) {
        Ok(prepared) => prepared,
        Err(resp) => return resp,
    };
    let user = UserRecord {
        id: user_id,
        email: email.clone(),
        created_at: ctx.now,
    };
    // Unique-email race with a concurrent mint: the loser simply fails.
    if ctx.store.insert_user(&user).await.is_err() {
        return internal();
    }
    if ctx.store.insert_session(&record).await.is_err() {
        return internal();
    }
    if overlimit {
        // A capped account is an account whose daily ingest count is
        // pre-seeded to the fixed daily ingest limit: the intake guard
        // refuses it without hammering a real path, and the seed (not the
        // plan string, which is never stored) is what the guard reads.
        let cap = u64::from(crate::plan_limits::DAILY_INGEST_LIMIT);
        if ctx
            .store
            .seed_qa_ingest_usage(&user.id, ctx.now, cap)
            .await
            .is_err()
        {
            return internal();
        }
    }
    ctx.audit(&user.id, "qa.test_account_mint", true).await;
    json_response(201, serde_json::json!({"email": email, "session": token}))
}

/// Render a plan-limit denial on the wire (`docs/api-errors.md` 402 /
/// 429 rows): status, names-only body, and the limit + reset headers (plus
/// `retry-after` on 429) the client needs to render the upgrade path.
/// `Response.retry_after` is rendered by the entry as the wire
/// `retry-after` header, so the copy inside `Denial::headers()` is
/// dropped when the field is set — a 429 never carries the header twice.
fn plan_limit_refusal(denial: crate::plan_limits::Denial) -> Response {
    let extra_headers = denial
        .headers()
        .into_iter()
        .filter(|(name, _)| {
            denial.retry_after_secs.is_none() || name != crate::plan_limits::RETRY_AFTER_HEADER
        })
        .collect();
    Response {
        status: denial.status,
        body: denial.body_json(),
        set_cookie: None,
        retry_after: denial.retry_after_secs,
        extra_headers,
    }
}

/// Cap guard on the NZB intake path (`POST /api/ingest`): the caller's
/// session (Bearer transport) is authenticated, today's ingest count
/// is checked against the fixed non-plan daily ingest limit
/// (`plan_limits::DAILY_INGEST_LIMIT`, ADR 0004 revision of 2026-11-03),
/// and a capped caller is refused with the plan-limit wire. Everyone else
/// falls through to the plain 404 — the ingest-write path owns the accept
/// arm and has not landed yet, so this slice only refuses; it never
/// accepts a job. The same limit applies to every caller until the
/// user-ingest retirement removes the intake path. The request body is never inspected: refusal is an
/// identity-plus-usage decision, so a capped caller cannot buy
/// processing by shaping the upload.
async fn ingest_cap_guard<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _token) = match ctx.authenticate_bearer_session(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let ingest_today = match ctx.store.ingest_jobs_today(&claims.user_id, ctx.now).await {
        Ok(usage) => u32::try_from(usage).unwrap_or(u32::MAX),
        Err(_) => return internal(),
    };
    match crate::plan_limits::check_ingest_write(ingest_today).denial() {
        Some(denial) => plan_limit_refusal(denial),
        None => not_found(),
    }
}

// --- Admin surface (migration 0028) ---------------------------------------------------------
//
// Session-only, role-gated by `authenticate_admin`, and non-enumerable
// (TWO-70 A-AC1): the scope map marks every path below `SessionOnly`,
// so bearer credentials never reach these handlers through the gate,
// and the guard answers every non-admin probe — presented bearer,
// missing, bad or non-admin session — with the plain 404. Preflight
// agrees (`options_preflight_served` excludes the whole namespace).
// Role and settings writes land together with their extended audit
// rows in one store transaction that re-reads the actor's role under
// its locks (`AdminWriteOutcome`).

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminRoleBody {
    role: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminSettingBody {
    value: u32,
}

/// `GET /v1/admin/settings` — every typed setting at its effective
/// value (stored row, or the built-in default when no row exists). A
/// stored row that no longer parses fails closed with the generic 500
/// instead of silently reporting the default.
async fn admin_settings_list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if let Err(resp) = ctx.authenticate_admin(req, false).await {
        return resp;
    }
    let rows = match ctx.store.app_settings().await {
        Ok(rows) => rows,
        Err(_) => return internal(),
    };
    let mut settings = serde_json::Map::new();
    for key in crate::admin::ALL_SETTING_KEYS {
        let stored = rows
            .iter()
            .find(|(name, _)| name == key.as_str())
            .map(|(_, value)| value.as_str());
        match key.resolve(stored) {
            Some(value) => {
                settings.insert(key.as_str().to_string(), serde_json::json!(value));
            }
            None => return internal(),
        }
    }
    json_response(200, serde_json::json!({"settings": settings}))
}

/// `PUT /v1/admin/settings/{key}` — write one typed setting. The key
/// must be one of the closed `SettingKey` set and the value must
/// satisfy its bounds; anything else writes nothing.
async fn admin_setting_put<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = match ctx.authenticate_admin(req, true).await {
        Ok(claims) => claims,
        Err(resp) => return resp,
    };
    let Some(raw_key) = req.path.strip_prefix("/v1/admin/settings/") else {
        return not_found();
    };
    if raw_key.is_empty() || raw_key.contains('/') {
        return not_found();
    }
    let Some(key) = crate::admin::SettingKey::parse(raw_key) else {
        return not_found();
    };
    let Some(body) = parse_body::<AdminSettingBody>(req) else {
        return validation_failed("invalid request");
    };
    let Some(value) = key.validate(body.value) else {
        return validation_failed("setting value out of range");
    };
    let rendered = crate::admin::SettingKey::render(value);
    let previous = match ctx
        .store
        .admin_set_app_setting(
            &claims.user_id,
            key.as_str(),
            &rendered,
            &ctx.source_ip,
            ctx.now,
        )
        .await
    {
        Ok(AdminWriteOutcome::Applied(previous)) => previous,
        // The actor lost admin between the guard and the write
        // transaction's re-check: answer as any non-admin is answered.
        Ok(AdminWriteOutcome::ActorNotAdmin | AdminWriteOutcome::SubjectMissing) => {
            return not_found();
        }
        // A settings write has no subject user, so a store reporting
        // the role-write fence here is broken, never a client answer.
        Ok(AdminWriteOutcome::AccountDeleting) => return internal(),
        Err(_) => return internal(),
    };
    // The previous value is reported at its effective resolution: no
    // prior row means the default was in force, and a prior row that
    // no longer parses reports as null rather than echoing corrupt
    // stored text back.
    let previous_value = match previous.as_deref() {
        Some(raw) => key.parse_stored(raw).map(|v| serde_json::json!(v)),
        None => Some(serde_json::json!(key.default_value())),
    };
    json_response(
        200,
        serde_json::json!({
            "key": key.as_str(),
            "value": value,
            "previous_value": previous_value,
        }),
    )
}

/// `PUT /v1/admin/users/{id}/role` — promote or demote one user. An
/// admin can never change their own role through the API: the first
/// admin comes from the reviewed operator step (docs/ops.md), and
/// self-demotion is the one-click path to locking every admin out.
async fn admin_user_role_put<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = match ctx.authenticate_admin(req, true).await {
        Ok(claims) => claims,
        Err(resp) => return resp,
    };
    let Some(rest) = req.path.strip_prefix("/v1/admin/users/") else {
        return not_found();
    };
    let Some((subject_id, "role")) = rest.split_once('/') else {
        return not_found();
    };
    if subject_id.is_empty() || subject_id.contains('/') {
        return not_found();
    }
    let Some(body) = parse_body::<AdminRoleBody>(req) else {
        return validation_failed("invalid request");
    };
    let Some(new_role) = crate::admin::parse_role(&body.role) else {
        return validation_failed("unknown role");
    };
    if subject_id == claims.user_id {
        return validation_failed("admins cannot change their own role");
    }
    let previous = match ctx
        .store
        .admin_set_user_role(
            &claims.user_id,
            subject_id,
            new_role,
            &ctx.source_ip,
            ctx.now,
        )
        .await
    {
        Ok(AdminWriteOutcome::Applied(previous)) => previous,
        // Unknown subject, or the actor stopped being an admin
        // between the guard and the write transaction's re-check —
        // both are the plain 404.
        Ok(AdminWriteOutcome::ActorNotAdmin | AdminWriteOutcome::SubjectMissing) => {
            return not_found();
        }
        // The subject is scheduled for deletion: ADR 0002 rule 3
        // fences the write, answered with the typed 409.
        Ok(AdminWriteOutcome::AccountDeleting) => {
            return account_deleting_response();
        }
        Err(_) => return internal(),
    };
    let Some(previous) = previous else {
        // A role write always replaces a stored role (the subject row
        // exists and `users.role` is NOT NULL); a store reporting
        // otherwise is broken, never a success with a null old role.
        return internal();
    };
    json_response(
        200,
        serde_json::json!({
            "user_id": subject_id,
            "role": new_role,
            "previous_role": previous,
        }),
    )
}

// --- Admin ingest (migration 0034) ---------------------------------------------------------

/// 413 for an admin NZB upload whose body breached the collection cap:
/// the in-handler pre-gate answers oversized admin uploads this way,
/// with the null-job failure shape — no row, no audit, no parse. This
/// shape serves direct-`handle` callers only: the production entry gate
/// (`dispatch_collected` in `worker_entry.rs`) answers `Oversized` with
/// the route-blind uniform 400 (`invalid_request`) before auth, never
/// dispatching, so an oversized body through the entry never reaches
/// this arm.
pub(crate) fn admin_ingest_oversized() -> Response {
    admin_ingest_failure(413, "payload_too_large", "too_large", None)
}

/// One admin ingest failure body: `{job_id, status, code, reason}` —
/// the registry wire code for the status plus the fixed vocabulary
/// reason. Carries no NZB bytes and no parser detail; `job_id` is null
/// exactly when no content was parsed (empty or oversized bodies).
fn admin_ingest_failure(status: u16, code: &str, reason: &str, job_id: Option<&str>) -> Response {
    json_response(
        status,
        serde_json::json!({
            "job_id": job_id,
            "status": "failed",
            "code": code,
            "reason": reason,
        }),
    )
}

/// Redacted job view for a stored admin row: counts and the failure
/// reason only. `deduplicated` marks a POST that converged on a
/// pre-existing row (list/get views never carry it).
fn admin_ingest_view(job: &StoredAdminIngestJob, deduplicated: Option<bool>) -> serde_json::Value {
    match admin_ingest::view_status(&job.db_status) {
        Some("accepted") => {
            let mut view = serde_json::json!({
                "job_id": job.job_id,
                "status": "accepted",
                "accepted_at": job.accepted_at,
                "nzb_bytes": job.nzb_bytes,
                "file_count": job.file_count,
                "segment_count": job.segment_count,
                "has_original": job.has_original,
                "uploader_user_id": job.uploader_user_id,
            });
            if let Some(deduplicated) = deduplicated {
                view["deduplicated"] = deduplicated.into();
            }
            view
        }
        _ => {
            let mut view = serde_json::json!({
                "job_id": job.job_id,
                "status": "failed",
                "code": job
                    .failure_reason
                    .as_deref()
                    .and_then(admin_ingest::FailureReason::parse)
                    .map(|reason| reason.wire_code())
                    .unwrap_or("invalid_request"),
                "reason": job.failure_reason,
            });
            if let Some(deduplicated) = deduplicated {
                view["deduplicated"] = deduplicated.into();
            }
            view
        }
    }
}

/// `POST /v1/admin/ingest` — accept one raw NZB upload. The guard runs
/// first (mutation, so a stale admin session steps up); the body then
/// classifies in intake order (empty, too large, not-NZB heuristic,
/// real core parse, total-segment cap). Accepted bytes are encrypted
/// into the no-expiry original and everything commits in one guarded
/// store transaction; a rejected body still writes its failed job row
/// (same transaction, same audit shape). Empty and oversized bodies
/// write nothing and audit nothing — there is no parsed content to
/// name. Randomness failure is the existing 503 `unavailable`
/// envelope, like every other envelope fill.
async fn admin_ingest_upload<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = match ctx.authenticate_admin(req, true).await {
        Ok(claims) => claims,
        Err(resp) => return resp,
    };
    match admin_ingest::classify_upload(&req.body) {
        Err(reason) => {
            if reason == admin_ingest::FailureReason::Empty {
                return admin_ingest_failure(400, reason.wire_code(), reason.as_str(), None);
            }
            let hex = admin_ingest::content_sha256_hex(&req.body);
            let attempt = AdminIngestAttempt {
                actor_user_id: claims.user_id.clone(),
                job_id: admin_ingest::derive_job_id(&req.body),
                idempotency_key: admin_ingest::idempotency_key(&hex),
                content_sha256_hex: hex,
                nzb_bytes: req.body.len() as u64,
                file_count: None,
                segment_count: None,
                failure: Some(reason),
                original: None,
                kek_version: ctx.cfg.kek_version,
                source_ip: ctx.source_ip.clone(),
                now: ctx.now,
            };
            match ctx.store.admin_ingest_upload(&attempt).await {
                // A fresh rejection answers the reason's own status;
                // a replay converges on the stored failed row (200).
                Ok(AdminIngestUploadOutcome::Stored { job, deduplicated }) => {
                    let status = if deduplicated {
                        200
                    } else {
                        reason.http_status()
                    };
                    let view = admin_ingest_view(&job, Some(deduplicated));
                    json_response(status, view)
                }
                // The actor lost admin between the guard and the write
                // transaction's re-check: answer as any non-admin is.
                Ok(AdminIngestUploadOutcome::ActorNotAdmin) => not_found(),
                // The actor scheduled account deletion: ADR 0002 rule 3
                // fences the write, answered with the typed 409.
                Ok(AdminIngestUploadOutcome::AccountDeleting) => account_deleting_response(),
                Err(_) => internal(),
            }
        }
        Ok(valid) => {
            let hex = admin_ingest::content_sha256_hex(&req.body);
            let job_id = admin_ingest::derive_job_id(&req.body);
            let current_kek = match ctx.cfg.kek_by_version(ctx.cfg.kek_version) {
                Some(kek) => kek,
                None => return internal(),
            };
            let envelope = match admin_ingest::encrypt_ingest_original(
                current_kek,
                ctx.cfg.kek_version,
                &hex,
                &job_id,
                &req.body,
            ) {
                Ok(envelope) => envelope,
                Err(crate::vault::VaultError::RandomnessUnavailable) => return unavailable(),
                Err(_) => return internal(),
            };
            let attempt = AdminIngestAttempt {
                actor_user_id: claims.user_id.clone(),
                job_id,
                idempotency_key: admin_ingest::idempotency_key(&hex),
                content_sha256_hex: hex,
                nzb_bytes: req.body.len() as u64,
                file_count: Some(valid.file_count),
                segment_count: Some(valid.segment_count),
                failure: None,
                original: Some(envelope),
                kek_version: ctx.cfg.kek_version,
                source_ip: ctx.source_ip.clone(),
                now: ctx.now,
            };
            match ctx.store.admin_ingest_upload(&attempt).await {
                // Fresh acceptance is 201; a replay converges on the
                // stored row (200 with `deduplicated: true`).
                Ok(AdminIngestUploadOutcome::Stored { job, deduplicated }) => {
                    let status = if deduplicated { 200 } else { 201 };
                    let view = admin_ingest_view(&job, Some(deduplicated));
                    json_response(status, view)
                }
                Ok(AdminIngestUploadOutcome::ActorNotAdmin) => not_found(),
                Ok(AdminIngestUploadOutcome::AccountDeleting) => account_deleting_response(),
                Err(_) => internal(),
            }
        }
    }
}

/// `GET /v1/admin/ingest/{id}` — one stored admin job, redacted.
/// Unknown ids, pipeline rows and malformed shapes are the plain 404.
async fn admin_ingest_get<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if ctx.authenticate_admin(req, false).await.is_err() {
        return not_found();
    }
    let Some(rest) = req.path.strip_prefix("/v1/admin/ingest/") else {
        return not_found();
    };
    if rest.is_empty() || rest.contains('/') || rest.contains('?') {
        return not_found();
    }
    match ctx.store.admin_ingest_get(rest).await {
        Ok(Some(job)) => {
            if admin_ingest::view_status(&job.db_status).is_none() {
                return internal();
            }
            json_response(200, admin_ingest_view(&job, None))
        }
        Ok(None) => not_found(),
        Err(_) => internal(),
    }
}

/// `GET /v1/admin/ingest` — stored admin jobs, newest first, redacted.
/// The runtime strips the query string before handlers run; the entry
/// preserves it for exactly this route, so `?status=&limit=&offset=`
/// filters apply in production and in tests alike. An unknown status
/// is the 422 `validation_failed` envelope; unparsable windows fall
/// back to the bounded default, never an error.
async fn admin_ingest_list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if ctx.authenticate_admin(req, false).await.is_err() {
        return not_found();
    }
    let (bare, query) = match req.path.split_once('?') {
        Some((bare, query)) => (bare, query),
        None => (req.path.as_str(), ""),
    };
    if bare != "/v1/admin/ingest" {
        return not_found();
    }
    let mut status_raw: Option<&str> = None;
    let mut limit_raw: Option<&str> = None;
    let mut offset_raw: Option<&str> = None;
    for pair in query.split('&') {
        let (key, value) = match pair.split_once('=') {
            Some(split) => split,
            None => continue,
        };
        match key {
            "status" => status_raw = Some(value),
            "limit" => limit_raw = Some(value),
            "offset" => offset_raw = Some(value),
            _ => {}
        }
    }
    let Some(status) = admin_ingest::parse_status_filter(status_raw) else {
        return validation_failed("unknown status");
    };
    // The store filters on the ledger status; the view names it
    // `accepted` while the row stays `queued` for the future dispatch
    // slice (the source discriminator keeps the pipeline off it).
    let db_status = match status {
        None => None,
        Some("accepted") => Some("queued"),
        Some("failed") => Some("failed"),
        Some(_) => return internal(),
    };
    let (limit, offset) = admin_ingest::parse_list_window(limit_raw, offset_raw);
    match ctx.store.admin_ingest_list(db_status, limit, offset).await {
        Ok(jobs) => {
            let mut views = Vec::with_capacity(jobs.len());
            for job in &jobs {
                if admin_ingest::view_status(&job.db_status).is_none() {
                    return internal();
                }
                views.push(admin_ingest_view(job, None));
            }
            json_response(
                200,
                serde_json::json!({"jobs": views, "limit": limit, "offset": offset}),
            )
        }
        Err(_) => internal(),
    }
}
// --- Admin invites (migration 0033) --------------------------------------------------------
//
// API-only in M1: issue (plaintext shown once in the response, hash
// only at rest), list/search by issuer/status, revoke. Everything
// here rides the same non-enumerable `authenticate_admin` guard as
// the 0028 surface, and every write commits with its audit row in
// one transaction that re-reads the actor's role under its locks.

/// Cap on one admin invite list response; pagination is post-M1.
const ADMIN_INVITE_LIST_LIMIT: usize = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminInviteIssueBody {
    /// When set, redemption requires signup with this exact email.
    email: Option<String>,
    /// Per-invite expiry in days (1–30); absent means the
    /// `invite_default_expiry_days` setting.
    expires_in_days: Option<u32>,
}

/// One invite as the admin API reports it: lifecycle fields only —
/// the code and its hash are never in any response.
fn invite_admin_json(record: &InviteRecord, now: u64) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "status": record.status(now),
        "kind": record.kind,
        "email": record.email,
        "issued_by": record.issued_by,
        "created_at": record.created_at,
        "expires_at": record.expires_at,
        "redeemed_by": record.redeemed_by,
        "redeemed_at": record.redeemed_at,
        "revoked_at": record.revoked_at,
        "revoked_by": record.revoked_by,
    })
}

/// `POST /v1/admin/invites` — issue one admin invite. The plaintext
/// code is returned exactly once, in this response; only its hash is
/// stored, and neither is ever logged or audited (the audit row
/// references the new row id).
async fn admin_invite_issue<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = match ctx.authenticate_admin(req, true).await {
        Ok(claims) => claims,
        Err(resp) => return resp,
    };
    let Some(body) = parse_body::<AdminInviteIssueBody>(req) else {
        return validation_failed("invalid request");
    };
    let email = match body.email.as_deref() {
        Some(raw) => match normalize_email(raw) {
            Some(email) => Some(email),
            None => return validation_failed("invalid email"),
        },
        None => None,
    };
    let expiry_key = crate::admin::SettingKey::InviteDefaultExpiryDays;
    let expires_in_days = match body.expires_in_days {
        Some(days) => match expiry_key.validate(days) {
            Some(days) => days,
            None => return validation_failed("invite expiry out of range"),
        },
        None => {
            let rows = match ctx.store.app_settings().await {
                Ok(rows) => rows,
                Err(_) => return internal(),
            };
            let stored = rows
                .iter()
                .find(|(name, _)| name == expiry_key.as_str())
                .map(|(_, value)| value.as_str());
            match expiry_key.resolve(stored) {
                Some(days) => days,
                None => return internal(),
            }
        }
    };
    let code = match auth::generate_invite_code() {
        Ok(code) => code,
        Err(auth::AuthError::RandomnessUnavailable) => return unavailable(),
        Err(_) => return internal(),
    };
    let record = InviteRecord::new_issued(
        auth::hash_invite_code(&code),
        Some(claims.user_id.clone()),
        auth::INVITE_KIND_ADMIN,
        email,
        ctx.now,
        ctx.now
            .saturating_add(u64::from(expires_in_days).saturating_mul(86_400)),
    );
    let invite_id = match ctx
        .store
        .admin_issue_invite(&claims.user_id, &record, &ctx.source_ip, ctx.now)
        .await
    {
        Ok(AdminInviteIssueOutcome::Issued(id)) => id,
        // The actor lost admin between the guard and the write
        // transaction's re-check: answer as any non-admin is answered.
        Ok(AdminInviteIssueOutcome::ActorNotAdmin) => return not_found(),
        Err(_) => return internal(),
    };
    let mut issued = record;
    issued.id = invite_id;
    json_response(
        201,
        serde_json::json!({
            "invite": invite_admin_json(&issued, ctx.now),
            // Shown once: the issuer copies it now. It is not stored,
            // not logged, and cannot be read back.
            "code": code,
        }),
    )
}

/// `GET /v1/admin/invites` — every invite (newest first, capped),
/// optionally narrowed by `/status/{status}` or `/issuer/{user_id}`.
/// Filters ride path segments: `Request` carries no query string and
/// the M1 surface is API-only.
async fn admin_invite_list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if let Err(resp) = ctx.authenticate_admin(req, false).await {
        return resp;
    }
    let (issuer, status) = if req.path == "/v1/admin/invites" {
        (None, None)
    } else if let Some(rest) = req.path.strip_prefix("/v1/admin/invites/") {
        let Some((kind, value)) = rest.split_once('/') else {
            return not_found();
        };
        if value.is_empty() || value.contains('/') {
            return not_found();
        }
        match kind {
            "status" => match value {
                "active" | "expired" | "redeemed" | "revoked" => (None, Some(value)),
                _ => return not_found(),
            },
            "issuer" => (Some(value), None),
            _ => return not_found(),
        }
    } else {
        return not_found();
    };
    let rows = match ctx
        .store
        .admin_list_invites(issuer, status, ctx.now, ADMIN_INVITE_LIST_LIMIT)
        .await
    {
        Ok(rows) => rows,
        Err(_) => return internal(),
    };
    let invites: Vec<serde_json::Value> = rows
        .iter()
        .map(|record| invite_admin_json(record, ctx.now))
        .collect();
    json_response(200, serde_json::json!({"invites": invites}))
}

/// `POST /v1/admin/invites/{id}/revoke` — revoke one unredeemed
/// invite. Revocation wins redemption races (the store's conditional
/// write); an already-revoked invite is an idempotent success, an
/// already-redeemed one is a conflict — revocation never reaches
/// back into a created account.
async fn admin_invite_revoke<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = match ctx.authenticate_admin(req, true).await {
        Ok(claims) => claims,
        Err(resp) => return resp,
    };
    let Some(rest) = req.path.strip_prefix("/v1/admin/invites/") else {
        return not_found();
    };
    let Some((raw_id, "revoke")) = rest.split_once('/') else {
        return not_found();
    };
    // Strict decimal id: no sign, no whitespace, no leading zeros.
    let invite_id: i64 = if raw_id.is_empty()
        || !raw_id.bytes().all(|b| b.is_ascii_digit())
        || (raw_id.len() > 1 && raw_id.starts_with('0'))
    {
        return not_found();
    } else {
        match raw_id.parse() {
            Ok(id) => id,
            Err(_) => return not_found(),
        }
    };
    match ctx
        .store
        .admin_revoke_invite(&claims.user_id, invite_id, &ctx.source_ip, ctx.now)
        .await
    {
        Ok(AdminInviteRevokeOutcome::Revoked | AdminInviteRevokeOutcome::AlreadyRevoked) => {
            json_response(
                200,
                serde_json::json!({"id": invite_id, "status": "revoked"}),
            )
        }
        Ok(AdminInviteRevokeOutcome::AlreadyRedeemed) => {
            validation_failed("invite already redeemed")
        }
        Ok(AdminInviteRevokeOutcome::Missing | AdminInviteRevokeOutcome::ActorNotAdmin) => {
            not_found()
        }
        Err(_) => internal(),
    }
}

// --- Play grant issuance -----------------------------------------------------------------
//
// `POST /v1/plays` is THE enforcement point: the stream Worker has no DB
// binding, so subscription, the monthly hour cap, and the
// one-concurrent-stream lease are all evaluated here before a grant is
// minted (`crate::playback` documents the gate order and lease rules).
// Cookie-session only for now: the scope map keeps `Bearer(PlaysWrite)`
// on this path, so a presented native-rest bearer is gate-checked in
// `handle()` (invalid keys plain-404 before dispatch, never rescued by a
// cookie) but cannot mint until native bearer dispatch lands; the
// heartbeat path is `Deny` in the scope map and never enters the gate.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlayIssueBody {
    pub(crate) track_id: String,
    /// Client-generated playback session id (per tab/device): the lease
    /// holder name. Distinct from the account session cookie — two
    /// signed-in devices share one account but hold different holders.
    pub(crate) session_id: String,
    /// Continuation key only: present when refreshing a grant for the
    /// caller's own current play. Never accepted as a fresh play id —
    /// new starts always mint one server-side, so a replay cannot be
    /// under-metered by reusing an old id.
    pub(crate) play_id: Option<String>,
    /// Explicit "play here instead": take the lease from the other
    /// session now rather than waiting out its TTL.
    #[serde(default)]
    pub(crate) takeover: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayHeartbeatBody {
    session_id: String,
    play_id: String,
    lease_epoch: u64,
}

/// Client session ids name the lease holder: bounded by the
/// `playback_leases` length CHECK (1..=128), compared for equality only.
/// The byte cap is at least as strict as the column's character cap.
fn valid_client_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
}

/// `402 subscription_required` with the paywall `reason` discriminator
/// plus the upgrade pointer (`docs/api-errors.md`): the registered
/// envelope extended by names-only fields, like the plan-limit body.
fn subscription_refusal(refusal: crate::playback::SubscriptionRefusal) -> Response {
    let error = crate::api_error::ApiError::subscription_required("subscription required");
    Response {
        status: error.status,
        body: serde_json::json!({
            "code": error.code,
            "message": error.message,
            "retryable": error.retryable,
            "reason": refusal.reason(),
            (crate::plan_limits::UPGRADE_KEY): crate::plan_limits::UPGRADE_URL,
        })
        .to_string(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

/// Internal issuance result, deliberately not Debug: it contains a bearer
/// grant. Only the native HTTP adapter serializes it; other surfaces can
/// forward the grant privately without parsing or exposing a JSON response.
pub(crate) struct IssuedPlay {
    pub(crate) grant: crate::play_grant::IssuedGrant,
    pub(crate) play_id: String,
    pub(crate) lease_epoch: u64,
    pub(crate) lease_expires_at: u64,
}

impl IssuedPlay {
    fn into_response(self) -> Response {
        json_response(
            200,
            serde_json::json!({
                "grant": self.grant.token,
                "play_id": self.play_id,
                "lease_epoch": self.lease_epoch,
                "expires_at": self.grant.exp_unix,
                "lease_expires_at": self.lease_expires_at,
                "heartbeat_interval_secs": crate::playback::HEARTBEAT_INTERVAL_SECS,
            }),
        )
    }
}

fn mint_play(
    secret: &[u8],
    user_id: &str,
    track_id: &str,
    play_id: &str,
    epoch: u64,
    now: u64,
    lease_expires_at: u64,
) -> Result<IssuedPlay, Response> {
    match crate::play_grant::issue_v2_at(
        user_id,
        track_id,
        play_id,
        epoch,
        now,
        crate::play_grant::MAX_GRANT_TTL,
        Some(secret),
    ) {
        Ok(grant) => Ok(IssuedPlay {
            grant,
            play_id: play_id.to_string(),
            lease_epoch: epoch,
            lease_expires_at,
        }),
        Err(crate::play_grant::MintError::NotConfigured) => Err(internal()),
        Err(crate::play_grant::MintError::InvalidIdentity) => {
            Err(validation_failed("invalid request"))
        }
    }
}

async fn play_issue<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
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
    match issue_play_for_user(ctx, &claims.user_id, parse_body::<PlayIssueBody>(req)).await {
        Ok(issued) => issued.into_response(),
        Err(response) => response,
    }
}

/// Shared issuance after surface-specific authentication. This is not an
/// authorizer: callers supply only their authenticated user's id and must
/// enforce that surface's catalogue/source access before requesting a grant.
/// It accepts no HTTP credentials and creates no account session. Keep the
/// typed grant internal; compatibility forwarding must use a private header,
/// never serialize a native grant into a compatibility response or URL.
pub(crate) async fn issue_play_for_user<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    body: Option<PlayIssueBody>,
) -> Result<IssuedPlay, Response>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // The lease writers are fenced on `deleting_at` (ADR 0002 rule 3);
    // answer the typed 409 up front rather than a lease 429/500 after
    // the fence refuses the write.
    if matches!(ctx.store.account_deleting(user_id).await, Ok(true)) {
        return Err(account_deleting_response());
    }
    // An unprovisioned (or too-short) secret fails closed before any
    // lease write: an unmintable grant must never burn a lease epoch or
    // displace the current holder.
    let Some(secret) = ctx
        .cfg
        .play_grant_secret
        .as_deref()
        .filter(|secret| secret.len() >= crate::play_grant::MIN_SECRET_BYTES)
    else {
        return Err(internal());
    };
    let Some(body) = body else {
        return Err(validation_failed("invalid request"));
    };
    if !valid_client_session_id(&body.session_id) {
        return Err(validation_failed("invalid session_id"));
    }
    // The track id must be meterable before any lease write: the
    // ledger field rule is the single authority (probed with a
    // placeholder play id — the same probe the minter runs with the
    // real one). User ids are server-generated; an unmeterable one
    // fails this same check rather than minting an unbillable grant.
    if crate::ledger::derive_play_idempotency_key(user_id, &body.track_id, "play").is_err() {
        return Err(validation_failed("invalid track_id"));
    }
    let lease = match ctx.store.playback_lease(user_id).await {
        Ok(row) => row,
        Err(_) => return Err(internal()),
    };
    // Continuation: the same client session refreshing the grant for
    // its own current play (seek/resume/refresh ahead of grant
    // expiry). Skips the subscription and cap gates — the track that
    // crosses the cap, or whose subscription lapses mid-play,
    // finishes; only the next NEW start is refused. The conditional
    // touch either re-extends the same epoch or reports the caller
    // lost the lease between the read and now, in which case this is
    // simply a new start through the gates below.
    if let (Some(play_id), Some(row)) = (body.play_id.as_deref(), lease.as_ref()) {
        if row.is_continuation(&body.session_id, play_id, &body.track_id) {
            if crate::ledger::derive_play_idempotency_key(user_id, &body.track_id, play_id).is_err()
            {
                return Err(validation_failed("invalid play_id"));
            }
            let lease_expires_at = ctx.now.saturating_add(crate::playback::LEASE_TTL_SECS);
            match ctx
                .store
                .touch_playback_lease(
                    user_id,
                    &body.session_id,
                    play_id,
                    row.epoch,
                    ctx.now,
                    lease_expires_at,
                )
                .await
            {
                Ok(true) => {
                    return mint_play(
                        secret,
                        user_id,
                        &body.track_id,
                        play_id,
                        row.epoch,
                        ctx.now,
                        lease_expires_at,
                    );
                }
                Ok(false) => {}
                Err(_) => return Err(internal()),
            }
        }
    }
    // New start, gate 1: subscription. `now` saturates into i64 rather
    // than wrapping — an absurd clock reads as far-future (closed),
    // never as the distant past (entitled forever).
    let subscription = match ctx.store.live_subscription(user_id).await {
        Ok(row) => row,
        Err(_) => return Err(internal()),
    };
    let effective = crate::billing::resolve_effective_plan(
        subscription.as_ref().map(|row| row.as_view()),
        i64::try_from(ctx.now).unwrap_or(i64::MAX),
    );
    if !effective.entitled {
        let ever_paid = match ctx.store.ever_subscribed(user_id).await {
            Ok(ever) => ever,
            Err(_) => return Err(internal()),
        };
        let refusal = crate::playback::subscription_refusal(&effective, ever_paid)
            .expect("an unentitled effective plan always refuses");
        return Err(subscription_refusal(refusal));
    }
    // Caps are plan DATA from the `plans` row; a missing row fails
    // closed to the reserved `free` caps (0 h) — the 402 below, never
    // a silently uncapped grant.
    let caps = match ctx.store.plan_caps(effective.plan.as_str()).await {
        Ok(Some(caps)) => caps,
        Ok(None) => crate::plan_limits::PlanId::Free.caps(),
        Err(_) => return Err(internal()),
    };
    // Gate 2: the monthly hour cap. An unreadable meter refuses the
    // new start (store default `Err`) rather than reading as zero.
    let period_month = crate::ledger::period_month_for_timestamp(ctx.now);
    let used_seconds = match ctx
        .store
        .monthly_stream_seconds(user_id, &period_month)
        .await
    {
        Ok(seconds) => seconds,
        Err(_) => return Err(internal()),
    };
    if let Some(denial) =
        crate::plan_limits::check_stream_hours_with_caps(&caps, used_seconds).denial()
    {
        return Err(plan_limit_refusal(denial));
    }
    // A zero concurrency allowance refuses with the lease 429 wire and
    // the static retry: no lease exists to wait on. Allowances above
    // one are still enforced as one — the lease table holds a single
    // row per user by design (every seeded plan allows exactly one).
    if caps.max_concurrent_streams == 0 {
        return Err(plan_limit_refusal(
            crate::plan_limits::deny_concurrent_lease(
                &caps,
                crate::plan_limits::CONCURRENT_RETRY_AFTER_SECS,
            ),
        ));
    }
    // Gate 3: the lease. The play id is minted here, never taken from
    // the client (a client-chosen fresh id could replay an old play
    // and under-meter). The atomic acquire bumps the epoch in the
    // store; `Held` is the other-session 429 with the live TTL.
    let play_id = match random_id::<16>() {
        Ok(id) => id,
        Err(_) => return Err(unavailable()),
    };
    let lease_expires_at = ctx.now.saturating_add(crate::playback::LEASE_TTL_SECS);
    match ctx
        .store
        .acquire_playback_lease(
            user_id,
            &body.session_id,
            &body.track_id,
            &play_id,
            ctx.now,
            lease_expires_at,
            body.takeover,
        )
        .await
    {
        Ok(crate::playback::LeaseAcquire::Acquired { epoch }) => mint_play(
            secret,
            user_id,
            &body.track_id,
            &play_id,
            epoch,
            ctx.now,
            lease_expires_at,
        ),
        Ok(crate::playback::LeaseAcquire::Held { retry_after_secs }) => {
            // Zero rows back can also mean the deletion fence closed
            // after the up-front check (the marker landed mid-request):
            // re-read it so that race answers the typed 409, not a
            // concurrent-streams 429.
            if matches!(ctx.store.account_deleting(user_id).await, Ok(true)) {
                Err(account_deleting_response())
            } else {
                Err(plan_limit_refusal(
                    crate::plan_limits::deny_concurrent_lease(&caps, retry_after_secs),
                ))
            }
        }
        Err(_) => Err(internal()),
    }
}

/// Heartbeat: the holder's keepalive and the takeover signal in one.
/// Always 200 for lease state (`active` extends, `superseded` tells the
/// displaced player to pause, `expired` means no row) — lease state is
/// an answer, not an error. No subscription or cap gates: a current
/// play finishes even when a new start would be refused.
async fn play_heartbeat<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
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
    // Same fence as issuance (ADR 0002 rule 3): a deleting account's
    // heartbeat is the typed 409, never an `active` extension.
    if matches!(ctx.store.account_deleting(&claims.user_id).await, Ok(true)) {
        return account_deleting_response();
    }
    // Same fail-closed secret gate as issuance: a misconfigured
    // deployment must not let heartbeats keep a lease alive that no
    // new grant could be minted against.
    if ctx
        .cfg
        .play_grant_secret
        .as_deref()
        .filter(|secret| secret.len() >= crate::play_grant::MIN_SECRET_BYTES)
        .is_none()
    {
        return internal();
    }
    let Some(body) = parse_body::<PlayHeartbeatBody>(req) else {
        return validation_failed("invalid request");
    };
    if !valid_client_session_id(&body.session_id) {
        return validation_failed("invalid session_id");
    }
    let lease = match ctx.store.playback_lease(&claims.user_id).await {
        Ok(row) => row,
        Err(_) => return internal(),
    };
    let epoch = lease.as_ref().map_or(body.lease_epoch, |row| row.epoch);
    let state = crate::playback::LeaseHeartbeat::classify(
        lease.as_ref(),
        &body.session_id,
        &body.play_id,
        body.lease_epoch,
    );
    let (state, lease_expires_at) = if state == crate::playback::LeaseHeartbeat::Active {
        let expires = ctx.now.saturating_add(crate::playback::LEASE_TTL_SECS);
        match ctx
            .store
            .touch_playback_lease(
                &claims.user_id,
                &body.session_id,
                &body.play_id,
                body.lease_epoch,
                ctx.now,
                expires,
            )
            .await
        {
            Ok(true) => (state, Some(expires)),
            // Zero rows: a raced takeover landed between the read and
            // the touch, or the deletion fence closed after the
            // up-front check. Re-read deletion so the fence race
            // answers the typed 409; otherwise report the truth, do
            // not extend a lost lease.
            Ok(false) => {
                if matches!(ctx.store.account_deleting(&claims.user_id).await, Ok(true)) {
                    return account_deleting_response();
                }
                (crate::playback::LeaseHeartbeat::Superseded, None)
            }
            Err(_) => return internal(),
        }
    } else {
        (state, None)
    };
    let mut body_json = serde_json::json!({"lease": state.as_str(), "lease_epoch": epoch});
    if let Some(expires) = lease_expires_at {
        body_json["lease_expires_at"] = serde_json::json!(expires);
    }
    json_response(200, body_json)
}

// --- Meter sink (internal) ---------------------------------------------------
//
// `POST /internal/v1/play-windows` is not a user route: only the
// stream Worker's per-user Durable Objects call it, over their `API`
// service binding, authenticating with the dedicated meter-sink HMAC
// (crate::meter_sink). It takes no session, no bearer, and no origin
// pin (there is no browser), and it is deliberately not in
// `is_known_path` — preflights stay 404 and the browser surface never
// learns it exists. Each accepted window derives its seconds
// server-side from the delivered union and writes through the
// monotonic ledger upsert; the ack dispositions tell the DO which
// windows are terminal (crate::meter_sink module docs).

/// Ingest one signed batch of cumulative play windows.
async fn meter_sink_ingest<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    use crate::meter_sink::{PlayWindowOutcome, SinkError};
    match crate::meter_sink::verify(
        ctx.cfg.meter_sink_secret.as_deref(),
        req.authorization.as_deref(),
        &req.body,
        ctx.now,
    ) {
        Ok(()) => {}
        // Unprovisioned is a server misconfiguration, not a caller
        // fault; the DO retries on its alarm either way.
        Err(SinkError::NotConfigured) => return internal(),
        Err(SinkError::Unauthorized) => {
            let error = crate::api_error::ApiError::unauthorized("unauthorized");
            return Response {
                status: error.status,
                body: error.body_json(),
                set_cookie: None,
                retry_after: None,
                extra_headers: Vec::new(),
            };
        }
        Err(SinkError::Malformed) => return internal(),
    }
    let windows = match crate::meter_sink::parse_batch(&req.body) {
        Ok(windows) => windows,
        Err(_) => return validation_failed("invalid request"),
    };
    let mut accepted = Vec::new();
    let mut fenced = Vec::new();
    let mut rejected = Vec::new();
    // Sequential on purpose: a batch is one user's plays, and the DO
    // retries the whole batch on a 500 — the upsert's monotonicity
    // makes partially-applied retries converge.
    for window in &windows {
        let meta = match ctx
            .store
            .track_meter_meta(&window.user_id, &window.track_id)
            .await
        {
            Ok(meta) => meta,
            Err(_) => return internal(),
        };
        // The stream-reported blob length is the confirmed size of
        // what was actually served; the cache row is the backup.
        let file_bytes = window.file_bytes.or(meta.file_bytes).filter(|b| *b > 0);
        let duration_ms = crate::ledger::resolve_duration_ms(
            meta.track_duration_ms,
            meta.mb_duration_ms,
            meta.bitrate_kbps,
            file_bytes,
        );
        let prepared = crate::ledger::prepare_play_window(
            &window.user_id,
            &window.track_id,
            &window.play_id,
            window.union_bytes,
            file_bytes,
            duration_ms,
            window.started_at,
        );
        let prepared = match prepared {
            Ok(prepared) => prepared,
            // Unpreparable windows never converge on retry: name them
            // terminal so the DO drops them instead of alarming forever.
            Err(_) => {
                rejected.push(window.play_id.clone());
                continue;
            }
        };
        match ctx.store.record_play_window(&prepared).await {
            Ok(PlayWindowOutcome::Written | PlayWindowOutcome::Covered) => {
                accepted.push(window.play_id.clone());
            }
            Ok(PlayWindowOutcome::Fenced) => fenced.push(window.play_id.clone()),
            Err(_) => return internal(),
        }
    }
    json_response(
        200,
        serde_json::json!({
            "accepted": accepted,
            "fenced": fenced,
            "rejected": rejected,
        }),
    )
}

// --- Dispatch ------------------------------------------------------------------------------

/// `u64` handler time as Stripe-epoch `i64` (saturates only past the year
/// 292 billion; production values always fit).
fn billing_now_i64(now: u64) -> i64 {
    i64::try_from(now).unwrap_or(i64::MAX)
}

/// Best-effort billing alert: recorded, never fatal. Audit write failure
/// must not turn a stored reconcile pass into a 500 (which Stripe would
/// retry into a duplicate-safe but noisy replay).
async fn billing_alert<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, user_id: &str, action: &str)
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let _ = ctx
        .store
        .insert_audit(&crate::vault::audit_event(
            user_id,
            action,
            false,
            &ctx.source_ip,
            ctx.now,
        ))
        .await;
}

/// Shared reconcile driver for one Stripe subscription: retrieve the
/// remote subscription plus its paid invoices (every invoice read FULLY —
/// line pagination is followed until exhausted, a first page is never
/// the full invoice) and fold them into the store through the pure
/// `billing_reconcile::reconcile_subscription`. `Err(())` is a Stripe or
/// store failure (the caller 500s so the delivery retries); fail-closed
/// passes still persist their observed row and return `Ok` with the
/// marker set, so the caller can alert and disable.
async fn reconcile_remote_subscription<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    subscription_id: &str,
) -> Result<crate::billing_reconcile::ReconcileOutcome, ()>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let subscription = ctx
        .stripe
        .retrieve_subscription(subscription_id.to_string())
        .await
        .map_err(|_| ())?;
    let input = ctx
        .store
        .load_reconcile_input(user_id, None)
        .await
        .map_err(|_| ())?;
    let listed = ctx
        .stripe
        .list_invoices(crate::stripe::ListInvoices {
            customer_id: subscription.customer.clone(),
            subscription_id: Some(subscription.id.clone()),
            paid_only: true,
            limit: 100,
            // The real transport pages the list envelope itself to
            // exhaustion; the fakes answer the full filtered set.
            starting_after: None,
        })
        .await
        .map_err(|_| ())?;
    let mut invoices = Vec::with_capacity(listed.len());
    for mut invoice in listed {
        loop {
            let has_more = invoice
                .lines
                .as_ref()
                .map(|lines| lines.has_more)
                .unwrap_or(false);
            if !has_more {
                break;
            }
            let cursor = invoice
                .lines
                .as_ref()
                .and_then(|lines| lines.data.last())
                .map(|line| line.id.clone());
            let page = ctx
                .stripe
                .list_invoice_lines(invoice.id.clone(), cursor, 100)
                .await
                .map_err(|_| ())?;
            match invoice.lines.as_mut() {
                Some(lines) => {
                    lines.data.extend(page.data);
                    lines.has_more = page.has_more;
                }
                None => {
                    invoice.lines = Some(crate::stripe::InvoiceLines {
                        data: page.data,
                        has_more: page.has_more,
                    });
                }
            }
        }
        invoices.push(invoice);
    }
    let outcome = crate::billing_reconcile::reconcile_subscription(
        &input,
        &crate::billing_reconcile::RemoteSubscription {
            subscription,
            invoices,
        },
        billing_now_i64(ctx.now),
    );
    ctx.store
        .store_reconcile_outcome(user_id, &outcome)
        .await
        .map_err(|_| ())?;
    Ok(outcome)
}

/// Reconcile one completed Checkout Session (the webhook's handoff):
/// resolve the user — the session's operation binding first (`op id`
/// from `client_reference_id`), the customer binding second — then fold
/// the remote subscription into the store. Unknown users ack 200 with an
/// alert and no state change. An unconfirmed or open session mints
/// nothing: no subscription id, no store touch.
async fn reconcile_checkout_session<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    session_id: &str,
) -> Result<(), ()>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let session = ctx
        .stripe
        .retrieve_checkout_session(session_id.to_string())
        .await
        .map_err(|_| ())?;
    if session.status.as_deref() != Some("complete")
        || session.payment_status.as_deref() != Some("paid")
    {
        return Ok(());
    }
    let Some(subscription_id) = session.subscription.clone() else {
        return Ok(());
    };
    let mut user_id: Option<String> = None;
    let mut op: Option<crate::billing_reconcile::StoredCheckoutOp> = None;
    if let Some(client_reference) = session.client_reference_id.as_deref() {
        match ctx.store.checkout_operation_by_id(client_reference).await {
            Ok(Some(found)) => {
                user_id = Some(found.user_id.clone());
                op = Some(found);
            }
            Ok(None) => {}
            Err(_) => return Err(()),
        }
    }
    if user_id.is_none() {
        if let Some(customer) = session.customer.as_deref() {
            user_id = ctx
                .store
                .user_for_stripe_customer(customer)
                .await
                .map_err(|_| ())?;
        }
    }
    let Some(user_id) = user_id else {
        billing_alert(
            ctx,
            "stripe:unknown-customer",
            BILLING_UNKNOWN_CUSTOMER_ACTION,
        )
        .await;
        return Ok(());
    };
    let outcome = reconcile_remote_subscription(ctx, &user_id, &subscription_id).await?;
    // The operation that sourced this session resolves with it: clean
    // passes succeed it, fail-closed passes park it for the hourly job.
    // Best-effort (the reconcile itself already stored): a lost mark only
    // leaves the flight open, never double-bills.
    if let Some(op) = op {
        let state = if outcome.disable_new_purchases() {
            "needs_reconcile"
        } else {
            "succeeded"
        };
        let price_id = op.price_id.clone().unwrap_or_default();
        let _ = ctx
            .store
            .update_checkout_operation(&op.id, state, &price_id, Some(session_id))
            .await;
    }
    if outcome.disable_new_purchases() {
        billing_alert(ctx, &user_id, BILLING_FAIL_CLOSED_ACTION).await;
    }
    Ok(())
}

async fn billing_webhook<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Unprovisioned secrets fail closed before any parse: Stripe retries
    // the generic 500; an unverifiable webhook is never acked as
    // received. The bearer key gates the Stripe calls below (the
    // transport fails closed without it, same 500).
    if ctx.cfg.stripe_secret_key.is_none() || ctx.cfg.stripe_webhook_secret.is_none() {
        return internal();
    }
    let webhook_secret = ctx.cfg.stripe_webhook_secret.as_deref().unwrap_or_default();
    // The signature is verified over the RAW body before any JSON
    // parsing: every failure shares one sealed 400 (no verification
    // oracle), and no state is touched on any failure.
    let header = match req.stripe_signature.as_deref() {
        Some(header) => header,
        None => return invalid_request(),
    };
    let event = match crate::stripe::verify_webhook_signature(
        &req.body,
        header,
        webhook_secret,
        billing_now_i64(ctx.now),
    ) {
        Ok(event) => event,
        Err(_) => return invalid_request(),
    };
    // First claim wins. Duplicates of a verified non-checkout event (or a
    // live-mode event) still ack 200 with no state change — those paths
    // never write state, so nothing can be lost. A duplicate
    // `checkout.session.completed`, however, is REPROCESSED idempotently
    // instead of acked: the release below is a same-DB DELETE, so a store
    // outage that fails the release — or a worker death after the claim —
    // would otherwise leave the paid event consumed and unprocessed.
    // Reconciliation converges (monotonic upsert under the per-user lock,
    // history idempotent on the invoice id, operation mark idempotent), so
    // reprocessing a duplicate lands the same stored state, and a still-
    // failing pass 500s so Stripe retries anyway. A store failure on the
    // claim itself 500s so Stripe retries the delivery.
    let _first_claim = match ctx.store.claim_webhook_event(&event.id).await {
        Ok(first) => first,
        Err(_) => return internal(),
    };
    if event.event_type != "checkout.session.completed" {
        // Verified but not ours: ack, no state change. Live-mode events
        // land here too — never Stripe-called, never stored.
        return json_response(200, serde_json::json!({"ok": true}));
    }
    if event.livemode {
        // A live-mode event on a test-mode worker is misconfiguration or
        // a probe: alert, ack, and never call Stripe or touch state.
        billing_alert(
            ctx,
            &format!("stripe:{}", event.id),
            BILLING_UNKNOWN_CUSTOMER_ACTION,
        )
        .await;
        return json_response(200, serde_json::json!({"ok": true}));
    }
    // The session object is verified JSON now; a missing id is a sender
    // bug (400), not a retryable failure.
    let session_id = match event.object.get("id").and_then(|id| id.as_str()) {
        Some(id) => id.to_string(),
        None => return invalid_request(),
    };
    match reconcile_checkout_session(ctx, &session_id).await {
        Ok(()) => json_response(200, serde_json::json!({"ok": true})),
        Err(()) => {
            // The claim is released so the retried delivery reprocesses
            // instead of acking 200 on a consumed claim. Best-effort: a
            // failed release still 500s, so Stripe retries anyway — and a
            // redelivery that finds the stale claim reprocesses it above
            // rather than dropping the payment.
            let _ = ctx.store.release_webhook_event(&event.id).await;
            internal()
        }
    }
}

/// Fresh single-flight operation id: UUID v4 text (the store casts the
/// id column). Secure-randomness failure fails the purchase closed —
/// never with a predictable id.
fn new_operation_id() -> Result<String, crate::random::CryptoError> {
    let bytes = crate::random::secure_random_bytes::<16>()?;
    let mut hex = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        let mut byte = *byte;
        if index == 6 {
            byte = (byte & 0x0f) | 0x40;
        }
        if index == 8 {
            byte = (byte & 0x3f) | 0x80;
        }
        if matches!(index, 4 | 6 | 8 | 10) {
            hex.push('-');
        }
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok(hex)
}

/// Free a held live slot (`incomplete` / `unpaid` / `paused`) before a
/// replacement checkout mints its session. The held subscription is
/// re-read from Stripe and the decision branches on the REMOTE status —
/// the stored row may be stale (only `checkout.session.completed`
/// webhooks reconcile, so a payment recovered since the last reconcile
/// still reads `unpaid` locally). Terminal remote reconciles straight
/// into the released slot; still-held remote cancels first and then
/// reconciles. Only once the slot is free may the caller proceed —
/// otherwise the replacement's reconcile would collide with the
/// still-held row and fail a paid subscription closed. A remote that
/// recovered to `active` / `past_due` reconciles (coverage and history
/// land) but refuses the purchase: canceling it would destroy the newly
/// paid subscription, and minting a second one double-bills. An unknown
/// remote status fails closed with an alert: no cancel, no reconcile, no
/// session — the held row stays reserved so later attempts keep failing
/// closed the same way. `Failed` is retryable (Stripe or store failure);
/// nothing is minted on any path but the freed slot.
async fn release_held_subscription_slot<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    subscription_id: &str,
) -> Result<(), EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let subscription = ctx
        .stripe
        .retrieve_subscription(subscription_id.to_string())
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    match subscription.status.as_str() {
        // Still held: cancel first, then reconcile the terminal state out
        // of the slot.
        "incomplete" | "unpaid" | "paused" => {
            ctx.stripe
                .cancel_subscription(subscription_id.to_string())
                .await
                .map_err(|_| EnsureCheckoutError::Failed)?;
            reconcile_remote_subscription(ctx, user_id, subscription_id)
                .await
                .map_err(|_| EnsureCheckoutError::Failed)?;
            Ok(())
        }
        // Already terminal remotely: nothing to cancel, reconcile it out
        // of the slot.
        status if crate::billing_reconcile::is_terminal_status(status) => {
            reconcile_remote_subscription(ctx, user_id, subscription_id)
                .await
                .map_err(|_| EnsureCheckoutError::Failed)?;
            Ok(())
        }
        // Recovered since the last reconcile: store the paid state, then
        // refuse — the account is already subscribed, so no cancel and no
        // second session.
        "active" | "past_due" => {
            reconcile_remote_subscription(ctx, user_id, subscription_id)
                .await
                .map_err(|_| EnsureCheckoutError::Failed)?;
            Err(EnsureCheckoutError::Unavailable)
        }
        // Unknown provider state: fail closed. No cancel (never destroy
        // what we cannot classify), no reconcile (the stale held row stays
        // reserved), no session — just the alert.
        _ => {
            billing_alert(ctx, user_id, BILLING_FAIL_CLOSED_ACTION).await;
            Err(EnsureCheckoutError::Unavailable)
        }
    }
}

/// The customer id a converged purchase attempt must create its session
/// under: the account binding re-read *after* the single-flight insert,
/// never the snapshot taken before this caller's own customer create.
/// Two racers that both start unbound each mint a Stripe customer; the
/// insert converges them on the winner's operation, and the winner's bind
/// owns the account. The op INSERT and the bind are separate statements,
/// so a loser can converge before the winner's bind lands: with no bound
/// customer yet the attempt fails retryable (the retry reads the winner's
/// binding) instead of falling back to its orphan. A converged attempt
/// never sessions on a customer that is not the account-bound one.
async fn converged_session_customer<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    _snapshot_customer: Option<String>,
    _created_customer: String,
) -> Result<String, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let fresh = ctx
        .store
        .load_reconcile_input(user_id, None)
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?
        .stripe_customer_id;
    // No fallback to the snapshot or the freshly minted orphan: in the
    // pre-bind window (op visible, winner bind not yet landed) there is no
    // bound customer, so the attempt fails retryable and the retry reads
    // the winner's binding. A converged attempt never sessions on a
    // customer that is not the account-bound one.
    fresh.ok_or(EnsureCheckoutError::Failed)
}

/// Create the Checkout Session under one operation id and record it, so
/// the next attempt reuses instead of recreating. The Stripe idempotency
/// key derives from the operation id, so even a lost `update` replays
/// into the same session on retry.
async fn create_session_under<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    operation_id: &str,
    customer_id: &str,
    price_id: &str,
    success_url: &str,
    cancel_url: &str,
) -> Result<String, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let session = ctx
        .stripe
        .create_checkout_session(crate::stripe::CreateCheckoutSession {
            operation_id: operation_id.to_string(),
            customer_id: customer_id.to_string(),
            price_id: price_id.to_string(),
            success_url: success_url.to_string(),
            cancel_url: cancel_url.to_string(),
        })
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    // Conditional mark: the session already exists under our idempotency
    // key, so a lost mark only costs a retrieve on the next attempt — but
    // a mark that lands after a deliberate supersede must not resurrect
    // the retired row. A missed mark means the flight moved on (the
    // replacer expired this very session before retiring the row), so the
    // in-hand session is dead: fail retryable and let the retry converge
    // onto the replacement instead of handing out an expired URL.
    let marked = ctx
        .store
        .update_checkout_operation(operation_id, "submitted", price_id, Some(&session.id))
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    if !marked {
        return Err(EnsureCheckoutError::Failed);
    }
    session.url.ok_or(EnsureCheckoutError::Failed)
}

/// Serialized purchase attempt (single flight): reuse the open Checkout
/// Session while open, else create under the converged operation id. No
/// user-facing route in this slice — the wiring card's routes call this.
/// `test_clock` is the staging-proof seam (customer creation only, never
/// a clock the worker manages).
///
/// `Unavailable` means the price is not a selectable test-mode mapping
/// (or the binding is otherwise inconsistent), the account already holds
/// a live subscription, or the stored row is an unknown provider state
/// failing closed: no Stripe call is made, and new purchases stay
/// disabled. An alert is raised except for the already-subscribed case.
/// `Failed` means a Stripe or store failure (retryable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureCheckoutError {
    Unavailable,
    Failed,
}

/// What one retrieved flight session decides: reuse its URL, retire it so
/// the caller mints a replacement, accept the completed purchase, or keep
/// the flight without minting.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FlightSessionDecision {
    /// Open, price-matched, with a URL: hand it over.
    Reuse(String),
    /// Deliberately retired (expired first while still open, or confirmed
    /// dead): the caller mints the replacement under a fresh operation.
    Replace,
    /// Complete and paid: reconciled through the shared driver; the caller
    /// refuses the duplicate purchase.
    Completed,
    /// Complete but unpaid, or any other unrecognized state: the flight
    /// stays open for the real completion; the caller fails retryable
    /// without minting beside unknown state.
    Retained,
}

/// Branch on one flight session retrieved fresh from Stripe (never trusted
/// from the row): reuse the open price-matched session, expire a still-open
/// unusable one before its deliberate replacement, reconcile a completed
/// paid session through the shared driver, or retain the flight without
/// minting for anything else. Only `status == "expired"` deliberately
/// replaces among the closed states: a `complete` session that is not paid
/// (`unpaid` async processing, `no_payment_required` subscription already
/// created) can still succeed, so a retry must never mint a second
/// subscription checkout beside it. A Stripe-404 (past retention, never
/// minted) replaces like a confirmed-expired session; any other retrieve
/// failure fails retryable instead of minting beside unknown state.
async fn handle_flight_session<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    op: &crate::billing_reconcile::StoredCheckoutOp,
    session_id: &str,
    price_id: &str,
) -> Result<FlightSessionDecision, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let session = match ctx
        .stripe
        .retrieve_checkout_session(session_id.to_string())
        .await
    {
        Ok(session) => session,
        Err(err) if err.http_status == 404 => {
            // Missing (past retention, never minted): same deliberate
            // replacement as a confirmed-expired session.
            replace_checkout_operation(ctx, op, None).await?;
            return Ok(FlightSessionDecision::Replace);
        }
        Err(_) => return Err(EnsureCheckoutError::Failed),
    };
    if session.status.as_deref() == Some("open") {
        if crate::billing_reconcile::checkout_price_matches(op, price_id) {
            if let Some(url) = session.url {
                return Ok(FlightSessionDecision::Reuse(url));
            }
        }
        // Open but unusable for this attempt: price-mismatched (the flight
        // belongs to another price) or URL-less. Expire it first — a
        // replacement never mints beside a possibly-payable session — then
        // replace deliberately.
        replace_checkout_operation(ctx, op, Some(session_id)).await?;
        return Ok(FlightSessionDecision::Replace);
    }
    if session.status.as_deref() == Some("complete")
        && session.payment_status.as_deref() == Some("paid")
    {
        // The purchase completed but its webhook was lost (or lands after
        // this retry): reconcile the paid session through the shared driver
        // instead of minting a second payable session. Money already landed,
        // whatever price it names — no replacement is minted on this path.
        reconcile_checkout_session(ctx, session_id)
            .await
            .map_err(|_| EnsureCheckoutError::Failed)?;
        return Ok(FlightSessionDecision::Completed);
    }
    if session.status.as_deref() == Some("expired") {
        // Confirmed dead without payment: no expiry call needed — Stripe
        // converges the state itself — just supersede and replace.
        replace_checkout_operation(ctx, op, None).await?;
        return Ok(FlightSessionDecision::Replace);
    }
    // Complete but not paid, or any other unrecognized state: the flight
    // stays open for the real completion and nothing mints beside it —
    // retryable, never a blind replace.
    Ok(FlightSessionDecision::Retained)
}

/// Recover the Stripe session for a session-less operation under its
/// ORIGINAL operation id and recorded price, and carry the recovered id on
/// the operation (best-effort mark repair — a lost mark only costs a
/// re-resolve on the next attempt). A missing `session_id` does not mean
/// Stripe has no session: the create mark is best-effort, so the same-key
/// replay recovers the original create without duplicating, and when
/// nothing was ever created the replay first-creates exactly one session
/// under that key. Either way the caller retires the recovered session per
/// its retrieved state before superseding — a replacement never mints
/// beside a possibly-payable session. Any Stripe failure fails retryable
/// with the flight untouched: nothing supersedes beside unknown state.
async fn resolve_sessionless_operation<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    mut op: crate::billing_reconcile::StoredCheckoutOp,
    recorded_price_id: &str,
    customer_id: &str,
    success_url: &str,
    cancel_url: &str,
) -> Result<crate::billing_reconcile::StoredCheckoutOp, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let recovered = ctx
        .stripe
        .create_checkout_session(crate::stripe::CreateCheckoutSession {
            operation_id: op.id.clone(),
            customer_id: customer_id.to_string(),
            price_id: recorded_price_id.to_string(),
            success_url: success_url.to_string(),
            cancel_url: cancel_url.to_string(),
        })
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    op.session_id = Some(recovered.id.clone());
    // Conditional, like the create-path mark: the in-memory row already
    // carries the recovered id for this attempt, so a lost mark only costs
    // a re-resolve on the next one — never a duplicate session. A missed
    // mark means the row retired while the replay was in flight (a
    // concurrent replacer superseded it): fail retryable with the flight
    // untouched instead of acting on the dead row.
    let state = op.state.clone();
    let marked = ctx
        .store
        .update_checkout_operation(&op.id, &state, recorded_price_id, op.session_id.as_deref())
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    if !marked {
        return Err(EnsureCheckoutError::Failed);
    }
    Ok(op)
}

pub async fn ensure_checkout_session<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    price_id: &str,
    success_url: &str,
    cancel_url: &str,
    test_clock: Option<&str>,
) -> Result<String, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let input = ctx
        .store
        .load_reconcile_input(user_id, None)
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    // Plan ids come only from `plan_prices`: an unmapped, unselectable,
    // or live-mode price disables the purchase (alert, no Stripe call),
    // it never mints a guess.
    let selectable = input
        .bindings
        .iter()
        .any(|binding| !binding.livemode && binding.selectable && binding.price_id == price_id);
    if !selectable {
        billing_alert(ctx, user_id, BILLING_PURCHASE_UNAVAILABLE_ACTION).await;
        return Err(EnsureCheckoutError::Unavailable);
    }
    // No second subscription while the live slot is held: an
    // active/past_due row means the account is subscribed (the
    // webhook-before-response race lands here — the purchase is refused
    // instead of minting a stray session), and any other stored status
    // except the known held ones is an unknown provider state failing
    // closed, which keeps new purchases disabled. Held (`incomplete`,
    // `unpaid`, `paused`) rows reserve the slot but confer nothing, so a
    // retry may still proceed below. Terminal rows sit outside the slot
    // (`live` is `None`) and never reach this guard.
    if let Some(live) = input.live.as_ref() {
        if !matches!(live.status.as_str(), "incomplete" | "unpaid" | "paused") {
            if !matches!(live.status.as_str(), "active" | "past_due") {
                billing_alert(ctx, user_id, BILLING_PURCHASE_UNAVAILABLE_ACTION).await;
            }
            return Err(EnsureCheckoutError::Unavailable);
        }
        // Held (`incomplete` / `unpaid` / `paused`) rows reserve the slot
        // but confer nothing: confirm the held subscription terminal (or
        // cancel it) and reconcile it out of the slot before minting the
        // replacement, so the replacement's reconcile never collides with
        // the still-held row. A retryable failure mints nothing.
        release_held_subscription_slot(ctx, user_id, &live.stripe_subscription_id).await?;
    }
    let open = ctx
        .store
        .open_checkout_operation(user_id)
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?;
    if let Some(mut op) = open {
        // A session-less operation whose recorded price no longer matches
        // is resolved under its ORIGINAL operation id and price before
        // anything retires it: a lost mark does not mean Stripe has no
        // session, so the same-key replay recovers it first and the flight
        // below retires it per its retrieved state. Only then does the
        // flight supersede — a replacement never mints beside a
        // possibly-payable session. A price-matched orphan still repairs
        // under its own id further below (same key replays the lost create,
        // never duplicates). An unresolvable flight (no bound customer to
        // replay with, or any Stripe failure) fails retryable with the
        // flight untouched: nothing supersedes beside unknown state.
        if op.session_id.is_none()
            && !crate::billing_reconcile::checkout_price_matches(&op, price_id)
        {
            let Some(customer_id) = input.stripe_customer_id.clone() else {
                return Err(EnsureCheckoutError::Failed);
            };
            let recorded = op.price_id.clone().unwrap_or_else(|| price_id.to_string());
            op = resolve_sessionless_operation(
                ctx,
                op,
                &recorded,
                &customer_id,
                success_url,
                cancel_url,
            )
            .await?;
        }
        if let Some(session_id) = op.session_id.clone() {
            // An open session is verified fresh against Stripe before reuse,
            // and its recorded price must match the newly requested one: a
            // second attempt naming a different plan never inherits the first
            // attempt's session. A confirmed-expired or Stripe-404-missing
            // session is deliberately replaced — the dead operation is
            // superseded (releasing the flight) and a fresh operation mints
            // under a fresh Stripe idempotency key, so a repair can never
            // replay a dead session inside Stripe's retention window. Lost
            // responses and same-price concurrent attempts still converge
            // here: no second session is minted for them.
            return match handle_flight_session(ctx, &op, &session_id, price_id).await? {
                FlightSessionDecision::Reuse(url) => Ok(url),
                FlightSessionDecision::Replace => {
                    create_fresh_checkout_session(
                        ctx,
                        user_id,
                        input.stripe_customer_id.clone(),
                        price_id,
                        success_url,
                        cancel_url,
                        test_clock,
                    )
                    .await
                }
                FlightSessionDecision::Completed => Err(EnsureCheckoutError::Unavailable),
                FlightSessionDecision::Retained => Err(EnsureCheckoutError::Failed),
            };
        }
        // Orphan operation (no session recorded — either never created, or
        // the mark was lost) whose price still matches: repair under its own
        // id (same key replays the lost create, never duplicates).
        // (Mismatched orphans resolved above and carry a session now.)
        let Some(customer_id) = input.stripe_customer_id.clone() else {
            billing_alert(ctx, user_id, BILLING_PURCHASE_UNAVAILABLE_ACTION).await;
            return Err(EnsureCheckoutError::Unavailable);
        };
        return create_session_under(ctx, &op.id, &customer_id, price_id, success_url, cancel_url)
            .await;
    }
    create_fresh_checkout_session(
        ctx,
        user_id,
        input.stripe_customer_id.clone(),
        price_id,
        success_url,
        cancel_url,
        test_clock,
    )
    .await
}

/// Fresh-operation purchase attempt (no usable open flight): mint the
/// operation id, ensure the customer (with the test-clock seam), then
/// insert single-flight — a racer converges on the winner's row below
/// instead of minting a second subscription. The converged operation's
/// recorded price is checked before minting: a single-flight loser that
/// asked for a different price replaces deliberately instead of
/// inheriting the winner's session.
async fn create_fresh_checkout_session<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    snapshot_customer: Option<String>,
    price_id: &str,
    success_url: &str,
    cancel_url: &str,
    test_clock: Option<&str>,
) -> Result<String, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let operation_id = new_operation_id().map_err(|_| EnsureCheckoutError::Failed)?;
    let mut snapshot_customer = snapshot_customer;
    let mut minted_customer: Option<String> = None;
    loop {
        let customer_id = match snapshot_customer.clone() {
            Some(customer_id) => customer_id,
            None => match minted_customer.clone() {
                Some(customer_id) => customer_id,
                None => {
                    let created = ctx
                        .stripe
                        .create_customer(crate::stripe::CreateCustomer {
                            operation_id: operation_id.clone(),
                            user_id: Some(user_id.to_string()),
                            test_clock: test_clock.map(str::to_string),
                        })
                        .await
                        .map_err(|_| EnsureCheckoutError::Failed)?;
                    minted_customer = Some(created.id.clone());
                    created.id
                }
            },
        };
        let outcome = ctx
            .store
            .create_checkout_operation(&operation_id, user_id, price_id, Some(&customer_id))
            .await
            .map_err(|_| EnsureCheckoutError::Failed)?;
        let converged = outcome.op();
        // A converged loser that named a different price must not inherit the
        // winner's session (which keeps the first attempt's price). A
        // session-less winner is still creating (or lost its mark): recover
        // under the winner's key and price first, exactly like an orphan —
        // otherwise the replace below would orphan a payable session.
        // Either way the winner's session retires per its retrieved state
        // before the retry mints under this attempt's fresh operation.
        if !crate::billing_reconcile::checkout_price_matches(converged, price_id) {
            let resolved = match converged.session_id.clone() {
                Some(_) => converged.clone(),
                None => {
                    let customer_id = converged_session_customer(
                        ctx,
                        user_id,
                        snapshot_customer.clone(),
                        customer_id.clone(),
                    )
                    .await?;
                    let recorded = converged
                        .price_id
                        .clone()
                        .unwrap_or_else(|| price_id.to_string());
                    resolve_sessionless_operation(
                        ctx,
                        converged.clone(),
                        &recorded,
                        &customer_id,
                        success_url,
                        cancel_url,
                    )
                    .await?
                }
            };
            let Some(session_id) = resolved.session_id.clone() else {
                return Err(EnsureCheckoutError::Failed);
            };
            match handle_flight_session(ctx, &resolved, &session_id, price_id).await? {
                // Unreachable: the price mismatches by construction, so the
                // winner's session is never reusable here. Fail safe — the
                // retry heals it, and nothing mints beside unknown state.
                FlightSessionDecision::Reuse(_) => return Err(EnsureCheckoutError::Failed),
                FlightSessionDecision::Replace => {
                    // Re-read the binding (the supersede released the flight, so the
                    // retry sees whatever the winner bound). A still-unbound retry
                    // reuses this attempt's already-minted customer instead of
                    // minting a second orphan.
                    snapshot_customer = snapshot_customer_fresh(ctx, user_id).await?;
                    continue;
                }
                FlightSessionDecision::Completed => return Err(EnsureCheckoutError::Unavailable),
                FlightSessionDecision::Retained => return Err(EnsureCheckoutError::Failed),
            }
        }
        // Converge on the winner-bound customer: a racing loser that started
        // unbound minted its own orphan customer before the insert, and the
        // pre-insert snapshot still names nothing. Re-read the binding — the
        // payable session must sit on the bound customer.
        let customer_id =
            converged_session_customer(ctx, user_id, snapshot_customer.clone(), customer_id)
                .await?;
        return create_session_under(
            ctx,
            &converged.id,
            &customer_id,
            price_id,
            success_url,
            cancel_url,
        )
        .await;
    }
}

/// Re-read the account's bound customer after a deliberate replacement
/// (the supersede released the flight, so the retry reads whatever the
/// winner bound). `None` when still unbound — the caller then reuses its
/// already-minted customer instead of minting a second orphan.
async fn snapshot_customer_fresh<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
) -> Result<Option<String>, EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    Ok(ctx
        .store
        .load_reconcile_input(user_id, None)
        .await
        .map_err(|_| EnsureCheckoutError::Failed)?
        .stripe_customer_id)
}

/// Deliberately retire an open flight: expire the still-open Stripe
/// session when one is named (so no second payable session lingers
/// beside its replacement), then mark the operation superseded — the
/// flight releases, and the replacement inserts a fresh operation with a
/// fresh Stripe idempotency key. Confirmed-dead sessions pass `None` (no
/// expiry call: Stripe already converged the state). Any expiry or store
/// failure fails the attempt retryable: a replacement never mints beside
/// a possibly-payable session.
async fn replace_checkout_operation<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    op: &crate::billing_reconcile::StoredCheckoutOp,
    expire_session_id: Option<&str>,
) -> Result<(), EnsureCheckoutError>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if let Some(session_id) = expire_session_id {
        ctx.stripe
            .expire_checkout_session(session_id.to_string())
            .await
            .map_err(|_| EnsureCheckoutError::Failed)?;
    }
    // A supersede is idempotent retirement: the conditional mark may
    // report `false` when the row already retired, which is success here.
    ctx.store
        .update_checkout_operation(
            &op.id,
            crate::billing_reconcile::SUPERSEDED_OP_STATE,
            &op.price_id.clone().unwrap_or_default(),
            op.session_id.as_deref(),
        )
        .await
        .map(|_| ())
        .map_err(|_| EnsureCheckoutError::Failed)
}

/// Every exact path the router serves. The Worker answers `OPTIONS` on
/// these paths — plus the single-segment dynamic revoke id and the
/// scope-map-served native paths (see `options_preflight_served`) — with
/// an empty 204 (the entry renders credentialed CORS headers only for an
/// exact `Origin` match); anything else 404s, so a preflight never maps
/// the API surface. The staging-only `POST /api/test-codes` and
/// `POST /api/test-accounts` seams are
/// deliberately absent: their POST arms match independently, and listing
/// them here would answer `OPTIONS` with 204 in every posture — making the
/// seams enumerable from production. `POST /api/ingest` IS listed: it is
/// a real route (cap guard today, write path later), not a seam. The
/// admin routes are absent for the same non-enumerability reason
/// (TWO-70 A-AC1): their callers are first-party and same-origin, so
/// nothing needs a preflight answer, and log lines name them `unknown`.
pub fn is_known_path(path: &str) -> bool {
    matches!(
        path,
        "/healthz"
            | "/api/ingest"
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
            | "/v1/plays"
            | "/v1/plays/heartbeat"
            | "/v1/credentials"
            | "/v1/subsonic-credentials"
            | "/v1/plans"
            | "/v1/me/usage"
            | "/v1/me/subscription"
            | "/v1/library"
            | "/api/library"
            | "/v1/artists"
            | "/v1/releases"
            | "/v1/tracks"
    )
}

/// Paths a browser preflight may map: exact dispatch paths, the
/// single-segment dynamic revoke id, and every scope-map-served native
/// path (a lawful bearer HEAD revalidation or PATCH edit must clear
/// preflight). Unserved shapes — unknown paths, empty or nested playlist
/// segments, nested credential ids — stay 404, so a preflight never maps
/// the API surface. A presented `Authorization` header never changes this
/// posture (the OPTIONS method maps `Deny`, so the bearer gate is
/// skipped): preflight answers the path, not the caller.
///
/// The `/v1/admin/` namespace is excluded wholesale (TWO-70 A-AC1): the
/// admin guard answers every non-admin caller with the plain 404, and a
/// 204 preflight would enumerate the surface the guard hides. Prefix,
/// not shape: every path under the namespace — real, mistyped or
/// imaginary — 404s identically, while lookalikes without the trailing
/// slash (`/v1/adminx`) never enter the namespace and keep their normal
/// (unserved) posture. The scope map still classifies the real admin
/// operations `SessionOnly` for the bearer gate; preflight simply never
/// consults it for these paths.
fn options_preflight_served(path: &str) -> bool {
    if path == "/v1/admin" || path.starts_with("/v1/admin/") {
        return false;
    }
    if is_known_path(path) {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/v1/credentials/") {
        return !rest.is_empty() && !rest.contains('/');
    }
    if let Some(rest) = path.strip_prefix("/v1/subsonic-credentials/") {
        return crate::meter_sink::is_uuid_shaped(rest);
    }
    // Playlist subpaths preflight only as single items: nested,
    // trailing-slash, or empty-segment shapes are denied before spend on
    // every write method and have no dispatch reader, so they stay
    // hidden on preflight too.
    if let Some(rest) = path.strip_prefix("/v1/playlists/") {
        if rest.is_empty() || rest.contains('/') {
            return false;
        }
    }
    // Release subpaths preflight only as single items for the same reason:
    // the detail reader serves exactly one segment, and deeper shapes
    // (including the reserved `/v1/releases/{mbid}/art` path, whose reader
    // has not landed) 404 in dispatch. The art card revisits this when its
    // reader lands.
    if let Some(rest) = path.strip_prefix("/v1/releases/") {
        if rest.is_empty() || rest.contains('/') {
            return false;
        }
    }
    ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"]
        .iter()
        .any(|method| {
            crate::native_rest::scope_for_operation(method, path)
                != crate::native_rest::RouteDecision::Deny
        })
}

// --- Stripe billing: webhook + single-flight checkout -------------------------------
//
// Checkout shape only, test mode only (ADR 0005). The webhook route is a
// signature carve-out: `Stripe-Signature` HMAC-SHA256 over the raw body
// is the authentication, so no session cookie, bearer, or Access gate
// applies here — and the scope map keeps the path `Deny` so bearer
// credentials can never enter it. Staging Cloudflare Access bypass is
// the wiring card's job, not this slice's. There is deliberately no
// user-facing checkout/portal route here either: `ensure_checkout_session`
// is the serialized purchase attempt those routes will call.
//
// Both the webhook and the inline/API paths reconcile through ONE
// function (`reconcile_remote_subscription` below, pure core in
// `billing_reconcile::reconcile_subscription`), so the same remote state
// always lands the same stored state whichever path runs first
// (webhook-before-response and lost-response races included).

/// Stripe webhook route. Unauthenticated by design (the signature is the
/// auth); stays out of `is_known_path` so browser preflight stays 404 and
/// the served log line keeps the `unknown` template like other unlisted
/// paths. Stripe retries non-2xx, so every failure here is either a 400
/// (sender's fault: fix and resend) or a 500 (ours: retried).
pub const BILLING_WEBHOOK_PATH: &str = "/v1/billing/webhook";

/// Audit action for a fail-closed reconcile pass: observed state stored,
/// nothing advanced, new purchases disabled.
const BILLING_FAIL_CLOSED_ACTION: &str = "billing.reconcile_fail_closed";
/// Audit action for a verified webhook that binds to no user (unknown or
/// tombstoned customer): acked 200 without state change; live
/// cancel-and-alert belongs to the account-deletion slice.
const BILLING_UNKNOWN_CUSTOMER_ACTION: &str = "billing.webhook_unknown_customer";
/// Audit action when a purchase cannot proceed (price unmapped or
/// unselectable, binding inconsistent): no Stripe call made.
const BILLING_PURCHASE_UNAVAILABLE_ACTION: &str = "billing.purchase_unavailable";

/// Empty 204 for a preflight against a known path. Carries no ACA headers
/// itself: the entry appends them exactly when the request `Origin` equals
/// the configured origin (never reflected, never `*`). `pub(crate)` so the
/// entry's render test pins the real preflight shape instead of rebuilding it.
pub(crate) fn preflight_response() -> Response {
    Response {
        status: 204,
        body: String::new(),
        set_cookie: None,
        retry_after: None,
        extra_headers: Vec::new(),
    }
}

/// Route one request. Unknown routes are a plain 404; known routes map
/// every domain failure through their surface's single generic body.
/// A PRESENTED bearer token is authenticated here off `req.authorization`
/// and never rescued by a session cookie (no session-cookie fallback for
/// a presented credential); a request with NO `Authorization` header skips
/// the bearer gate and falls through to the dispatch below, so session
/// cookie holders see no behavior change (ADR 0010). The catalogue slice-1
/// `/v1` data routes (`/v1/library`, `/v1/artists`, `/v1/releases`,
/// `/v1/tracks`, `/v1/releases/{mbid}`) run their session-or-bearer
/// dispatch off the identity captured here; the `/api/library` legacy
/// alias sits outside the bearer scope map (like `/api/ingest`) and runs
/// its own session-or-bearer-session dispatch in the handler — a presented
/// `Authorization` header is verified as a session with no cookie rescue,
/// a header-absent call falls back to the session cookie; every other
/// bearer-scope route still lands on the 404 below — default-deny holds
/// until its route lands the same way. A presented-but-invalid bearer fails
/// closed here with that same plain 404 — uniform posture, never a distinct
/// 401 that would disclose route existence — and is never rescued by a cookie.
/// `POST /v1/plays` is the other landed exception: it runs as a
/// cookie-session route behind the same gate, so a cookie-only caller is
/// served by the handler while a gate-passing bearer still needs the session
/// (native bearer issuance is a later slice).
pub async fn handle<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.body.len() > max_body_bytes(&req.method, &req.path) {
        // Oversized admin uploads answer the 413 failure shape (with a
        // null job: nothing was parsed, so no row and no audit), never
        // the generic 400. This branch serves only direct-handle
        // callers: the entry's collection gate answers the uniform 400
        // instead, because it runs before authentication and a
        // route-distinctive answer there would disclose the admin
        // ingest path to unauthenticated callers.
        if req.method == "POST" && req.path == "/v1/admin/ingest" {
            return admin_ingest_oversized();
        }
        return invalid_request();
    }
    // Bearer identity for the catalogue arms. The gate already spent the
    // lookup budgets, so the arms reuse this row — never a second
    // `authorize_bearer` call, which would double-spend rate-limit budgets.
    let mut bearer_user: Option<String> = None;
    if let crate::native_rest::RouteDecision::Bearer(required) =
        crate::native_rest::scope_for_operation(&req.method, &req.path)
    {
        // Only a presented Authorization header enters the bearer gate:
        // header-absent callers (session cookies, anonymous) fall through
        // to the dispatch below.
        if req.authorization.is_some() {
            match authorize_bearer(ctx, req.authorization.as_deref(), required).await {
                Ok(row) => bearer_user = Some(row.record.user_id.clone()),
                Err(resp) => return resp,
            }
        }
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/healthz") => json_response(
            200,
            serde_json::json!({"ok": true, "build_sha": ctx.cfg.build_sha}),
        ),
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
        ("GET", "/v1/plans") => plans_list(ctx, req).await,
        ("GET", "/v1/me/usage") => usage_get(ctx, req).await,
        ("GET", "/v1/me/subscription") => subscription_get(ctx, req).await,
        ("POST", QA_TEST_CODES_PATH) => qa_test_codes(ctx, req).await,
        ("POST", QA_TEST_ACCOUNTS_PATH) => qa_test_accounts(ctx, req).await,
        // NZB intake cap guard: refuses capped callers with the plan-limit
        // wire, 404s everyone else until the write path lands. The scope
        // map denies this path to bearer credentials, so the gate above
        // skips it and the guard owns session authentication.
        ("POST", "/api/ingest") => ingest_cap_guard(ctx, req).await,
        // Play grant issuance + lease heartbeat: cookie-session handlers
        // (see the section above). A presented native-rest bearer on
        // `/v1/plays` is gate-checked above (invalid keys plain-404)
        // but does not authenticate here; the heartbeat path is `Deny`
        // in the scope map, so it never enters the gate.
        ("POST", "/v1/plays") => play_issue(ctx, req).await,
        ("POST", "/v1/plays/heartbeat") => play_heartbeat(ctx, req).await,
        // Internal meter sink: HMAC-authenticated inside the handler
        // (crate::meter_sink), no session/bearer surface. The scope
        // map denies this path to bearer credentials, so the gate
        // above never sees its Authorization header; it is absent
        // from `is_known_path` on purpose (no browser preflight).
        ("POST", crate::meter_sink::SINK_PATH) => meter_sink_ingest(ctx, req).await,
        ("POST", "/v1/subsonic-credentials") => subsonic_handlers::mint(ctx, req).await,
        ("GET", "/v1/subsonic-credentials") => subsonic_handlers::list(ctx, req).await,
        ("DELETE", path) if path.starts_with("/v1/subsonic-credentials/") => {
            subsonic_handlers::revoke(ctx, req).await
        }
        ("POST", "/v1/credentials") => credentials_mint(ctx, req).await,
        ("GET", "/v1/credentials") => credentials_list(ctx, req).await,
        // Stripe webhook carve-out: unauthenticated by design (the
        // `Stripe-Signature` HMAC over the raw body is the auth), so it
        // bypasses the bearer gate above like the cookie/session paths do
        // — and the scope map keeps the path `Deny` so bearer credentials
        // can never enter it. Deliberately outside `is_known_path`.
        ("POST", BILLING_WEBHOOK_PATH) => billing_webhook(ctx, req).await,
        // Catalogue slice 1: session-or-bearer reads over the
        // ingest ledger plus the MusicBrainz mirror.
        ("GET", "/v1/library") => catalogue_library(ctx, req, bearer_user.as_deref()).await,
        // Legacy alias the M1 journey owns: the same entries as
        // `/v1/library`, never a reshaped contract — but outside the
        // bearer scope map, so its handler owns authentication (the
        // `/api/ingest` precedent): a presented `Authorization` header is
        // verified as a session with no cookie rescue, a header-absent
        // call falls back to the session cookie.
        ("GET", "/api/library") => catalogue_library_alias(ctx, req).await,
        ("GET", "/v1/artists") => catalogue_artists(ctx, req, bearer_user.as_deref()).await,
        ("GET", "/v1/releases") => catalogue_releases(ctx, req, bearer_user.as_deref()).await,
        ("GET", "/v1/tracks") => catalogue_tracks(ctx, req, bearer_user.as_deref()).await,
        // Unserved nested release paths never reach authentication or stores.
        ("GET", path)
            if path
                .strip_prefix("/v1/releases/")
                .is_some_and(|rest| !rest.is_empty() && !rest.contains('/')) =>
        {
            catalogue_release(ctx, req, bearer_user.as_deref()).await
        }
        // Single-segment credential ids only; deeper shapes 404 inside
        // the handler. `options_preflight_served` keeps browser preflights
        // working for these dynamic paths (exact-path `is_known_path`
        // cannot name them).
        ("DELETE", path) if path.starts_with("/v1/credentials/") => {
            credentials_revoke(ctx, req).await
        }
        ("GET", "/v1/admin/settings") => admin_settings_list(ctx, req).await,
        ("POST", "/v1/admin/ingest") => admin_ingest_upload(ctx, req).await,
        // The entry preserves the list query string for exactly this
        // route (the runtime strips it everywhere else), so the status
        // filter and window survive to the handler in production.
        ("GET", path) if path == "/v1/admin/ingest" || path.starts_with("/v1/admin/ingest?") => {
            admin_ingest_list(ctx, req).await
        }
        // Single-segment job ids only; deeper or malformed shapes 404
        // inside the handler.
        ("GET", path) if path.starts_with("/v1/admin/ingest/") => admin_ingest_get(ctx, req).await,
        // Single-segment setting keys and `{id}/role` only; deeper or
        // malformed shapes 404 inside the handlers. Neither path gets
        // an `is_known_path` or preflight answer: the admin namespace
        // is non-enumerable (see `options_preflight_served`).
        ("PUT", path) if path.starts_with("/v1/admin/settings/") => {
            admin_setting_put(ctx, req).await
        }
        ("PUT", path) if path.starts_with("/v1/admin/users/") => {
            admin_user_role_put(ctx, req).await
        }
        // Admin invites (migration 0033): issue, list (with the
        // `/status/{s}` and `/issuer/{id}` segment filters), revoke.
        // Same non-enumerable posture as the rest of the namespace.
        ("POST", "/v1/admin/invites") => admin_invite_issue(ctx, req).await,
        ("GET", "/v1/admin/invites") => admin_invite_list(ctx, req).await,
        ("GET", path) if path.starts_with("/v1/admin/invites/") => {
            admin_invite_list(ctx, req).await
        }
        ("POST", path) if path.starts_with("/v1/admin/invites/") => {
            admin_invite_revoke(ctx, req).await
        }
        // Preflight answers exactly the served paths (exact dispatch
        // paths, the single-segment revoke id, and the scope-map-served
        // native paths); unserved shapes stay 404 on preflight too.
        ("OPTIONS", path) if options_preflight_served(path) => preflight_response(),
        _ => not_found(),
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
        /// Staging-only code-seam outbox: latest raw token per synthetic
        /// mailbox (mirrors the migration 0018 table).
        qa_codes: RefCell<HashMap<String, QaIssuedCode>>,
        invites: RefCell<HashMap<String, InviteRecord>>,
        /// Next surrogate invite id to hand out (mirror of the
        /// migration 0033 identity column).
        next_invite_id: RefCell<i64>,
        /// Mirror of `users.invited_by` (migration 0033), keyed by the
        /// invited user's id.
        invited_by: RefCell<HashMap<String, Option<String>>>,
        sessions: RefCell<HashMap<String, SessionRecord>>,
        refresh_retries: RefCell<HashMap<String, (String, u64)>>,
        lose_next_refresh_ack: RefCell<bool>,
        native_credentials: RefCell<HashMap<String, NativeCredentialRow>>,
        subsonic_credentials:
            RefCell<HashMap<String, (String, crate::subsonic_lifecycle::ListedCredential)>>,
        fail_subsonic_write: RefCell<bool>,
        revoke_session_before_mint: RefCell<bool>,
        fail_bulk_revoke: RefCell<bool>,
        /// User ids that still own a legacy `provider_vaults` row. The
        /// product no longer reads or writes vaults; the reaper and
        /// magic admission keep protecting these accounts.
        legacy_vault_users: RefCell<HashSet<String>>,
        audits: RefCell<Vec<AuditEvent>>,
        /// Mirror of `users.role` (migration 0028): an absent entry is
        /// the column default (`member`) for an existing user.
        user_roles: RefCell<HashMap<String, String>>,
        /// Mirror of `app_settings` (migration 0028), keyed by setting
        /// key with the raw stored text.
        app_settings_rows: RefCell<HashMap<String, String>>,
        rate_limit_slots: RefCell<HashMap<(String, String, u32), u64>>,
        delete_user_before_next_challenge: RefCell<Option<String>>,
        delete_user_after_next_challenge: RefCell<Option<String>>,
        orphan_user_after_next_challenge: RefCell<Option<String>>,
        challenge_reads: RefCell<usize>,
        rate_limit_slot_probes: RefCell<usize>,
        /// Bearer-gate pins: every `reserve_rate_limit_pair` call and every
        /// `native_credential_by_hmac` lookup. The shape gate must return
        /// before either is touched, so a malformed token leaves both at
        /// zero while a well-formed unknown key increments both.
        rate_limit_pair_calls: RefCell<usize>,
        native_credential_lookups: RefCell<usize>,
        fail_session_insert: RefCell<bool>,
        fail_magic_audit: RefCell<bool>,
        /// Role-read outage pin: `user_role` fails like a production
        /// query error, which the admin guard must collapse to the
        /// plain 404 (never a distinguishing 500).
        fail_user_role_lookup: RefCell<bool>,
        /// Session-row lookup outage pin: `session_by_id` fails like a
        /// production query error, which `authenticate` maps to a 500.
        fail_session_lookup: RefCell<bool>,
        lose_magic_claim: RefCell<bool>,
        magic_redemptions: RefCell<usize>,
        fail_reauth_revocation: RefCell<bool>,
        fail_reauth_insert: RefCell<bool>,
        /// Bearer-gate store-error pin: when set, the HMAC lookup fails
        /// like a production query outage instead of answering Ok.
        fail_native_credential_lookup: RefCell<bool>,
        /// Users whose `users.deleting_at` is set (ADR 0002 rule 3):
        /// fenced writers refuse them, mirroring the production SQL.
        deleting_users: RefCell<std::collections::HashSet<String>>,
        /// Mirror of the daily ingest-job count, keyed by
        /// `(user_id, UTC epoch day)`: the epoch-day key matches the
        /// production UTC-day boundary over `ingest_jobs.accepted_at`.
        usage: RefCell<HashMap<(String, u64), u64>>,
        /// Stripe billing mirror: bound customer per user, the stored
        /// subscription row per user, and the full `plan_prices` map the
        /// reconcile input loads whole.
        billing_customers: RefCell<HashMap<String, String>>,
        billing_rows: RefCell<HashMap<String, crate::billing_reconcile::ReconciledRow>>,
        // Displaced rows are retained without changing the per-user projection.
        billing_shadow: RefCell<HashMap<(String, String), crate::billing_reconcile::ReconciledRow>>,
        billing_bindings: RefCell<Vec<crate::billing_reconcile::PriceBinding>>,
        /// Claimed webhook event ids (first claim wins, mirroring
        /// `billing_webhook_events`).
        webhook_events: RefCell<HashSet<String>>,
        /// Single-flight checkout operations by id.
        checkout_ops: RefCell<HashMap<String, crate::billing_reconcile::StoredCheckoutOp>>,
        /// Pre-bind window pin: when set, `create_checkout_operation`
        /// inserts the winner's row WITHOUT binding the customer, mirroring
        /// production where the op INSERT and the bind are separate
        /// statements. A loser converging while the flag is set sees the op
        /// but no binding.
        defer_customer_bind: RefCell<bool>,
        /// Race pin: when set, `create_checkout_operation` lands this
        /// winner row just before its own insert, mirroring a racer that
        /// wins the single flight between this attempt's open-flight read
        /// and its insert. The attempt must converge onto (and, on a price
        /// mismatch, deliberately replace) the winner.
        plant_winner_on_next_create: RefCell<Option<crate::billing_reconcile::StoredCheckoutOp>>,
        /// Paid-invoice history by Stripe invoice id (idempotent on the id,
        /// mirroring `paid_invoice_history`).
        invoice_history: RefCell<HashMap<String, crate::billing_reconcile::PaidInvoiceRow>>,
        /// Billing store-outage pin: when set, every billing method fails
        /// like a production query outage instead of touching state.
        fail_billing: RefCell<bool>,
        /// Billing display reads: plan rows served verbatim, the live-slot
        /// subscription per user, whether any subscription row ever
        /// existed, and metered stream seconds keyed by
        /// `(user_id, period_month)` mirroring the ledger SUM.
        plans: RefCell<Vec<PlanRow>>,
        subscriptions: RefCell<HashMap<String, SubscriptionRow>>,
        ever_subscribed: RefCell<HashSet<String>>,
        stream_seconds: RefCell<HashMap<(String, String), u64>>,
        /// Billing-read outage pin: the three display reads fail like a
        /// production query error (generic 500, never invented data).
        fail_billing_reads: RefCell<bool>,
        /// Mirror of the admin ingest ledger (migration 0034): stored
        /// admin job rows by content-derived id. Pipeline rows never
        /// live here — the readers fence on `source = 'admin'` in
        /// production and this map holds admin rows only.
        ingest_jobs: RefCell<HashMap<String, StoredAdminIngestJob>>,
        /// Mirror of `ingest_originals`: the stored v2 envelope plus
        /// the content hash and KEK version the AAD binds, keyed by job
        /// id. Accepted uploads land exactly one entry; failed uploads
        /// and duplicates land none (a duplicate keeps the first
        /// envelope, like production's `ON CONFLICT DO NOTHING`).
        ingest_originals:
            RefCell<HashMap<String, (crate::admin_ingest::IngestOriginalEnvelope, String, u32)>>,
        /// Live-slot subscription rows by user (the playback gate's
        /// `resolve_effective_plan` input).
        playback_subscriptions: RefCell<HashMap<String, crate::playback::SubscriptionRow>>,
        /// Users whose history holds any paid coverage
        /// (`ever_subscribed` → the `lapsed` discriminator).
        ever_paid_users: RefCell<HashSet<String>>,
        /// `plans` caps rows by plan id; an absent id is the
        /// missing-row case the handler fails closed to free caps.
        plan_caps_rows: RefCell<HashMap<String, crate::plan_limits::PlanCaps>>,
        /// One `playback_leases` row per user.
        playback_leases: RefCell<HashMap<String, crate::playback::LeaseRow>>,
        /// Deletion-marker race pin: when set, the next lease write
        /// (`acquire_playback_lease` / `touch_playback_lease`) first
        /// lands the named user in `deleting_users`, mirroring a marker
        /// that arrives after the handler's up-front `account_deleting`
        /// probe but before the statement's writer fence evaluates.
        mark_deleting_before_next_lease_write: RefCell<Option<String>>,
        /// Derivation metadata keyed by `(user_id, track_id)`; absent
        /// pairs resolve to the all-`None` meta (the fallback rung),
        /// mirroring the production zero-row answer.
        meter_meta: RefCell<HashMap<(String, String), crate::meter_sink::TrackMeterMeta>>,
        /// Stored play windows by idempotency key, mirroring the
        /// monotonic upsert (see `record_play_window`).
        play_windows: RefCell<HashMap<String, crate::ledger::PlayWindow>>,
        /// Catalogue slice-1 fixtures: joined library rows by user id plus
        /// the shared mirror collections and per-release track lists.
        /// Tests seed these in store (query) order; the bound and the
        /// ordering itself are pinned by the SQL regression test, not here.
        catalogue_library: RefCell<HashMap<String, Vec<crate::catalogue::LibraryDbRow>>>,
        catalogue_artists: RefCell<Vec<crate::catalogue::ArtistDbRow>>,
        catalogue_releases: RefCell<Vec<crate::catalogue::ReleaseDbRow>>,
        catalogue_tracks: RefCell<Vec<crate::catalogue::TrackDbRow>>,
        catalogue_release_tracks:
            RefCell<HashMap<String, Vec<crate::catalogue::ReleaseTrackDbRow>>>,
        /// Catalogue store-error pin: when set, every catalogue read fails
        /// like a production query outage.
        fail_catalogue_reads: RefCell<bool>,
        /// Fail only the second read in release detail, after membership.
        fail_catalogue_release_tracks: RefCell<bool>,
    }

    fn merge_billing_row(
        existing: Option<&crate::billing_reconcile::ReconciledRow>,
        outcome: &crate::billing_reconcile::ReconcileOutcome,
    ) -> crate::billing_reconcile::ReconciledRow {
        // Mirrors the production upsert's monotonic coverage CASE: a
        // pass that folded an older snapshot commits without
        // regressing confirmed `paid_through_at` (or the plan and
        // anchor tied to it). An equal-end pass carrying an anchor
        // relabels the plan and anchor (the defined equal-end
        // plan-switch outcome) — but only when the stored row still
        // matches what the pass read — so a keep committing after a
        // relabel, or an older relabel committing after a newer one,
        // keeps the stored ones instead of reversing the newer
        // plan/anchor. An anchor-less tie keeps the stored ones too.
        // The same staleness predicate guards
        // `status` and `grace_until`: strictly older money — or the
        // anchor-less equal-end shape only the stale fold emits — keeps
        // the stored verbatim fields instead of writing stale ones over
        // a fresher pass. Bindings still update verbatim; history still
        // converges on the invoice id.
        match existing {
            Some(existing)
                if existing.stripe_subscription_id == outcome.row.stripe_subscription_id =>
            {
                let advances = outcome.row.paid_through_unix.is_some_and(|end| {
                    existing
                        .paid_through_unix
                        .is_none_or(|previous| end > previous)
                });
                let relabels = !advances
                && outcome.row.paid_invoice_id.is_some()
                && matches!(
                    (outcome.row.paid_through_unix, existing.paid_through_unix),
                    (Some(end), Some(previous)) if end == previous
                )
                // Mirrors the tie arm (`subscriptions.plan_id =
                // $14 AND subscriptions.paid_invoice_id IS NOT
                // DISTINCT FROM $13`): only a pass whose read row
                // is still stored relabels — a late keep, or an
                // older relabel, committing after the row moved
                // keeps the stored plan/anchor. `None` read plan
                // never equals a stored plan, so an anchor-less
                // keep never relabels either.
                && outcome.read_plan.as_deref() == Some(existing.plan_id.as_str())
                && existing.paid_invoice_id == outcome.read_anchor;
                let stale = match (outcome.row.paid_through_unix, existing.paid_through_unix) {
                    (Some(end), Some(previous)) => {
                        end < previous
                            || (end == previous
                                && outcome.row.paid_invoice_id.is_none()
                                && existing.paid_invoice_id.is_some())
                    }
                    _ => false,
                };
                let mut row = outcome.row.clone();
                if !advances && !relabels {
                    row.paid_through_unix = existing.paid_through_unix;
                    row.plan_id = existing.plan_id.clone();
                    row.paid_invoice_id = existing.paid_invoice_id.clone();
                } else if row.paid_invoice_id.is_none() {
                    row.paid_invoice_id = existing.paid_invoice_id.clone();
                }
                if stale {
                    row.status = existing.status.clone();
                    row.grace_until_unix = existing.grace_until_unix;
                }
                row
            }
            _ => outcome.row.clone(),
        }
    }

    impl FakeStore {
        /// Member releases for a fake user, mirroring the production
        /// membership read: distinct well-formed release MBIDs from the
        /// fake ledger, sorted for a stable page.
        fn fake_member_releases(&self, user_id: &str) -> Vec<String> {
            let mut mbids: Vec<String> = self
                .catalogue_library
                .borrow()
                .get(user_id)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|row| {
                    row.release_mbid
                        .as_deref()
                        .and_then(crate::catalogue::enrichment_key)
                })
                .collect();
            mbids.sort();
            mbids.dedup();
            mbids
        }
        /// Member (release, recording) pairs for a fake user, mirroring
        /// the production pairs read: exact well-formed pairs from the
        /// fake ledger's full recording arrays, sorted for a stable page.
        fn fake_member_pairs(&self, user_id: &str) -> Vec<(String, String)> {
            let mut pairs: Vec<(String, String)> = self
                .catalogue_library
                .borrow()
                .get(user_id)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|row| {
                    row.release_mbid
                        .as_deref()
                        .and_then(crate::catalogue::enrichment_key)
                        .map(|release| {
                            row.recording_mbids
                                .iter()
                                .filter_map(|recording| {
                                    crate::catalogue::enrichment_key(recording)
                                        .map(|recording| (release.clone(), recording))
                                })
                                .collect::<Vec<_>>()
                        })
                })
                .flatten()
                .collect();
            pairs.sort();
            pairs.dedup();
            pairs
        }
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
        async fn complete_registration(
            &self,
            challenge: &PasskeyChallenge,
            magic: &MagicLinkRecord,
            invite: Option<&InviteRecord>,
            invite_audit: Option<&AuditEvent>,
            credential: &PasskeyCredential,
            now: u64,
        ) -> Result<(), StoreError> {
            // Mirror the production transaction, including rollback when a
            // later conditional claim loses or credential insertion fails.
            let challenges_before = self.challenges.borrow().clone();
            let magic_before = self.magic.borrow().clone();
            let invites_before = self.invites.borrow().clone();
            let audits_before = self.audits.borrow().len();
            let invited_by_before = self.invited_by.borrow().clone();
            let result = async {
                let mut claims = vec![
                    auth::SingleUseClaim::Challenge {
                        record: challenge,
                        now,
                    },
                    auth::SingleUseClaim::MagicLink { record: magic, now },
                ];
                if let Some(record) = invite {
                    claims.push(auth::SingleUseClaim::Invite { record });
                }
                for claim in claims {
                    if self.claim(claim).await.map_err(|_| StoreError)? != 1 {
                        return Err(StoreError);
                    }
                }
                if let Some(record) = invite {
                    // Mirror the production stamps + in-transaction
                    // audit: `invited_by` comes from the CLAIMED stored
                    // row, not the caller's pre-claim snapshot, so an
                    // issuer deleted after `invite_by_hash` (stored
                    // `issued_by` already nulled, ON DELETE SET NULL)
                    // cannot break the enrollment.
                    let claimed_issuer = self
                        .invites
                        .borrow()
                        .get(&record.code_hash)
                        .and_then(|claimed| claimed.issued_by.clone());
                    self.invited_by
                        .borrow_mut()
                        .insert(credential.user_id.clone(), claimed_issuer);
                    let audit = invite_audit.ok_or(StoreError)?;
                    self.audits.borrow_mut().push(audit.clone());
                }
                self.insert_credential(credential).await
            }
            .await;
            if result.is_err() {
                *self.challenges.borrow_mut() = challenges_before;
                *self.magic.borrow_mut() = magic_before;
                *self.invites.borrow_mut() = invites_before;
                self.audits.borrow_mut().truncate(audits_before);
                *self.invited_by.borrow_mut() = invited_by_before;
            }
            result
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
            let mut magic = self.magic.borrow_mut();
            magic.retain(|_, row| {
                row.user_id != record.user_id || row.expires_at > record.issued_at
            });
            magic.insert(record.token_hash.clone(), record.clone());
            Ok(())
        }
        async fn magic_by_hash(
            &self,
            token_hash: &str,
        ) -> Result<Option<MagicLinkRecord>, StoreError> {
            Ok(self.magic.borrow().get(token_hash).cloned())
        }
        async fn record_qa_issued_code(
            &self,
            email: &str,
            token: &str,
            now: u64,
        ) -> Result<(), StoreError> {
            self.qa_codes.borrow_mut().insert(
                email.to_string(),
                QaIssuedCode {
                    token: token.to_string(),
                    issued_at: now,
                },
            );
            Ok(())
        }
        async fn qa_issued_code(&self, email: &str) -> Result<Option<QaIssuedCode>, StoreError> {
            Ok(self.qa_codes.borrow().get(email).cloned())
        }
        async fn ingest_jobs_today(&self, user_id: &str, now: u64) -> Result<u64, StoreError> {
            Ok(self
                .usage
                .borrow()
                .get(&(user_id.to_string(), now / 86_400))
                .copied()
                .unwrap_or(0))
        }
        async fn seed_qa_ingest_usage(
            &self,
            user_id: &str,
            now: u64,
            ingest_jobs: u64,
        ) -> Result<(), StoreError> {
            let mut usage = self.usage.borrow_mut();
            let slot = usage
                .entry((user_id.to_string(), now / 86_400))
                .or_insert(0);
            *slot = (*slot).max(ingest_jobs);
            Ok(())
        }
        async fn plans(&self) -> Result<Vec<PlanRow>, StoreError> {
            if *self.fail_billing_reads.borrow() {
                return Err(StoreError);
            }
            Ok(self.plans.borrow().clone())
        }
        async fn subscription_snapshot(
            &self,
            user_id: &str,
        ) -> Result<(Option<SubscriptionRow>, bool), StoreError> {
            if *self.fail_billing_reads.borrow() {
                return Err(StoreError);
            }
            Ok((
                self.subscriptions.borrow().get(user_id).cloned(),
                self.ever_subscribed.borrow().contains(user_id),
            ))
        }
        async fn stream_seconds_month(
            &self,
            user_id: &str,
            period_month: &str,
        ) -> Result<u64, StoreError> {
            if *self.fail_billing_reads.borrow() {
                return Err(StoreError);
            }
            Ok(self
                .stream_seconds
                .borrow()
                .get(&(user_id.to_string(), period_month.to_string()))
                .copied()
                .unwrap_or(0))
        }
        async fn live_subscription(
            &self,
            user_id: &str,
        ) -> Result<Option<crate::playback::SubscriptionRow>, StoreError> {
            Ok(self.playback_subscriptions.borrow().get(user_id).cloned())
        }
        async fn ever_subscribed(&self, user_id: &str) -> Result<bool, StoreError> {
            Ok(self.ever_paid_users.borrow().contains(user_id))
        }
        async fn plan_caps(
            &self,
            plan_id: &str,
        ) -> Result<Option<crate::plan_limits::PlanCaps>, StoreError> {
            Ok(self.plan_caps_rows.borrow().get(plan_id).copied())
        }
        async fn monthly_stream_seconds(
            &self,
            user_id: &str,
            period_month: &str,
        ) -> Result<u64, StoreError> {
            Ok(self
                .stream_seconds
                .borrow()
                .get(&(user_id.to_string(), period_month.to_string()))
                .copied()
                .unwrap_or(0))
        }
        async fn playback_lease(
            &self,
            user_id: &str,
        ) -> Result<Option<crate::playback::LeaseRow>, StoreError> {
            Ok(self.playback_leases.borrow().get(user_id).cloned())
        }
        async fn acquire_playback_lease(
            &self,
            user_id: &str,
            session_id: &str,
            track_id: &str,
            play_id: &str,
            now: u64,
            expires_at: u64,
            takeover: bool,
        ) -> Result<crate::playback::LeaseAcquire, StoreError> {
            // Mirror the conditional upsert: insert at epoch 1, or
            // replace at epoch+1 when expired, same-session, or
            // takeover — else Held with the remaining TTL. The
            // statement's deletion fence writes nothing (zero rows,
            // which the Pg glue reads back as Held), no epoch burned.
            if let Some(marker) = self.mark_deleting_before_next_lease_write.take() {
                self.deleting_users.borrow_mut().insert(marker);
            }
            if self.deleting_users.borrow().contains(user_id) {
                return Ok(crate::playback::LeaseAcquire::Held {
                    retry_after_secs: self
                        .playback_leases
                        .borrow()
                        .get(user_id)
                        .map_or(0, |row| row.retry_after_secs(now)),
                });
            }
            let mut leases = self.playback_leases.borrow_mut();
            match leases.get(user_id) {
                Some(existing)
                    if existing.is_live(now) && existing.session_id != session_id && !takeover =>
                {
                    Ok(crate::playback::LeaseAcquire::Held {
                        retry_after_secs: existing.retry_after_secs(now),
                    })
                }
                existing => {
                    let epoch = existing.map_or(1, |row| row.epoch + 1);
                    leases.insert(
                        user_id.to_string(),
                        crate::playback::LeaseRow {
                            session_id: session_id.to_string(),
                            track_id: track_id.to_string(),
                            play_id: play_id.to_string(),
                            epoch,
                            expires_at,
                        },
                    );
                    Ok(crate::playback::LeaseAcquire::Acquired { epoch })
                }
            }
        }
        async fn touch_playback_lease(
            &self,
            user_id: &str,
            session_id: &str,
            play_id: &str,
            epoch: u64,
            _now: u64,
            expires_at: u64,
        ) -> Result<bool, StoreError> {
            // Mirror the conditional UPDATE: only the exact
            // (session, play, epoch) holder extends, monotonically
            // (an out-of-order older deadline never shortens), and
            // never for a deleting account (in-statement fence).
            if let Some(marker) = self.mark_deleting_before_next_lease_write.take() {
                self.deleting_users.borrow_mut().insert(marker);
            }
            if self.deleting_users.borrow().contains(user_id) {
                return Ok(false);
            }
            let mut leases = self.playback_leases.borrow_mut();
            match leases.get_mut(user_id) {
                Some(row)
                    if row.session_id == session_id
                        && row.play_id == play_id
                        && row.epoch == epoch =>
                {
                    row.expires_at = row.expires_at.max(expires_at);
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn track_meter_meta(
            &self,
            user_id: &str,
            track_id: &str,
        ) -> Result<crate::meter_sink::TrackMeterMeta, StoreError> {
            Ok(self
                .meter_meta
                .borrow()
                .get(&(user_id.to_string(), track_id.to_string()))
                .copied()
                .unwrap_or_default())
        }
        async fn record_play_window(
            &self,
            window: &crate::ledger::PlayWindow,
        ) -> Result<crate::meter_sink::PlayWindowOutcome, StoreError> {
            use crate::meter_sink::PlayWindowOutcome;
            // Mirror the production writer fence: the upsert's guard
            // SELECT finds no row for a deleting or nonexistent user,
            // so nothing writes and the sink acks `fenced`.
            if self.deleting_users.borrow().contains(&window.user_id)
                || !self.users.borrow().contains_key(&window.user_id)
            {
                return Ok(PlayWindowOutcome::Fenced);
            }
            // Mirror the monotonic upsert: a stored row already
            // covering this union wins; otherwise the new snapshot
            // replaces it and the monthly total moves by the delta.
            let mut windows = self.play_windows.borrow_mut();
            let previous = windows.get(&window.idempotency_key);
            if let Some(existing) = previous {
                if existing.bytes_served >= window.bytes_served {
                    return Ok(PlayWindowOutcome::Covered);
                }
            }
            let previous_seconds = previous.map_or(0, |w| w.stream_seconds);
            windows.insert(window.idempotency_key.clone(), window.clone());
            drop(windows);
            let mut seconds = self.stream_seconds.borrow_mut();
            let entry = seconds
                .entry((window.user_id.clone(), window.period_month.clone()))
                .or_insert(0);
            *entry = entry
                .saturating_sub(previous_seconds)
                .saturating_add(window.stream_seconds);
            Ok(PlayWindowOutcome::Written)
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
            *self.rate_limit_pair_calls.borrow_mut() += 1;
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
            let vault_owners: HashSet<String> = self.legacy_vault_users.borrow().clone();
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
            let admins: HashSet<String> = self
                .user_roles
                .borrow()
                .iter()
                .filter(|(_, role)| crate::admin::is_admin_role(Some(role)))
                .map(|(id, _)| id.clone())
                .collect();
            let deleting_users = self.deleting_users.borrow();
            self.users.borrow_mut().retain(|_, user| {
                user.created_at > cutoff
                    || deleting_users.contains(&user.id)
                    || admins.contains(&user.id)
                    || credentialed.contains(&user.id)
                    || active_registrations.contains(&user.id)
                    || vault_owners.contains(&user.id)
                    || session_owners.contains(&user.id)
                    || magic_owners.contains(&user.id)
                    || protected_user_id == Some(user.id.as_str())
            });
            Ok(())
        }
        async fn invite_by_hash(
            &self,
            code_hash: &str,
        ) -> Result<Option<InviteRecord>, StoreError> {
            // Mirror the production join: the issuer's current email
            // rides the record so the pure prepare can refuse
            // self-invites.
            let mut record = self.invites.borrow().get(code_hash).cloned();
            if let Some(record) = &mut record {
                record.issuer_email = record
                    .issued_by
                    .as_deref()
                    .and_then(|id| self.users.borrow().get(id).map(|u| u.email.clone()));
            }
            Ok(record)
        }
        async fn insert_session(&self, record: &SessionRecord) -> Result<(), StoreError> {
            if *self.fail_session_insert.borrow() {
                return Err(StoreError);
            }
            self.sessions
                .borrow_mut()
                .insert(record.id.clone(), record.clone());
            Ok(())
        }
        async fn redeem_magic_link(
            &self,
            record: &MagicLinkRecord,
            now: u64,
            session: &SessionRecord,
        ) -> Result<bool, StoreError> {
            *self.magic_redemptions.borrow_mut() += 1;
            if *self.lose_magic_claim.borrow() {
                return Ok(false);
            }
            let mut magic = self.magic.borrow_mut();
            let Some(row) = magic.get_mut(&record.token_hash) else {
                return Ok(false);
            };
            if row.user_id != record.user_id
                || row.email != record.email
                || row.expires_at != record.expires_at
                || row.expires_at <= now
                || row.used
            {
                return Ok(false);
            }
            if session.user_id != record.user_id || *self.fail_session_insert.borrow() {
                return Err(StoreError);
            }
            let mut sessions = self.sessions.borrow_mut();
            if sessions.contains_key(&session.id) {
                return Err(StoreError);
            }
            sessions.insert(session.id.clone(), session.clone());
            row.used = true;
            Ok(true)
        }
        async fn redeem_magic_link_and_audit(
            &self,
            magic: &MagicLinkRecord,
            now: u64,
            session: &SessionRecord,
            audit: &AuditEvent,
        ) -> Result<bool, StoreError> {
            // The database fixtures separately prove rollback after both
            // writes. Here the store's error pins the handler's no-cookie
            // response to that failed transaction, without faking SQL.
            if *self.fail_magic_audit.borrow() {
                return Err(StoreError);
            }
            let redeemed = self.redeem_magic_link(magic, now, session).await?;
            if redeemed {
                let _ = self.insert_audit(audit).await;
            }
            Ok(redeemed)
        }
        async fn session_by_id(&self, id: &str) -> Result<Option<SessionRecord>, StoreError> {
            if *self.fail_session_lookup.borrow() {
                return Err(StoreError);
            }
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
                    self.refresh_retries.borrow_mut().remove(id);
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn rotate_session_with_retry(
            &self,
            previous: &SessionClaims,
            next: &SessionClaims,
            retry_hash: &str,
            now: u64,
        ) -> Result<Option<SessionClaims>, StoreError> {
            let mut sessions = self.sessions.borrow_mut();
            let Some(row) = sessions.get_mut(&previous.session_id) else {
                return Ok(None);
            };
            if row.revoked
                || row.user_id != previous.user_id
                || row.created_at != previous.created_at
                || now >= row.expires_at
            {
                return Ok(None);
            }
            let mut retries = self.refresh_retries.borrow_mut();
            let issued_at = if row.rotation_counter == previous.rotation_counter
                && row.expires_at == previous.expires_at
            {
                row.rotation_counter = next.rotation_counter;
                row.expires_at = next.expires_at;
                retries.insert(row.id.clone(), (retry_hash.to_string(), now));
                now
            } else {
                match retries.get(&row.id) {
                    Some((hash, at))
                        if row.rotation_counter == next.rotation_counter
                            && hash == retry_hash
                            && *at <= now
                            && now.saturating_sub(*at) < session::REFRESH_RETRY_SECS =>
                    {
                        *at
                    }
                    _ => return Ok(None),
                }
            };
            // Simulate a committed UPDATE whose acknowledgement is lost.
            if self.lose_next_refresh_ack.replace(false) {
                return Err(StoreError);
            }
            Ok(Some(SessionClaims {
                session_id: row.id.clone(),
                user_id: row.user_id.clone(),
                created_at: row.created_at,
                issued_at,
                expires_at: row.expires_at,
                rotation_counter: row.rotation_counter,
            }))
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
        async fn revoke_single_session(&self, id: &str, user_id: &str) -> Result<bool, StoreError> {
            let mut sessions = self.sessions.borrow_mut();
            match sessions.get_mut(id) {
                Some(row) if row.user_id == user_id && !row.revoked => {
                    row.revoked = true;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn insert_subsonic_credential(
            &self,
            row: &crate::subsonic_lifecycle::PreparedCredential,
            session: &SessionClaims,
        ) -> Result<bool, StoreError> {
            if !self
                .sessions
                .borrow()
                .get(&session.session_id)
                .is_some_and(|r| {
                    !r.revoked
                        && r.user_id == row.record.user_id
                        && r.rotation_counter == session.rotation_counter
                })
            {
                return Ok(false);
            }
            if *self.fail_subsonic_write.borrow() {
                return Err(StoreError);
            }
            let owner = &row.record.user_id;
            if !self.users.borrow().contains_key(owner)
                || self.deleting_users.borrow().contains(owner)
            {
                return Ok(false);
            }
            let mut rows = self.subsonic_credentials.borrow_mut();
            if rows
                .values()
                .filter(|(u, r)| u == owner && r.revoked_at.is_none())
                .count()
                >= 32
            {
                return Ok(false);
            }
            rows.insert(
                row.record.id.clone(),
                (
                    owner.clone(),
                    crate::subsonic_lifecycle::ListedCredential {
                        id: row.record.id.clone(),
                        label: row.label.clone(),
                        auth_mode: match row.record.auth_mode {
                            crate::subsonic_auth::AuthMode::ApiKey => "apikey",
                            crate::subsonic_auth::AuthMode::Token => "token",
                            crate::subsonic_auth::AuthMode::Both => "both",
                        }
                        .to_string(),
                        created_at: row.created_at,
                        last_used_at: None,
                        revoked_at: None,
                    },
                ),
            );
            Ok(true)
        }
        async fn subsonic_credentials_for_user(
            &self,
            user_id: &str,
        ) -> Result<Vec<crate::subsonic_lifecycle::ListedCredential>, StoreError> {
            Ok(self
                .subsonic_credentials
                .borrow()
                .values()
                .filter(|(u, _)| u == user_id)
                .map(|(_, r)| r.clone())
                .collect())
        }
        async fn revoke_subsonic_credential(
            &self,
            id: &str,
            user_id: &str,
            now: u64,
        ) -> Result<bool, StoreError> {
            if *self.fail_subsonic_write.borrow() {
                return Err(StoreError);
            }
            let mut rows = self.subsonic_credentials.borrow_mut();
            let Some((u, row)) = rows.get_mut(id) else {
                return Ok(false);
            };
            if u != user_id {
                return Ok(false);
            }
            row.revoked_at.get_or_insert(now);
            Ok(true)
        }
        async fn revoke_all_subsonic_credentials(
            &self,
            user_id: &str,
            now: u64,
        ) -> Result<u64, StoreError> {
            if *self.fail_subsonic_write.borrow() {
                return Err(StoreError);
            }
            let mut count = 0;
            for (u, row) in self.subsonic_credentials.borrow_mut().values_mut() {
                if u == user_id && row.revoked_at.is_none() {
                    row.revoked_at = Some(now);
                    count += 1;
                }
            }
            Ok(count)
        }
        async fn insert_native_credential(
            &self,
            row: &NativeCredentialRow,
            session_id: &str,
        ) -> Result<(), StoreError> {
            // Pin a session transition after handler authentication.
            if *self.revoke_session_before_mint.borrow() {
                self.sessions
                    .borrow_mut()
                    .get_mut(session_id)
                    .expect("mint must bind the authenticated session")
                    .revoked = true;
            }
            // Mirror the production account and session fences.
            if self.deleting_users.borrow().contains(&row.record.user_id)
                || !self
                    .sessions
                    .borrow()
                    .get(session_id)
                    .is_some_and(|session| {
                        session.user_id == row.record.user_id && !session.revoked
                    })
            {
                return Err(StoreError);
            }
            let mut rows = self.native_credentials.borrow_mut();
            // Mirror the migration 0019 composite UNIQUE: neither the id
            // nor the (pepper_version, key_hmac) identity may repeat.
            if rows.contains_key(&row.record.id)
                || rows.values().any(|existing| {
                    existing.record.pepper_version == row.record.pepper_version
                        && existing.record.key_hmac == row.record.key_hmac
                })
            {
                return Err(StoreError);
            }
            rows.insert(row.record.id.clone(), row.clone());
            Ok(())
        }
        async fn native_credentials_for_user(
            &self,
            user_id: &str,
        ) -> Result<Vec<NativeCredentialRow>, StoreError> {
            let mut rows: Vec<NativeCredentialRow> = self
                .native_credentials
                .borrow()
                .values()
                .filter(|row| row.record.user_id == user_id)
                .cloned()
                .collect();
            rows.sort_by(|a, b| {
                (a.record.created_at, &a.record.id).cmp(&(b.record.created_at, &b.record.id))
            });
            Ok(rows)
        }
        async fn native_credential_by_hmac(
            &self,
            pepper_version: u32,
            key_hmac: &str,
        ) -> Result<Option<NativeCredentialRow>, StoreError> {
            *self.native_credential_lookups.borrow_mut() += 1;
            if *self.fail_native_credential_lookup.borrow() {
                return Err(StoreError);
            }
            Ok(self
                .native_credentials
                .borrow()
                .values()
                .find(|row| {
                    row.record.pepper_version == pepper_version && row.record.key_hmac == key_hmac
                })
                .cloned())
        }
        async fn revoke_native_credential(
            &self,
            id: &str,
            user_id: &str,
            now: u64,
        ) -> Result<bool, StoreError> {
            let mut rows = self.native_credentials.borrow_mut();
            match rows.get_mut(id) {
                // Owner-scoped and sticky: a foreign id reports absence,
                // an already-revoked row keeps its original timestamp.
                Some(row) if row.record.user_id == user_id => {
                    if row.record.revoked_at.is_none() {
                        row.record.revoked_at = Some(now);
                    }
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn revoke_sessions_and_native_credentials(
            &self,
            user_id: &str,
            now: u64,
        ) -> Result<u64, StoreError> {
            if *self.fail_bulk_revoke.borrow() {
                return Err(StoreError);
            }
            if user_id.is_empty() {
                return Ok(0);
            }
            self.revoke_all_subsonic_credentials(user_id, now).await?;
            for row in self.sessions.borrow_mut().values_mut() {
                if row.user_id == user_id {
                    row.revoked = true;
                }
            }
            let mut revoked = 0;
            for row in self.native_credentials.borrow_mut().values_mut() {
                if row.record.user_id == user_id && row.record.revoked_at.is_none() {
                    row.record.revoked_at = Some(now);
                    revoked += 1;
                }
            }
            Ok(revoked)
        }
        async fn record_native_credential_use(&self, id: &str, now: u64) -> Result<(), StoreError> {
            if let Some(row) = self.native_credentials.borrow_mut().get_mut(id) {
                row.last_used_at = Some(now);
            }
            Ok(())
        }
        async fn catalogue_library(
            &self,
            user_id: &str,
        ) -> Result<Vec<crate::catalogue::LibraryDbRow>, StoreError> {
            if *self.fail_catalogue_reads.borrow() {
                return Err(StoreError);
            }
            Ok(self
                .catalogue_library
                .borrow()
                .get(user_id)
                .cloned()
                .unwrap_or_default())
        }
        async fn catalogue_artists(
            &self,
            user_id: &str,
        ) -> Result<Vec<crate::catalogue::ArtistDbRow>, StoreError> {
            if *self.fail_catalogue_reads.borrow() {
                return Err(StoreError);
            }
            let member = self.fake_member_releases(user_id);
            let artist_mbids: std::collections::HashSet<String> = self
                .catalogue_releases
                .borrow()
                .iter()
                .filter(|row| member.iter().any(|mbid| mbid == &row.mbid))
                .filter_map(|row| row.artist_mbid.clone())
                .collect();
            Ok(self
                .catalogue_artists
                .borrow()
                .iter()
                .filter(|row| artist_mbids.contains(&row.mbid))
                .cloned()
                .collect())
        }
        async fn catalogue_releases(
            &self,
            user_id: &str,
        ) -> Result<Vec<crate::catalogue::ReleaseDbRow>, StoreError> {
            if *self.fail_catalogue_reads.borrow() {
                return Err(StoreError);
            }
            let member = self.fake_member_releases(user_id);
            Ok(self
                .catalogue_releases
                .borrow()
                .iter()
                .filter(|row| member.iter().any(|mbid| mbid == &row.mbid))
                .cloned()
                .collect())
        }
        async fn catalogue_tracks(
            &self,
            user_id: &str,
        ) -> Result<Vec<crate::catalogue::TrackDbRow>, StoreError> {
            if *self.fail_catalogue_reads.borrow() {
                return Err(StoreError);
            }
            let pairs = self.fake_member_pairs(user_id);
            let owned: std::collections::HashSet<&(String, String)> = pairs.iter().collect();
            Ok(self
                .catalogue_tracks
                .borrow()
                .iter()
                .filter(|row| {
                    crate::catalogue::enrichment_key(&row.release_mbid)
                        .and_then(|release| {
                            crate::catalogue::enrichment_key(&row.recording_mbid)
                                .map(|recording| (release, recording))
                        })
                        .is_some_and(|pair| owned.contains(&pair))
                })
                .cloned()
                .collect())
        }
        async fn catalogue_release(
            &self,
            user_id: &str,
            mbid: &str,
        ) -> Result<Option<crate::catalogue::ReleaseDbRow>, StoreError> {
            if *self.fail_catalogue_reads.borrow() {
                return Err(StoreError);
            }
            let Some(key) = crate::catalogue::enrichment_key(mbid) else {
                return Ok(None);
            };
            if !self.fake_member_releases(user_id).iter().any(|m| m == &key) {
                return Ok(None);
            }
            Ok(self
                .catalogue_releases
                .borrow()
                .iter()
                .find(|row| row.mbid == mbid)
                .cloned())
        }
        async fn catalogue_release_tracks(
            &self,
            user_id: &str,
            mbid: &str,
        ) -> Result<Vec<crate::catalogue::ReleaseTrackDbRow>, StoreError> {
            if *self.fail_catalogue_reads.borrow() || *self.fail_catalogue_release_tracks.borrow() {
                return Err(StoreError);
            }
            let Some(key) = crate::catalogue::enrichment_key(mbid) else {
                return Ok(Vec::new());
            };
            if !self.fake_member_releases(user_id).iter().any(|m| m == &key) {
                return Ok(Vec::new());
            }
            Ok(self
                .catalogue_release_tracks
                .borrow()
                .get(mbid)
                .cloned()
                .unwrap_or_default())
        }
        async fn account_deleting(&self, user_id: &str) -> Result<bool, StoreError> {
            Ok(self.deleting_users.borrow().contains(user_id))
        }
        async fn account_admitted(&self, user_id: &str) -> Result<bool, StoreError> {
            // Mirror the production EXISTS probe: credential, any
            // session row, vault, or a non-member role (an absent
            // `user_roles` entry is the `member` column default).
            let admitted = self
                .credentials
                .borrow()
                .values()
                .any(|c| c.user_id == user_id)
                || self
                    .sessions
                    .borrow()
                    .values()
                    .any(|s| s.user_id == user_id)
                || self.legacy_vault_users.borrow().contains(user_id)
                || self
                    .user_roles
                    .borrow()
                    .get(user_id)
                    .is_some_and(|role| role.as_str() != crate::admin::ROLE_MEMBER);
            Ok(admitted)
        }
        async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
            self.audits.borrow_mut().push(event.clone());
            Ok(())
        }
        async fn user_role(&self, user_id: &str) -> Result<Option<String>, StoreError> {
            if *self.fail_user_role_lookup.borrow() {
                return Err(StoreError);
            }
            if !self.users.borrow().contains_key(user_id) {
                return Ok(None);
            }
            Ok(Some(
                self.user_roles
                    .borrow()
                    .get(user_id)
                    .cloned()
                    .unwrap_or_else(|| crate::admin::ROLE_MEMBER.to_string()),
            ))
        }
        async fn admin_set_user_role(
            &self,
            actor_user_id: &str,
            subject_user_id: &str,
            new_role: &str,
            source_ip: &str,
            now: u64,
        ) -> Result<AdminWriteOutcome, StoreError> {
            // Mirror the production transaction: re-read the actor's
            // role at write time and refuse a demoted actor's write.
            match self.user_role(actor_user_id).await? {
                role if crate::admin::is_admin_role(role.as_deref()) => {}
                _ => return Ok(AdminWriteOutcome::ActorNotAdmin),
            }
            if !self.users.borrow().contains_key(subject_user_id) {
                return Ok(AdminWriteOutcome::SubjectMissing);
            }
            // Mirror the production fence: a subject scheduled for
            // deletion takes no role write (and no audit row).
            if self.deleting_users.borrow().contains(subject_user_id) {
                return Ok(AdminWriteOutcome::AccountDeleting);
            }
            let previous = self
                .user_roles
                .borrow()
                .get(subject_user_id)
                .cloned()
                .unwrap_or_else(|| crate::admin::ROLE_MEMBER.to_string());
            self.user_roles
                .borrow_mut()
                .insert(subject_user_id.to_string(), new_role.to_string());
            self.audits.borrow_mut().push(vault::audit_event_extended(
                actor_user_id,
                crate::admin::ROLE_CHANGED_AUDIT_ACTION,
                true,
                source_ip,
                now,
                Some(subject_user_id),
                serde_json::json!({"old": previous, "new": new_role}),
            ));
            Ok(AdminWriteOutcome::Applied(Some(previous)))
        }
        async fn app_settings(&self) -> Result<Vec<(String, String)>, StoreError> {
            Ok(self
                .app_settings_rows
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
        async fn admin_set_app_setting(
            &self,
            actor_user_id: &str,
            key: &str,
            new_value: &str,
            source_ip: &str,
            now: u64,
        ) -> Result<AdminWriteOutcome, StoreError> {
            match self.user_role(actor_user_id).await? {
                role if crate::admin::is_admin_role(role.as_deref()) => {}
                _ => return Ok(AdminWriteOutcome::ActorNotAdmin),
            }
            let previous = self
                .app_settings_rows
                .borrow_mut()
                .insert(key.to_string(), new_value.to_string());
            self.audits.borrow_mut().push(vault::audit_event_extended(
                actor_user_id,
                crate::admin::SETTINGS_CHANGED_AUDIT_ACTION,
                true,
                source_ip,
                now,
                None,
                serde_json::json!({"key": key, "old": previous, "new": new_value}),
            ));
            Ok(AdminWriteOutcome::Applied(previous))
        }
        async fn claim_webhook_event(&self, event_id: &str) -> Result<bool, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            Ok(self
                .webhook_events
                .borrow_mut()
                .insert(event_id.to_string()))
        }
        async fn release_webhook_event(&self, event_id: &str) -> Result<(), StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            self.webhook_events.borrow_mut().remove(event_id);
            Ok(())
        }
        async fn load_reconcile_input(
            &self,
            user_id: &str,
            target_subscription_id: Option<&str>,
        ) -> Result<crate::billing_reconcile::ReconcileInput, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            let current = self.billing_rows.borrow().get(user_id).cloned();
            let selected = match target_subscription_id {
                Some(target) => current
                    .filter(|row| row.stripe_subscription_id == target)
                    .or_else(|| {
                        self.billing_shadow
                            .borrow()
                            .get(&(user_id.to_string(), target.to_string()))
                            .cloned()
                    }),
                None => current.filter(|row| {
                    matches!(
                        row.status.as_str(),
                        "incomplete" | "active" | "past_due" | "unpaid" | "paused"
                    )
                }),
            };
            let live = selected.map(|row| crate::billing_reconcile::LiveRow {
                stripe_subscription_id: row.stripe_subscription_id.clone(),
                stripe_customer_id: row.stripe_customer_id.clone(),
                stripe_item_id: row.stripe_item_id.clone(),
                stripe_price_id: row.stripe_price_id.clone(),
                plan_id: row.plan_id.clone(),
                status: row.status.clone(),
                paid_through_unix: row.paid_through_unix,
                paid_invoice_id: row.paid_invoice_id.clone(),
                grace_until_unix: row.grace_until_unix,
            });
            Ok(crate::billing_reconcile::ReconcileInput {
                stripe_customer_id: self.billing_customers.borrow().get(user_id).cloned(),
                live,
                bindings: self.billing_bindings.borrow().clone(),
            })
        }
        async fn user_for_stripe_customer(
            &self,
            customer_id: &str,
        ) -> Result<Option<String>, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            Ok(self
                .billing_customers
                .borrow()
                .iter()
                .find(|(_, customer)| customer.as_str() == customer_id)
                .map(|(user, _)| user.clone()))
        }
        async fn store_reconcile_outcome(
            &self,
            user_id: &str,
            outcome: &crate::billing_reconcile::ReconcileOutcome,
        ) -> Result<(), StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            if !outcome.explicit_stale {
                let mut rows = self.billing_rows.borrow_mut();
                let mut shadow = self.billing_shadow.borrow_mut();
                let key = (
                    user_id.to_string(),
                    outcome.row.stripe_subscription_id.clone(),
                );
                let current = rows.get(user_id);
                let stored = current
                    .filter(|row| row.stripe_subscription_id == outcome.row.stripe_subscription_id)
                    .or_else(|| shadow.get(&key));
                let merged = merge_billing_row(stored, outcome);
                if let Some(existing) = current {
                    if existing.stripe_subscription_id != merged.stripe_subscription_id {
                        shadow.insert(
                            (user_id.to_string(), existing.stripe_subscription_id.clone()),
                            existing.clone(),
                        );
                    }
                }
                if let Some(retained) = shadow.get_mut(&key) {
                    *retained = merged.clone();
                }
                rows.insert(user_id.to_string(), merged);
            }
            let mut history = self.invoice_history.borrow_mut();
            for row in &outcome.history {
                history
                    .entry(row.stripe_invoice_id.clone())
                    .or_insert_with(|| row.clone());
            }
            Ok(())
        }
        async fn open_checkout_operation(
            &self,
            user_id: &str,
        ) -> Result<Option<crate::billing_reconcile::StoredCheckoutOp>, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            Ok(self
                .checkout_ops
                .borrow()
                .values()
                .find(|op| {
                    op.user_id == user_id
                        && crate::billing_reconcile::is_single_flight_state(&op.state)
                })
                .cloned())
        }
        async fn create_checkout_operation(
            &self,
            op_id: &str,
            user_id: &str,
            price_id: &str,
            customer_id: Option<&str>,
        ) -> Result<crate::billing_reconcile::CreateOpOutcome, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            // Race pin: land the winner between the caller's open-flight
            // read and this insert (consumed once).
            if let Some(winner) = self.plant_winner_on_next_create.borrow_mut().take() {
                self.checkout_ops
                    .borrow_mut()
                    .insert(winner.id.clone(), winner);
            }
            let mut ops = self.checkout_ops.borrow_mut();
            // The partial unique admits at most one open flight per user:
            // a racer converges on the winner's row below instead of
            // minting a second subscription.
            if let Some(winner) = ops.values().find(|op| {
                op.user_id == user_id && crate::billing_reconcile::is_single_flight_state(&op.state)
            }) {
                return Ok(crate::billing_reconcile::CreateOpOutcome::Existing(
                    winner.clone(),
                ));
            }
            // Bind the Stripe customer to the account, but only when
            // unbound — an existing binding is never overwritten by a
            // racing second customer create. Skipped while the test holds
            // the pre-bind window open (production binds in a separate
            // statement after the op INSERT).
            if let Some(customer) = customer_id {
                // Skipped while the test holds the pre-bind window open
                // (production binds in a separate statement after the op
                // INSERT).
                if !*self.defer_customer_bind.borrow() {
                    self.billing_customers
                        .borrow_mut()
                        .entry(user_id.to_string())
                        .or_insert_with(|| customer.to_string());
                }
            }
            let op = crate::billing_reconcile::StoredCheckoutOp {
                id: op_id.to_string(),
                user_id: user_id.to_string(),
                state: "pending".to_string(),
                price_id: Some(price_id.to_string()),
                session_id: None,
                subscription_id: None,
            };
            ops.insert(op_id.to_string(), op.clone());
            Ok(crate::billing_reconcile::CreateOpOutcome::Created(op))
        }
        async fn checkout_operation_by_id(
            &self,
            op_id: &str,
        ) -> Result<Option<crate::billing_reconcile::StoredCheckoutOp>, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            Ok(self.checkout_ops.borrow().get(op_id).cloned())
        }
        async fn update_checkout_operation(
            &self,
            op_id: &str,
            state: &str,
            price_id: &str,
            session_id: Option<&str>,
        ) -> Result<bool, StoreError> {
            if *self.fail_billing.borrow() {
                return Err(StoreError);
            }
            // Mirrors the production conditional mark: only an unresolved
            // row takes the mark. A missing or already-retired row (lost
            // mark, or superseded while its Stripe create was still in
            // flight) reports `false` instead of resurrecting the flight.
            let mut ops = self.checkout_ops.borrow_mut();
            let Some(op) = ops.get_mut(op_id) else {
                return Ok(false);
            };
            if !crate::billing_reconcile::is_single_flight_state(&op.state) {
                return Ok(false);
            }
            op.state = state.to_string();
            op.price_id = Some(price_id.to_string());
            op.session_id = session_id.map(str::to_string);
            Ok(true)
        }
        async fn admin_ingest_upload(
            &self,
            attempt: &AdminIngestAttempt,
        ) -> Result<AdminIngestUploadOutcome, StoreError> {
            // Mirror the production transaction: re-read the actor's
            // role at write time and refuse a demoted actor's write;
            // a deleting actor is fenced like every other writer of
            // user-owned state (ADR 0002 rule 3).
            match self.user_role(&attempt.actor_user_id).await? {
                role if crate::admin::is_admin_role(role.as_deref()) => {}
                _ => return Ok(AdminIngestUploadOutcome::ActorNotAdmin),
            }
            if self
                .deleting_users
                .borrow()
                .contains(&attempt.actor_user_id)
            {
                return Ok(AdminIngestUploadOutcome::AccountDeleting);
            }
            if let Some(job) = self.ingest_jobs.borrow().get(&attempt.job_id) {
                // Convergent replay: the deterministic id already
                // names a row (a second admin uploading the same bytes
                // lands here too — identity is content-derived, never
                // the uploader). The first envelope wins, like the
                // production `ON CONFLICT DO NOTHING`.
                let job = job.clone();
                let mut details = serde_json::json!({"job_id": job.job_id, "outcome": "duplicate"});
                if let Some(reason) = &job.failure_reason {
                    details["reason_code"] = reason.clone().into();
                }
                self.audits.borrow_mut().push(vault::audit_event_extended(
                    &attempt.actor_user_id,
                    crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION,
                    true,
                    &attempt.source_ip,
                    attempt.now,
                    None,
                    details,
                ));
                return Ok(AdminIngestUploadOutcome::Stored {
                    job,
                    deduplicated: true,
                });
            }
            let db_status = if attempt.failure.is_some() {
                "failed"
            } else {
                "queued"
            };
            let job = StoredAdminIngestJob {
                job_id: attempt.job_id.clone(),
                db_status: db_status.to_string(),
                accepted_at: attempt.now,
                nzb_bytes: attempt.nzb_bytes,
                file_count: attempt.file_count,
                segment_count: attempt.segment_count,
                failure_reason: attempt.failure.map(|reason| reason.as_str().to_string()),
                has_original: attempt.original.is_some(),
                uploader_user_id: attempt.actor_user_id.clone(),
            };
            // The (failure, original) coupling the handler builds:
            // failed attempts carry no envelope, accepted attempts
            // always do.
            debug_assert_eq!(job.has_original, attempt.failure.is_none());
            if let Some(envelope) = &attempt.original {
                self.ingest_originals.borrow_mut().insert(
                    attempt.job_id.clone(),
                    (
                        envelope.clone(),
                        attempt.content_sha256_hex.clone(),
                        attempt.kek_version,
                    ),
                );
            }
            self.ingest_jobs
                .borrow_mut()
                .insert(attempt.job_id.clone(), job.clone());
            let mut details = serde_json::json!({"job_id": job.job_id, "outcome": "accepted"});
            if let Some(reason) = &job.failure_reason {
                details["outcome"] = "rejected".into();
                details["reason_code"] = reason.clone().into();
            }
            self.audits.borrow_mut().push(vault::audit_event_extended(
                &attempt.actor_user_id,
                crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION,
                true,
                &attempt.source_ip,
                attempt.now,
                None,
                details,
            ));
            Ok(AdminIngestUploadOutcome::Stored {
                job,
                deduplicated: false,
            })
        }
        async fn admin_ingest_get(
            &self,
            job_id: &str,
        ) -> Result<Option<StoredAdminIngestJob>, StoreError> {
            Ok(self.ingest_jobs.borrow().get(job_id).cloned())
        }
        async fn admin_ingest_list(
            &self,
            status: Option<&str>,
            limit: u64,
            offset: u64,
        ) -> Result<Vec<StoredAdminIngestJob>, StoreError> {
            let mut jobs: Vec<StoredAdminIngestJob> = self
                .ingest_jobs
                .borrow()
                .values()
                .filter(|job| status.is_none_or(|want| job.db_status == want))
                .cloned()
                .collect();
            jobs.sort_by(|a, b| {
                b.accepted_at
                    .cmp(&a.accepted_at)
                    .then(a.job_id.cmp(&b.job_id))
            });
            Ok(jobs
                .into_iter()
                .skip(offset as usize)
                .take(limit.max(1) as usize)
                .collect())
        }
        async fn admin_ingest_delete(
            &self,
            actor_user_id: &str,
            job_id: &str,
        ) -> Result<AdminIngestDeleteOutcome, StoreError> {
            match self.user_role(actor_user_id).await? {
                role if crate::admin::is_admin_role(role.as_deref()) => {}
                _ => return Ok(AdminIngestDeleteOutcome::ActorNotAdmin),
            }
            if !self.ingest_jobs.borrow().contains_key(job_id) {
                return Ok(AdminIngestDeleteOutcome::Missing);
            }
            // The original goes first, then the job — the production
            // `RESTRICT` order. No audit row: deletion writes no new
            // observable state.
            self.ingest_originals.borrow_mut().remove(job_id);
            self.ingest_jobs.borrow_mut().remove(job_id);
            Ok(AdminIngestDeleteOutcome::Deleted)
        }
        async fn admin_issue_invite(
            &self,
            actor_user_id: &str,
            record: &InviteRecord,
            source_ip: &str,
            now: u64,
        ) -> Result<AdminInviteIssueOutcome, StoreError> {
            // Mirror the production transaction: re-read the actor's
            // role at write time and refuse a demoted actor's write.
            match self.user_role(actor_user_id).await? {
                role if crate::admin::is_admin_role(role.as_deref()) => {}
                _ => return Ok(AdminInviteIssueOutcome::ActorNotAdmin),
            }
            let id = {
                let mut next = self.next_invite_id.borrow_mut();
                *next += 1;
                *next
            };
            let mut stored = record.clone();
            stored.id = id;
            if self
                .invites
                .borrow_mut()
                .insert(stored.code_hash.clone(), stored)
                .is_some()
            {
                return Err(StoreError);
            }
            self.audits.borrow_mut().push(vault::audit_event_extended(
                actor_user_id,
                auth::INVITE_ISSUED_AUDIT_ACTION,
                true,
                source_ip,
                now,
                None,
                serde_json::json!({
                    "invite_id": id,
                    "kind": record.kind,
                    "email_bound": record.email.is_some(),
                    "expires_at": record.expires_at,
                }),
            ));
            Ok(AdminInviteIssueOutcome::Issued(id))
        }
        async fn admin_list_invites(
            &self,
            issuer: Option<&str>,
            status: Option<&str>,
            now: u64,
            limit: usize,
        ) -> Result<Vec<InviteRecord>, StoreError> {
            let mut rows: Vec<InviteRecord> = self
                .invites
                .borrow()
                .values()
                .filter(|row| issuer.is_none_or(|id| row.issued_by.as_deref() == Some(id)))
                .filter(|row| status.is_none_or(|wanted| row.status(now) == wanted))
                .cloned()
                .collect();
            rows.sort_by(|a, b| b.id.cmp(&a.id).then(b.created_at.cmp(&a.created_at)));
            rows.truncate(limit);
            Ok(rows)
        }
        async fn admin_revoke_invite(
            &self,
            actor_user_id: &str,
            invite_id: i64,
            source_ip: &str,
            now: u64,
        ) -> Result<AdminInviteRevokeOutcome, StoreError> {
            match self.user_role(actor_user_id).await? {
                role if crate::admin::is_admin_role(role.as_deref()) => {}
                _ => return Ok(AdminInviteRevokeOutcome::ActorNotAdmin),
            }
            let mut invites = self.invites.borrow_mut();
            let Some(row) = invites.values_mut().find(|row| row.id == invite_id) else {
                return Ok(AdminInviteRevokeOutcome::Missing);
            };
            if row.redeemed_at.is_some() || row.used {
                return Ok(AdminInviteRevokeOutcome::AlreadyRedeemed);
            }
            if row.revoked_at.is_some() {
                return Ok(AdminInviteRevokeOutcome::AlreadyRevoked);
            }
            row.revoked_at = Some(now);
            row.revoked_by = Some(actor_user_id.to_string());
            drop(invites);
            self.audits.borrow_mut().push(vault::audit_event_extended(
                actor_user_id,
                auth::INVITE_REVOKED_AUDIT_ACTION,
                true,
                source_ip,
                now,
                None,
                serde_json::json!({"invite_id": invite_id}),
            ));
            Ok(AdminInviteRevokeOutcome::Revoked)
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
                    // Mirror the production conditional UPDATE: every
                    // spend predicate is re-proven against the stored
                    // row (and the redeemer's user row) at claim time.
                    let redeemed_at = record.redeemed_at.unwrap_or(0);
                    let redeemer_id = record.redeemed_by.clone().unwrap_or_default();
                    let users = self.users.borrow();
                    let redeemer_email = users.get(&redeemer_id).map(|u| u.email.clone());
                    let mut invites = self.invites.borrow_mut();
                    match invites.get_mut(&record.code_hash) {
                        Some(row)
                            if !row.used
                                && row.redeemed_at.is_none()
                                && row.revoked_at.is_none()
                                && row.expires_at > redeemed_at
                                && row.email.as_ref().is_none_or(|bound| {
                                    redeemer_email
                                        .as_ref()
                                        .is_some_and(|email| email.eq_ignore_ascii_case(bound))
                                })
                                && !{
                                    let issuer_email = row
                                        .issued_by
                                        .as_deref()
                                        .and_then(|id| users.get(id))
                                        .map(|u| u.email.clone());
                                    matches!(
                                        (&issuer_email, &redeemer_email),
                                        (Some(issuer), Some(redeemer))
                                            if issuer.eq_ignore_ascii_case(redeemer)
                                    )
                                } =>
                        {
                            row.used = true;
                            row.redeemed_by = record.redeemed_by.clone();
                            row.redeemed_at = record.redeemed_at;
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
        async fn probe(&self, target: &ValidatedTarget, _tls: bool) -> bool {
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

    /// Scripted Stripe fixture: sessions, subscriptions, and full invoices
    /// by id, a customer-create log for the test-clock seam assert, and a
    /// per-method failure pin for Stripe-outage paths. No network, no
    /// secrets: every id is synthetic (`cs_test_*`, `sub_test_*`, ...).
    #[derive(Default)]
    struct FakeStripe {
        sessions: RefCell<HashMap<String, crate::stripe::CheckoutSession>>,
        subscriptions: RefCell<HashMap<String, crate::stripe::Subscription>>,
        invoices: RefCell<HashMap<String, crate::stripe::Invoice>>,
        /// Remainder line pages per invoice: `list_invoice_lines` answers
        /// the next page from here, so tests pin the full-read follow-up
        /// instead of trusting the first page.
        line_pages: RefCell<HashMap<String, Vec<crate::stripe::InvoiceLine>>>,
        /// Customer-create log `(user_id, test_clock)` for the seam assert.
        customers_created: RefCell<Vec<(Option<String>, Option<String>)>>,
        /// Checkout sessions minted through `create_checkout_session`.
        sessions_created: RefCell<usize>,
        /// Customers the minted sessions were bound to (proves a
        /// converged attempt sessions on the winner-bound customer,
        /// never a racing loser's orphan).
        session_customers: RefCell<Vec<String>>,
        /// Subscription ids passed to `cancel_subscription` (proves a
        /// recovered or unknown remote is never canceled).
        canceled: RefCell<Vec<String>>,
        /// Session ids passed to `expire_checkout_session` (proves a
        /// price-mismatched or dead session is deliberately retired
        /// before its replacement mints).
        expired: RefCell<Vec<String>>,
        /// Operation ids that minted through `create_checkout_session`
        /// (proves a replacement mints under a fresh operation — hence a
        /// fresh Stripe idempotency key — never the dead session's).
        /// Same-key replays are not mints and are not logged here.
        session_create_ops: RefCell<Vec<String>>,
        /// Session id minted per operation id: the production Stripe
        /// replays a create under a seen idempotency key instead of
        /// minting again, and the orphan-recovery path depends on it
        /// (recover under the old key, then retire per the retrieved
        /// state). Replays answer the stored session without bumping
        /// `sessions_created`.
        sessions_by_op: RefCell<HashMap<String, String>>,
        /// Session ids that retrieve as Stripe 404s (proves a missing
        /// session repairs under a fresh key instead of replaying).
        missing_sessions: RefCell<HashSet<String>>,
        /// Method names that fail like a Stripe outage.
        fail: RefCell<HashSet<String>>,
        /// Next customer id minted by `create_customer`.
        next_customer: RefCell<Option<String>>,
    }

    impl FakeStripe {
        fn fail(&self, method: &str) -> bool {
            self.fail.borrow().contains(method)
        }

        fn err<T>(&self) -> crate::stripe::StripeResult<T> {
            Err(crate::stripe::StripeError::transport())
        }
    }

    impl crate::stripe::StripeTransport for FakeStripe {
        fn create_customer(
            &self,
            req: crate::stripe::CreateCustomer,
        ) -> Pin<Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Customer>> + '_>>
        {
            Box::pin(async move {
                if self.fail("create_customer") {
                    return self.err();
                }
                self.customers_created
                    .borrow_mut()
                    .push((req.user_id.clone(), req.test_clock.clone()));
                let id = self
                    .next_customer
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "cus_test_1".to_string());
                Ok(crate::stripe::Customer {
                    id,
                    livemode: false,
                    test_clock: req.test_clock,
                    email: None,
                })
            })
        }
        fn create_checkout_session(
            &self,
            req: crate::stripe::CreateCheckoutSession,
        ) -> Pin<
            Box<
                dyn Future<Output = crate::stripe::StripeResult<crate::stripe::CheckoutSession>>
                    + '_,
            >,
        > {
            Box::pin(async move {
                if self.fail("create_checkout_session") {
                    return self.err();
                }
                // Same-key replay, like the real idempotency key: answer
                // the stored session without minting again.
                if let Some(id) = self.sessions_by_op.borrow().get(&req.operation_id).cloned() {
                    if let Some(session) = self.sessions.borrow().get(&id).cloned() {
                        return Ok(session);
                    }
                }
                let mut created = self.sessions_created.borrow_mut();
                *created += 1;
                self.session_create_ops
                    .borrow_mut()
                    .push(req.operation_id.clone());
                self.session_customers
                    .borrow_mut()
                    .push(req.customer_id.clone());
                let id = format!("cs_test_{created}");
                let url = format!("https://checkout.stripe.com/pay/{id}");
                let session = crate::stripe::CheckoutSession {
                    id: id.clone(),
                    status: Some("open".to_string()),
                    payment_status: Some("unpaid".to_string()),
                    customer: Some(req.customer_id),
                    subscription: None,
                    url: Some(url),
                    client_reference_id: Some(req.operation_id.clone()),
                    livemode: false,
                };
                self.sessions_by_op
                    .borrow_mut()
                    .insert(req.operation_id, id.clone());
                self.sessions.borrow_mut().insert(id, session.clone());
                Ok(session)
            })
        }
        fn retrieve_checkout_session(
            &self,
            session_id: String,
        ) -> Pin<
            Box<
                dyn Future<Output = crate::stripe::StripeResult<crate::stripe::CheckoutSession>>
                    + '_,
            >,
        > {
            Box::pin(async move {
                if self.fail("retrieve_checkout_session") {
                    return self.err();
                }
                if self.missing_sessions.borrow().contains(&session_id) {
                    return Err(crate::stripe::StripeError::api(
                        404,
                        Some("resource_missing".to_string()),
                        "No such checkout session".to_string(),
                    ));
                }
                self.sessions
                    .borrow()
                    .get(&session_id)
                    .cloned()
                    .ok_or_else(crate::stripe::StripeError::transport)
            })
        }
        fn expire_checkout_session(
            &self,
            session_id: String,
        ) -> Pin<
            Box<
                dyn Future<Output = crate::stripe::StripeResult<crate::stripe::CheckoutSession>>
                    + '_,
            >,
        > {
            Box::pin(async move {
                if self.fail("expire_checkout_session") {
                    return self.err();
                }
                self.expired.borrow_mut().push(session_id.clone());
                // Like the real `POST /v1/checkout/sessions/{id}/expire`:
                // the stored session transitions and the expired object
                // comes back, so a follow-up retrieve observes it; an
                // unknown id 404s.
                let mut sessions = self.sessions.borrow_mut();
                match sessions.get_mut(&session_id) {
                    Some(session) => {
                        session.status = Some("expired".to_string());
                        session.url = None;
                        Ok(session.clone())
                    }
                    None => Err(crate::stripe::StripeError::api(
                        404,
                        Some("resource_missing".to_string()),
                        "No such checkout session".to_string(),
                    )),
                }
            })
        }
        fn retrieve_subscription(
            &self,
            subscription_id: String,
        ) -> Pin<
            Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Subscription>> + '_>,
        > {
            Box::pin(async move {
                if self.fail("retrieve_subscription") {
                    return self.err();
                }
                self.subscriptions
                    .borrow()
                    .get(&subscription_id)
                    .cloned()
                    .ok_or_else(crate::stripe::StripeError::transport)
            })
        }
        fn retrieve_invoice(
            &self,
            invoice_id: String,
        ) -> Pin<Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Invoice>> + '_>>
        {
            Box::pin(async move {
                if self.fail("retrieve_invoice") {
                    return self.err();
                }
                self.invoices
                    .borrow()
                    .get(&invoice_id)
                    .cloned()
                    .ok_or_else(crate::stripe::StripeError::transport)
            })
        }
        fn list_invoice_lines(
            &self,
            invoice_id: String,
            _starting_after: Option<String>,
            _limit: u8,
        ) -> Pin<
            Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::InvoiceLines>> + '_>,
        > {
            Box::pin(async move {
                if self.fail("list_invoice_lines") {
                    return self.err();
                }
                let data = self
                    .line_pages
                    .borrow_mut()
                    .remove(&invoice_id)
                    .unwrap_or_default();
                Ok(crate::stripe::InvoiceLines {
                    data,
                    has_more: false,
                })
            })
        }
        fn list_invoices(
            &self,
            req: crate::stripe::ListInvoices,
        ) -> Pin<
            Box<dyn Future<Output = crate::stripe::StripeResult<Vec<crate::stripe::Invoice>>> + '_>,
        > {
            Box::pin(async move {
                if self.fail("list_invoices") {
                    return self.err();
                }
                Ok(self
                    .invoices
                    .borrow()
                    .values()
                    .filter(|invoice| {
                        if let Some(customer) = req.customer_id.as_deref() {
                            if invoice.customer.as_deref() != Some(customer) {
                                return false;
                            }
                        }
                        if let Some(subscription) = req.subscription_id.as_deref() {
                            if invoice.subscription.as_deref() != Some(subscription) {
                                return false;
                            }
                        }
                        !req.paid_only || invoice.status == "paid"
                    })
                    .cloned()
                    .collect())
            })
        }
        fn cancel_subscription(
            &self,
            subscription_id: String,
        ) -> Pin<
            Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Subscription>> + '_>,
        > {
            Box::pin(async move {
                if self.fail("cancel_subscription") {
                    return self.err();
                }
                // Like the real `DELETE /v1/subscriptions/{id}`: the call
                // transitions the subscription and returns the canceled
                // object, so a follow-up retrieve observes terminal state.
                self.canceled.borrow_mut().push(subscription_id.clone());
                let mut subscriptions = self.subscriptions.borrow_mut();
                let mut subscription = subscriptions
                    .get(&subscription_id)
                    .cloned()
                    .ok_or_else(crate::stripe::StripeError::transport)?;
                subscription.status = "canceled".to_string();
                subscriptions.insert(subscription_id, subscription.clone());
                Ok(subscription)
            })
        }
    }

    struct Harness {
        config: Config,
        store: FakeStore,
        mailer: FakeMailer,
        dialer: FakeDialer,
        dns: FakeDns,
        stripe: FakeStripe,
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
            stripe: FakeStripe::default(),
        }
    }

    fn stripe_harness() -> Harness {
        // Webhook/ensure tests provision the Stripe secrets the handlers
        // gate on (synthetic values — the transport is the fake, so no
        // secret ever leaves the test).
        let mut h = harness();
        h.config = cfg().with_stripe(
            Some("sk_test_synthetic".to_string()),
            Some("whsec_synthetic".to_string()),
        );
        h
    }

    impl Harness {
        fn ctx(&self) -> Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns> {
            Ctx {
                cfg: &self.config,
                store: &self.store,
                mailer: &self.mailer,
                dialer: &self.dialer,
                dns: &self.dns,
                stripe: &self.stripe,
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
                stripe: &self.stripe,
                now: NOW,
                source_ip: source_ip.to_string(),
            };
            let req = Request {
                method: method.to_string(),
                path: path.to_string(),
                cookie: None,
                origin: Some(ORIGIN.to_string()),
                authorization: None,
                stripe_signature: None,
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
                authorization: None,
                stripe_signature: None,
                body: body.map(|b| b.into_bytes()).unwrap_or_default(),
            };
            block_on(handle(&self.ctx(), &req))
        }

        /// Bearer-route call with an explicit `Authorization` header value
        /// and no session cookie: the shape the header-only transport
        /// requires. Pass `origin: None` unless the case needs otherwise —
        /// bearer routes never read it.
        fn call_bearer(&self, method: &str, path: &str, authorization: Option<String>) -> Response {
            let req = Request {
                method: method.to_string(),
                path: path.to_string(),
                cookie: None,
                origin: None,
                authorization,
                stripe_signature: None,
                body: Vec::new(),
            };
            block_on(handle(&self.ctx(), &req))
        }

        /// Stripe webhook delivery: raw body bytes plus the
        /// `Stripe-Signature` value, exactly as the Worker entry forwards
        /// them (the signature covers the RAW body, never re-encoded JSON).
        fn call_webhook(&self, raw_body: &[u8], signature: Option<String>) -> Response {
            let req = Request {
                method: "POST".to_string(),
                path: BILLING_WEBHOOK_PATH.to_string(),
                cookie: None,
                origin: None,
                authorization: None,
                stripe_signature: signature,
                body: raw_body.to_vec(),
            };
            block_on(handle(&self.ctx(), &req))
        }
    }

    // --- Stripe billing fixtures -----------------------------------------------------
    //
    // Synthetic ids only (`*_test_*`); no secret material anywhere. Times
    // are Stripe-epoch `i64` around `NOW`.

    const BILLING_NOW: i64 = NOW as i64;
    const PRICE_PLUS: &str = "price_plus_test";
    const PRICE_PRO: &str = "price_pro_test";
    const PLAN_PLUS: &str = "plus";
    const PLAN_PRO: &str = "pro";
    const BILLING_PERIOD: i64 = 30 * 24 * 3_600;

    fn test_bindings() -> Vec<crate::billing_reconcile::PriceBinding> {
        vec![
            crate::billing_reconcile::PriceBinding {
                price_id: PRICE_PLUS.to_string(),
                plan_id: PLAN_PLUS.to_string(),
                selectable: true,
                livemode: false,
            },
            crate::billing_reconcile::PriceBinding {
                price_id: PRICE_PRO.to_string(),
                plan_id: PLAN_PRO.to_string(),
                selectable: true,
                livemode: false,
            },
        ]
    }

    fn seed_billing_user(h: &Harness, user_id: &str) {
        seed_user(h, user_id, &format!("{user_id}@example.com"));
        h.store
            .billing_bindings
            .borrow_mut()
            .extend(test_bindings());
    }

    fn test_session(
        id: &str,
        status: &str,
        payment_status: &str,
        customer: Option<&str>,
        subscription: Option<&str>,
        client_reference: Option<&str>,
    ) -> crate::stripe::CheckoutSession {
        crate::stripe::CheckoutSession {
            id: id.to_string(),
            status: Some(status.to_string()),
            payment_status: Some(payment_status.to_string()),
            customer: customer.map(str::to_string),
            subscription: subscription.map(str::to_string),
            url: Some(format!("https://checkout.stripe.com/pay/{id}")),
            client_reference_id: client_reference.map(str::to_string),
            livemode: false,
        }
    }

    fn test_subscription(
        id: &str,
        customer: &str,
        status: &str,
        price: &str,
    ) -> crate::stripe::Subscription {
        crate::stripe::Subscription {
            id: id.to_string(),
            customer: Some(customer.to_string()),
            status: status.to_string(),
            cancel_at_period_end: false,
            current_period_end: Some(BILLING_NOW + BILLING_PERIOD),
            items: Some(crate::stripe::SubscriptionItems {
                data: vec![crate::stripe::SubscriptionItem {
                    id: "si_test_1".to_string(),
                    price: Some(price.to_string()),
                }],
            }),
            livemode: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn test_invoice(
        id: &str,
        customer: &str,
        subscription: &str,
        price: &str,
        period_start: i64,
        period_end: i64,
        paid_at: i64,
    ) -> crate::stripe::Invoice {
        crate::stripe::Invoice {
            id: id.to_string(),
            customer: Some(customer.to_string()),
            subscription: Some(subscription.to_string()),
            status: "paid".to_string(),
            amount_paid: 1_000,
            currency: Some("usd".to_string()),
            status_transitions: Some(crate::stripe::InvoiceStatusTransitions {
                paid_at: Some(paid_at),
            }),
            lines: Some(crate::stripe::InvoiceLines {
                data: vec![crate::stripe::InvoiceLine {
                    id: format!("{id}_line_1"),
                    amount: 1_000,
                    currency: Some("usd".to_string()),
                    price: Some(price.to_string()),
                    period: Some(crate::stripe::InvoiceLinePeriod {
                        start: Some(period_start),
                        end: Some(period_end),
                    }),
                }],
                has_more: false,
            }),
            livemode: false,
        }
    }

    /// Raw `checkout.session.completed` body: the session id rides
    /// `data.object.id`, exactly the shape the webhook parses.
    fn checkout_completed_body(event_id: &str, session_id: &str, livemode: bool) -> Vec<u8> {
        serde_json::json!({
            "id": event_id,
            "type": "checkout.session.completed",
            "livemode": livemode,
            "data": {"object": {"id": session_id}},
        })
        .to_string()
        .into_bytes()
    }

    fn other_event_body(event_id: &str, event_type: &str) -> Vec<u8> {
        serde_json::json!({
            "id": event_id,
            "type": event_type,
            "livemode": false,
            "data": {"object": {"id": "in_test_9"}},
        })
        .to_string()
        .into_bytes()
    }

    fn sign(body: &[u8], t: i64) -> String {
        crate::stripe::test_signature_header(body, t, "whsec_synthetic")
    }

    fn webhook_ok(resp: &Response) {
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, r#"{"ok":true}"#);
    }

    fn audits_with(h: &Harness, action: &str) -> usize {
        h.store
            .audits
            .borrow()
            .iter()
            .filter(|event| event.action == action)
            .count()
    }

    fn live_for(h: &Harness, user_id: &str) -> Option<crate::billing_reconcile::LiveRow> {
        block_on(h.store.load_reconcile_input(user_id, None))
            .ok()
            .and_then(|input| input.live)
    }

    /// Paid first period through the webhook: op-bound session, active
    /// subscription, one qualifying invoice. Returns the event body for
    /// redelivery tests.
    fn deliver_first_period(h: &Harness, event_id: &str) -> Vec<u8> {
        let period_start = BILLING_NOW;
        let period_end = BILLING_NOW + BILLING_PERIOD;
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                period_start,
                period_end,
                period_start + 60,
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body(event_id, "cs_test_1", false);
        let resp = h.call_webhook(&body, Some(sign(&body, BILLING_NOW)));
        webhook_ok(&resp);
        body
    }

    #[test]
    fn billing_webhook_valid_advances_and_records_history() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        deliver_first_period(&h, "evt_test_1");
        // Coverage advanced from the confirmed paid invoice only.
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.status, "active");
        assert_eq!(live.plan_id, PLAN_PLUS);
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(live.grace_until_unix, None);
        // Exactly one history row, idempotent on the invoice id.
        let history = h.store.invoice_history.borrow();
        assert_eq!(history.len(), 1);
        let row = &history["in_test_1"];
        assert_eq!(row.stripe_subscription_id, "sub_test_1");
        assert_eq!(row.plan_id, PLAN_PLUS);
        assert_eq!(row.period_start_unix, BILLING_NOW);
        assert_eq!(row.period_end_unix, BILLING_NOW + BILLING_PERIOD);
        assert_eq!(row.amount_cents, 1_000);
        assert_eq!(row.currency, "usd");
        assert_eq!(row.paid_at_unix, BILLING_NOW + 60);
        // The sourcing operation resolved with the subscription.
        let op = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(op.state, "succeeded");
        assert_eq!(op.session_id.as_deref(), Some("cs_test_1"));
        // No alerts on the clean path.
        assert_eq!(audits_with(&h, BILLING_FAIL_CLOSED_ACTION), 0);
        assert_eq!(audits_with(&h, BILLING_UNKNOWN_CUSTOMER_ACTION), 0);
    }

    #[test]
    fn billing_direct_retrieval_matches_webhook_state() {
        // Reconcile WITHOUT any webhook delivery reaches the same stored
        // state as the webhook path: the same fake remote subscription +
        // paid invoices through `reconcile_remote_subscription` (the hourly
        // `billing-reconcile` job's entry point) must converge on the
        // identical live row and history. No session, no event, no
        // operation exists on this harness — retrieval alone drives it.
        let period_start = BILLING_NOW;
        let period_end = BILLING_NOW + BILLING_PERIOD;
        let direct = stripe_harness();
        seed_billing_user(&direct, "u1");
        direct.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        direct.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                period_start,
                period_end,
                period_start + 60,
            ),
        );
        let outcome = block_on(reconcile_remote_subscription(
            &direct.ctx(),
            "u1",
            "sub_test_1",
        ))
        .expect("direct retrieval reconciles");
        assert!(outcome.fail_closed.is_none());
        let direct_live = live_for(&direct, "u1").expect("direct live row stored");
        assert_eq!(direct_live.status, "active");
        assert_eq!(direct_live.plan_id, PLAN_PLUS);
        assert_eq!(direct_live.paid_through_unix, Some(period_end));
        assert_eq!(direct_live.grace_until_unix, None);

        // The webhook path over the identical remote state, for comparison.
        let webhook = stripe_harness();
        seed_billing_user(&webhook, "u1");
        deliver_first_period(&webhook, "evt_test_1");
        let webhook_live = live_for(&webhook, "u1").expect("webhook live row stored");
        assert_eq!(direct_live, webhook_live);
        assert_eq!(
            direct.store.invoice_history.borrow().clone(),
            webhook.store.invoice_history.borrow().clone(),
            "direct retrieval must record the same history as the webhook path"
        );
        assert_eq!(direct.store.invoice_history.borrow().len(), 1);
        // No alerts on either clean path.
        assert_eq!(audits_with(&direct, BILLING_FAIL_CLOSED_ACTION), 0);
        assert_eq!(audits_with(&direct, BILLING_UNKNOWN_CUSTOMER_ACTION), 0);
    }

    #[test]
    fn billing_webhook_rejects_tampered_bad_signature_and_stale() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                None,
            ),
        );
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        let good = sign(&body, BILLING_NOW);
        // Tampered body under the good signature.
        let mut tampered = body.clone();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'}' { b']' } else { b'}' };
        assert_eq!(h.call_webhook(&tampered, Some(good.clone())).status, 400);
        // Wrong-secret signature over the intact body.
        let bad = crate::stripe::test_signature_header(&body, BILLING_NOW, "whsec_wrong");
        assert_eq!(h.call_webhook(&body, Some(bad)).status, 400);
        // Missing signature.
        assert_eq!(h.call_webhook(&body, None).status, 400);
        // Stale timestamp (past the 5-minute tolerance).
        assert_eq!(
            h.call_webhook(&body, Some(sign(&body, BILLING_NOW - 600)))
                .status,
            400
        );
        // Future timestamp past the same symmetric tolerance.
        assert_eq!(
            h.call_webhook(&body, Some(sign(&body, BILLING_NOW + 600)))
                .status,
            400
        );
        // No failure touched state or raised an alert.
        assert!(live_for(&h, "u1").is_none());
        assert!(h.store.invoice_history.borrow().is_empty());
        assert!(h.store.webhook_events.borrow().is_empty());
        assert!(h.store.audits.borrow().is_empty());
    }

    #[test]
    fn billing_webhook_duplicate_checkout_reprocesses_idempotently() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let body = deliver_first_period(&h, "evt_test_1");
        // Redelivery of the same event id reprocesses idempotently and
        // still acks 200: coverage and history converge on the same stored
        // state instead of dropping the payment on a consumed claim (this
        // is what lets a claim orphaned by a store outage or a dead worker
        // land on redelivery).
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
    }

    #[test]
    fn billing_webhook_out_of_order_invoices_never_regress() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        // Two paid periods; the later one reconciles first.
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        for (id, start) in [
            ("in_test_1", BILLING_NOW),
            ("in_test_2", BILLING_NOW + BILLING_PERIOD),
        ] {
            h.stripe.invoices.borrow_mut().insert(
                id.to_string(),
                test_invoice(
                    id,
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PLUS,
                    start,
                    start + BILLING_PERIOD,
                    start + 60,
                ),
            );
        }
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        // Newer period first, then the older redelivery under a fresh id.
        let first = checkout_completed_body("evt_test_1", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&first, Some(sign(&first, BILLING_NOW))));
        let second = checkout_completed_body("evt_test_2", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&second, Some(sign(&second, BILLING_NOW))));
        // Coverage sits at the later period; each invoice left exactly one
        // history row; the duplicate pass regressed nothing.
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(
            live.paid_through_unix,
            Some(BILLING_NOW + 2 * BILLING_PERIOD)
        );
        assert_eq!(h.store.invoice_history.borrow().len(), 2);
        let third = checkout_completed_body("evt_test_3", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&third, Some(sign(&third, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(
            live.paid_through_unix,
            Some(BILLING_NOW + 2 * BILLING_PERIOD)
        );
        assert_eq!(h.store.invoice_history.borrow().len(), 2);
    }

    #[test]
    fn billing_webhook_transient_failure_redelivers_and_stores() {
        // A transient failure AFTER the webhook claim must not consume the
        // event: the first delivery races a Stripe 5xx mid-reconcile, the
        // handler 500s (so Stripe retries) and releases the claim, and
        // redelivery of the same event id processes and stores.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        h.stripe
            .fail
            .borrow_mut()
            .insert("retrieve_subscription".to_string());
        assert_eq!(
            h.call_webhook(&body, Some(sign(&body, BILLING_NOW))).status,
            500
        );
        assert!(
            live_for(&h, "u1").is_none(),
            "the failed pass stores nothing"
        );
        // The same event id redelivered: the released claim lets it
        // reprocess instead of acking 200 on a consumed claim.
        h.stripe.fail.borrow_mut().clear();
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("redelivery stores");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
    }

    #[test]
    fn billing_webhook_claimed_but_unprocessed_redelivers_and_stores() {
        // A claim orphaned by a store outage (the same-DB release failed)
        // or a worker death after the claim must not consume the event:
        // redelivery of the same id finds the stale claim and reprocesses
        // idempotently instead of acking 200 with nothing stored.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        // The worker died after the claim, before any Stripe call.
        assert!(block_on(h.store.claim_webhook_event("evt_test_1")).expect("claim first wins"));
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("redelivery stores");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
    }

    #[test]
    fn billing_webhook_non_checkout_and_livemode_ack_without_state() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        // A verified non-checkout event acks 200 with no state change.
        let other = other_event_body("evt_test_9", "invoice.payment_succeeded");
        webhook_ok(&h.call_webhook(&other, Some(sign(&other, BILLING_NOW))));
        assert!(live_for(&h, "u1").is_none());
        assert!(h.store.invoice_history.borrow().is_empty());
        // A live-mode event on this test-mode worker acks 200, alerts, and
        // never calls Stripe or stores anything — even for a completed
        // checkout shape.
        let live_body = checkout_completed_body("evt_test_live", "cs_live_1", true);
        webhook_ok(&h.call_webhook(&live_body, Some(sign(&live_body, BILLING_NOW))));
        assert_eq!(audits_with(&h, BILLING_UNKNOWN_CUSTOMER_ACTION), 1);
        assert!(live_for(&h, "u1").is_none());
        assert!(h.store.invoice_history.borrow().is_empty());
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);
    }

    #[test]
    fn billing_webhook_unprovisioned_secrets_fail_closed() {
        // No Stripe secrets provisioned: the same generic 500 as any store
        // outage, so Stripe retries the delivery instead of losing it.
        let h = harness();
        seed_billing_user(&h, "u1");
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        assert_eq!(
            h.call_webhook(&body, Some(sign(&body, BILLING_NOW))).status,
            500
        );
        assert!(live_for(&h, "u1").is_none());
    }

    #[test]
    fn billing_webhook_unknown_customer_acks_with_alert() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        // No operation binding and no customer binding: the verified event
        // acks 200 with an alert and no state change. (Live
        // cancel-and-alert belongs to the account-deletion slice.)
        h.stripe.sessions.borrow_mut().insert(
            "cs_ghost_1".to_string(),
            test_session(
                "cs_ghost_1",
                "complete",
                "paid",
                Some("cus_ghost_1"),
                Some("sub_ghost_1"),
                None,
            ),
        );
        let body = checkout_completed_body("evt_ghost_1", "cs_ghost_1", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        assert_eq!(audits_with(&h, BILLING_UNKNOWN_CUSTOMER_ACTION), 1);
        assert!(live_for(&h, "u1").is_none());
        assert!(h.store.invoice_history.borrow().is_empty());
    }

    #[test]
    fn billing_webhook_unconfirmed_session_mints_nothing() {
        for (session_status, payment_status) in [
            ("open", "unpaid"),
            ("open", "no_payment_required"),
            ("complete", "unpaid"),
        ] {
            let h = stripe_harness();
            seed_billing_user(&h, "u1");
            h.stripe.sessions.borrow_mut().insert(
                "cs_test_1".to_string(),
                test_session(
                    "cs_test_1",
                    session_status,
                    payment_status,
                    Some("cus_test_1"),
                    None,
                    Some("op_test_1"),
                ),
            );
            block_on(h.store.create_checkout_operation(
                "op_test_1",
                "u1",
                PRICE_PLUS,
                Some("cus_test_1"),
            ))
            .expect("op insert");
            let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
            // No subscription id (unconfirmed or open): acked, no store
            // touch, the flight stays open for the real completion.
            webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
            assert!(live_for(&h, "u1").is_none());
            assert!(h.store.invoice_history.borrow().is_empty());
            let op = block_on(h.store.checkout_operation_by_id("op_test_1"))
                .expect("op lookup")
                .expect("op stored");
            assert_eq!(op.state, "pending");
        }
    }

    #[test]
    fn billing_webhook_store_outage_retries() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        *h.store.fail_billing.borrow_mut() = true;
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                None,
            ),
        );
        // Claim itself fails: 500, so Stripe retries the delivery.
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        assert_eq!(
            h.call_webhook(&body, Some(sign(&body, BILLING_NOW))).status,
            500
        );
    }

    #[test]
    fn billing_fail_closed_unknown_price_trialing_and_unknown_status() {
        // Unknown price: the applied item price has no test-mode mapping.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        let mut sub = test_subscription("sub_test_1", "cus_test_1", "active", "price_unknown");
        h.stripe
            .subscriptions
            .borrow_mut()
            .insert("sub_test_1".to_string(), sub.clone());
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                "price_unknown",
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        // Observed state recorded verbatim, nothing advanced, purchases
        // disabled, alert raised, flight parked for the hourly job.
        let live = live_for(&h, "u1").expect("verbatim row stored");
        assert_eq!(live.status, "active");
        assert_eq!(live.paid_through_unix, None);
        assert_eq!(audits_with(&h, BILLING_FAIL_CLOSED_ACTION), 1);
        let op = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(op.state, "needs_reconcile");
        assert!(h.store.invoice_history.borrow().is_empty());
        // A retry while fail-closed refuses the purchase (no Stripe call).
        let ctx = h.ctx();
        let err = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ));
        assert_eq!(err, Err(EnsureCheckoutError::Unavailable));
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);

        // Trialing: an unknown provider state on the Checkout shape, same
        // fail-closed landing.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        sub.status = "trialing".to_string();
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe
            .subscriptions
            .borrow_mut()
            .insert("sub_test_1".to_string(), sub);
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_2", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        // `trialing` sits outside the live slot by design (the same
        // five-status predicate as the read path), but the verbatim row
        // is still recorded: fail-closed, alerted, nothing advanced.
        let stored = h.store.billing_rows.borrow();
        let row = stored.get("u1").expect("verbatim row stored");
        assert_eq!(row.status, "trialing");
        assert_eq!(row.paid_through_unix, None);
        drop(stored);
        assert!(live_for(&h, "u1").is_none(), "trialing holds no live slot");
        assert_eq!(audits_with(&h, BILLING_FAIL_CLOSED_ACTION), 1);

        // Unknown status string: recorded verbatim, same fail-closed
        // landing.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let mut weird =
            test_subscription("sub_test_1", "cus_test_1", "weird_status_xyz", PRICE_PLUS);
        weird.livemode = false;
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe
            .subscriptions
            .borrow_mut()
            .insert("sub_test_1".to_string(), weird);
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_3", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        // Unknown statuses sit outside the live slot by design (the same
        // five-status predicate as the read path), but the verbatim row is
        // still recorded: fail-closed, alerted, nothing advanced.
        let stored = h.store.billing_rows.borrow();
        assert_eq!(
            stored.get("u1").map(|row| row.status.as_str()),
            Some("weird_status_xyz")
        );
        drop(stored);
        assert_eq!(audits_with(&h, BILLING_FAIL_CLOSED_ACTION), 1);
        assert!(h.store.invoice_history.borrow().is_empty());
    }

    #[test]
    fn billing_renewal_grace_set_once_kept_and_cleared() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        deliver_first_period(&h, "evt_test_1");
        let paid_end = BILLING_NOW + BILLING_PERIOD;
        // First failed renewal: the subscription reads `past_due` with no
        // new money, so the pass sets a fixed 7-day grace from the paid
        // end. Delivered as a later completed session for the same
        // subscription (the renewal attempt's own checkout).
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "past_due", PRICE_PLUS),
        );
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_2".to_string(),
            test_session(
                "cs_test_2",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_2"),
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_2",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_2", "cs_test_2", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.status, "past_due");
        assert_eq!(live.paid_through_unix, Some(paid_end));
        assert_eq!(
            live.grace_until_unix,
            Some(paid_end + crate::billing_reconcile::RENEWAL_GRACE_SECS)
        );
        // A duplicate failed-renewal pass never extends the stored grace.
        let body = checkout_completed_body("evt_test_3", "cs_test_2", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(
            live.grace_until_unix,
            Some(paid_end + crate::billing_reconcile::RENEWAL_GRACE_SECS)
        );
        // Recovery: new money reconciled clears the grace and advances.
        let recovered_end = paid_end + BILLING_PERIOD;
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_2".to_string(),
            test_invoice(
                "in_test_2",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                paid_end,
                recovered_end,
                paid_end + 60,
            ),
        );
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_3".to_string(),
            test_session(
                "cs_test_3",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_3"),
            ),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_3",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_4", "cs_test_3", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.status, "active");
        assert_eq!(live.paid_through_unix, Some(recovered_end));
        assert_eq!(live.grace_until_unix, None);
        assert_eq!(h.store.invoice_history.borrow().len(), 2);
    }

    #[test]
    fn billing_concurrent_attempts_converge_on_one_session() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        // Two attempts with no flight: the second converges on the
        // winner's operation id (same idempotency key), so Stripe mints
        // exactly one session and both callers share its URL.
        let ctx = h.ctx();
        let url_a = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("first attempt");
        // Script the created session as still-open for the reuse check.
        let created_id = url_a
            .rsplit('/')
            .next()
            .expect("session url ends with the id");
        h.stripe.sessions.borrow_mut().insert(
            created_id.to_string(),
            crate::stripe::CheckoutSession {
                id: created_id.to_string(),
                status: Some("open".to_string()),
                payment_status: Some("unpaid".to_string()),
                customer: Some("cus_test_1".to_string()),
                subscription: None,
                url: Some(url_a.clone()),
                client_reference_id: None,
                livemode: false,
            },
        );
        // Point the winner's op at the created session (the mark the
        // create path records).
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("flight open");
        block_on(h.store.update_checkout_operation(
            &open.id,
            "submitted",
            PRICE_PLUS,
            Some(created_id),
        ))
        .expect("mark submitted");
        let url_b = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("second attempt");
        // Lost-response replay: same URL, one Stripe session, one flight.
        assert_eq!(url_a, url_b);
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        // The customer bound once (no overwrite by the racer).
        assert_eq!(
            h.store.billing_customers.borrow().get("u1"),
            Some(&"cus_test_1".to_string())
        );
    }

    #[test]
    fn billing_converged_attempt_uses_winner_bound_customer() {
        // Two first-purchase racers both start unbound, so each mints its
        // own Stripe customer before the single-flight insert converges
        // them on the winner's operation. The payable session must sit on
        // the winner-bound customer, never the loser's orphan (which
        // reconciliation would fail closed on customer mismatch for a user
        // who paid). The session customer re-reads the binding after
        // convergence instead of trusting the pre-insert snapshot.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        *h.stripe.next_customer.borrow_mut() = Some("cus_winner".to_string());
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("winner attempt");
        assert!(url.ends_with("cs_test_1"), "{url}");
        // The loser already minted its orphan before converging on the
        // winner's row (sequential stand-in for the interleave: its own
        // snapshot still names no customer).
        let outcome = block_on(h.store.create_checkout_operation(
            "op_loser",
            "u1",
            PRICE_PLUS,
            Some("cus_orphan"),
        ))
        .expect("converge");
        assert!(matches!(
            outcome,
            crate::billing_reconcile::CreateOpOutcome::Existing(_)
        ));
        let resolved = block_on(converged_session_customer(
            &ctx,
            "u1",
            None,
            "cus_orphan".to_string(),
        ))
        .expect("resolve");
        assert_eq!(resolved, "cus_winner");
        // The winner's own session sat on the bound customer too, and the
        // orphan never touched the account binding.
        assert_eq!(
            h.stripe.session_customers.borrow().as_slice(),
            ["cus_winner".to_string()].as_slice()
        );
        assert_eq!(
            h.store.billing_customers.borrow().get("u1"),
            Some(&"cus_winner".to_string())
        );
    }

    #[test]
    fn billing_converged_attempt_before_bind_fails_retryable() {
        // The pre-bind window: the winner's op INSERT is visible but its
        // customer bind has not landed yet (production publishes them as
        // separate statements; the fake holds the bind back on request). A
        // loser converging here must fail retryable — never resolve to its
        // own orphan — so the retry reads the winner's binding and the
        // payable session never sits on an unbound customer.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        *h.store.defer_customer_bind.borrow_mut() = true;
        block_on(h.store.create_checkout_operation(
            "op_winner",
            "u1",
            PRICE_PLUS,
            Some("cus_winner"),
        ))
        .expect("winner op visible, bind deferred");
        *h.store.defer_customer_bind.borrow_mut() = false;
        // The loser minted its orphan before converging on the winner's
        // row; its own id never binds the account.
        let outcome = block_on(h.store.create_checkout_operation(
            "op_loser",
            "u1",
            PRICE_PLUS,
            Some("cus_orphan"),
        ))
        .expect("converge");
        assert!(matches!(
            outcome,
            crate::billing_reconcile::CreateOpOutcome::Existing(_)
        ));
        let ctx = h.ctx();
        assert_eq!(
            block_on(converged_session_customer(
                &ctx,
                "u1",
                None,
                "cus_orphan".to_string(),
            )),
            Err(EnsureCheckoutError::Failed),
            "no bound customer yet: retry, never the orphan"
        );
        assert!(
            h.store.billing_customers.borrow().get("u1").is_none(),
            "the orphan never binds the account"
        );
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no session minted without a bound customer"
        );
    }

    #[test]
    fn billing_held_subscription_releases_before_replacement() {
        // A held (`incomplete`) row reserves the live slot but confers
        // nothing. A retry must confirm the held subscription terminal
        // (canceling it here) and reconcile it out of the slot before
        // minting the replacement — otherwise the replacement's
        // reconcile would collide with the still-held row and fail a paid
        // subscription closed.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        h.store.billing_rows.borrow_mut().insert(
            "u1".to_string(),
            crate::billing_reconcile::ReconciledRow {
                stripe_subscription_id: "sub_old".to_string(),
                stripe_customer_id: "cus_test_1".to_string(),
                stripe_item_id: "si_test_1".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: PLAN_PLUS.to_string(),
                status: "incomplete".to_string(),
                current_period_end_unix: None,
                cancel_at_period_end: false,
                paid_through_unix: None,
                paid_invoice_id: None,
                grace_until_unix: None,
            },
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_old".to_string(),
            test_subscription("sub_old", "cus_test_1", "incomplete", PRICE_PLUS),
        );
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("replacement proceeds");
        assert!(url.ends_with("cs_test_1"), "{url}");
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        // The held subscription was canceled and reconciled terminal — the
        // stored row sits outside the live slot, so the slot is free.
        assert_eq!(
            h.stripe.subscriptions.borrow()["sub_old"].status,
            "canceled"
        );
        assert_eq!(h.store.billing_rows.borrow()["u1"].status, "canceled");
        assert!(
            live_for(&h, "u1").is_none(),
            "terminal row sits outside the live slot"
        );
    }

    #[test]
    fn billing_held_slot_with_recovered_remote_reconciles_and_refuses() {
        // Stale-local/recovered-remote: the stored row is still
        // `incomplete` but Stripe already shows `active` with a paid
        // invoice (only checkout completions reconcile, so the local row
        // can lag a recovered payment). The retry must NOT cancel the
        // newly paid subscription: it reconciles the paid state in place
        // (coverage + history land) and refuses the purchase as
        // already-subscribed — no cancel call, no new session.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        h.store.billing_rows.borrow_mut().insert(
            "u1".to_string(),
            crate::billing_reconcile::ReconciledRow {
                stripe_subscription_id: "sub_old".to_string(),
                stripe_customer_id: "cus_test_1".to_string(),
                stripe_item_id: "si_test_1".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: PLAN_PLUS.to_string(),
                status: "incomplete".to_string(),
                current_period_end_unix: None,
                cancel_at_period_end: false,
                paid_through_unix: None,
                paid_invoice_id: None,
                grace_until_unix: None,
            },
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_old".to_string(),
            test_subscription("sub_old", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_old",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        let ctx = h.ctx();
        assert_eq!(
            block_on(ensure_checkout_session(
                &ctx,
                "u1",
                PRICE_PLUS,
                "https://example.com/ok",
                "https://example.com/cancel",
                None,
            )),
            Err(EnsureCheckoutError::Unavailable)
        );
        assert!(
            h.stripe.canceled.borrow().is_empty(),
            "a recovered subscription must never be canceled"
        );
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no replacement session for an already-subscribed account"
        );
        let live = live_for(&h, "u1").expect("recovered coverage stored");
        assert_eq!(live.status, "active");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
    }

    #[test]
    fn billing_held_slot_with_unknown_remote_fails_closed_without_cancel() {
        // Unknown remote status: fail closed — alert, no cancel, no
        // reconcile, no session. The stale held row stays reserved so later
        // attempts keep failing closed instead of minting a second
        // subscription against a state we cannot classify.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        h.store.billing_rows.borrow_mut().insert(
            "u1".to_string(),
            crate::billing_reconcile::ReconciledRow {
                stripe_subscription_id: "sub_old".to_string(),
                stripe_customer_id: "cus_test_1".to_string(),
                stripe_item_id: "si_test_1".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: PLAN_PLUS.to_string(),
                status: "incomplete".to_string(),
                current_period_end_unix: None,
                cancel_at_period_end: false,
                paid_through_unix: None,
                paid_invoice_id: None,
                grace_until_unix: None,
            },
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_old".to_string(),
            test_subscription("sub_old", "cus_test_1", "past_due_unmapped", PRICE_PLUS),
        );
        let ctx = h.ctx();
        assert_eq!(
            block_on(ensure_checkout_session(
                &ctx,
                "u1",
                PRICE_PLUS,
                "https://example.com/ok",
                "https://example.com/cancel",
                None,
            )),
            Err(EnsureCheckoutError::Unavailable)
        );
        assert!(
            h.stripe.canceled.borrow().is_empty(),
            "an unclassifiable remote must never be canceled"
        );
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no session against an unknown remote state"
        );
        assert_eq!(audits_with(&h, BILLING_FAIL_CLOSED_ACTION), 1);
        let live = live_for(&h, "u1").expect("held row stays reserved");
        assert_eq!(live.status, "incomplete");
        assert!(live.paid_through_unix.is_none());
        assert!(h.store.invoice_history.borrow().is_empty());
    }

    #[test]
    fn billing_reconcile_outcome_never_regresses_coverage() {
        // Two passes commit out of order: the fresher pass first, then a
        // pass folded from an older snapshot. Coverage (`paid_through_at`
        // and the plan/anchor tied to it) stays at the fresher values —
        // the production upsert's monotonic CASE, mirrored by the fake —
        // and the stale pass keeps the stored status/grace instead of
        // writing older verbatim fields over the fresher pass. Both paid
        // invoices still keep their history rows.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        let row = |paid: Option<i64>, anchor: Option<&str>, status: &str, grace: Option<i64>| {
            crate::billing_reconcile::ReconciledRow {
                stripe_subscription_id: "sub_test_1".to_string(),
                stripe_customer_id: "cus_test_1".to_string(),
                stripe_item_id: "si_test_1".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: PLAN_PLUS.to_string(),
                status: status.to_string(),
                current_period_end_unix: Some(BILLING_NOW + BILLING_PERIOD),
                cancel_at_period_end: false,
                paid_through_unix: paid,
                paid_invoice_id: anchor.map(str::to_string),
                grace_until_unix: grace,
            }
        };
        let history_row = |id: &str, end: i64| crate::billing_reconcile::PaidInvoiceRow {
            stripe_invoice_id: id.to_string(),
            stripe_subscription_id: "sub_test_1".to_string(),
            plan_id: PLAN_PLUS.to_string(),
            period_start_unix: BILLING_NOW,
            period_end_unix: end,
            amount_cents: 1_000,
            currency: "usd".to_string(),
            paid_at_unix: BILLING_NOW + 60,
        };
        let fresh_grace = BILLING_NOW + BILLING_PERIOD + 7 * 24 * 3_600;
        let fresh = crate::billing_reconcile::ReconcileOutcome {
            row: row(
                Some(BILLING_NOW + BILLING_PERIOD),
                Some("in_test_2"),
                "past_due",
                Some(fresh_grace),
            ),
            history: vec![history_row("in_test_2", BILLING_NOW + BILLING_PERIOD)],
            fail_closed: None,
            explicit_stale: false,
            // Inserts (no stored row yet), so the guard never reads this.
            read_plan: None,
            read_anchor: None,
        };
        block_on(h.store.store_reconcile_outcome("u1", &fresh)).expect("fresh stores");
        let stale = crate::billing_reconcile::ReconcileOutcome {
            row: row(Some(BILLING_NOW), Some("in_test_1"), "active", None),
            history: vec![history_row("in_test_1", BILLING_NOW)],
            fail_closed: None,
            explicit_stale: false,
            // Read the fresh row: strictly-older money, so the stale arm
            // fires on the ends regardless.
            read_plan: Some(PLAN_PLUS.to_string()),
            read_anchor: Some("in_test_2".to_string()),
        };
        block_on(h.store.store_reconcile_outcome("u1", &stale)).expect("stale stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(
            live.paid_through_unix,
            Some(BILLING_NOW + BILLING_PERIOD),
            "an older snapshot never regresses coverage"
        );
        assert_eq!(live.plan_id, PLAN_PLUS);
        assert_eq!(
            h.store.billing_rows.borrow()["u1"]
                .paid_invoice_id
                .as_deref(),
            Some("in_test_2"),
            "the anchor stays with the advancing money"
        );
        assert_eq!(
            live.status, "past_due",
            "a stale snapshot never regresses status"
        );
        assert_eq!(
            live.grace_until_unix,
            Some(fresh_grace),
            "a stale snapshot never clears a fresher grace"
        );
        assert_eq!(
            h.store.invoice_history.borrow().len(),
            2,
            "both paid invoices keep their history rows"
        );
    }

    /// Fold input over the handlers-test bindings (PLUS/PRO): stored live
    /// row plus the full price map, so `reconcile_subscription` runs the
    /// real fold instead of a hand-built row.
    fn fold_input(
        plan: &str,
        status: &str,
        paid_through: Option<i64>,
        grace: Option<i64>,
        anchor: Option<&str>,
    ) -> crate::billing_reconcile::ReconcileInput {
        crate::billing_reconcile::ReconcileInput {
            stripe_customer_id: Some("cus_test_1".to_string()),
            live: Some(crate::billing_reconcile::LiveRow {
                stripe_subscription_id: "sub_test_1".to_string(),
                stripe_customer_id: "cus_test_1".to_string(),
                stripe_item_id: "si_test_1".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: plan.to_string(),
                status: status.to_string(),
                paid_through_unix: paid_through,
                paid_invoice_id: anchor.map(str::to_string),
                grace_until_unix: grace,
            }),
            bindings: test_bindings(),
        }
    }

    fn stored_seed(
        plan: &str,
        status: &str,
        paid_through: Option<i64>,
        anchor: Option<&str>,
        grace: Option<i64>,
    ) -> crate::billing_reconcile::ReconcileOutcome {
        crate::billing_reconcile::ReconcileOutcome {
            row: crate::billing_reconcile::ReconciledRow {
                stripe_subscription_id: "sub_test_1".to_string(),
                stripe_customer_id: "cus_test_1".to_string(),
                stripe_item_id: "si_test_1".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: plan.to_string(),
                status: status.to_string(),
                current_period_end_unix: Some(BILLING_NOW + BILLING_PERIOD),
                cancel_at_period_end: false,
                paid_through_unix: paid_through,
                paid_invoice_id: anchor.map(str::to_string),
                grace_until_unix: grace,
            },
            history: vec![],
            fail_closed: None,
            explicit_stale: false,
            // Seeds commit as inserts (no stored row yet), so the
            // equal-end adopt arm never reads this.
            read_plan: None,
            read_anchor: None,
        }
    }

    #[test]
    fn billing_shadow_preserves_per_user_projection() {
        let h = stripe_harness();
        let first = stored_seed(PLAN_PLUS, "active", Some(BILLING_NOW), Some("in_1"), None);
        block_on(h.store.store_reconcile_outcome("u1", &first)).unwrap();
        block_on(h.store.store_reconcile_outcome("u2", &first)).unwrap();

        let mut replacement = stored_seed(PLAN_PLUS, "canceled", None, None, None);
        replacement.row.stripe_subscription_id = "sub_other".to_string();
        block_on(h.store.store_reconcile_outcome("u1", &replacement)).unwrap();
        assert!(
            live_for(&h, "u1").is_none(),
            "terminal projection remains hidden"
        );
        assert_eq!(
            live_for(&h, "u2").unwrap().stripe_subscription_id,
            "sub_test_1"
        );
        let shadow = h.store.billing_shadow.borrow();
        let displaced = &shadow[&(String::from("u1"), String::from("sub_test_1"))];
        assert_eq!(displaced.status, "active");
        assert_eq!(displaced.paid_through_unix, Some(BILLING_NOW));
        assert_eq!(displaced.paid_invoice_id.as_deref(), Some("in_1"));
        assert_eq!(shadow.len(), 1);
        drop(shadow);

        block_on(h.store.store_reconcile_outcome("u1", &first)).unwrap();
        assert_eq!(
            live_for(&h, "u1").unwrap().stripe_subscription_id,
            "sub_test_1"
        );
        let shadow = h.store.billing_shadow.borrow();
        assert_eq!(
            shadow[&(String::from("u1"), String::from("sub_other"))].status,
            "canceled"
        );
        assert_eq!(shadow.len(), 2);
    }

    #[test]
    fn billing_target_selector_preserves_none_and_isolates_user_and_id() {
        let h = stripe_harness();
        let a = stored_seed(PLAN_PLUS, "canceled", Some(100), Some("in_100"), Some(10));
        let mut b = stored_seed(PLAN_PLUS, "active", Some(200), Some("in_200"), None);
        b.row.stripe_subscription_id = "sub_other".into();
        let mut stale = a.clone();
        stale.row.paid_through_unix = Some(50);
        for row in [&a, &b, &stale, &b] {
            block_on(h.store.store_reconcile_outcome("u1", row)).unwrap();
        }
        let select = |user, target| {
            block_on(h.store.load_reconcile_input(user, target))
                .unwrap()
                .live
        };
        let retained = select("u1", Some("sub_test_1")).unwrap();
        assert_eq!(retained.status, "canceled");
        assert_eq!(retained.paid_through_unix, Some(100));
        assert_eq!(retained.paid_invoice_id.as_deref(), Some("in_100"));
        assert_eq!(retained.grace_until_unix, Some(10));
        assert_eq!(
            select("u1", None).unwrap().stripe_subscription_id,
            "sub_other"
        );
        assert_eq!(select("u1", Some("sub_other")).unwrap().status, "active");
        assert!(select("u2", Some("sub_test_1")).is_none());
        assert!(select("u1", Some("missing")).is_none());
        let mut other = a.clone();
        other.row.paid_through_unix = Some(999);
        block_on(h.store.store_reconcile_outcome("u2", &other)).unwrap();
        assert_eq!(
            select("u2", Some("sub_test_1")).unwrap().paid_through_unix,
            Some(999)
        );
        let mut current = b.row.clone();
        current.paid_through_unix = Some(123);
        h.store
            .billing_rows
            .borrow_mut()
            .insert("u1".into(), current);
        h.store
            .billing_shadow
            .borrow_mut()
            .insert(("u1".into(), "sub_other".into()), b.row.clone());
        assert_eq!(
            select("u1", Some("sub_other")).unwrap().paid_through_unix,
            Some(123)
        );
        *h.store.fail_billing.borrow_mut() = true;
        assert!(block_on(h.store.load_reconcile_input("u1", Some("sub_test_1"))).is_err());
        assert!(block_on(h.store.load_reconcile_input("u1", None)).is_err());
    }

    #[test]
    fn billing_shadow_retains_rows_across_repeated_displacement() {
        let h = stripe_harness();
        let a = stored_seed(PLAN_PLUS, "canceled", Some(100), Some("in_100"), Some(10));
        let mut b = stored_seed(PLAN_PLUS, "canceled", None, None, None);
        b.row.stripe_subscription_id = "sub_other".to_string();
        let mut other = a.clone();
        other.row.stripe_subscription_id = "sub_user_2".to_string();
        other.row.paid_through_unix = Some(999);
        let put = |user: &str, outcome: &crate::billing_reconcile::ReconcileOutcome| {
            block_on(h.store.store_reconcile_outcome(user, outcome)).unwrap();
        };
        let key = ("u1".to_string(), "sub_test_1".to_string());
        put("u1", &a);
        put("u2", &other);
        put("u1", &b);
        assert!(live_for(&h, "u1").is_none());
        let before_rows = h.store.billing_rows.borrow().clone();
        let before_shadow = h.store.billing_shadow.borrow().clone();
        let before_history = h.store.invoice_history.borrow().clone();
        *h.store.fail_billing.borrow_mut() = true;
        assert!(block_on(h.store.store_reconcile_outcome("u1", &a)).is_err());
        *h.store.fail_billing.borrow_mut() = false;
        assert_eq!(*h.store.billing_rows.borrow(), before_rows);
        assert_eq!(*h.store.billing_shadow.borrow(), before_shadow);
        assert_eq!(*h.store.invoice_history.borrow(), before_history);
        let mut stale = a.clone();
        stale.row.paid_through_unix = Some(50);
        stale.row.status = "incomplete_expired".to_string();
        let mut expected = stale.row.clone();
        expected.paid_through_unix = a.row.paid_through_unix;
        expected.paid_invoice_id = a.row.paid_invoice_id.clone();
        expected.plan_id = a.row.plan_id.clone();
        expected.status = a.row.status.clone();
        expected.grace_until_unix = a.row.grace_until_unix;
        for _ in 0..2 {
            put("u1", &stale);
            assert_eq!(h.store.billing_rows.borrow()["u1"], expected);
            assert_eq!(h.store.billing_shadow.borrow()[&key], expected);
            assert!(live_for(&h, "u1").is_none());
            put("u1", &b);
            assert_eq!(h.store.billing_rows.borrow()["u1"], b.row);
            assert_eq!(h.store.billing_shadow.borrow()[&key], expected);
        }
        let mut advanced = a.clone();
        advanced.row.paid_through_unix = Some(150);
        advanced.row.plan_id = PLAN_PRO.to_string();
        advanced.row.paid_invoice_id = Some("in_150".to_string());
        put("u1", &advanced);
        assert_eq!(h.store.billing_rows.borrow()["u1"], advanced.row);
        assert_eq!(h.store.billing_shadow.borrow()[&key], advanced.row);
        advanced.row.paid_through_unix = Some(200);
        advanced.row.paid_invoice_id = Some("in_200".to_string());
        put("u1", &advanced);
        assert_eq!(h.store.billing_rows.borrow()["u1"], advanced.row);
        assert_eq!(h.store.billing_shadow.borrow()[&key], advanced.row);
        put("u1", &b);
        assert_eq!(h.store.billing_rows.borrow()["u1"], b.row);
        put("u1", &a);
        assert_eq!(h.store.billing_rows.borrow()["u1"], advanced.row);
        assert_eq!(h.store.billing_shadow.borrow()[&key], advanced.row);
        assert_eq!(h.store.billing_rows.borrow()["u2"], other.row);

        // A directly seeded current row wins over a disagreeing shadow copy.
        let mut seeded = advanced.row.clone();
        seeded.paid_through_unix = Some(120);
        h.store
            .billing_rows
            .borrow_mut()
            .insert("u1".into(), seeded.clone());
        put("u1", &a);
        assert_eq!(h.store.billing_rows.borrow()["u1"], seeded);
        assert_eq!(h.store.billing_shadow.borrow()[&key], seeded);

        // The projection filter is unchanged, but retained status changes its result.
        for (stored_status, incoming_status, visible) in
            [("active", "canceled", true), ("canceled", "active", false)]
        {
            let h = stripe_harness();
            let stored = stored_seed(PLAN_PLUS, stored_status, Some(100), Some("in_100"), None);
            let mut incoming = stored.clone();
            incoming.row.status = incoming_status.into();
            incoming.row.paid_through_unix = Some(50);
            for outcome in [&stored, &b, &incoming] {
                block_on(h.store.store_reconcile_outcome("u1", outcome)).unwrap();
            }
            assert_eq!(h.store.billing_rows.borrow()["u1"], stored.row);
            assert_eq!(live_for(&h, "u1").is_some(), visible);
        }
    }

    #[test]
    fn billing_shadow_matches_existing_upsert_field_guards() {
        for case in [
            "valid",
            "shared",
            "read-plan",
            "read-anchor",
            "no-anchor",
            "null-end",
            "first-end",
            "advance-null-anchor",
        ] {
            let h = stripe_harness();
            let mut stored =
                stored_seed(PLAN_PLUS, "canceled", Some(100), Some("in_old"), Some(10));
            if case == "first-end" {
                stored.row.paid_through_unix = None;
            }
            let mut replacement = stored.clone();
            replacement.row.stripe_subscription_id = "sub_other".into();
            replacement.row.status = "canceled".into();
            let mut incoming = stored_seed(
                PLAN_PRO,
                "incomplete_expired",
                Some(100),
                Some("in_new"),
                Some(20),
            );
            incoming.row.stripe_customer_id = "cus_new".into();
            incoming.row.stripe_item_id = "si_new".into();
            incoming.row.stripe_price_id = "price_new".into();
            incoming.row.current_period_end_unix = Some(200);
            incoming.row.cancel_at_period_end = true;
            incoming.read_plan = Some(PLAN_PLUS.into());
            incoming.read_anchor = Some("in_old".into());
            match case {
                "shared" => incoming.row.paid_invoice_id = Some("in_old".into()),
                "read-plan" => incoming.read_plan = Some(PLAN_PRO.into()),
                "read-anchor" => incoming.read_anchor = Some("in_wrong".into()),
                "no-anchor" | "advance-null-anchor" => incoming.row.paid_invoice_id = None,
                "null-end" => incoming.row.paid_through_unix = None,
                _ => {}
            }
            if case == "advance-null-anchor" {
                incoming.row.paid_through_unix = Some(150);
            }
            let mut expected = incoming.row.clone();
            if matches!(case, "read-plan" | "read-anchor" | "no-anchor" | "null-end") {
                expected.paid_through_unix = stored.row.paid_through_unix;
                expected.plan_id = stored.row.plan_id.clone();
                expected.paid_invoice_id = stored.row.paid_invoice_id.clone();
            } else if case == "advance-null-anchor" {
                expected.paid_invoice_id = stored.row.paid_invoice_id.clone();
            }
            if case == "no-anchor" {
                expected.status = stored.row.status.clone();
                expected.grace_until_unix = stored.row.grace_until_unix;
            }
            for outcome in [&stored, &replacement, &incoming] {
                block_on(h.store.store_reconcile_outcome("u1", outcome)).unwrap();
            }
            let key = ("u1".to_string(), "sub_test_1".to_string());
            assert_eq!(
                h.store.billing_rows.borrow()["u1"],
                expected,
                "{case} current"
            );
            assert_eq!(
                h.store.billing_shadow.borrow()[&key],
                expected,
                "{case} retained"
            );
            if case == "advance-null-anchor" {
                let fresh = stripe_harness();
                for outcome in [&stored, &incoming] {
                    block_on(fresh.store.store_reconcile_outcome("u1", outcome)).unwrap();
                }
                assert_eq!(fresh.store.billing_rows.borrow()["u1"], expected, "same-ID");
            }
        }
    }

    #[test]
    fn billing_equal_end_plan_switch_persists_through_store() {
        // The pure fold relabels on an equal-end plan switch, and the
        // production upsert's plan/anchor CASEs must persist that relabel:
        // stored PLUS ending E, paid PRO invoice also ending E → the row
        // becomes PRO anchored on the new invoice. A strict-greater guard
        // would keep PLUS and the old anchor forever (no later money can
        // fix a stuck plan), so this test folds the real snapshot and
        // commits through the store instead of asserting the fold alone.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let end = BILLING_NOW + BILLING_PERIOD;
        block_on(h.store.store_reconcile_outcome(
            "u1",
            &stored_seed(PLAN_PLUS, "active", Some(end), Some("in_old"), None),
        ))
        .expect("seed stores");
        let switched = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
            invoices: vec![
                test_invoice(
                    "in_old",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PLUS,
                    BILLING_NOW,
                    end,
                    BILLING_NOW,
                ),
                test_invoice(
                    "in_new",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PRO,
                    BILLING_NOW,
                    end,
                    BILLING_NOW + 60,
                ),
            ],
        };
        // The pass reads the stored row (anchor `in_old`, present in the
        // snapshot): a real pass always folds what it read, and the write
        // only trusts a relabel whose read row is still stored.
        let outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_old")),
            &switched,
            BILLING_NOW,
        );
        assert_eq!(outcome.row.plan_id, PLAN_PRO, "the fold relabels");
        assert_eq!(
            outcome.row.paid_invoice_id.as_deref(),
            Some("in_new"),
            "the fold anchors on the new evidence"
        );
        block_on(h.store.store_reconcile_outcome("u1", &outcome)).expect("switch stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(
            live.plan_id, PLAN_PRO,
            "the equal-end relabel persists in the write"
        );
        assert_eq!(live.paid_through_unix, Some(end), "no advance");
        assert_eq!(
            h.store.billing_rows.borrow()["u1"]
                .paid_invoice_id
                .as_deref(),
            Some("in_new"),
            "the anchor follows the new evidence"
        );
        assert_eq!(live.status, "active");
        assert!(live.grace_until_unix.is_none());
    }

    #[test]
    fn billing_explicit_stale_fold_never_changes_stored_fields() {
        let h = stripe_harness();
        let end = BILLING_NOW + BILLING_PERIOD;
        let seed = stored_seed(PLAN_PLUS, "active", Some(end), Some("in_seed"), None);
        block_on(h.store.store_reconcile_outcome("u1", &seed)).unwrap();
        let mut subscription = test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS);
        subscription.cancel_at_period_end = true;
        subscription.current_period_end = Some(end + 100);
        let remote = crate::billing_reconcile::RemoteSubscription {
            subscription,
            invoices: vec![test_invoice(
                "in_old",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW - BILLING_PERIOD,
                BILLING_NOW,
                BILLING_NOW - 100,
            )],
        };
        let stale = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_seed")),
            &remote,
            BILLING_NOW,
        );
        assert!(stale.explicit_stale);
        assert_eq!(stale.history.len(), 1);
        block_on(h.store.store_reconcile_outcome("u1", &stale)).unwrap();
        assert_eq!(h.store.billing_rows.borrow()["u1"], seed.row);
        assert!(h.store.invoice_history.borrow().contains_key("in_old"));
        let fresh_remote = crate::billing_reconcile::RemoteSubscription {
            invoices: vec![test_invoice(
                "in_seed",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                end,
                BILLING_NOW,
            )],
            ..remote
        };
        let fresh = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_seed")),
            &fresh_remote,
            BILLING_NOW,
        );
        assert!(!fresh.explicit_stale);
        block_on(h.store.store_reconcile_outcome("u1", &fresh)).unwrap();
        assert!(h.store.billing_rows.borrow()["u1"].cancel_at_period_end);
        assert_eq!(
            h.store.billing_rows.borrow()["u1"].current_period_end_unix,
            Some(end + 100)
        );
    }

    #[test]
    fn billing_stale_fold_commit_race_keeps_fresher_status_and_grace() {
        // Interleaving built from REAL fold outcomes (not hand-built
        // rows): stale pass S folds an older snapshot (E1 money) over
        // stored E2/active, fresh pass F folds the equal-E2 failed renewal
        // (past_due + first grace). F commits first, S second — S must
        // compare strictly older in the write guard and keep the stored
        // status/grace instead of overwriting them with older verbatim
        // fields. Hand-building S with the stored end would bypass the
        // fold's normalization and miss exactly this.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let e2 = BILLING_NOW + BILLING_PERIOD;
        let e1 = BILLING_NOW;
        block_on(h.store.store_reconcile_outcome(
            "u1",
            &stored_seed(PLAN_PLUS, "active", Some(e2), Some("in_seed"), None),
        ))
        .expect("seed stores");
        h.store.invoice_history.borrow_mut().insert(
            "in_seed".to_string(),
            crate::billing_reconcile::PaidInvoiceRow {
                stripe_invoice_id: "in_seed".to_string(),
                stripe_subscription_id: "sub_test_1".to_string(),
                plan_id: PLAN_PLUS.to_string(),
                period_start_unix: BILLING_NOW - BILLING_PERIOD,
                period_end_unix: e2,
                amount_cents: 1_000,
                currency: "usd".to_string(),
                paid_at_unix: BILLING_NOW,
            },
        );
        // S reads stored E2/active with remote money E1 < E2: stale.
        let older = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
            invoices: vec![test_invoice(
                "in_old",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                e1 - BILLING_PERIOD,
                e1,
                BILLING_NOW - 100,
            )],
        };
        let stale_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(e2), None, None),
            &older,
            BILLING_NOW,
        );
        assert_eq!(
            stale_outcome.row.paid_through_unix,
            Some(e1),
            "the stale fold carries its evidence end into the write guard"
        );
        // F reads the same stored row with the equal-E2 failed renewal. A
        // real fresh listing still carries the stored anchor invoice (the
        // failure adds no new invoice, it does not erase the paid one), so
        // the snapshot contains in_seed and the keep re-pins it — an
        // anchor-less equal-end row against an anchored stored row is the
        // stale shape, and must not be mistaken for this fresh outcome.
        let failed = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "past_due", PRICE_PLUS),
            invoices: vec![test_invoice(
                "in_seed",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW - BILLING_PERIOD,
                e2,
                BILLING_NOW,
            )],
        };
        let fresh_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(e2), None, Some("in_seed")),
            &failed,
            BILLING_NOW,
        );
        let grace = e2 + crate::billing_reconcile::RENEWAL_GRACE_SECS;
        assert_eq!(fresh_outcome.row.status, "past_due");
        assert_eq!(fresh_outcome.row.grace_until_unix, Some(grace));
        assert_eq!(
            fresh_outcome.row.paid_invoice_id.as_deref(),
            Some("in_seed"),
            "the fresh keep re-pins the stored anchor it observed"
        );
        // F commits first, S second.
        block_on(h.store.store_reconcile_outcome("u1", &fresh_outcome)).expect("fresh stores");
        block_on(h.store.store_reconcile_outcome("u1", &stale_outcome)).expect("stale stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.paid_through_unix, Some(e2), "coverage never regresses");
        assert_eq!(
            live.status, "past_due",
            "the second-committing stale pass keeps the fresher status"
        );
        assert_eq!(
            live.grace_until_unix,
            Some(grace),
            "the second-committing stale pass keeps the fresher grace"
        );
        assert_eq!(live.plan_id, PLAN_PLUS);
        assert_eq!(
            h.store.invoice_history.borrow().len(),
            2,
            "both the seed and the stale invoice keep history rows"
        );
    }

    #[test]
    fn billing_stale_without_item_plan_money_keeps_recovery() {
        // The NULL-evidence stale shape, built from REAL fold outcomes:
        // stale pass S reads stored E2/active anchored `in_seed`, but its
        // snapshot lost the anchor AND holds no item-plan money at all
        // (empty listing) — so the fold emits no evidence end. The write
        // guard only fires on non-NULL evidence, so the fold falls back to
        // the stored end with the anchor cleared: the anchor-less
        // equal-end shape the guard already reads as stale. F (the
        // equal-E2 failed renewal: `past_due` + first grace) commits first,
        // S second — S must keep the recovery's status/grace instead of
        // overwriting them with the older verbatim fields.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let e2 = BILLING_NOW + BILLING_PERIOD;
        block_on(h.store.store_reconcile_outcome(
            "u1",
            &stored_seed(PLAN_PLUS, "active", Some(e2), Some("in_seed"), None),
        ))
        .expect("seed stores");
        let emptied = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
            invoices: vec![],
        };
        let stale_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(e2), None, Some("in_seed")),
            &emptied,
            BILLING_NOW,
        );
        assert_eq!(
            stale_outcome.row.paid_through_unix,
            Some(e2),
            "a stale pass with no item-plan money carries the stored end so the guard can see it"
        );
        assert!(
            stale_outcome.row.paid_invoice_id.is_none(),
            "only the stale branch emits the anchor-less shape"
        );
        assert_eq!(
            stale_outcome.read_anchor.as_deref(),
            Some("in_seed"),
            "the write needs the read anchor to tell keeps from relabels"
        );
        let failed = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "past_due", PRICE_PLUS),
            invoices: vec![test_invoice(
                "in_seed",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW - BILLING_PERIOD,
                e2,
                BILLING_NOW,
            )],
        };
        let fresh_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(e2), None, Some("in_seed")),
            &failed,
            BILLING_NOW,
        );
        let grace = e2 + crate::billing_reconcile::RENEWAL_GRACE_SECS;
        assert_eq!(fresh_outcome.row.status, "past_due");
        assert_eq!(fresh_outcome.row.grace_until_unix, Some(grace));
        // F commits first, S second.
        block_on(h.store.store_reconcile_outcome("u1", &fresh_outcome)).expect("fresh stores");
        block_on(h.store.store_reconcile_outcome("u1", &stale_outcome)).expect("stale stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.paid_through_unix, Some(e2), "coverage never regresses");
        assert_eq!(
            live.status, "past_due",
            "a stale pass with no evidence money keeps the recovery status"
        );
        assert_eq!(
            live.grace_until_unix,
            Some(grace),
            "a stale pass with no evidence money keeps the recovery grace"
        );
        assert_eq!(live.plan_id, PLAN_PLUS);
        assert_eq!(
            h.store.billing_rows.borrow()["u1"]
                .paid_invoice_id
                .as_deref(),
            Some("in_seed"),
            "the anchor stays with the recovery"
        );
    }

    #[test]
    fn billing_stale_held_and_terminal_commit_orders_keep_fresh_status_and_grace() {
        // Both passes fold the same stored row. Only the fresh snapshot
        // contains its anchor; equal coverage must not hide stale evidence
        // when the held/terminal pass commits after a status/grace change.
        let end = BILLING_NOW + BILLING_PERIOD;
        let grace = end + crate::billing_reconcile::RENEWAL_GRACE_SECS;
        for status in [
            "incomplete",
            "unpaid",
            "paused",
            "canceled",
            "incomplete_expired",
        ] {
            for (before, after) in [("active", "past_due"), ("past_due", "active")] {
                for empty in [false, true] {
                    for stale_first in [false, true] {
                        let h = stripe_harness();
                        seed_billing_user(&h, "u1");
                        let initial_grace = (before == "past_due").then_some(grace);
                        block_on(h.store.store_reconcile_outcome(
                            "u1",
                            &stored_seed(
                                PLAN_PLUS,
                                before,
                                Some(end),
                                Some("in_seed"),
                                initial_grace,
                            ),
                        ))
                        .expect("seed stores");
                        let input = fold_input(
                            PLAN_PLUS,
                            before,
                            Some(end),
                            initial_grace,
                            Some("in_seed"),
                        );
                        let invoice = |id, invoice_end| {
                            test_invoice(
                                id,
                                "cus_test_1",
                                "sub_test_1",
                                PRICE_PLUS,
                                BILLING_NOW - BILLING_PERIOD,
                                invoice_end,
                                BILLING_NOW,
                            )
                        };
                        let stale = crate::billing_reconcile::reconcile_subscription(
                            &input,
                            &crate::billing_reconcile::RemoteSubscription {
                                subscription: test_subscription(
                                    "sub_test_1",
                                    "cus_test_1",
                                    status,
                                    PRICE_PLUS,
                                ),
                                invoices: if empty {
                                    vec![]
                                } else {
                                    vec![invoice("in_old", BILLING_NOW)]
                                },
                            },
                            BILLING_NOW,
                        );
                        assert_eq!(stale.fail_closed, None);
                        assert_eq!(stale.row.status, before);
                        assert_eq!(stale.row.grace_until_unix, initial_grace);
                        assert_eq!(stale.row.paid_through_unix, Some(end));
                        assert_eq!(
                            stale.row.paid_invoice_id, None,
                            "stale evidence, never a re-pin"
                        );
                        assert_eq!(stale.read_anchor.as_deref(), Some("in_seed"));
                        assert!(
                            stale.history.is_empty(),
                            "held/terminal paths do not backfill history"
                        );
                        let fresh = crate::billing_reconcile::reconcile_subscription(
                            &input,
                            &crate::billing_reconcile::RemoteSubscription {
                                subscription: test_subscription(
                                    "sub_test_1",
                                    "cus_test_1",
                                    after,
                                    PRICE_PLUS,
                                ),
                                invoices: vec![invoice("in_seed", end)],
                            },
                            BILLING_NOW,
                        );
                        let commits = if stale_first {
                            [&stale, &fresh]
                        } else {
                            [&fresh, &stale]
                        };
                        for outcome in commits {
                            block_on(h.store.store_reconcile_outcome("u1", outcome))
                                .expect("pass stores");
                        }
                        let row = h.store.billing_rows.borrow()["u1"].clone();
                        assert_eq!(
                            row.status, after,
                            "{status}, empty={empty}, stale_first={stale_first}"
                        );
                        assert_eq!(row.grace_until_unix, (after == "past_due").then_some(grace));
                        assert_eq!(row.plan_id, PLAN_PLUS);
                        assert_eq!(row.paid_through_unix, Some(end));
                        assert_eq!(row.paid_invoice_id.as_deref(), Some("in_seed"));
                        assert_eq!(h.store.invoice_history.borrow().len(), 1);
                    }
                }
            }
        }
    }

    #[test]
    fn billing_permuted_invoice_history_replays_idempotently() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let line = |price, start, end| {
            test_invoice(
                "in_mix",
                "cus_test_1",
                "sub_test_1",
                price,
                BILLING_NOW + start,
                BILLING_NOW + end,
                BILLING_NOW,
            )
            .lines
            .expect("lines")
            .data
            .remove(0)
        };
        let lines = [
            line(PRICE_PRO, 1, 10),
            line(PRICE_PLUS, 0, 15),
            line(PRICE_PRO, 10, 30),
        ];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut invoice = test_invoice(
                "in_mix",
                "cus_test_1",
                "sub_test_1",
                PRICE_PRO,
                BILLING_NOW + 1,
                BILLING_NOW + 30,
                BILLING_NOW,
            );
            invoice.lines.as_mut().expect("lines").data = order.map(|i| lines[i].clone()).to_vec();
            let input = block_on(h.store.load_reconcile_input("u1", None)).expect("input loads");
            let outcome = crate::billing_reconcile::reconcile_subscription(
                &input,
                &crate::billing_reconcile::RemoteSubscription {
                    subscription: test_subscription(
                        "sub_test_1",
                        "cus_test_1",
                        "active",
                        PRICE_PRO,
                    ),
                    invoices: vec![invoice],
                },
                BILLING_NOW,
            );
            assert_eq!(outcome.history.len(), 1);
            assert_eq!(outcome.history[0].period_start_unix, BILLING_NOW + 1);
            block_on(h.store.store_reconcile_outcome("u1", &outcome)).expect("replay stores");
            let history = h.store.invoice_history.borrow();
            assert_eq!(
                history.len(),
                1,
                "all permutations retain the same invoice identity"
            );
            assert_eq!(history["in_mix"], outcome.history[0]);
        }
    }

    #[test]
    fn billing_relabel_first_keep_second_keeps_new_plan() {
        // Relabel-then-keep commit order, built from REAL fold outcomes:
        // keep pass K folds the pre-switch snapshot (plus/E/`in_plus`) and
        // re-pins the anchor it read; relabel pass R folds the switched
        // snapshot (pro item, both invoices, the switch paid later) over
        // the same stored row. R commits first, K second — the stored row
        // no longer matches what K read, so the write keeps the relabeled
        // plan/anchor instead of flipping the row back to plus until the
        // next reconcile.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let end = BILLING_NOW + BILLING_PERIOD;
        block_on(h.store.store_reconcile_outcome(
            "u1",
            &stored_seed(PLAN_PLUS, "active", Some(end), Some("in_plus"), None),
        ))
        .expect("seed stores");
        let plus_view = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
            invoices: vec![test_invoice(
                "in_plus",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                end,
                BILLING_NOW,
            )],
        };
        let keep_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_plus")),
            &plus_view,
            BILLING_NOW,
        );
        assert_eq!(keep_outcome.row.plan_id, PLAN_PLUS, "the keep folds plus");
        assert_eq!(
            keep_outcome.row.paid_invoice_id.as_deref(),
            Some("in_plus"),
            "the keep re-pins the anchor it read"
        );
        let switched = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
            invoices: vec![
                test_invoice(
                    "in_plus",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PLUS,
                    BILLING_NOW,
                    end,
                    BILLING_NOW,
                ),
                test_invoice(
                    "in_new",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PRO,
                    BILLING_NOW,
                    end,
                    BILLING_NOW + 60,
                ),
            ],
        };
        let relabel_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_plus")),
            &switched,
            BILLING_NOW,
        );
        assert_eq!(
            relabel_outcome.row.plan_id, PLAN_PRO,
            "the relabel adopts the current item plan"
        );
        assert_eq!(
            relabel_outcome.row.paid_invoice_id.as_deref(),
            Some("in_new"),
            "the relabel anchors on the new evidence"
        );
        assert_eq!(
            relabel_outcome.read_plan.as_deref(),
            Some(PLAN_PLUS),
            "the relabel records the plan it read"
        );
        assert_eq!(
            relabel_outcome.read_anchor.as_deref(),
            Some("in_plus"),
            "the relabel records the anchor it read"
        );
        // R commits first, K second.
        block_on(h.store.store_reconcile_outcome("u1", &relabel_outcome)).expect("relabel stores");
        block_on(h.store.store_reconcile_outcome("u1", &keep_outcome)).expect("keep stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.paid_through_unix, Some(end), "no advance");
        assert_eq!(
            live.plan_id, PLAN_PRO,
            "the late keep does not reverse the committed relabel"
        );
        assert_eq!(
            h.store.billing_rows.borrow()["u1"]
                .paid_invoice_id
                .as_deref(),
            Some("in_new"),
            "the anchor stays with the new evidence"
        );
        assert_eq!(live.status, "active");
    }

    #[test]
    fn billing_shared_invoice_relabel_persists_through_store() {
        // One paid invoice carrying qualifying lines for BOTH plans, same
        // end: stored PLUS/E anchored on `in_shared`, the item now reports
        // PRO. The fold relabels onto the shared invoice — emitted anchor
        // equalling the read anchor — and the write must still persist it
        // when nothing moved the row meanwhile: gating the adopt on
        // emitted-vs-read difference would leave PLUS stuck forever (no
        // later money can fix a same-invoice switch), so the arm compares
        // stored-vs-read instead. Built from a REAL fold outcome.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let end = BILLING_NOW + BILLING_PERIOD;
        block_on(h.store.store_reconcile_outcome(
            "u1",
            &stored_seed(PLAN_PLUS, "active", Some(end), Some("in_shared"), None),
        ))
        .expect("seed stores");
        let mut shared = test_invoice(
            "in_shared",
            "cus_test_1",
            "sub_test_1",
            PRICE_PLUS,
            BILLING_NOW,
            end,
            BILLING_NOW,
        );
        shared
            .lines
            .as_mut()
            .expect("invoice lines")
            .data
            .push(crate::stripe::InvoiceLine {
                id: "in_shared_line_2".to_string(),
                amount: 1_000,
                currency: Some("usd".to_string()),
                price: Some(PRICE_PRO.to_string()),
                period: Some(crate::stripe::InvoiceLinePeriod {
                    start: Some(BILLING_NOW),
                    end: Some(end),
                }),
            });
        let switched = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
            invoices: vec![shared],
        };
        let relabel_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_shared")),
            &switched,
            BILLING_NOW,
        );
        assert_eq!(
            relabel_outcome.row.plan_id, PLAN_PRO,
            "the fold relabels onto the shared invoice"
        );
        assert_eq!(
            relabel_outcome.row.paid_invoice_id.as_deref(),
            Some("in_shared"),
            "the shared invoice anchors both plans"
        );
        assert_eq!(
            relabel_outcome.read_anchor.as_deref(),
            Some("in_shared"),
            "emitted and read anchors coincide here by construction"
        );
        block_on(h.store.store_reconcile_outcome("u1", &relabel_outcome)).expect("relabel stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(
            live.plan_id, PLAN_PRO,
            "a same-invoice relabel persists when the row never moved"
        );
        assert_eq!(
            h.store.billing_rows.borrow()["u1"]
                .paid_invoice_id
                .as_deref(),
            Some("in_shared"),
            "the anchor stays on the shared invoice"
        );
        assert_eq!(live.status, "active");
    }

    #[test]
    fn billing_older_relabel_committing_second_keeps_newer_switch() {
        // Two paid switches, same end, reverse commit order — built from
        // REAL fold outcomes: older pass O reads stored plus/E/`in_plus`
        // and relabels onto its own newer-paid evidence `in_old`; newer
        // pass N relabels onto `in_new` (paid later still) and commits
        // first. O commits second with a different anchor than it read, so
        // an emitted-vs-read gate would revert the row to the older
        // switch; the stored-vs-read arm keeps the newer plan/anchor.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let end = BILLING_NOW + BILLING_PERIOD;
        block_on(h.store.store_reconcile_outcome(
            "u1",
            &stored_seed(PLAN_PLUS, "active", Some(end), Some("in_plus"), None),
        ))
        .expect("seed stores");
        let older_view = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
            invoices: vec![
                test_invoice(
                    "in_plus",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PLUS,
                    BILLING_NOW,
                    end,
                    BILLING_NOW,
                ),
                test_invoice(
                    "in_old",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PRO,
                    BILLING_NOW,
                    end,
                    BILLING_NOW + 60,
                ),
            ],
        };
        let older_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_plus")),
            &older_view,
            BILLING_NOW,
        );
        assert_eq!(
            older_outcome.row.paid_invoice_id.as_deref(),
            Some("in_old"),
            "the older pass relabels onto its own evidence"
        );
        let newer_view = crate::billing_reconcile::RemoteSubscription {
            subscription: test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
            invoices: vec![
                test_invoice(
                    "in_plus",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PLUS,
                    BILLING_NOW,
                    end,
                    BILLING_NOW,
                ),
                test_invoice(
                    "in_new",
                    "cus_test_1",
                    "sub_test_1",
                    PRICE_PRO,
                    BILLING_NOW,
                    end,
                    BILLING_NOW + 120,
                ),
            ],
        };
        let newer_outcome = crate::billing_reconcile::reconcile_subscription(
            &fold_input(PLAN_PLUS, "active", Some(end), None, Some("in_plus")),
            &newer_view,
            BILLING_NOW,
        );
        assert_eq!(
            newer_outcome.row.paid_invoice_id.as_deref(),
            Some("in_new"),
            "the newer pass relabels onto the later payment"
        );
        // Newer commits first, older second.
        block_on(h.store.store_reconcile_outcome("u1", &newer_outcome)).expect("newer stores");
        block_on(h.store.store_reconcile_outcome("u1", &older_outcome)).expect("older stores");
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.plan_id, PLAN_PRO);
        assert_eq!(
            h.store.billing_rows.borrow()["u1"]
                .paid_invoice_id
                .as_deref(),
            Some("in_new"),
            "an older relabel committing second never reverts a newer switch"
        );
        assert_eq!(live.status, "active");
    }

    #[test]
    fn billing_completed_session_under_open_op_reconciles_without_recreating() {
        // The purchase completed but its webhook was lost: a retry finds
        // the completed paid session under the still-open operation. It
        // must reconcile through the shared driver — not mint a second
        // payable session under the same idempotency key. Only a
        // confirmed-expired or missing session repairs.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_test_1"),
        ))
        .expect("mark submitted");
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        let ctx = h.ctx();
        let err = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ));
        assert_eq!(err, Err(EnsureCheckoutError::Unavailable));
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no second session minted"
        );
        let live = live_for(&h, "u1").expect("reconciled");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
    }

    #[test]
    fn billing_price_mismatched_open_session_expires_and_replaces() {
        // The first attempt names PLUS and leaves its session open; the
        // second attempt names PRO. The second attempt must not inherit the
        // first attempt's session (which keeps the first attempt's price):
        // the open session is expired first — a replacement never mints
        // beside a possibly-payable session — the flight is superseded, and
        // the replacement mints under a fresh operation id, hence a fresh
        // Stripe idempotency key.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_old_1"),
        ))
        .expect("mark submitted");
        h.stripe.sessions.borrow_mut().insert(
            "cs_old_1".to_string(),
            test_session(
                "cs_old_1",
                "open",
                "unpaid",
                Some("cus_test_1"),
                None,
                Some("op_test_1"),
            ),
        );
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PRO,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("price-mismatched attempt replaces");
        assert!(url.ends_with("cs_test_1"), "{url}");
        assert_eq!(
            h.stripe.expired.borrow().as_slice(),
            ["cs_old_1".to_string()].as_slice(),
            "the mismatched open session is expired before replacement"
        );
        // The dead session transitioned, like the real expire endpoint.
        let dead = &h.stripe.sessions.borrow()["cs_old_1"];
        assert_eq!(dead.status.as_deref(), Some("expired"));
        assert!(dead.url.is_none());
        // The old flight is superseded (releasing the slot) with its price
        // and session preserved for the audit trail.
        let old = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(old.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(old.price_id.as_deref(), Some(PRICE_PLUS));
        assert_eq!(old.session_id.as_deref(), Some("cs_old_1"));
        // Exactly one new session, minted under a fresh operation id — a
        // fresh idempotency key, never the dead session's — on the bound
        // customer.
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(minted.len(), 1);
        assert_ne!(
            minted[0], "op_test_1",
            "the replacement mints under a fresh operation id"
        );
        assert_eq!(
            h.stripe.session_customers.borrow().as_slice(),
            ["cus_test_1".to_string()].as_slice()
        );
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("replacement flight open");
        assert_eq!(open.id, minted[0]);
        assert_eq!(open.price_id.as_deref(), Some(PRICE_PRO));
        assert_eq!(open.session_id.as_deref(), Some("cs_test_1"));
        assert_eq!(
            h.store.billing_customers.borrow().get("u1"),
            Some(&"cus_test_1".to_string())
        );
    }

    #[test]
    fn billing_session_mark_misses_retired_and_missing_rows() {
        // The conditional mark only lands on unresolved rows: a mark that
        // arrives after a deliberate supersede reports `false` and leaves
        // the retired row untouched instead of resurrecting `submitted` on
        // it; a missing row reports `false` the same way.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        assert!(
            block_on(h.store.update_checkout_operation(
                "op_test_1",
                "submitted",
                PRICE_PLUS,
                Some("cs_test_1"),
            ))
            .expect("mark"),
            "unresolved row takes the mark"
        );
        assert!(
            block_on(h.store.update_checkout_operation(
                "op_test_1",
                crate::billing_reconcile::SUPERSEDED_OP_STATE,
                PRICE_PLUS,
                Some("cs_test_1"),
            ))
            .expect("retire"),
            "open row retires"
        );
        assert!(
            !block_on(h.store.update_checkout_operation(
                "op_test_1",
                "submitted",
                PRICE_PLUS,
                Some("cs_late_1"),
            ))
            .expect("late mark"),
            "retired row rejects the late mark"
        );
        assert!(
            !block_on(h.store.update_checkout_operation(
                "op_missing_1",
                "submitted",
                PRICE_PLUS,
                Some("cs_late_1"),
            ))
            .expect("missing mark"),
            "missing row reports a miss"
        );
        let op = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(op.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(
            op.session_id.as_deref(),
            Some("cs_test_1"),
            "the late session id never lands on the retired row"
        );
    }

    #[test]
    fn billing_late_create_mark_after_supersede_fails_without_reviving() {
        // A replacer superseded op A while A's original Stripe create was
        // still in flight. The late-arriving create must NOT resurrect
        // `submitted` on the retired row (which would re-occupy the flight
        // and hand out the now-expired session's URL): the conditional mark
        // misses, the attempt fails retryable, and the flight stays free
        // for the replacement.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let created = block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let ctx = h.ctx();
        block_on(replace_checkout_operation(&ctx, created.op(), None)).expect("supersede");
        let open = block_on(h.store.open_checkout_operation("u1")).expect("open read");
        assert_eq!(open, None, "supersede releases the flight");
        // Late create under the retired id: Stripe replays the create (same
        // key), but the mark misses the retired row.
        let err = block_on(create_session_under(
            &ctx,
            "op_test_1",
            "cus_test_1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
        ));
        assert_eq!(err, Err(EnsureCheckoutError::Failed));
        let op = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(op.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(
            op.session_id, None,
            "late mark never lands on the retired row"
        );
    }

    #[test]
    fn billing_converged_loser_with_different_price_replaces() {
        // A racer wins the single flight between this attempt's open-flight
        // read and its insert, naming a different price (the TOCTOU behind
        // the converged-session price check). The loser must not inherit
        // the winner's session: expire + supersede, then mint under its own
        // fresh operation — reusing its already-minted customer instead of
        // minting a second orphan.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        *h.stripe.next_customer.borrow_mut() = Some("cus_loser_1".to_string());
        h.stripe.sessions.borrow_mut().insert(
            "cs_race_1".to_string(),
            test_session(
                "cs_race_1",
                "open",
                "unpaid",
                Some("cus_winner_1"),
                None,
                Some("op_race_winner"),
            ),
        );
        *h.store.plant_winner_on_next_create.borrow_mut() =
            Some(crate::billing_reconcile::StoredCheckoutOp {
                id: "op_race_winner".to_string(),
                user_id: "u1".to_string(),
                state: "pending".to_string(),
                price_id: Some(PRICE_PRO.to_string()),
                session_id: Some("cs_race_1".to_string()),
                subscription_id: None,
            });
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("mismatched loser replaces");
        assert!(url.ends_with("cs_test_1"), "{url}");
        assert_eq!(
            h.stripe.expired.borrow().as_slice(),
            ["cs_race_1".to_string()].as_slice(),
            "the winner's open session is expired before replacement"
        );
        let winner = block_on(h.store.checkout_operation_by_id("op_race_winner"))
            .expect("op lookup")
            .expect("winner stored");
        assert_eq!(winner.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(winner.price_id.as_deref(), Some(PRICE_PRO));
        assert_eq!(winner.session_id.as_deref(), Some("cs_race_1"));
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(minted.len(), 1);
        assert_ne!(
            minted[0], "op_race_winner",
            "the loser mints under its own fresh operation id"
        );
        // One customer create: the retry reuses the loser's orphan once the
        // winner's flight is retired and no binding exists yet.
        assert_eq!(
            h.stripe.customers_created.borrow().as_slice(),
            [(Some("u1".to_string()), None)].as_slice()
        );
        assert_eq!(
            h.stripe.session_customers.borrow().as_slice(),
            ["cus_loser_1".to_string()].as_slice()
        );
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("replacement flight open");
        assert_eq!(open.id, minted[0]);
        assert_eq!(open.price_id.as_deref(), Some(PRICE_PLUS));
        assert_eq!(open.session_id.as_deref(), Some("cs_test_1"));
    }

    #[test]
    fn billing_expired_session_repair_mints_under_fresh_key() {
        // The recorded session is confirmed dead (expired without payment):
        // no expiry call is needed — Stripe already converged the state —
        // the dead operation is superseded and the repair mints under a
        // fresh operation id, so the repair can never replay the dead
        // session inside Stripe's retention window. Same price: the replace
        // is about the dead session, not a plan switch.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_old_1"),
        ))
        .expect("mark submitted");
        let mut dead = test_session(
            "cs_old_1",
            "expired",
            "unpaid",
            Some("cus_test_1"),
            None,
            Some("op_test_1"),
        );
        dead.url = None;
        h.stripe
            .sessions
            .borrow_mut()
            .insert("cs_old_1".to_string(), dead);
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("expired repair mints");
        assert!(url.ends_with("cs_test_1"), "{url}");
        assert!(
            h.stripe.expired.borrow().is_empty(),
            "a confirmed-dead session needs no expiry call"
        );
        let old = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(old.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(old.session_id.as_deref(), Some("cs_old_1"));
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(minted.len(), 1);
        assert_ne!(
            minted[0], "op_test_1",
            "the repair mints under a fresh operation id"
        );
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("replacement flight open");
        assert_eq!(open.id, minted[0]);
        assert_eq!(open.session_id.as_deref(), Some("cs_test_1"));
    }

    #[test]
    fn billing_missing_session_repair_mints_under_fresh_key() {
        // The recorded session is gone from Stripe (404, past retention or
        // never minted): the same deliberate replacement as a
        // confirmed-expired session — supersede, then mint fresh.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_gone_1"),
        ))
        .expect("mark submitted");
        h.stripe
            .missing_sessions
            .borrow_mut()
            .insert("cs_gone_1".to_string());
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("missing-session repair mints");
        assert!(url.ends_with("cs_test_1"), "{url}");
        assert!(
            h.stripe.expired.borrow().is_empty(),
            "a missing session needs no expiry call"
        );
        let old = block_on(h.store.checkout_operation_by_id("op_test_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(old.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(minted.len(), 1);
        assert_ne!(
            minted[0], "op_test_1",
            "the repair mints under a fresh operation id"
        );
    }

    #[test]
    fn billing_session_retrieve_outage_fails_without_replacement() {
        // An unconfirmed retrieve (any non-404 Stripe failure) fails
        // retryable: no replacement mints beside a session whose state is
        // unknown, and the flight stays open for the retry.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_test_1"),
        ))
        .expect("mark submitted");
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "open",
                "unpaid",
                Some("cus_test_1"),
                None,
                Some("op_test_1"),
            ),
        );
        h.stripe
            .fail
            .borrow_mut()
            .insert("retrieve_checkout_session".to_string());
        let ctx = h.ctx();
        assert_eq!(
            block_on(ensure_checkout_session(
                &ctx,
                "u1",
                PRICE_PLUS,
                "https://example.com/ok",
                "https://example.com/cancel",
                None,
            )),
            Err(EnsureCheckoutError::Failed)
        );
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no replacement beside unknown state"
        );
        assert!(h.stripe.expired.borrow().is_empty());
        assert!(h.stripe.session_create_ops.borrow().is_empty());
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("flight stays open for the retry");
        assert_eq!(open.id, "op_test_1");
        assert_eq!(open.session_id.as_deref(), Some("cs_test_1"));
        assert!(live_for(&h, "u1").is_none());
    }

    #[test]
    fn billing_complete_unpaid_session_retains_flight_without_minting() {
        // A `complete`/`unpaid` session is still processing asynchronously
        // (the create request does not restrict payment methods): a retry
        // must not supersede it and mint a second subscription checkout
        // while the first can still succeed. The flight stays open for the
        // real completion and the attempt fails retryable — no supersede,
        // no mint.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_unpaid_1"),
        ))
        .expect("mark submitted");
        h.stripe.sessions.borrow_mut().insert(
            "cs_unpaid_1".to_string(),
            test_session(
                "cs_unpaid_1",
                "complete",
                "unpaid",
                Some("cus_test_1"),
                None,
                Some("op_test_1"),
            ),
        );
        let ctx = h.ctx();
        assert_eq!(
            block_on(ensure_checkout_session(
                &ctx,
                "u1",
                PRICE_PLUS,
                "https://example.com/ok",
                "https://example.com/cancel",
                None,
            )),
            Err(EnsureCheckoutError::Failed)
        );
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no second subscription checkout beside a completable session"
        );
        assert!(h.stripe.expired.borrow().is_empty());
        assert!(h.stripe.session_create_ops.borrow().is_empty());
        let flight = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("flight stays open for the real completion");
        assert_eq!(flight.id, "op_test_1");
        assert_eq!(flight.session_id.as_deref(), Some("cs_unpaid_1"));
        assert!(live_for(&h, "u1").is_none());
    }

    #[test]
    fn billing_complete_no_payment_required_session_retains_flight_without_minting() {
        // A `complete`/`no_payment_required` session already created its
        // subscription: same deliberate-retention as `unpaid` — the flight
        // stays open (the webhook reconciles the subscription out of the
        // slot) and no second checkout mints.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        block_on(h.store.update_checkout_operation(
            "op_test_1",
            "submitted",
            PRICE_PLUS,
            Some("cs_npr_1"),
        ))
        .expect("mark submitted");
        h.stripe.sessions.borrow_mut().insert(
            "cs_npr_1".to_string(),
            test_session(
                "cs_npr_1",
                "complete",
                "no_payment_required",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        let ctx = h.ctx();
        assert_eq!(
            block_on(ensure_checkout_session(
                &ctx,
                "u1",
                PRICE_PLUS,
                "https://example.com/ok",
                "https://example.com/cancel",
                None,
            )),
            Err(EnsureCheckoutError::Failed)
        );
        assert_eq!(
            *h.stripe.sessions_created.borrow(),
            0,
            "no second checkout beside the created subscription"
        );
        assert!(h.stripe.expired.borrow().is_empty());
        assert!(h.stripe.session_create_ops.borrow().is_empty());
        let flight = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("flight stays open for the webhook completion");
        assert_eq!(flight.id, "op_test_1");
        assert_eq!(flight.session_id.as_deref(), Some("cs_npr_1"));
    }

    #[test]
    fn billing_orphan_with_live_session_and_price_mismatch_expires_before_replacing() {
        // A missing `session_id` does not mean Stripe has no session: the
        // create mark is best-effort, and until the replay the same-key
        // path was the only recovery. Here the orphan's PLUS session is
        // still live in Stripe while the retry names PRO: the old flight
        // must recover under its original key and price, expire the
        // payable session, and only then supersede and mint fresh — never
        // leave the first attempt's session payable beside the PRO one.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        block_on(h.store.create_checkout_operation(
            "op_old_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        // The mark was lost, but the session is live under the old key.
        h.stripe.sessions.borrow_mut().insert(
            "cs_live_1".to_string(),
            test_session(
                "cs_live_1",
                "open",
                "unpaid",
                Some("cus_test_1"),
                None,
                Some("op_old_1"),
            ),
        );
        h.stripe
            .sessions_by_op
            .borrow_mut()
            .insert("op_old_1".to_string(), "cs_live_1".to_string());
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PRO,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("mismatched orphan resolves then replaces");
        // The replay recovered (not re-minted): exactly one mint, the
        // replacement under its fresh operation id.
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(minted.len(), 1);
        assert_ne!(
            minted[0], "op_old_1",
            "the replacement mints under a fresh operation id"
        );
        assert!(url.ends_with("cs_test_1"), "{url}");
        assert_eq!(
            h.stripe.expired.borrow().as_slice(),
            ["cs_live_1".to_string()].as_slice(),
            "the recovered payable session expires before its replacement mints"
        );
        let dead = &h.stripe.sessions.borrow()["cs_live_1"];
        assert_eq!(dead.status.as_deref(), Some("expired"));
        let old = block_on(h.store.checkout_operation_by_id("op_old_1"))
            .expect("op lookup")
            .expect("op stored");
        assert_eq!(old.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(old.price_id.as_deref(), Some(PRICE_PLUS));
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("replacement flight open");
        assert_eq!(open.id, minted[0]);
        assert_eq!(open.price_id.as_deref(), Some(PRICE_PRO));
        assert_eq!(open.session_id.as_deref(), Some("cs_test_1"));
    }

    #[test]
    fn billing_orphan_mismatch_with_no_session_stillborn_expires_and_replaces() {
        // The mismatched orphan never created anything: the same-key
        // replay first-creates exactly one session under the old key, which
        // is immediately expired (never payable) before the supersede and
        // the fresh mint. No payable session lingers on either key.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        block_on(h.store.create_checkout_operation(
            "op_old_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PRO,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("mismatched orphan resolves then replaces");
        assert_eq!(*h.stripe.sessions_created.borrow(), 2);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(
            minted.as_slice(),
            ["op_old_1".to_string(), minted[1].clone()].as_slice(),
            "the replay recovers under the old key, the replacement under a fresh one"
        );
        assert_ne!(minted[1], "op_old_1");
        assert!(url.ends_with("cs_test_2"), "{url}");
        assert_eq!(
            h.stripe.expired.borrow().as_slice(),
            ["cs_test_1".to_string()].as_slice(),
            "the stillborn session expires before the replacement mints"
        );
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("replacement flight open");
        assert_eq!(open.price_id.as_deref(), Some(PRICE_PRO));
        assert_eq!(open.session_id.as_deref(), Some("cs_test_2"));
    }

    #[test]
    fn billing_converged_sessionless_winner_with_price_mismatch_resolves_before_replacing() {
        // A single-flight winner that has not recorded a session yet
        // (still creating, or its mark was lost) while the loser names a
        // different price: the loser must recover under the WINNER's key
        // and price before retiring it — superseding blind would orphan a
        // payable session the winner is about to mark.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_customers
            .borrow_mut()
            .insert("u1".to_string(), "cus_test_1".to_string());
        *h.store.plant_winner_on_next_create.borrow_mut() =
            Some(crate::billing_reconcile::StoredCheckoutOp {
                id: "op_race_winner".to_string(),
                user_id: "u1".to_string(),
                state: "pending".to_string(),
                price_id: Some(PRICE_PRO.to_string()),
                session_id: None,
                subscription_id: None,
            });
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("mismatched loser resolves the winner then replaces");
        // One replay under the winner's key (expired), one fresh mint under
        // the loser's own operation.
        assert_eq!(*h.stripe.sessions_created.borrow(), 2);
        let minted = h.stripe.session_create_ops.borrow();
        assert_eq!(minted.len(), 2);
        assert_eq!(
            minted[0], "op_race_winner",
            "the recovery replays under the winner's key, never a fresh one"
        );
        assert_ne!(
            minted[1], "op_race_winner",
            "the loser mints under its own fresh operation id"
        );
        assert!(url.ends_with("cs_test_2"), "{url}");
        assert_eq!(
            h.stripe.expired.borrow().as_slice(),
            ["cs_test_1".to_string()].as_slice(),
            "the winner's recovered session expires before the replacement mints"
        );
        assert!(
            h.stripe.customers_created.borrow().is_empty(),
            "no orphan customer: the account was already bound"
        );
        let winner = block_on(h.store.checkout_operation_by_id("op_race_winner"))
            .expect("op lookup")
            .expect("winner stored");
        assert_eq!(winner.state, crate::billing_reconcile::SUPERSEDED_OP_STATE);
        assert_eq!(winner.price_id.as_deref(), Some(PRICE_PRO));
        assert_eq!(
            winner.session_id.as_deref(),
            Some("cs_test_1"),
            "the recovery repairs the winner's mark before retiring it"
        );
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("replacement flight open");
        assert_eq!(open.id, minted[1]);
        assert_eq!(open.price_id.as_deref(), Some(PRICE_PLUS));
        assert_eq!(open.session_id.as_deref(), Some("cs_test_2"));
    }

    #[test]
    fn billing_earlier_plan_invoices_keep_history_rows() {
        // The subscription now sits on PRO after a plan change; the fold
        // still writes one history row per confirmed paid invoice — the
        // earlier PLUS invoice keeps its own plan label — while coverage
        // follows the current item's plan. Redelivery stays idempotent on
        // the invoice id.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_plus_1".to_string(),
            test_invoice(
                "in_plus_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_pro_1".to_string(),
            test_invoice(
                "in_pro_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PRO,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 2 * BILLING_PERIOD,
                BILLING_NOW + BILLING_PERIOD + 60,
            ),
        );
        let outcome = block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1"))
            .expect("reconciles");
        assert!(outcome.fail_closed.is_none());
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.plan_id, PLAN_PRO);
        assert_eq!(
            live.paid_through_unix,
            Some(BILLING_NOW + 2 * BILLING_PERIOD)
        );
        let history = h.store.invoice_history.borrow();
        assert_eq!(history.len(), 2);
        assert_eq!(history["in_plus_1"].plan_id, PLAN_PLUS);
        assert_eq!(history["in_pro_1"].plan_id, PLAN_PRO);
        assert_eq!(
            history["in_plus_1"].period_end_unix,
            BILLING_NOW + BILLING_PERIOD
        );
        assert_eq!(
            history["in_pro_1"].period_end_unix,
            BILLING_NOW + 2 * BILLING_PERIOD
        );
        drop(history);
        // Redelivery of the same remote state: same two rows, no dupes.
        block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1")).expect("reconciles");
        assert_eq!(
            h.store.invoice_history.borrow().len(),
            2,
            "history stays idempotent on the invoice id"
        );
    }

    #[test]
    fn billing_webhook_before_response_keeps_one_subscription() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        // The purchase attempt mints the session; the completion webhook
        // lands before the caller retries (lost response).
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ))
        .expect("attempt");
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        let created_id = url
            .rsplit('/')
            .next()
            .expect("session url ends with the id");
        let open = block_on(h.store.open_checkout_operation("u1"))
            .expect("open lookup")
            .expect("flight open");
        // The user completes `created_id`; the webhook resolves it.
        h.stripe.sessions.borrow_mut().insert(
            created_id.to_string(),
            test_session(
                created_id,
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some(&open.id),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        h.stripe.invoices.borrow_mut().insert(
            "in_test_1".to_string(),
            test_invoice(
                "in_test_1",
                "cus_test_1",
                "sub_test_1",
                PRICE_PLUS,
                BILLING_NOW,
                BILLING_NOW + BILLING_PERIOD,
                BILLING_NOW + 60,
            ),
        );
        let body = checkout_completed_body("evt_test_1", created_id, false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        // The retried purchase refuses instead of minting a second
        // subscription; the stored state is the webhook's single advance.
        let err = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ));
        assert_eq!(err, Err(EnsureCheckoutError::Unavailable));
        assert_eq!(*h.stripe.sessions_created.borrow(), 1);
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
    }

    #[test]
    fn billing_ensure_passes_through_test_clock_and_refuses_unmapped() {
        // Fresh account with a staging test clock: the customer create
        // carries the clock, and the session URL is returned.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        *h.stripe.next_customer.borrow_mut() = Some("cus_clock_1".to_string());
        let ctx = h.ctx();
        let url = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            PRICE_PLUS,
            "https://example.com/ok",
            "https://example.com/cancel",
            Some("test_clock_1"),
        ))
        .expect("clocked attempt");
        assert!(url.starts_with("https://checkout.stripe.com/pay/"));
        assert_eq!(
            h.store.billing_customers.borrow().get("u1"),
            Some(&"cus_clock_1".to_string())
        );
        // The clock rode the customer create, never anywhere else.
        assert_eq!(
            h.stripe.customers_created.borrow().as_slice(),
            &[(Some("u1".to_string()), Some("test_clock_1".to_string()))]
        );
        // Unmapped price: alert, no Stripe call, no customer minted.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let ctx = h.ctx();
        let err = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            "price_unknown",
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ));
        assert_eq!(err, Err(EnsureCheckoutError::Unavailable));
        assert_eq!(audits_with(&h, BILLING_PURCHASE_UNAVAILABLE_ACTION), 1);
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);
        assert!(h.store.billing_customers.borrow().get("u1").is_none());
        // Unselectable (non-purchase) mapping: same refusal.
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        h.store
            .billing_bindings
            .borrow_mut()
            .push(crate::billing_reconcile::PriceBinding {
                price_id: "price_legacy".to_string(),
                plan_id: PLAN_PLUS.to_string(),
                selectable: false,
                livemode: false,
            });
        let ctx = h.ctx();
        let err = block_on(ensure_checkout_session(
            &ctx,
            "u1",
            "price_legacy",
            "https://example.com/ok",
            "https://example.com/cancel",
            None,
        ));
        assert_eq!(err, Err(EnsureCheckoutError::Unavailable));
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);
    }

    #[test]
    fn billing_invoice_line_pagination_reads_every_page() {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        // First page carries no lines but `has_more`; the remainder
        // arrives through the lines listing, and coverage still advances.
        let mut first = test_invoice(
            "in_test_1",
            "cus_test_1",
            "sub_test_1",
            PRICE_PLUS,
            BILLING_NOW,
            BILLING_NOW + BILLING_PERIOD,
            BILLING_NOW + 60,
        );
        first.lines = Some(crate::stripe::InvoiceLines {
            data: Vec::new(),
            has_more: true,
        });
        h.stripe
            .invoices
            .borrow_mut()
            .insert("in_test_1".to_string(), first);
        h.stripe.line_pages.borrow_mut().insert(
            "in_test_1".to_string(),
            vec![crate::stripe::InvoiceLine {
                id: "in_test_1_line_1".to_string(),
                amount: 1_000,
                currency: Some("usd".to_string()),
                price: Some(PRICE_PLUS.to_string()),
                period: Some(crate::stripe::InvoiceLinePeriod {
                    start: Some(BILLING_NOW),
                    end: Some(BILLING_NOW + BILLING_PERIOD),
                }),
            }],
        );
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_1".to_string(),
            test_session(
                "cs_test_1",
                "complete",
                "paid",
                Some("cus_test_1"),
                Some("sub_test_1"),
                Some("op_test_1"),
            ),
        );
        h.stripe.subscriptions.borrow_mut().insert(
            "sub_test_1".to_string(),
            test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PLUS),
        );
        block_on(h.store.create_checkout_operation(
            "op_test_1",
            "u1",
            PRICE_PLUS,
            Some("cus_test_1"),
        ))
        .expect("op insert");
        let body = checkout_completed_body("evt_test_1", "cs_test_1", false);
        webhook_ok(&h.call_webhook(&body, Some(sign(&body, BILLING_NOW))));
        let live = live_for(&h, "u1").expect("live row stored");
        assert_eq!(live.paid_through_unix, Some(BILLING_NOW + BILLING_PERIOD));
        assert_eq!(h.store.invoice_history.borrow().len(), 1);
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

    const PLAY_SECRET: &[u8] = b"test-play-grant-secret-32-bytes-long!";

    /// Harness with the play-grant secret provisioned (the default
    /// `cfg()` leaves it unset — the fail-closed case below).
    fn play_harness() -> Harness {
        let mut h = harness();
        h.config = cfg().with_play_grant_secret(PLAY_SECRET.to_vec());
        h
    }

    /// Seed a live entitled subscription for `user_id` on `plan_id`
    /// plus its `plans`-row caps (hours converted like production).
    fn seed_entitled(h: &Harness, user_id: &str, plan_id: &str, hours: u64) {
        h.store.playback_subscriptions.borrow_mut().insert(
            user_id.to_string(),
            crate::playback::SubscriptionRow {
                plan_id: plan_id.to_string(),
                status: "active".to_string(),
                cancel_at_period_end: false,
                paid_through_at_unix: Some(i64::try_from(NOW).unwrap() + 3600),
                grace_until_unix: None,
                current_period_end_unix: Some(i64::try_from(NOW).unwrap() + 3600),
            },
        );
        h.store.plan_caps_rows.borrow_mut().insert(
            plan_id.to_string(),
            crate::plan_limits::caps_from_plan_row(hours, 1),
        );
    }

    fn play_call(h: &Harness, cookie: &str, body: serde_json::Value) -> Response {
        h.call_full(
            "POST",
            "/v1/plays",
            Some(body.to_string()),
            Some(cookie.to_string()),
            Some(ORIGIN),
        )
    }

    fn play_user(h: &Harness) -> String {
        seed_user(h, "user-plays-1", "plays@example.com");
        magic_login(h, "plays@example.com")
    }

    fn shared_play_call(
        h: &Harness,
        user_id: &str,
        session_id: &str,
        track_id: &str,
        play_id: Option<String>,
    ) -> Result<IssuedPlay, Response> {
        block_on(issue_play_for_user(
            &h.ctx(),
            user_id,
            Some(PlayIssueBody {
                track_id: track_id.to_string(),
                session_id: session_id.to_string(),
                play_id,
                takeover: false,
            }),
        ))
    }

    #[test]
    fn shared_play_core_returns_typed_grant_without_creating_account_sessions() {
        let h = play_harness();
        seed_user(&h, "user-plays-1", "plays@example.com");
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let issued = shared_play_call(&h, "user-plays-1", "compat-holder", "track-9", None)
            .expect("authenticated identity can use shared gates without a cookie");
        let expected = crate::play_grant::issue_v2_at(
            "user-plays-1",
            "track-9",
            &issued.play_id,
            issued.lease_epoch,
            NOW,
            crate::play_grant::MAX_GRANT_TTL,
            Some(PLAY_SECRET),
        )
        .unwrap();
        assert_eq!(issued.grant, expected);
        assert_eq!(issued.lease_epoch, 1);
        assert_eq!(issued.lease_expires_at, NOW + 45);
        let native_wire = issued.into_response();
        assert_eq!(native_wire.status, 200);
        assert!(native_wire.set_cookie.is_none());
        assert!(h.store.sessions.borrow().is_empty());
    }

    #[test]
    fn shared_play_core_and_native_route_contend_for_one_user_lease() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let first = shared_play_call(&h, "user-plays-1", "compat-holder", "track-9", None)
            .expect("first holder");
        let refused = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "native-tab"}),
        );
        assert_eq!(refused.status, 429);
        assert_eq!(refused.retry_after, Some(45));
        let takeover = play_call(
            &h,
            &cookie,
            serde_json::json!({
                "track_id": "track-10", "session_id": "native-tab", "takeover": true,
            }),
        );
        assert_eq!(takeover.status, 200);
        let refused = shared_play_call(
            &h,
            "user-plays-1",
            "compat-holder",
            "track-9",
            Some(first.play_id),
        )
        .map(|_| ())
        .expect_err("old compatibility play cannot renew after native takeover");
        assert_eq!(refused.status, 429);
        let current = h.store.playback_leases.borrow()["user-plays-1"].clone();
        assert_eq!(current.session_id, "native-tab");
        assert_eq!(current.epoch, first.lease_epoch + 1);
    }

    #[test]
    fn shared_play_core_enforces_subscription_and_cap_but_preserves_continuation() {
        let h = play_harness();
        seed_user(&h, "user-plays-1", "plays@example.com");
        let unsubscribed = shared_play_call(&h, "user-plays-1", "compat-holder", "track-9", None)
            .map(|_| ())
            .expect_err("unsubscribed");
        assert_eq!(unsubscribed.status, 402);
        assert!(h.store.playback_leases.borrow().is_empty());
        seed_entitled(&h, "user-plays-1", "plus", 1);
        let first = shared_play_call(&h, "user-plays-1", "compat-holder", "track-9", None)
            .expect("entitled first start");
        h.store.stream_seconds.borrow_mut().insert(
            (
                "user-plays-1".to_string(),
                crate::ledger::period_month_for_timestamp(NOW),
            ),
            3600,
        );
        let continued = shared_play_call(
            &h,
            "user-plays-1",
            "compat-holder",
            "track-9",
            Some(first.play_id.clone()),
        )
        .expect("current track finishes across the cap");
        assert_eq!(continued.play_id, first.play_id);
        assert_eq!(continued.lease_epoch, first.lease_epoch);
        let capped = shared_play_call(&h, "user-plays-1", "compat-holder", "track-10", None)
            .map(|_| ())
            .expect_err("next start at cap");
        assert_eq!(capped.status, 402);
        assert_eq!(h.store.playback_leases.borrow()["user-plays-1"].epoch, 1);
    }

    #[test]
    fn shared_play_core_preserves_pre_write_validation_and_configuration_gates() {
        let h = play_harness();
        seed_user(&h, "user-plays-1", "plays@example.com");
        seed_entitled(&h, "user-plays-1", "plus", 100);
        for (session_id, track_id) in [("", "track-9"), ("holder", "not/meterable")] {
            let refused = shared_play_call(&h, "user-plays-1", session_id, track_id, None)
                .map(|_| ())
                .expect_err("invalid lease input");
            assert_eq!(refused.status, 422);
        }
        let malformed = block_on(issue_play_for_user(&h.ctx(), "user-plays-1", None))
            .map(|_| ())
            .expect_err("missing body");
        assert_eq!(malformed.status, 422);
        let mut unconfigured = play_harness();
        unconfigured.config.play_grant_secret = None;
        let refused = shared_play_call(&unconfigured, "user-plays-1", "holder", "track-9", None)
            .map(|_| ())
            .expect_err("missing signing secret");
        assert_eq!(refused.status, 500);
        h.store
            .deleting_users
            .borrow_mut()
            .insert("user-plays-1".to_string());
        let deleting = block_on(issue_play_for_user(&h.ctx(), "user-plays-1", None))
            .map(|_| ())
            .expect_err("deletion refusal precedes malformed body");
        assert_eq!(deleting.status, 409);
        assert!(h.store.playback_leases.borrow().is_empty());
        assert!(unconfigured.store.playback_leases.borrow().is_empty());
    }

    #[test]
    fn play_issue_mints_a_grant_for_an_entitled_user() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let resp = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let play_id = body["play_id"].as_str().expect("server-minted play id");
        assert!(!play_id.is_empty());
        // The handler bound exactly (user, track, play, epoch 1) at NOW:
        // re-minting through the vector-pinned minter reproduces it.
        let expected = crate::play_grant::issue_v2_at(
            "user-plays-1",
            "track-9",
            play_id,
            1,
            NOW,
            crate::play_grant::MAX_GRANT_TTL,
            Some(PLAY_SECRET),
        )
        .expect("minter reproduces the handler grant");
        assert_eq!(body["grant"], expected.token);
        assert_eq!(body["lease_epoch"], 1);
        assert_eq!(body["expires_at"], NOW + 900);
        assert_eq!(body["lease_expires_at"], NOW + 45);
        assert_eq!(body["heartbeat_interval_secs"], 15);
        let lease = h.store.playback_leases.borrow()["user-plays-1"].clone();
        assert_eq!(lease.session_id, "tab-a");
        assert_eq!(lease.track_id, "track-9");
        assert_eq!(lease.play_id, play_id);
    }

    #[test]
    fn play_issue_refuses_unsubscribed_with_reasons_and_no_lease() {
        let h = play_harness();
        let cookie = play_user(&h);
        let resp = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 402, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "subscription_required");
        assert_eq!(body["reason"], "never_subscribed");
        assert_eq!(body["retryable"], false);
        assert_eq!(body["upgrade_url"], "/pricing");
        // Once-paid history flips only the reason, never the refusal.
        h.store
            .ever_paid_users
            .borrow_mut()
            .insert("user-plays-1".to_string());
        let resp = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 402);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["reason"], "lapsed");
        assert!(
            h.store.playback_leases.borrow().is_empty(),
            "a refused start must not burn a lease epoch"
        );
    }

    #[test]
    fn play_issue_cap_refuses_new_starts_but_continuation_finishes() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 1);
        let period = crate::ledger::period_month_for_timestamp(NOW);
        let started = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(started.status, 200, "{}", started.body);
        let started: serde_json::Value = serde_json::from_str(&started.body).unwrap();
        let play_id = started["play_id"].as_str().unwrap().to_string();
        // The play crosses the cap mid-track (3600 s = the 1 h cap).
        h.store
            .stream_seconds
            .borrow_mut()
            .insert(("user-plays-1".to_string(), period), 3600);
        // Continuation (same session + play + track) still mints, and
        // reuses the same play at the same epoch.
        let resumed = play_call(
            &h,
            &cookie,
            serde_json::json!({
                "track_id": "track-9",
                "session_id": "tab-a",
                "play_id": play_id,
            }),
        );
        assert_eq!(resumed.status, 200, "{}", resumed.body);
        let resumed: serde_json::Value = serde_json::from_str(&resumed.body).unwrap();
        assert_eq!(resumed["play_id"], play_id);
        assert_eq!(resumed["lease_epoch"], 1);
        // The NEXT new start (queue advance / repeat-one shape) is the
        // one refused, with the plan-limit headers.
        let next = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-10", "session_id": "tab-a"}),
        );
        assert_eq!(next.status, 402, "{}", next.body);
        let body: serde_json::Value = serde_json::from_str(&next.body).unwrap();
        assert_eq!(body["code"], "plan_limit_exceeded");
        assert!(next.extra_headers.contains(&(
            "x-plan-limit".to_string(),
            "stream_hours_month=1".to_string()
        )));
        assert!(next
            .extra_headers
            .contains(&("x-plan-reset".to_string(), "next-utc-month".to_string())));
    }

    #[test]
    fn play_issue_lease_429_same_session_replace_and_takeover() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let first = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(first.status, 200, "{}", first.body);
        let first: serde_json::Value = serde_json::from_str(&first.body).unwrap();
        let first_play = first["play_id"].as_str().unwrap().to_string();
        // A second client session is refused 429 with the live TTL, and
        // `retry-after` appears exactly once (the rendered field), never
        // duplicated inside extra_headers.
        let held = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-b"}),
        );
        assert_eq!(held.status, 429, "{}", held.body);
        assert_eq!(held.retry_after, Some(45));
        assert!(
            !held
                .extra_headers
                .iter()
                .any(|(name, _)| name == "retry-after"),
            "retry-after must not be duplicated: {:?}",
            held.extra_headers
        );
        assert!(held.extra_headers.contains(&(
            "x-plan-limit".to_string(),
            "concurrent_streams=1".to_string()
        )));
        let body: serde_json::Value = serde_json::from_str(&held.body).unwrap();
        assert_eq!(body["code"], "rate_limited");
        // The holding session itself is never self-refused: a new track
        // replaces the play and bumps the epoch.
        let replaced = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-10", "session_id": "tab-a"}),
        );
        assert_eq!(replaced.status, 200, "{}", replaced.body);
        let replaced: serde_json::Value = serde_json::from_str(&replaced.body).unwrap();
        assert_eq!(replaced["lease_epoch"], 2);
        assert_ne!(replaced["play_id"], first_play);
        // Explicit takeover: epoch bumps again; the displaced holder's
        // heartbeat now reports superseded (it self-pauses), the new
        // holder's reports active and extends.
        let taken = play_call(
            &h,
            &cookie,
            serde_json::json!({
                "track_id": "track-11",
                "session_id": "tab-b",
                "takeover": true,
            }),
        );
        assert_eq!(taken.status, 200, "{}", taken.body);
        let taken: serde_json::Value = serde_json::from_str(&taken.body).unwrap();
        assert_eq!(taken["lease_epoch"], 3);
        let beat = |session: &str, play: &serde_json::Value, epoch: u64| {
            h.call_full(
                "POST",
                "/v1/plays/heartbeat",
                Some(
                    serde_json::json!({
                        "session_id": session,
                        "play_id": play,
                        "lease_epoch": epoch,
                    })
                    .to_string(),
                ),
                Some(cookie.clone()),
                Some(ORIGIN),
            )
        };
        let displaced = beat("tab-a", &replaced["play_id"], 2);
        assert_eq!(displaced.status, 200, "{}", displaced.body);
        let displaced: serde_json::Value = serde_json::from_str(&displaced.body).unwrap();
        assert_eq!(displaced["lease"], "superseded");
        let holder = beat("tab-b", &taken["play_id"], 3);
        assert_eq!(holder.status, 200, "{}", holder.body);
        let holder: serde_json::Value = serde_json::from_str(&holder.body).unwrap();
        assert_eq!(holder["lease"], "active");
        assert_eq!(holder["lease_expires_at"], NOW + 45);
    }

    #[test]
    fn play_heartbeat_without_a_lease_reports_expired() {
        let h = play_harness();
        let cookie = play_user(&h);
        let resp = h.call_full(
            "POST",
            "/v1/plays/heartbeat",
            Some(
                serde_json::json!({
                    "session_id": "tab-a",
                    "play_id": "play-none",
                    "lease_epoch": 1,
                })
                .to_string(),
            ),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["lease"], "expired");
    }

    #[test]
    fn play_routes_answer_account_deleting_for_a_deleting_account() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        h.store
            .deleting_users
            .borrow_mut()
            .insert("user-plays-1".to_string());
        // Issuance: the typed 409, no lease written, no grant minted.
        let resp = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 409, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "account_deleting");
        assert!(
            h.store.playback_leases.borrow().is_empty(),
            "a deleting account must not acquire a lease"
        );
        // Heartbeat: same 409, even for a lease row seeded directly —
        // the touch must never extend a deleting account's lease.
        h.store.playback_leases.borrow_mut().insert(
            "user-plays-1".to_string(),
            crate::playback::LeaseRow {
                session_id: "tab-a".to_string(),
                track_id: "track-9".to_string(),
                play_id: "play-seeded".to_string(),
                epoch: 7,
                expires_at: NOW + 10,
            },
        );
        let resp = h.call_full(
            "POST",
            "/v1/plays/heartbeat",
            Some(
                serde_json::json!({
                    "session_id": "tab-a",
                    "play_id": "play-seeded",
                    "lease_epoch": 7,
                })
                .to_string(),
            ),
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 409, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "account_deleting");
        assert_eq!(
            h.store.playback_leases.borrow()["user-plays-1"].expires_at,
            NOW + 10,
            "a deleting account's lease must not extend"
        );
        // The FakeStore mirrors the statement fences directly, so a
        // handler regression that skipped the pre-check still could
        // not write (acquire reads Held, touch reads false).
        let acquired = block_on(h.store.acquire_playback_lease(
            "user-plays-1",
            "tab-b",
            "track-9",
            "play-x",
            NOW,
            NOW + 45,
            true,
        ))
        .expect("store call succeeds");
        assert!(matches!(
            acquired,
            crate::playback::LeaseAcquire::Held { .. }
        ));
        let touched = block_on(h.store.touch_playback_lease(
            "user-plays-1",
            "tab-a",
            "play-seeded",
            7,
            NOW,
            NOW + 45,
        ))
        .expect("store call succeeds");
        assert!(!touched);
    }

    #[test]
    fn play_issue_answers_account_deleting_when_the_marker_lands_mid_request() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        // The marker lands after the up-front probe, inside the lease
        // write: the fenced acquire reads Held, and the post-write
        // re-read — not the concurrency 429 — classifies the refusal.
        *h.store.mark_deleting_before_next_lease_write.borrow_mut() =
            Some("user-plays-1".to_string());
        let resp = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 409, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "account_deleting");
        assert!(
            h.store.playback_leases.borrow().is_empty(),
            "the fenced acquire writes nothing"
        );
    }

    #[test]
    fn play_heartbeat_answers_account_deleting_when_the_marker_lands_mid_request() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        h.store.playback_leases.borrow_mut().insert(
            "user-plays-1".to_string(),
            crate::playback::LeaseRow {
                session_id: "tab-a".to_string(),
                track_id: "track-9".to_string(),
                play_id: "play-seeded".to_string(),
                epoch: 7,
                expires_at: NOW + 10,
            },
        );
        // Same race on the touch: zero rows after the fence closes
        // mid-request answers the typed 409, not `superseded`.
        *h.store.mark_deleting_before_next_lease_write.borrow_mut() =
            Some("user-plays-1".to_string());
        let resp = h.call_full(
            "POST",
            "/v1/plays/heartbeat",
            Some(
                serde_json::json!({
                    "session_id": "tab-a",
                    "play_id": "play-seeded",
                    "lease_epoch": 7,
                })
                .to_string(),
            ),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 409, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "account_deleting");
        assert_eq!(
            h.store.playback_leases.borrow()["user-plays-1"].expires_at,
            NOW + 10,
            "the fenced touch must not extend the lease"
        );
    }

    #[test]
    fn play_issue_fails_closed_without_a_provisioned_secret() {
        // Default cfg(): no play-grant secret at all.
        let h = harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let resp = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 500, "{}", resp.body);
        assert!(!resp.body.contains("v2."), "no token on a refused mint");
        assert!(
            h.store.playback_leases.borrow().is_empty(),
            "an unmintable grant must not acquire the lease"
        );
        // A too-short secret is the same fail-closed 500.
        let mut short = play_harness();
        short.config = cfg().with_play_grant_secret(b"short".to_vec());
        let cookie = play_user(&short);
        seed_entitled(&short, "user-plays-1", "plus", 100);
        let resp = play_call(
            &short,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
        );
        assert_eq!(resp.status, 500, "{}", resp.body);
    }

    #[test]
    fn play_heartbeat_fails_closed_without_a_provisioned_secret() {
        // Default cfg(): no play-grant secret at all. The heartbeat is
        // gated like issuance, so a misconfigured deployment cannot
        // keep extending a lease no grant could be minted against.
        let h = harness();
        let cookie = play_user(&h);
        h.store.playback_leases.borrow_mut().insert(
            "user-plays-1".to_string(),
            crate::playback::LeaseRow {
                session_id: "tab-a".to_string(),
                track_id: "track-9".to_string(),
                play_id: "play-seeded".to_string(),
                epoch: 7,
                expires_at: NOW + 10,
            },
        );
        let resp = h.call_full(
            "POST",
            "/v1/plays/heartbeat",
            Some(
                serde_json::json!({
                    "session_id": "tab-a",
                    "play_id": "play-seeded",
                    "lease_epoch": 7,
                })
                .to_string(),
            ),
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 500, "{}", resp.body);
        assert_eq!(
            h.store.playback_leases.borrow()["user-plays-1"].expires_at,
            NOW + 10,
            "a refused heartbeat must not extend the lease"
        );
    }

    #[test]
    fn play_routes_require_a_session_and_valid_identities() {
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let body = serde_json::json!({"track_id": "track-9", "session_id": "tab-a"});
        // No cookie: generic 401 on both routes.
        let anon = h.call("POST", "/v1/plays", body.clone());
        assert_eq!(anon.status, 401);
        let anon_beat = h.call(
            "POST",
            "/v1/plays/heartbeat",
            serde_json::json!({"session_id": "tab-a", "play_id": "p", "lease_epoch": 1}),
        );
        assert_eq!(anon_beat.status, 401);
        // Wrong origin: the mutation CSRF pin refuses before the handler.
        let cross = h.call_full(
            "POST",
            "/v1/plays",
            Some(body.to_string()),
            Some(cookie.clone()),
            Some("https://evil.example"),
        );
        assert_eq!(cross.status, 401);
        // An unmeterable track id is a validation refusal and writes nothing.
        let bad_track = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track.v1", "session_id": "tab-a"}),
        );
        assert_eq!(bad_track.status, 422, "{}", bad_track.body);
        // Unknown fields and an empty client session id are refused too.
        let unknown = play_call(
            &h,
            &cookie,
            serde_json::json!({
                "track_id": "track-9",
                "session_id": "tab-a",
                "epoch": 99,
            }),
        );
        assert_eq!(unknown.status, 422);
        let no_session = play_call(
            &h,
            &cookie,
            serde_json::json!({"track_id": "track-9", "session_id": ""}),
        );
        assert_eq!(no_session.status, 422);
        assert!(h.store.playback_leases.borrow().is_empty());
    }

    /// Real session/credential issuance through `handle`, with an entitled
    /// user and optionally an existing lease and metered window. No live I/O.
    fn play_bearer_harness(scope: &str, occupied: bool) -> (Harness, String, String) {
        let mut h = play_harness();
        h.config = h.config.with_native_rest_pepper(vec![7u8; 32], 1);
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let minted = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": [scope], "ttl_secs": 3600}),
        );
        assert_eq!(minted.status, 201, "credential issuance status");
        let body: serde_json::Value = serde_json::from_str(&minted.body).unwrap();
        let secret = mint_secret(&body);
        if occupied {
            let issued = play_call(
                &h,
                &cookie,
                serde_json::json!({"track_id": "track-9", "session_id": "existing-tab"}),
            );
            assert_eq!(issued.status, 200, "session grant positive control");
            let issued: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
            let window = crate::ledger::prepare_play_window(
                "user-plays-1",
                "track-9",
                issued["play_id"].as_str().unwrap(),
                100,
                Some(1000),
                Some(100_000),
                NOW,
            )
            .unwrap();
            block_on(h.store.record_play_window(&window)).unwrap();
        }
        (h, cookie, secret)
    }

    fn assert_play_bearer_refused(
        h: &Harness,
        secret: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        body: &[u8],
        status: u16,
    ) {
        let leases = h.store.playback_leases.borrow().clone();
        let windows = h.store.play_windows.borrow().clone();
        let seconds = h.store.stream_seconds.borrow().clone();
        let req = Request {
            method: "POST".to_string(),
            path: "/v1/plays".to_string(),
            cookie: cookie.map(str::to_string),
            origin: origin.map(str::to_string),
            authorization: Some(format!("Bearer {secret}")),
            stripe_signature: None,
            body: body.to_vec(),
        };
        let resp = block_on(handle(&h.ctx(), &req));
        assert_eq!(resp.status, status, "play-grant refusal status");
        let expected = match status {
            401 => generic_auth(),
            403 => scope_forbidden(),
            404 => not_found(),
            _ => panic!("unexpected refusal status"),
        };
        assert!(resp.body == expected.body, "exact names-only refusal body");
        assert!(resp.set_cookie.is_none());
        assert!(!resp.body.contains("\"grant\""));
        assert_eq!(*h.store.playback_leases.borrow(), leases);
        assert_eq!(*h.store.play_windows.borrow(), windows);
        assert_eq!(*h.store.stream_seconds.borrow(), seconds);
    }

    #[test]
    fn play_issue_in_scope_bearer_requires_origin_and_session() {
        for occupied in [false, true] {
            for (has_cookie, origin) in [
                (false, None),
                (false, Some(ORIGIN)),
                (true, None),
                (true, Some("https://wrong-origin.example")),
            ] {
                // Also pin ordering before configuration/body validation:
                // an unconfigured minter + malformed body must still be 401.
                for configured in [true, false] {
                    let (mut h, cookie, secret) = play_bearer_harness("plays:write", occupied);
                    if !configured {
                        h.config.play_grant_secret = None;
                    }
                    let valid_body = serde_json::json!({
                        "track_id": "track-9", "session_id": "new-tab", "takeover": true,
                    })
                    .to_string();
                    let body = if configured {
                        valid_body.as_bytes()
                    } else {
                        b"{"
                    };
                    assert_play_bearer_refused(
                        &h,
                        &secret,
                        has_cookie.then_some(cookie.as_str()),
                        origin,
                        body,
                        401,
                    );
                    let rows =
                        block_on(h.store.native_credentials_for_user("user-plays-1")).unwrap();
                    assert_eq!(rows.len(), 1);
                    assert_eq!(
                        rows[0].last_used_at,
                        Some(NOW),
                        "scope gate admitted bearer"
                    );
                }
            }
        }
    }

    #[test]
    fn play_issue_valid_wrong_scope_is_403_before_session_dispatch() {
        for occupied in [false, true] {
            for with_session in [false, true] {
                let (h, cookie, secret) = play_bearer_harness("library:read", occupied);
                assert_play_bearer_refused(
                    &h,
                    &secret,
                    with_session.then_some(cookie.as_str()),
                    with_session.then_some(ORIGIN),
                    b"{",
                    403,
                );
                let rows = block_on(h.store.native_credentials_for_user("user-plays-1")).unwrap();
                assert_eq!(rows[0].last_used_at, None, "wrong scope not admitted");
            }
        }
    }

    #[test]
    fn play_issue_invalid_bearers_are_plain_404_without_cookie_rescue() {
        for occupied in [false, true] {
            for with_session in [false, true] {
                for kind in ["malformed", "unknown", "revoked", "expired"] {
                    let (h, cookie, minted) = play_bearer_harness("plays:write", occupied);
                    let rows =
                        block_on(h.store.native_credentials_for_user("user-plays-1")).unwrap();
                    let secret = match kind {
                        "malformed" => "ntk_bad-shape".to_string(),
                        "unknown" => format!("ntk_{}", "0".repeat(43)),
                        "revoked" => {
                            block_on(h.store.revoke_native_credential(
                                &rows[0].record.id,
                                "user-plays-1",
                                NOW,
                            ))
                            .unwrap();
                            minted
                        }
                        "expired" => {
                            h.store
                                .native_credentials
                                .borrow_mut()
                                .get_mut(&rows[0].record.id)
                                .unwrap()
                                .record
                                .expires_at = NOW - 1;
                            minted
                        }
                        _ => unreachable!(),
                    };
                    assert_play_bearer_refused(
                        &h,
                        &secret,
                        with_session.then_some(cookie.as_str()),
                        with_session.then_some(ORIGIN),
                        br#"{"track_id":"track-9","session_id":"new-tab","takeover":true}"#,
                        404,
                    );
                }
            }
        }
    }

    #[test]
    fn play_issue_presented_invalid_bearer_still_plain_404() {
        // The bearer gate runs before dispatch on this path: a junk key
        // (even alongside a valid session cookie) gets the plain 404 —
        // the landed session route must not resurrect route disclosure
        // or cookie rescue for presented credentials.
        let h = play_harness();
        let cookie = play_user(&h);
        seed_entitled(&h, "user-plays-1", "plus", 100);
        let req = Request {
            method: "POST".to_string(),
            path: "/v1/plays".to_string(),
            cookie: Some(cookie),
            origin: Some(ORIGIN.to_string()),
            authorization: Some(
                "Bearer ntk_syntheticUnknownKey0000000000000000000000000".to_string(),
            ),
            stripe_signature: None,
            body: serde_json::json!({"track_id": "track-9", "session_id": "tab-a"})
                .to_string()
                .into_bytes(),
        };
        let resp = block_on(handle(&h.ctx(), &req));
        assert_eq!(resp.status, 404, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert!(h.store.playback_leases.borrow().is_empty());
    }

    #[test]
    fn play_paths_preflight_as_served_routes() {
        let h = play_harness();
        for path in ["/v1/plays", "/v1/plays/heartbeat"] {
            let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 204, "OPTIONS {path}");
            assert!(resp.body.is_empty());
        }
        assert!(is_known_path("/v1/plays"));
        assert!(is_known_path("/v1/plays/heartbeat"));
    }

    const SINK_SECRET: &[u8] = b"test-meter-sink-secret-32-bytes-long!!";
    const SINK_USER: &str = "u-meter";
    const SINK_TRACK: &str = "track-meter-1";

    /// Harness with the meter-sink secret provisioned and the sink
    /// user seeded (the writer fence refuses unknown users).
    fn sink_harness() -> Harness {
        let mut h = harness();
        h.config = cfg().with_meter_sink_secret(SINK_SECRET.to_vec());
        seed_user(&h, SINK_USER, "meter@example.com");
        h
    }

    fn sink_window(play_id: &str, union_bytes: u64) -> serde_json::Value {
        serde_json::json!({
            "user_id": SINK_USER,
            "track_id": SINK_TRACK,
            "play_id": play_id,
            "union_bytes": union_bytes,
            "file_bytes": 7_200_000u64,
            "started_at": NOW,
        })
    }

    /// POST a raw sink body with an optional precomputed header; `None`
    /// signs with the harness secret at NOW (the happy path).
    fn call_sink(h: &Harness, body: &str, authorization: Option<String>) -> Response {
        let authorization = authorization.or_else(|| {
            crate::meter_sink::authorization_header(Some(SINK_SECRET), NOW, body.as_bytes())
        });
        let req = Request {
            method: "POST".to_string(),
            path: crate::meter_sink::SINK_PATH.to_string(),
            cookie: None,
            origin: None,
            authorization,
            stripe_signature: None,
            body: body.as_bytes().to_vec(),
        };
        block_on(handle(&h.ctx(), &req))
    }

    fn ack_lists(resp: &Response) -> (Vec<String>, Vec<String>, Vec<String>) {
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let list = |key: &str| {
            body[key]
                .as_array()
                .expect("ack list")
                .iter()
                .map(|v| v.as_str().expect("play id").to_string())
                .collect()
        };
        (list("accepted"), list("fenced"), list("rejected"))
    }

    #[test]
    fn meter_sink_writes_windows_and_acks_dispositions() {
        let h = sink_harness();
        // 180 s track; a half-file union derives 90 s proportionally.
        h.store.meter_meta.borrow_mut().insert(
            (SINK_USER.to_string(), SINK_TRACK.to_string()),
            crate::meter_sink::TrackMeterMeta {
                track_duration_ms: Some(180_000),
                ..Default::default()
            },
        );
        let body = serde_json::json!({"windows": [sink_window("play-1", 3_600_000)]}).to_string();
        let resp = call_sink(&h, &body, None);
        assert_eq!(resp.status, 200, "{}", resp.body);
        let (accepted, fenced, rejected) = ack_lists(&resp);
        assert_eq!(accepted, vec!["play-1"]);
        assert!(fenced.is_empty() && rejected.is_empty());
        let stored = h.store.play_windows.borrow();
        let window = stored.values().next().expect("one stored window");
        assert_eq!(window.stream_seconds, 90);
        assert_eq!(window.bytes_served, 3_600_000);
        assert_eq!(window.file_bytes, Some(7_200_000));
        assert_eq!(window.duration_ms, Some(180_000));
        assert!(!window.rate_fallback);
        drop(stored);
        let period = crate::ledger::period_month_for_timestamp(NOW);
        assert_eq!(
            h.store.stream_seconds.borrow()[&(SINK_USER.to_string(), period)],
            90
        );
    }

    #[test]
    fn meter_sink_duplicate_and_stale_flushes_add_zero() {
        let h = sink_harness();
        h.store.meter_meta.borrow_mut().insert(
            (SINK_USER.to_string(), SINK_TRACK.to_string()),
            crate::meter_sink::TrackMeterMeta {
                track_duration_ms: Some(180_000),
                ..Default::default()
            },
        );
        let body = serde_json::json!({"windows": [sink_window("play-1", 3_600_000)]}).to_string();
        assert_eq!(call_sink(&h, &body, None).status, 200);
        // Exact redelivery: still accepted (converged), total unmoved.
        let resp = call_sink(&h, &body, None);
        let (accepted, fenced, rejected) = ack_lists(&resp);
        assert_eq!(accepted, vec!["play-1"]);
        assert!(fenced.is_empty() && rejected.is_empty());
        // A stale (smaller-union) flush likewise converges to zero.
        let stale = serde_json::json!({"windows": [sink_window("play-1", 1_800_000)]}).to_string();
        assert_eq!(call_sink(&h, &stale, None).status, 200);
        let period = crate::ledger::period_month_for_timestamp(NOW);
        assert_eq!(
            h.store.stream_seconds.borrow()[&(SINK_USER.to_string(), period)],
            90
        );
    }

    #[test]
    fn meter_sink_refuses_unsigned_and_forged_requests() {
        let h = sink_harness();
        let body = serde_json::json!({"windows": [sink_window("play-1", 3_600_000)]}).to_string();
        // Empty Authorization header (the unit tests pin absent too).
        let resp = call_sink(&h, &body, Some(String::new()));
        assert_eq!(resp.status, 401, "{}", resp.body);
        // Signed with the wrong key.
        let forged = crate::meter_sink::authorization_header(
            Some(b"forged-meter-sink-secret-32-bytes!!"),
            NOW,
            body.as_bytes(),
        )
        .expect("signs");
        assert_eq!(call_sink(&h, &body, Some(forged)).status, 401);
        // Correctly signed but stale (replay into another window).
        let stale = crate::meter_sink::authorization_header(
            Some(SINK_SECRET),
            NOW - crate::meter_sink::MAX_TIMESTAMP_SKEW_SECS - 1,
            body.as_bytes(),
        )
        .expect("signs");
        assert_eq!(call_sink(&h, &body, Some(stale)).status, 401);
        // Correctly signed header, tampered body.
        let good = crate::meter_sink::authorization_header(Some(SINK_SECRET), NOW, body.as_bytes())
            .expect("signs");
        let tampered = body.replace("3600000", "7200000");
        assert_eq!(call_sink(&h, &tampered, Some(good)).status, 401);
        assert!(
            h.store.play_windows.borrow().is_empty(),
            "refused requests write nothing"
        );
    }

    #[test]
    fn meter_sink_fails_closed_without_a_provisioned_secret() {
        let h = harness(); // default cfg(): no sink secret.
        seed_user(&h, SINK_USER, "meter@example.com");
        let body = serde_json::json!({"windows": [sink_window("play-1", 3_600_000)]}).to_string();
        let header =
            crate::meter_sink::authorization_header(Some(SINK_SECRET), NOW, body.as_bytes())
                .expect("signs");
        let resp = call_sink(&h, &body, Some(header));
        assert_eq!(resp.status, 500, "{}", resp.body);
        assert!(h.store.play_windows.borrow().is_empty());
    }

    #[test]
    fn meter_sink_signed_but_malformed_batch_is_422() {
        let h = sink_harness();
        let resp = call_sink(&h, "not json", None);
        assert_eq!(resp.status, 422, "{}", resp.body);
        // A window shape the wire forbids (unknown field) refuses whole.
        let extra = r#"{"windows":[{"user_id":"u-meter","track_id":"track-meter-1","play_id":"play-1","union_bytes":1,"file_bytes":null,"started_at":7,"seconds":9}]}"#;
        assert_eq!(call_sink(&h, extra, None).status, 422);
        assert!(h.store.play_windows.borrow().is_empty());
    }

    #[test]
    fn meter_sink_fenced_and_rejected_dispositions() {
        let h = sink_harness();
        h.store
            .deleting_users
            .borrow_mut()
            .insert(SINK_USER.to_string());
        let mut ghost = sink_window("play-ghost", 3_600_000);
        ghost["user_id"] = serde_json::json!("u-ghost");
        let body = serde_json::json!({"windows": [
            sink_window("play-fenced", 3_600_000),
            sink_window("play-zero", 0),
            // Identity characters outside [A-Za-z0-9_-] never prepare.
            sink_window("play bad", 3_600_000),
            ghost,
        ]})
        .to_string();
        let resp = call_sink(&h, &body, None);
        assert_eq!(resp.status, 200, "{}", resp.body);
        let (accepted, fenced, rejected) = ack_lists(&resp);
        assert!(accepted.is_empty());
        // Deleting account and unknown user both fence (terminal).
        assert_eq!(fenced, vec!["play-fenced", "play-ghost"]);
        // Zero union and bad identity can never prepare (terminal).
        assert_eq!(rejected, vec!["play-zero", "play bad"]);
        assert!(h.store.play_windows.borrow().is_empty());
    }

    #[test]
    fn meter_sink_unknown_track_meters_on_the_fallback_rung() {
        let h = sink_harness();
        // No meta seeded and no file size on the wire: nominal-rate
        // fallback derives ceil(96_000 / 48_000) = 2 s, snapshots NULL.
        let mut window = sink_window("play-fallback", 96_000);
        window["file_bytes"] = serde_json::Value::Null;
        let body = serde_json::json!({"windows": [window]}).to_string();
        let resp = call_sink(&h, &body, None);
        assert_eq!(resp.status, 200, "{}", resp.body);
        let stored = h.store.play_windows.borrow();
        let window = stored.values().next().expect("one stored window");
        assert_eq!(window.stream_seconds, 2);
        assert!(window.rate_fallback);
        assert_eq!(window.file_bytes, None);
        assert_eq!(window.duration_ms, None);
    }

    #[test]
    fn meter_sink_path_has_no_browser_surface() {
        let h = sink_harness();
        let resp = h.call_full(
            "OPTIONS",
            crate::meter_sink::SINK_PATH,
            None,
            None,
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 404, "no preflight maps the internal route");
        assert!(!is_known_path(crate::meter_sink::SINK_PATH));
        // No browser credential authenticates here either: without the
        // sink HMAC the handler answers 401 and writes nothing.
        let body = serde_json::json!({"windows": [sink_window("play-1", 3_600_000)]});
        let resp = h.call("POST", crate::meter_sink::SINK_PATH, body);
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(h.store.play_windows.borrow().is_empty());
    }

    const QA_TOKEN: &str = "test-qa-bearer-token";

    fn qa_harness() -> Harness {
        let mut h = harness();
        h.config = cfg().with_qa_code_seam(true).with_qa_auth_token(QA_TOKEN);
        h
    }

    impl Harness {
        fn call_seam(&self, body: serde_json::Value, bearer: Option<&str>) -> Response {
            let req = Request {
                method: "POST".to_string(),
                path: QA_TEST_CODES_PATH.to_string(),
                cookie: None,
                origin: None,
                authorization: bearer.map(|token| format!("Bearer {token}")),
                stripe_signature: None,
                body: body.to_string().into_bytes(),
            };
            block_on(handle(&self.ctx(), &req))
        }

        /// Same-origin POST carrying a raw `Authorization` header value.
        fn call_authorized(
            &self,
            path: &str,
            body: serde_json::Value,
            authorization: Option<&str>,
        ) -> Response {
            let req = Request {
                method: "POST".to_string(),
                path: path.to_string(),
                cookie: None,
                origin: Some(ORIGIN.to_string()),
                authorization: authorization.map(str::to_string),
                stripe_signature: None,
                body: body.to_string().into_bytes(),
            };
            block_on(handle(&self.ctx(), &req))
        }
    }

    #[test]
    fn qa_mailbox_gate_matches_only_the_reserved_test_domain() {
        assert!(is_example_test_mailbox(
            "e2e-wizard+20261017-000001@example.test"
        ));
        assert!(is_example_test_mailbox("A@EXAMPLE.TEST"));
        assert!(is_example_test_mailbox("  a@example.test  "));
        assert!(!is_example_test_mailbox("a@example.com"));
        assert!(!is_example_test_mailbox("a@example.test.evil.com"));
        assert!(!is_example_test_mailbox("a@evil-example.test"));
        assert!(!is_example_test_mailbox("a@example.test@evil.com"));
        assert!(!is_example_test_mailbox("@example.test"));
        assert!(!is_example_test_mailbox("not-an-email"));
        assert!(!is_example_test_mailbox(""));
        // Trailing-dot FQDN form is conservatively rejected.
        assert!(!is_example_test_mailbox("a@example.test."));
    }

    #[test]
    fn qa_bearer_compare_accepts_only_the_exact_secret() {
        assert!(qa_bearer_ok(Some(QA_TOKEN), QA_TOKEN));
        assert!(!qa_bearer_ok(None, QA_TOKEN));
        assert!(!qa_bearer_ok(Some(""), QA_TOKEN));
        assert!(!qa_bearer_ok(Some("wrong-token"), QA_TOKEN));
        assert!(!qa_bearer_ok(Some("test-qa-bearer-toke"), QA_TOKEN));
        assert!(!qa_bearer_ok(Some("test-qa-bearer-token!"), QA_TOKEN));
        assert!(!qa_bearer_ok(Some("TEST-QA-BEARER-TOKEN"), QA_TOKEN));
    }

    #[test]
    fn qa_seam_absent_without_the_staging_flag() {
        // Production posture: the constructor default serves the seam
        // path as a plain 404 — indistinguishable from an unknown route —
        // even for a well-formed synthetic mailbox.
        let h = harness();
        let resp = h.call(
            "POST",
            QA_TEST_CODES_PATH,
            serde_json::json!({"email": "a@example.test"}),
        );
        assert_eq!(resp.status, 404);
        assert_eq!(
            resp.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        let unknown = h.call(
            "POST",
            "/api/no-such-seam",
            serde_json::json!({"email": "a@example.test"}),
        );
        assert_eq!(unknown.status, 404);
        assert_eq!(unknown.body, resp.body);
        // The seam path is not enumerable: `OPTIONS` on it answers the
        // same 404 as an unknown path, with or without an `Origin`
        // (the e2e seam client never sends a CORS preflight).
        let pre = h.call_full("OPTIONS", QA_TEST_CODES_PATH, None, None, Some(ORIGIN));
        assert_eq!(pre.status, 404);
        assert_eq!(
            pre.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        let pre_unknown = h.call_full("OPTIONS", "/api/no-such-seam", None, None, Some(ORIGIN));
        assert_eq!(pre_unknown.status, 404);
        assert_eq!(pre_unknown.body, pre.body);
        let pre_no_origin = h.call_full("OPTIONS", QA_TEST_CODES_PATH, None, None, None);
        assert_eq!(pre_no_origin.status, 404);
    }

    #[test]
    fn qa_seam_fail_closed_without_a_staged_bearer() {
        // Flag armed but no QA_AUTH_TOKEN staged: there is nothing to
        // compare against, so every request 404s — arming the flag alone
        // never opens the seam.
        let mut h = harness();
        h.config = cfg().with_qa_code_seam(true);
        let resp = h.call_seam(
            serde_json::json!({"email": "a@example.test"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 404);
        assert_eq!(
            resp.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
    }

    #[test]
    fn qa_seam_rejects_a_bad_bearer_without_detail() {
        let h = qa_harness();
        for bearer in [None, Some(""), Some("wrong-token")] {
            let resp = h.call_seam(serde_json::json!({"email": "a@example.test"}), bearer);
            assert_eq!(resp.status, 401);
            assert_eq!(resp.body, r#"{"error":"unauthorized"}"#);
        }
        // A non-Bearer scheme is not a bearer either.
        let req = Request {
            method: "POST".to_string(),
            path: QA_TEST_CODES_PATH.to_string(),
            cookie: None,
            origin: None,
            authorization: Some(format!("Token {QA_TOKEN}")),
            stripe_signature: None,
            body: serde_json::json!({"email": "a@example.test"})
                .to_string()
                .into_bytes(),
        };
        let resp = block_on(handle(&h.ctx(), &req));
        assert_eq!(resp.status, 401);
        assert_eq!(resp.body, r#"{"error":"unauthorized"}"#);
    }

    #[test]
    fn qa_seam_rejects_real_mailboxes_and_captures_nothing_for_them() {
        let h = qa_harness();
        // Even a mailbox with a live issuance behind it: the seam only
        // ever answers example.test.
        seed_user(&h, "u-real", "a@example.com");
        let resp = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "a@example.com"}),
        );
        assert_eq!(resp.status, 200);
        assert_eq!(h.mailer.sent.borrow().len(), 1);
        let resp = h.call_seam(
            serde_json::json!({"email": "a@example.com"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 404);
        assert_eq!(
            resp.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        assert!(h.store.qa_codes.borrow().is_empty());
    }

    #[test]
    fn qa_seam_misses_without_an_issuance() {
        let h = qa_harness();
        let resp = h.call_seam(
            serde_json::json!({"email": "fresh@example.test"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 404);
        assert_eq!(
            resp.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        // Malformed bodies and non-mailboxes miss the same indistinct way.
        for body in [
            serde_json::json!({}),
            serde_json::json!({"email": "not-an-email"}),
            serde_json::json!({"email": 1}),
            serde_json::json!({"email": "a@example.test", "extra": true}),
        ] {
            let resp = h.call_seam(body, Some(QA_TOKEN));
            assert_eq!(resp.status, 404);
            assert_eq!(
                resp.body,
                r#"{"code":"not_found","message":"not found","retryable":false}"#
            );
        }
    }

    #[test]
    fn qa_seam_returns_the_issued_code_and_never_mints() {
        let h = qa_harness();
        // The real issuance path: the signup form's register/start mails
        // the inbox-proof code for the synthetic mailbox.
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "e2e-wizard@example.test"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let mailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        let users_before = h.store.users.borrow().len();
        let magic_before = h.store.magic.borrow().len();
        // The seam returns exactly the mailed token...
        let resp = h.call_seam(
            serde_json::json!({"email": "e2e-wizard@example.test"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let code: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            code.get("magic_token"),
            Some(&serde_json::Value::String(mailed.clone()))
        );
        // ...without minting anything itself: retrieval is read-only.
        assert_eq!(h.store.users.borrow().len(), users_before);
        assert_eq!(h.store.magic.borrow().len(), magic_before);
        // ...and the returned code is the live credential: the same token
        // redeems the magic finish, proving the seam hands back a genuinely
        // issued code rather than a lookalike.
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": mailed}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
    }

    #[test]
    fn qa_seam_refuses_an_expired_code() {
        let h = qa_harness();
        block_on(h.store.record_qa_issued_code(
            "stale@example.test",
            "stale-token",
            NOW.saturating_sub(auth::MAGIC_LINK_TTL_SECS + 1),
        ))
        .unwrap();
        let resp = h.call_seam(
            serde_json::json!({"email": "stale@example.test"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 404);
        assert_eq!(
            resp.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
    }

    #[test]
    fn qa_capture_skips_real_mailboxes_at_issuance() {
        // Belt-and-braces beside the retrieval-side domain pin: issuance
        // for a real mailbox writes no outbox row in the first place.
        let h = qa_harness();
        let resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "real@example.com"}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(!h.mailer.sent.borrow().is_empty());
        assert!(h.store.qa_codes.borrow().is_empty());
    }

    impl Harness {
        fn call_mint_seam(&self, body: serde_json::Value, bearer: Option<&str>) -> Response {
            let req = Request {
                method: "POST".to_string(),
                path: QA_TEST_ACCOUNTS_PATH.to_string(),
                cookie: None,
                origin: None,
                authorization: bearer.map(|token| format!("Bearer {token}")),
                stripe_signature: None,
                body: body.to_string().into_bytes(),
            };
            block_on(handle(&self.ctx(), &req))
        }
    }

    const NOT_FOUND_BODY: &str = r#"{"code":"not_found","message":"not found","retryable":false}"#;

    #[test]
    fn qa_mint_seam_absent_outside_the_staging_posture() {
        // Production posture (flag off) and flag-without-bearer both serve
        // the plain unknown-route 404 and mint nothing.
        let mut unarmed = harness();
        unarmed.config = cfg().with_qa_code_seam(true);
        for h in [harness(), unarmed] {
            let resp = h.call_mint_seam(
                serde_json::json!({"email": "a@example.test"}),
                Some(QA_TOKEN),
            );
            assert_eq!(resp.status, 404);
            assert_eq!(resp.body, NOT_FOUND_BODY);
            assert!(h.store.users.borrow().is_empty());
            assert!(h.store.sessions.borrow().is_empty());
            // Not enumerable through a preflight either.
            let pre = h.call_full("OPTIONS", QA_TEST_ACCOUNTS_PATH, None, None, Some(ORIGIN));
            assert_eq!(pre.status, 404);
            assert_eq!(pre.body, NOT_FOUND_BODY);
        }
    }

    #[test]
    fn qa_mint_seam_rejects_a_bad_bearer_without_detail() {
        let h = qa_harness();
        for bearer in [None, Some(""), Some("wrong-token")] {
            let resp = h.call_mint_seam(serde_json::json!({"email": "a@example.test"}), bearer);
            assert_eq!(resp.status, 401);
            assert_eq!(resp.body, r#"{"error":"unauthorized"}"#);
        }
        assert!(h.store.users.borrow().is_empty());
    }

    #[test]
    fn qa_mint_seam_refuses_real_mailboxes_and_malformed_bodies() {
        let h = qa_harness();
        for body in [
            serde_json::json!({"email": "a@example.com"}),
            serde_json::json!({"email": "a@example.test.evil.com"}),
            serde_json::json!({}),
            serde_json::json!({"email": 1}),
            serde_json::json!({"email": "a@example.test", "extra": true}),
        ] {
            let resp = h.call_mint_seam(body, Some(QA_TOKEN));
            assert_eq!(resp.status, 404);
            assert_eq!(resp.body, NOT_FOUND_BODY);
        }
        assert!(h.store.users.borrow().is_empty());
        assert!(h.store.sessions.borrow().is_empty());
    }

    #[test]
    fn qa_mint_seam_mints_an_account_with_a_live_session() {
        let h = qa_harness();
        for (email, plan) in [
            ("e2e+1@example.test", None),
            ("e2e+2@example.test", Some("free")),
        ] {
            let mut body = serde_json::json!({"email": email});
            if let Some(plan) = plan {
                body["plan"] = serde_json::json!(plan);
            }
            let resp = h.call_mint_seam(body, Some(QA_TOKEN));
            assert_eq!(resp.status, 201, "{}", resp.body);
            assert!(resp.set_cookie.is_none());
            let minted: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(minted["email"], email);
            let session = minted["session"].as_str().expect("session").to_string();
            let user = block_on(h.store.user_by_email(email))
                .unwrap()
                .expect("user");
            // The session is the real cookie credential for this account:
            // ordinary request authentication accepts it as that user.
            let req = Request {
                method: "GET".to_string(),
                path: "/v1/plans".to_string(),
                cookie: Some(format!("{}={session}", session::COOKIE_NAME)),
                origin: None,
                authorization: None,
                stripe_signature: None,
                body: Vec::new(),
            };
            let (claims, _) = block_on(h.ctx().authenticate(&req, false)).expect("authenticated");
            assert_eq!(claims.user_id, user.id);
        }
        let audits = h.store.audits.borrow();
        let actions: Vec<&str> = audits.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(actions, ["qa.test_account_mint", "qa.test_account_mint"]);
    }

    #[test]
    fn qa_mint_seam_refuses_unsupported_plans_and_existing_accounts() {
        let h = qa_harness();
        // Plans with no enforcement meaning are refused by name instead of
        // being minted as ordinary accounts.
        for plan in ["plus", ""] {
            let resp = h.call_mint_seam(
                serde_json::json!({"email": "capped@example.test", "plan": plan}),
                Some(QA_TOKEN),
            );
            assert_eq!(resp.status, 422, "{}", resp.body);
            assert!(resp.body.contains("unsupported plan"));
            assert!(!resp.body.contains("capped@"));
        }
        assert!(h.store.users.borrow().is_empty());
        // Mint, never sign-in: an existing account gets no new session.
        seed_user(&h, "u-existing", "taken@example.test");
        let resp = h.call_mint_seam(
            serde_json::json!({"email": "taken@example.test"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 422, "{}", resp.body);
        assert!(!resp.body.contains("taken@"));
        assert!(h.store.sessions.borrow().is_empty());
    }

    #[test]
    fn qa_mint_seam_mints_overlimit_with_seeded_ingest_usage() {
        let h = qa_harness();
        // The capped account mints as 201 like a free one, but today's
        // ingest count is pre-seeded to the fixed daily limit, so the
        // intake guard refuses it without touching a real path.
        let resp = h.call_mint_seam(
            serde_json::json!({"email": "capped@example.test", "plan": "overlimit"}),
            Some(QA_TOKEN),
        );
        assert_eq!(resp.status, 201, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["email"], "capped@example.test");
        let session = body["session"].as_str().unwrap();
        assert!(!session.is_empty());
        let user = block_on(h.store.user_by_email("capped@example.test"))
            .unwrap()
            .expect("minted user exists");
        let usage = block_on(h.store.ingest_jobs_today(&user.id, NOW)).unwrap();
        assert_eq!(usage, u64::from(crate::plan_limits::DAILY_INGEST_LIMIT));
    }

    /// Mint through the seam and return the raw per-account session token.
    fn mint_session(h: &Harness, email: &str, plan: Option<&str>) -> String {
        let mut payload = serde_json::json!({"email": email});
        if let Some(plan) = plan {
            payload["plan"] = serde_json::json!(plan);
        }
        let resp = h.call_mint_seam(payload, Some(QA_TOKEN));
        assert_eq!(resp.status, 201, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        body["session"].as_str().unwrap().to_string()
    }

    #[test]
    fn ingest_guard_refuses_seeded_account_with_plan_limit_wire() {
        let h = qa_harness();
        let session = mint_session(&h, "capped@example.test", Some("overlimit"));
        let resp = h.call_bearer("POST", "/api/ingest", Some(format!("Bearer {session}")));
        assert_eq!(resp.status, 402, "{}", resp.body);
        // Status-consistent refusal body: the terminal 402 code, a
        // non-retryable flag, and the upgrade pointer.
        assert_eq!(
            resp.body,
            r#"{"code":"plan_limit_exceeded","message":"plan limit reached","retryable":false,"upgrade_url":"/pricing"}"#
        );
        // The limit + reset headers the client renders the upgrade path
        // from; names-only by construction (no account data can flow in).
        let limit = resp
            .extra_headers
            .iter()
            .find(|(name, _)| name == "x-plan-limit")
            .expect("x-plan-limit header");
        assert!(limit.1.starts_with("ingest_today="), "{}", limit.1);
        let reset = resp
            .extra_headers
            .iter()
            .find(|(name, _)| name == "x-plan-reset")
            .expect("x-plan-reset header");
        assert_eq!(reset.1, "next-utc-day");
        assert!(resp.retry_after.is_none());
        assert!(!resp.body.contains("capped@"));
    }

    #[test]
    fn ingest_guard_falls_through_to_404_for_fresh_account() {
        let h = qa_harness();
        // A free account with no usage passes the guard to the plain 404:
        // the write path owns the accept arm and has not landed yet.
        let session = mint_session(&h, "fresh@example.test", None);
        let resp = h.call_bearer("POST", "/api/ingest", Some(format!("Bearer {session}")));
        assert_eq!(resp.status, 404, "{}", resp.body);
        assert!(resp.extra_headers.is_empty());
    }

    #[test]
    fn ingest_guard_rejects_unauthenticated_callers() {
        let h = qa_harness();
        // No bearer at all: the generic session 401 (this route is
        // documented, so there is no existence oracle to protect).
        let resp = h.call_bearer("POST", "/api/ingest", None);
        assert_eq!(resp.status, 401, "{}", resp.body);
        // A native-REST credential is not a session: fail closed, even
        // when the credential shape itself is well-formed.
        let resp = h.call_bearer(
            "POST",
            "/api/ingest",
            Some(format!("Bearer ntk_{}", "0".repeat(43))),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
    }

    // --- Billing/usage display reads (TWO-1675) ------------------------------
    //
    // NOW is 2023-11-14T22:13:20Z, so the usage period is `2023-11` and the
    // reset instant is 2023-12-01T00:00:00Z.

    fn seed_plans(h: &Harness) {
        *h.store.plans.borrow_mut() = vec![
            PlanRow {
                id: "free".to_string(),
                price_cents: 0,
                stream_hours_month: 0,
                max_concurrent_streams: 1,
            },
            PlanRow {
                id: "plus".to_string(),
                price_cents: 800,
                stream_hours_month: 100,
                max_concurrent_streams: 1,
            },
            PlanRow {
                id: "pro".to_string(),
                price_cents: 1500,
                stream_hours_month: 300,
                max_concurrent_streams: 1,
            },
            PlanRow {
                id: "max".to_string(),
                price_cents: 2500,
                stream_hours_month: 750,
                max_concurrent_streams: 1,
            },
        ];
    }

    fn billing_get(h: &Harness, path: &str, cookie: Option<String>) -> Response {
        h.call_full("GET", path, None, cookie, Some(ORIGIN))
    }

    #[test]
    fn billing_reads_require_a_session() {
        let h = harness();
        seed_plans(&h);
        for path in ["/v1/plans", "/v1/me/usage", "/v1/me/subscription"] {
            // No cookie: the generic session 401, never plan/account data.
            let resp = billing_get(&h, path, None);
            assert_eq!(resp.status, 401, "{path}: {}", resp.body);
            assert!(!resp.body.contains("plus"), "{path} leaked plan data");
            // A native-REST bearer is not a session (scope map: these reads
            // are SessionOnly): still the generic 401, never rescued.
            let resp = h.call_bearer("GET", path, Some(format!("Bearer ntk_{}", "0".repeat(43))));
            assert_eq!(resp.status, 401, "{path} bearer: {}", resp.body);
        }
    }

    #[test]
    fn plans_list_renders_rows_with_free_non_selectable() {
        let h = harness();
        seed_plans(&h);
        seed_user(&h, "u-1", "listener@example.com");
        let cookie = magic_login(&h, "listener@example.com");
        let resp = billing_get(&h, "/v1/plans", Some(cookie));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"plans": [
                {"id": "free", "priceCents": 0, "streamHoursMonth": 0, "maxConcurrentStreams": 1, "selectable": false},
                {"id": "plus", "priceCents": 800, "streamHoursMonth": 100, "maxConcurrentStreams": 1, "selectable": true},
                {"id": "pro", "priceCents": 1500, "streamHoursMonth": 300, "maxConcurrentStreams": 1, "selectable": true},
                {"id": "max", "priceCents": 2500, "streamHoursMonth": 750, "maxConcurrentStreams": 1, "selectable": true},
            ]})
        );
    }

    #[test]
    fn usage_get_reports_the_ledger_fold_and_reset_instant() {
        let h = harness();
        seed_user(&h, "u-1", "listener@example.com");
        let cookie = magic_login(&h, "listener@example.com");
        // No metered plays yet: zeros, never an error or a missing field.
        let resp = billing_get(&h, "/v1/me/usage", Some(cookie.clone()));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_eq!(
            resp.body,
            r#"{"period":"2023-11","resetsAt":"2023-12-01T00:00:00Z","streamSecondsMonth":0}"#
        );
        // The folded seconds for *this* period only: last month's plays do
        // not leak into the current period's figure.
        h.store
            .stream_seconds
            .borrow_mut()
            .insert(("u-1".to_string(), "2023-11".to_string()), 90 * 3600);
        h.store
            .stream_seconds
            .borrow_mut()
            .insert(("u-1".to_string(), "2023-10".to_string()), 999 * 3600);
        let resp = billing_get(&h, "/v1/me/usage", Some(cookie));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["streamSecondsMonth"], 90 * 3600);
        assert_eq!(body["resetsAt"], "2023-12-01T00:00:00Z");
    }

    #[test]
    fn subscription_get_names_never_active_and_lapsed_states() {
        let h = harness();
        seed_user(&h, "u-1", "listener@example.com");
        let cookie = magic_login(&h, "listener@example.com");
        // Never subscribed: 200 with the explicit state, nulls elsewhere —
        // a missing row is a state, not a 404.
        let resp = billing_get(&h, "/v1/me/subscription", Some(cookie.clone()));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "state": "no_subscription",
                "planId": null,
                "status": null,
                "currentPeriodEnd": null,
                "graceUntil": null,
                "cancelAtPeriodEnd": false,
            })
        );
        // Terminal-only history (canceled released the live slot): lapsed.
        h.store
            .ever_subscribed
            .borrow_mut()
            .insert("u-1".to_string());
        let resp = billing_get(&h, "/v1/me/subscription", Some(cookie.clone()));
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["state"], "lapsed");
        assert_eq!(body["planId"], serde_json::Value::Null);
        // Live paid coverage: active, stored status verbatim, ISO dates,
        // and no provider ids anywhere in the body.
        h.store.subscriptions.borrow_mut().insert(
            "u-1".to_string(),
            SubscriptionRow {
                plan_id: "pro".to_string(),
                status: "active".to_string(),
                cancel_at_period_end: true,
                paid_through_at_unix: Some(NOW as i64 + 86_400),
                grace_until_unix: None,
                current_period_end_unix: Some(NOW as i64 + 86_400),
            },
        );
        let resp = billing_get(&h, "/v1/me/subscription", Some(cookie));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "state": "active",
                "planId": "pro",
                "status": "active",
                "currentPeriodEnd": "2023-11-15T22:13:20Z",
                "graceUntil": null,
                "cancelAtPeriodEnd": true,
            })
        );
        assert!(!resp.body.contains("stripe"));
    }

    #[test]
    fn billing_reads_fail_closed_on_store_error() {
        let h = harness();
        seed_plans(&h);
        seed_user(&h, "u-1", "listener@example.com");
        let cookie = magic_login(&h, "listener@example.com");
        *h.store.fail_billing_reads.borrow_mut() = true;
        for path in ["/v1/plans", "/v1/me/usage", "/v1/me/subscription"] {
            let resp = billing_get(&h, path, Some(cookie.clone()));
            assert_eq!(resp.status, 500, "{path}: {}", resp.body);
        }
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
    fn magic_finish_lost_claim_never_authenticates() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        assert_eq!(
            h.call(
                "POST",
                "/v1/auth/magic/start",
                serde_json::json!({"email": "a@example.com"}),
            )
            .status,
            200
        );
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let hash = auth::hash_magic_token(&token);
        assert!(!h.store.magic.borrow()[&hash].used);
        *h.store.lose_magic_claim.borrow_mut() = true;
        let audits_before = h.store.audits.borrow().len();

        let lost = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );

        assert_eq!(*h.store.magic_redemptions.borrow(), 1);
        assert_eq!(lost.status, 401);
        assert_eq!(lost.body, r#"{"error":"check your email and try again"}"#);
        assert!(lost.set_cookie.is_none());
        assert!(h.store.sessions.borrow().is_empty());
        assert!(!h.store.magic.borrow()[&hash].used);
        assert_eq!(h.store.audits.borrow().len(), audits_before);
        assert!(!h
            .store
            .audits
            .borrow()
            .iter()
            .any(|event| event.action == "auth.magic"));
    }

    #[test]
    fn magic_finish_can_retry_after_session_insert_failure() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let started = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "a@example.com"}),
        );
        assert_eq!(started.status, 200);
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        *h.store.fail_session_insert.borrow_mut() = true;
        let failed = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(failed.status, 500);
        assert!(failed.set_cookie.is_none());
        assert!(h.store.sessions.borrow().is_empty());
        assert!(!h.store.magic.borrow()[&auth::hash_magic_token(&token)].used);
        assert!(!h
            .store
            .audits
            .borrow()
            .iter()
            .any(|event| event.action == "auth.magic"));
        *h.store.fail_session_insert.borrow_mut() = false;
        let retry = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(retry.status, 200, "{}", retry.body);
        assert!(retry.set_cookie.is_some());
        assert_eq!(h.store.sessions.borrow().len(), 1);
        assert!(h.store.magic.borrow()[&auth::hash_magic_token(&token)].used);
        assert_eq!(
            h.store
                .audits
                .borrow()
                .iter()
                .filter(|event| event.action == "auth.magic")
                .count(),
            1
        );
        let replay = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(replay.status, 401);
        assert!(replay.set_cookie.is_none());
        assert_eq!(h.store.sessions.borrow().len(), 1);
    }

    #[test]
    fn magic_finish_audit_transaction_error_never_sets_a_cookie() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        assert_eq!(
            h.call(
                "POST",
                "/v1/auth/magic/start",
                serde_json::json!({"email": "a@example.com"}),
            )
            .status,
            200
        );
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let hash = auth::hash_magic_token(&token);
        let audits_before = h.store.audits.borrow().len();
        *h.store.fail_magic_audit.borrow_mut() = true;
        let failed = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(failed.status, 500);
        assert!(failed.set_cookie.is_none());
        assert!(h.store.sessions.borrow().is_empty());
        assert!(!h.store.magic.borrow()[&hash].used);
        assert_eq!(h.store.audits.borrow().len(), audits_before);

        *h.store.fail_magic_audit.borrow_mut() = false;
        let retry = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(retry.status, 200, "{}", retry.body);
        assert!(retry.set_cookie.is_some());
        assert_eq!(h.store.sessions.borrow().len(), 1);
        assert!(h.store.magic.borrow()[&hash].used);
        assert_eq!(h.store.audits.borrow().len(), audits_before + 1);
        let replay = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(replay.status, 401);
        assert!(replay.set_cookie.is_none());
        assert_eq!(h.store.sessions.borrow().len(), 1);
        assert_eq!(h.store.audits.borrow().len(), audits_before + 1);
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
    fn magic_finish_fails_closed_when_the_session_lookup_errors() {
        // The account-swap guard must distinguish a storage failure from an
        // absent/invalid cookie: if looking up the existing session's row
        // errors, its owner is unproven, so the other account's link must
        // not be redeemed (and must not be spent) on the strength of the
        // failed lookup.
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
        let sessions_before = h.store.sessions.borrow().len();
        *h.store.fail_session_lookup.borrow_mut() = true;
        let resp = h.call_full(
            "POST",
            "/v1/auth/magic/finish",
            Some(serde_json::json!({"token": other_token}).to_string()),
            Some(victim_cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 500, "{}", resp.body);
        assert!(resp.set_cookie.is_none(), "no swapped-session cookie");
        assert_eq!(h.store.sessions.borrow().len(), sessions_before);
        assert!(!h.store.magic.borrow()[&auth::hash_magic_token(&other_token)].used);
        // Once the lookup recovers the token is still redeemable by its owner.
        *h.store.fail_session_lookup.borrow_mut() = false;
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": other_token}),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
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
            path: "/v1/plans".to_string(),
            cookie: Some(format!("nameless; {}=token-value", session::COOKIE_NAME)),
            origin: None,
            authorization: None,
            stripe_signature: None,
            body: Vec::new(),
        };
        assert_eq!(session_cookie_value(&req).as_deref(), Some("token-value"));
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
        let (token, record) = auth::issue_magic_link("u1", "a@example.com", NOW).expect("issue");
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
    fn refresh_retry_recovers_lost_response_only_with_original_client_secret() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let body = serde_json::json!({"retry_key": crate::b64::encode(&[42u8; 32])}).to_string();
        let refresh = |body: Option<String>, cookie: String| {
            h.call_full(
                "POST",
                "/v1/session/refresh",
                body,
                Some(cookie),
                Some(ORIGIN),
            )
        };
        // Commit the rotation but deliberately do not install its Set-Cookie.
        let lost = refresh(Some(body.clone()), cookie.clone());
        assert_eq!(lost.status, 200);
        let successor = cookie_of(&lost);
        assert_ne!(cookie, successor);
        assert_eq!(refresh(None, cookie.clone()).status, 401);
        let wrong_key =
            serde_json::json!({"retry_key": crate::b64::encode(&[43u8; 32])}).to_string();
        assert_eq!(refresh(Some(wrong_key), cookie.clone()).status, 401);
        let retried = refresh(Some(body.clone()), cookie.clone());
        assert_eq!(
            retried.status, 200,
            "a bound retry must recover the committed cookie"
        );
        assert_eq!(cookie_of(&retried), successor);
        assert_eq!(
            h.call_full("GET", "/v1/plans", None, Some(cookie.clone()), None)
                .status,
            401
        );
        assert_eq!(
            h.call_full("GET", "/v1/plans", None, Some(successor.clone()), None)
                .status,
            200
        );
        // A later rotation permanently closes the recovery slot.
        assert_eq!(refresh(None, successor).status, 200);
        assert_eq!(refresh(Some(body), cookie).status, 401);
    }

    fn bound_refresh(
        h: &Harness,
        cookie: &str,
        key: &str,
        now: u64,
        origin: Option<&str>,
    ) -> Response {
        let req = Request {
            method: "POST".into(),
            path: "/v1/session/refresh".into(),
            cookie: Some(cookie.into()),
            origin: origin.map(str::to_string),
            authorization: None,
            stripe_signature: None,
            body: serde_json::json!({"retry_key": key})
                .to_string()
                .into_bytes(),
        };
        let mut ctx = h.ctx();
        ctx.now = now;
        block_on(handle(&ctx, &req))
    }

    fn transported_refresh(
        h: &Harness,
        cookie: &str,
        body: &str,
        media_type: Option<&str>,
    ) -> Response {
        let ctx = h.ctx();
        let request = Request {
            method: "POST".into(),
            path: "/v1/session/refresh".into(),
            cookie: Some(cookie.into()),
            origin: Some(ORIGIN.into()),
            authorization: None,
            stripe_signature: None,
            body: Vec::new(),
        };
        block_on(crate::worker_entry::dispatch_collected(
            crate::worker_entry::CollectedBody::Complete(body.as_bytes().to_vec()),
            request,
            |collected_request| async move {
                crate::worker_entry::handle_with_refresh_transport_guard(
                    collected_request,
                    media_type,
                    |guarded_request| async move { handle(&ctx, &guarded_request).await },
                )
                .await
            },
        ))
    }

    #[test]
    fn refresh_transport_marker_prevents_empty_upload_downgrade() {
        // A lengthless aborted upload may be surfaced by the runtime as clean
        // empty EOF. Preserve the header opt-in through that exact Complete
        // decision; no auth, rotation or Set-Cookie may happen on missing JSON.
        for body in ["", "{", "{}", "{\"retry_key\":null}"] {
            let h = harness();
            seed_user(&h, "u1", "a@example.com");
            let cookie = magic_login(&h, "a@example.com");
            let response = transported_refresh(
                &h,
                &cookie,
                body,
                Some("application/vnd.nntune.session-refresh-bound+json"),
            );
            assert_eq!(response.status, 401);
            assert!(response.set_cookie.is_none());
            assert!(h
                .store
                .sessions
                .borrow()
                .values()
                .all(|row| row.rotation_counter == 0));
            assert!(h.store.refresh_retries.borrow().is_empty());
        }
        // Every accepted marker spelling must also reject a marked empty
        // upload: media types are case-insensitive and may carry parameters
        // or surrounding whitespace. An exact comparison, or dropping the
        // parameter split or trim, must turn one of these red.
        for media_type in [
            "Application/Vnd.Nntune.Session-Refresh-Bound+Json",
            "application/vnd.nntune.session-refresh-bound+json; charset=utf-8",
            "  application/vnd.nntune.session-refresh-bound+json ",
        ] {
            let h = harness();
            seed_user(&h, "u1", "a@example.com");
            let cookie = magic_login(&h, "a@example.com");
            let response = transported_refresh(&h, &cookie, "", Some(media_type));
            assert_eq!(response.status, 401, "media type {media_type:?}");
            assert!(response.set_cookie.is_none());
            assert!(h
                .store
                .sessions
                .borrow()
                .values()
                .all(|row| row.rotation_counter == 0));
            assert!(h.store.refresh_retries.borrow().is_empty());
        }
    }

    #[test]
    fn refresh_transport_requires_marked_recovery_but_preserves_legacy_empty_refresh() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let body = serde_json::json!({"retry_key": crate::b64::encode(&[42u8; 32])}).to_string();
        for media_type in [None, Some("application/json")] {
            let rejected = transported_refresh(&h, &cookie, &body, media_type);
            assert_eq!(rejected.status, 401);
            assert!(rejected.set_cookie.is_none());
        }
        let first = transported_refresh(
            &h,
            &cookie,
            &body,
            Some("application/vnd.nntune.session-refresh-bound+json"),
        );
        let retry = transported_refresh(
            &h,
            &cookie,
            &body,
            Some("application/vnd.nntune.session-refresh-bound+json"),
        );
        assert_eq!(first.status, 200);
        assert_eq!(cookie_of(&first), cookie_of(&retry));
        let legacy = transported_refresh(&h, &cookie_of(&first), "", None);
        assert_eq!(legacy.status, 200);
        assert!(h.store.refresh_retries.borrow().is_empty());
        let source = strip_line_comments(include_str!("worker_entry.rs"));
        let dispatch = source.split_once("pub async fn dispatch(").unwrap().1;
        assert!(dispatch.contains("headers_map.get(\"content-type\")"));
        let normalized = dispatch.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains(
            "handle_with_refresh_transport_guard( collected_request, refresh_content_type.as_deref(),"
        ));
        let guard = dispatch
            .find("handle_with_refresh_transport_guard")
            .unwrap();
        let handle = dispatch.find("handlers::handle(&ctx").unwrap();
        assert!(
            guard < handle,
            "runtime must reject downgrade before calling handlers"
        );
    }

    #[test]
    fn refresh_transport_accepts_each_marker_spelling_for_recovery() {
        // Positive counterpart to the marked-empty vectors: each accepted
        // spelling must also let a keyed request commit and recover the
        // same successor. A fresh harness per spelling keeps each first
        // commit and its recovery claim independent.
        for media_type in [
            "Application/Vnd.Nntune.Session-Refresh-Bound+Json",
            "application/vnd.nntune.session-refresh-bound+json; charset=utf-8",
            "  application/vnd.nntune.session-refresh-bound+json ",
        ] {
            let h = harness();
            seed_user(&h, "u1", "a@example.com");
            let cookie = magic_login(&h, "a@example.com");
            let body =
                serde_json::json!({"retry_key": crate::b64::encode(&[42u8; 32])}).to_string();
            let first = transported_refresh(&h, &cookie, &body, Some(media_type));
            assert_eq!(first.status, 200, "media type {media_type:?}");
            let retry = transported_refresh(&h, &cookie, &body, Some(media_type));
            assert_eq!(retry.status, 200, "media type {media_type:?}");
            assert_eq!(cookie_of(&first), cookie_of(&retry));
        }
    }

    #[test]
    fn refresh_retry_delivery_contract_settles_headers_before_next_cookie_mutation() {
        let protocol = include_str!("../README.md");
        assert!(protocol.contains("Cross-tab single-flight is mandatory"));
        assert!(protocol.contains("No hedged or overlapping live requests"));
        assert!(protocol.contains("settlement of every outstanding attempt"));
        assert!(protocol.contains("not** proof of that boundary"));
        for settle_before_advance in [false, true] {
            let h = harness();
            seed_user(&h, "u1", "a@example.com");
            let predecessor = magic_login(&h, "a@example.com");
            let key = crate::b64::encode(&[42u8; 32]);
            // Hold response A's HEADERS, not just its body. The second response
            // can carry the same successor; the database cannot order delivery.
            let delayed = bound_refresh(&h, &predecessor, &key, NOW, Some(ORIGIN));
            let delivered = bound_refresh(&h, &predecessor, &key, NOW, Some(ORIGIN));
            assert_eq!(delayed.status, 200);
            assert_eq!(delivered.status, 200);
            let mut cookie_jar = cookie_of(&delivered);
            assert_eq!(cookie_of(&delayed), cookie_jar);
            if settle_before_advance {
                // Drain every outstanding header before releasing the shared
                // cookie-writer lock or disposing of this operation's secret.
                cookie_jar = cookie_of(&delayed);
            }
            let advanced = h.call_full(
                "POST",
                "/v1/session/refresh",
                None,
                Some(cookie_jar),
                Some(ORIGIN),
            );
            assert_eq!(advanced.status, 200);
            cookie_jar = cookie_of(&advanced);
            if !settle_before_advance {
                // Unsupported schedule: same-name Set-Cookie replaces C2 with
                // C1, even though C2's response was already successfully used.
                cookie_jar = cookie_of(&delayed);
            }
            let authenticated = h.call_full("GET", "/v1/plans", None, Some(cookie_jar), None);
            assert_eq!(
                authenticated.status,
                if settle_before_advance { 200 } else { 401 }
            );
        }
    }

    #[test]
    fn refresh_retry_is_bounded_and_does_not_extend_lifetime() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let key = crate::b64::encode(&[42u8; 32]);
        let first = bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN));
        assert_eq!(first.status, 200);
        let before = h.store.sessions.borrow().clone();
        let recovered = bound_refresh(&h, &cookie, &key, NOW + 119, Some(ORIGIN));
        assert_eq!(recovered.status, 200);
        assert_eq!(cookie_of(&recovered), cookie_of(&first));
        assert!(recovered.set_cookie.unwrap().contains("Max-Age=3481"));
        for (id, row) in h.store.sessions.borrow().iter() {
            assert_eq!(row.rotation_counter, before[id].rotation_counter);
            assert_eq!(row.expires_at, before[id].expires_at);
            assert_eq!(row.created_at, before[id].created_at);
        }
        let expired = bound_refresh(&h, &cookie, &key, NOW + 120, Some(ORIGIN));
        assert_eq!(expired.status, 401);
        assert!(expired.set_cookie.is_none());
    }

    #[test]
    fn refresh_retry_rejects_revoked_and_cross_origin_requests() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let key = crate::b64::encode(&[42u8; 32]);
        for origin in [None, Some("https://attacker.example")] {
            assert_eq!(bound_refresh(&h, &cookie, &key, NOW, origin).status, 401);
        }
        let first = bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN));
        assert_eq!(first.status, 200);
        let successor = cookie_of(&first);
        for origin in [None, Some("https://attacker.example")] {
            assert_eq!(bound_refresh(&h, &cookie, &key, NOW, origin).status, 401);
        }
        assert_eq!(
            h.call_full(
                "POST",
                "/v1/session/revoke-all",
                None,
                Some(successor),
                Some(ORIGIN)
            )
            .status,
            200
        );
        assert_eq!(
            bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN)).status,
            401
        );
    }

    #[test]
    fn refresh_retry_recovers_a_committed_rotation_after_storage_ack_loss() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let key = crate::b64::encode(&[42u8; 32]);
        *h.store.lose_next_refresh_ack.borrow_mut() = true;
        let uncertain = bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN));
        assert_eq!(uncertain.status, 500);
        assert!(uncertain.set_cookie.is_none());
        assert_eq!(
            h.call_full("GET", "/v1/plans", None, Some(cookie.clone()), None)
                .status,
            401
        );
        let recovered = bound_refresh(&h, &cookie, &key, NOW + 1, Some(ORIGIN));
        assert_eq!(recovered.status, 200);
        assert_eq!(
            h.call_full("GET", "/v1/plans", None, Some(cookie_of(&recovered)), None)
                .status,
            200
        );
        assert!(h
            .store
            .sessions
            .borrow()
            .values()
            .all(|r| r.rotation_counter == 1));
    }

    #[test]
    fn refresh_retry_cannot_retrofit_recovery_onto_an_unbound_rotation() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let first = h.call_full(
            "POST",
            "/v1/session/refresh",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(first.status, 200);
        let key = crate::b64::encode(&[42u8; 32]);
        assert_eq!(
            bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN)).status,
            401
        );
        assert_eq!(
            h.call_full("GET", "/v1/plans", None, Some(cookie_of(&first)), None)
                .status,
            200
        );
    }

    #[test]
    fn refresh_retry_cannot_bypass_signed_token_or_current_row_checks() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let key = crate::b64::encode(&[42u8; 32]);
        let token = cookie.split_once('=').unwrap().1;
        let claims = session::verify_session(&h.config.session_secret, token, NOW).unwrap();
        let mut wrong_owner = claims.clone();
        wrong_owner.user_id = "other-user".into();
        let mut wrong_created = claims.clone();
        wrong_created.created_at -= 1;
        let mut wrong_expiry = claims.clone();
        wrong_expiry.expires_at += 1;
        for altered in [wrong_owner, wrong_created, wrong_expiry] {
            let signed =
                session::sign_refreshed_claims(&h.config.session_secret, &altered).unwrap();
            let cookie = format!("{}={signed}", session::COOKIE_NAME);
            assert_eq!(
                bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN)).status,
                401
            );
        }
        assert_eq!(
            bound_refresh(&h, &format!("{cookie}tampered"), &key, NOW, Some(ORIGIN)).status,
            401
        );
        assert!(h
            .store
            .sessions
            .borrow()
            .values()
            .all(|r| r.rotation_counter == 0));
        let first = bound_refresh(&h, &cookie, &key, NOW + 3599, Some(ORIGIN));
        assert_eq!(first.status, 200);
        // A still-open recovery window cannot revive an expired predecessor.
        assert_eq!(
            bound_refresh(&h, &cookie, &key, NOW + 3600, Some(ORIGIN)).status,
            401
        );
        h.store.sessions.borrow_mut().clear();
        assert_eq!(
            bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN)).status,
            401
        );
    }

    #[test]
    fn refresh_retry_rejects_malformed_keys_without_rotating() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        for key in [
            "".to_string(),
            "not-base64!".into(),
            crate::b64::encode(&[1u8; 31]),
            crate::b64::encode(&[1u8; 33]),
        ] {
            let response = bound_refresh(&h, &cookie, &key, NOW, Some(ORIGIN));
            assert_eq!(response.status, 401);
            assert!(response.set_cookie.is_none());
        }
        assert_eq!(
            h.call_full("GET", "/v1/plans", None, Some(cookie), None)
                .status,
            200
        );
        assert!(h
            .store
            .sessions
            .borrow()
            .values()
            .all(|r| r.rotation_counter == 0));
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
        let dead = h.call_full("GET", "/v1/plans", None, Some(cookie2), None);
        assert_eq!(dead.status, 401);
    }

    // --- Native REST credentials (ADR 0010) -------------------------------------

    fn subsonic_harness() -> Harness {
        let mut h = harness();
        h.config.subsonic_index = Some(
            crate::subsonic_config::SubsonicIndexConfig::parse(
                &serde_json::json!({"current_version":1,"keys":{"1":crate::b64::encode(&[7;32])}})
                    .to_string(),
            )
            .unwrap(),
        );
        h
    }

    fn subsonic_mint(h: &Harness, cookie: &str, body: serde_json::Value) -> Response {
        h.call_full(
            "POST",
            "/v1/subsonic-credentials",
            Some(body.to_string()),
            Some(cookie.to_string()),
            Some(ORIGIN),
        )
    }

    #[test]
    fn subsonic_lifecycle_show_once_listing_and_owner_revocation() {
        let h = subsonic_harness();
        seed_user(&h, "u1", "a@example.com");
        seed_user(&h, "u2", "b@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let foreign = magic_login(&h, "b@example.com");
        for mode in ["apikey", "token", "both"] {
            let response = subsonic_mint(
                &h,
                &cookie,
                serde_json::json!({"label":"client", "auth_mode":mode}),
            );
            assert_eq!(response.status, 201, "{}", response.body);
            assert!(response
                .extra_headers
                .contains(&("cache-control".into(), "private, no-store".into())));
            let value: serde_json::Value = serde_json::from_str(&response.body).unwrap();
            let id = value["id"].as_str().unwrap();
            assert_eq!(value["api_key"].is_string(), mode != "token");
            assert_eq!(value["token_secret"].is_string(), mode != "apikey");
            let listing = h.call_full(
                "GET",
                "/v1/subsonic-credentials",
                None,
                Some(cookie.clone()),
                None,
            );
            assert_eq!(listing.status, 200);
            assert!(
                !listing.body.contains("username")
                    && !listing.body.contains("secret")
                    && !listing.body.contains("api_key")
            );
            for key in ["username", "api_key", "token_secret"] {
                if let Some(s) = value[key].as_str() {
                    assert!(!listing.body.contains(s));
                    assert!(!format!("{:?}", h.store.audits.borrow()).contains(s));
                }
            }
            let path = format!("/v1/subsonic-credentials/{id}");
            assert_eq!(
                h.call_full("DELETE", &path, None, Some(foreign.clone()), Some(ORIGIN))
                    .status,
                404
            );
            assert_eq!(
                h.call_full("DELETE", &path, None, Some(cookie.clone()), Some(ORIGIN))
                    .status,
                200
            );
            assert_eq!(
                h.call_full("DELETE", &path, None, Some(cookie.clone()), Some(ORIGIN))
                    .status,
                200
            );
            assert_eq!(
                h.store.subsonic_credentials.borrow()[id].1.revoked_at,
                Some(NOW)
            );
        }
        let listing = h.call_full("GET", "/v1/subsonic-credentials", None, Some(foreign), None);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&listing.body).unwrap()["credentials"],
            serde_json::json!([])
        );
    }

    #[test]
    fn subsonic_lifecycle_requires_native_fresh_session_and_origin() {
        let h = subsonic_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let mut req = Request {
            method: "POST".into(),
            path: "/v1/subsonic-credentials".into(),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.into()),
            authorization: None,
            stripe_signature: None,
            body: br#"{"label":"client"}"#.to_vec(),
        };
        let ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            stripe: &h.stripe,
            now: NOW + session::PRIVILEGE_REAUTH_WINDOW_SECS + 1,
            source_ip: "198.51.100.9".into(),
        };
        let response = block_on(handle(&ctx, &req));
        assert_eq!(response.status, 401);
        assert!(response.body.contains("reauth_required"));
        req.origin = None;
        assert_ne!(
            h.call_full(
                "POST",
                &req.path,
                Some("{\"label\":\"client\"}".into()),
                Some(cookie.clone()),
                None
            )
            .status,
            201
        );
        req.cookie = None;
        req.authorization = Some("Bearer nsk_not_a_session".into());
        req.origin = Some(ORIGIN.into());
        assert_ne!(block_on(handle(&ctx, &req)).status, 201);
        assert!(h.store.subsonic_credentials.borrow().is_empty());
        req.method = "GET".into();
        req.cookie = Some(cookie);
        req.authorization = None;
        assert_eq!(block_on(handle(&ctx, &req)).status, 200);
    }

    #[test]
    fn subsonic_lifecycle_refuses_unconfigured_invalid_and_failed_writes() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        assert_eq!(
            subsonic_mint(&h, &cookie, serde_json::json!({"label":"client"})).status,
            503
        );
        let h = subsonic_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        for body in [
            serde_json::json!({"label":"client","auth_mode":"native"}),
            serde_json::json!({"label":"x".repeat(65)}),
            serde_json::json!({"label":"client","scope":"native-rest"}),
            serde_json::json!({"label":""}),
        ] {
            assert_eq!(subsonic_mint(&h, &cookie, body).status, 422);
        }
        *h.store.fail_subsonic_write.borrow_mut() = true;
        let response = subsonic_mint(&h, &cookie, serde_json::json!({"label":"client"}));
        assert_eq!(response.status, 500);
        assert!(
            !response.body.contains("nsk_")
                && !response.body.contains("nst_")
                && !response.body.contains("nsu_")
        );
        assert!(h.store.subsonic_credentials.borrow().is_empty());
    }

    #[test]
    fn subsonic_lifecycle_cap_deletion_and_recovery_are_fail_closed() {
        let h = subsonic_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        for _ in 0..32 {
            assert_eq!(
                subsonic_mint(&h, &cookie, serde_json::json!({"label":"client"})).status,
                201
            );
        }
        assert_eq!(
            subsonic_mint(&h, &cookie, serde_json::json!({"label":"client"})).status,
            422
        );
        h.store.deleting_users.borrow_mut().insert("u1".into());
        assert_eq!(
            subsonic_mint(&h, &cookie, serde_json::json!({"label":"client"})).status,
            409
        );
        h.store.deleting_users.borrow_mut().clear();
        *h.store.fail_subsonic_write.borrow_mut() = true;
        assert_eq!(
            h.call_full(
                "POST",
                "/v1/session/revoke-all",
                None,
                Some(cookie.clone()),
                Some(ORIGIN)
            )
            .status,
            500
        );
        assert_eq!(
            h.call_full(
                "GET",
                "/v1/subsonic-credentials",
                None,
                Some(cookie.clone()),
                None
            )
            .status,
            200
        );
        *h.store.fail_subsonic_write.borrow_mut() = false;
        assert_eq!(
            h.call_full(
                "POST",
                "/v1/session/revoke-all",
                None,
                Some(cookie),
                Some(ORIGIN)
            )
            .status,
            200
        );
        assert!(h
            .store
            .subsonic_credentials
            .borrow()
            .values()
            .all(|(_, r)| r.revoked_at == Some(NOW)));
    }

    fn peppered_cfg() -> Config {
        cfg().with_native_rest_pepper(vec![7u8; 32], 1)
    }

    fn peppered_harness() -> Harness {
        let mut h = harness();
        h.config = peppered_cfg();
        h
    }

    fn mint_call(h: &Harness, cookie: &str, body: serde_json::Value) -> Response {
        h.call_full(
            "POST",
            "/v1/credentials",
            Some(body.to_string()),
            Some(cookie.to_string()),
            Some(ORIGIN),
        )
    }

    fn mint_secret(body: &serde_json::Value) -> String {
        body["secret"]
            .as_str()
            .expect("mint returns secret")
            .to_string()
    }

    fn mint_id(body: &serde_json::Value) -> String {
        body["id"].as_str().expect("mint returns id").to_string()
    }

    #[test]
    fn credentials_mint_list_revoke_round_trip() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");

        let resp = mint_call(
            &h,
            &cookie,
            serde_json::json!({"label": "cli", "scopes": ["library:read"], "ttl_secs": 3600}),
        );
        assert_eq!(resp.status, 201, "{}", resp.body);
        let minted: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let id = mint_id(&minted);
        let secret = mint_secret(&minted);
        assert!(
            secret.starts_with("ntk_"),
            "display form carries the class prefix"
        );
        assert_eq!(
            minted["key_prefix"],
            secret[..crate::native_rest::CREDENTIAL_PREFIX.len()
                + crate::native_rest::KEY_PREFIX_LEN]
                .to_string()
        );
        assert_eq!(minted["scopes"], serde_json::json!(["library:read"]));
        assert_eq!(minted["expires_at"], NOW + 3600);

        // Stored HMAC-only: the row verifies against the secret but never
        // contains it, and the audit row names the id (never key material).
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].record.id, id);
        assert_eq!(rows[0].label, "cli");
        assert_eq!(rows[0].created_from_ip, "198.51.100.9");
        assert_ne!(rows[0].record.key_hmac, secret);
        assert_eq!(
            rows[0].record.key_hmac,
            crate::native_rest::credential_hmac(&secret, &[7u8; 32])
        );
        assert!(
            h.store
                .audits
                .borrow()
                .iter()
                .any(|e| e.action == format!("native_credential_created:{id}")),
            "issuance audits the credential id"
        );

        // Listing carries routing facts with no `secret` key anywhere.
        let list = h.call_full("GET", "/v1/credentials", None, Some(cookie.clone()), None);
        assert_eq!(list.status, 200, "{}", list.body);
        assert!(!list.body.contains(&secret));
        assert!(!list.body.contains(&rows[0].record.key_hmac));
        let listed: serde_json::Value = serde_json::from_str(&list.body).unwrap();
        assert_eq!(listed["credentials"].as_array().unwrap().len(), 1);
        assert_eq!(listed["credentials"][0]["id"], id);
        assert_eq!(
            listed["credentials"][0]["revoked_at"],
            serde_json::Value::Null
        );

        // Revoke is immediate and sticky-idempotent: twice still `ok: true`.
        for _ in 0..2 {
            let revoke = h.call_full(
                "DELETE",
                &format!("/v1/credentials/{id}"),
                None,
                Some(cookie.clone()),
                Some(ORIGIN),
            );
            assert_eq!(revoke.status, 200, "{}", revoke.body);
            assert_eq!(revoke.body, r#"{"ok":true}"#);
        }
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert!(rows[0].record.revoked_at.is_some());
        let list = h.call_full("GET", "/v1/credentials", None, Some(cookie), None);
        let listed: serde_json::Value = serde_json::from_str(&list.body).unwrap();
        assert!(listed["credentials"][0]["revoked_at"].is_number());
    }

    #[test]
    fn credentials_mint_and_revoke_require_fresh_session_but_list_does_not() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let stale_ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            now: NOW + session::PRIVILEGE_REAUTH_WINDOW_SECS + 1,
            source_ip: "198.51.100.9".to_string(),
            stripe: &h.stripe,
        };

        // Fresh issuance works (pins the 201 for the stale contrast below).
        let fresh = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(fresh.status, 201, "{}", fresh.body);
        let id = mint_id(&serde_json::from_str::<serde_json::Value>(&fresh.body).unwrap());

        // Stale sessions are denied the step-up with the CISO-approved
        // `reauth_required` response before any key material exists — and
        // mint nothing.
        let stale_mint_req = Request {
            method: "POST".to_string(),
            path: "/v1/credentials".to_string(),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            stripe_signature: None,
            body: serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60})
                .to_string()
                .into_bytes(),
        };
        let stale_mint = block_on(handle(&stale_ctx, &stale_mint_req));
        assert_eq!(stale_mint.status, 401);
        assert_eq!(
            stale_mint.body,
            r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
        );
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert_eq!(rows.len(), 1, "stale issuance must mint nothing");

        // Stale revocation is denied the same way; ordinary listing still
        // serves on the stale-but-valid session.
        let stale_revoke_req = Request {
            method: "DELETE".to_string(),
            path: format!("/v1/credentials/{id}"),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            stripe_signature: None,
            body: Vec::new(),
        };
        let stale_revoke = block_on(handle(&stale_ctx, &stale_revoke_req));
        assert_eq!(stale_revoke.status, 401);
        assert_eq!(
            stale_revoke.body,
            r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
        );
        let stale_list_req = Request {
            method: "GET".to_string(),
            path: "/v1/credentials".to_string(),
            cookie: Some(cookie),
            origin: None,
            authorization: None,
            stripe_signature: None,
            body: Vec::new(),
        };
        let stale_list = block_on(handle(&stale_ctx, &stale_list_req));
        assert_eq!(stale_list.status, 200, "{}", stale_list.body);
    }

    #[test]
    fn credentials_mint_rejects_unknown_scopes_and_bad_labels() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");

        // Cross-surface, case, and whitespace variants are not scopes.
        for scopes in [
            serde_json::json!(["subsonic"]),
            serde_json::json!(["Library:Read"]),
            serde_json::json!(["library:read "]),
            serde_json::json!(["library:read", "admin"]),
        ] {
            let resp = mint_call(
                &h,
                &cookie,
                serde_json::json!({"scopes": scopes, "ttl_secs": 60}),
            );
            assert_eq!(resp.status, 422, "scopes={scopes}: {}", resp.body);
            assert!(
                resp.body.contains("\"code\":\"validation_failed\""),
                "{}",
                resp.body
            );
        }
        // Unknown wire keys, over-long labels, and control characters fail
        // the same way (deny_unknown_fields + the 64-char CHECK mirror).
        for body in [
            serde_json::json!({"label": "cli", "scopes": ["library:read"], "ttl_secs": 60, "extra": 1}),
            serde_json::json!({"label": "l".repeat(65), "scopes": ["library:read"], "ttl_secs": 60}),
            serde_json::json!({"label": "cli\ninjected", "scopes": ["library:read"], "ttl_secs": 60}),
            serde_json::json!({"label": "cli", "scopes": ["library:read"]}),
        ] {
            let resp = mint_call(&h, &cookie, body);
            assert_eq!(resp.status, 422, "{}", resp.body);
        }
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert!(rows.is_empty(), "rejected mints store nothing");

        // The empty scope set is issuable (least-privilege floor) and the
        // TTL clamps fail-safe to the 365-day cap, even for `u64::MAX`.
        let bare = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": [], "ttl_secs": 60}),
        );
        assert_eq!(bare.status, 201, "{}", bare.body);
        let capped = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": u64::MAX}),
        );
        assert_eq!(capped.status, 201, "{}", capped.body);
        let capped_body: serde_json::Value = serde_json::from_str(&capped.body).unwrap();
        assert_eq!(
            capped_body["expires_at"],
            NOW + crate::native_rest::MAX_CREDENTIAL_TTL_SECS
        );
    }

    #[test]
    fn credentials_mint_fails_closed_without_a_provisioned_keyring() {
        // The default test config provisions no pepper: issuance must fail
        // closed (generic 500, never which secret is missing) while every
        // other route keeps serving.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let resp = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(resp.status, 500);
        assert_eq!(resp.body, r#"{"error":"internal error"}"#);
        let plans = h.call_full("GET", "/v1/plans", None, Some(cookie), None);
        assert_eq!(plans.status, 200, "unrelated routes keep serving");
    }

    #[test]
    fn credentials_revoke_is_owner_scoped_and_shape_checked() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        seed_user(&h, "u2", "b@example.com");
        let cookie_a = magic_login(&h, "a@example.com");
        let cookie_b = magic_login(&h, "b@example.com");
        let minted = mint_call(
            &h,
            &cookie_a,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(minted.status, 201, "{}", minted.body);
        let id = mint_id(&serde_json::from_str(&minted.body).unwrap());

        // Another user's id, an unknown id, an empty id, and a nested path
        // are all the same 404 — no oracle distinguishes them.
        for (cookie, path) in [
            (cookie_b.clone(), format!("/v1/credentials/{id}")),
            (cookie_a.clone(), "/v1/credentials/cred_missing".to_string()),
            (cookie_a.clone(), "/v1/credentials/".to_string()),
            (cookie_a.clone(), "/v1/credentials/a/b".to_string()),
        ] {
            let resp = h.call_full("DELETE", &path, None, Some(cookie), Some(ORIGIN));
            assert_eq!(resp.status, 404, "{path}: {}", resp.body);
        }
        // The victim row is untouched by the foreign probe.
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert_eq!(rows[0].record.revoked_at, None);
    }

    fn seed_admin(h: &Harness, id: &str, email: &str) -> String {
        seed_user(h, id, email);
        h.store
            .user_roles
            .borrow_mut()
            .insert(id.to_string(), crate::admin::ROLE_ADMIN.to_string());
        magic_login(h, email)
    }

    #[test]
    fn admin_settings_requires_admin_session_and_serves_defaults() {
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let member_cookie = magic_login(&h, "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");

        // The admin surface is non-enumerable (TWO-70 A-AC1): no
        // session, a garbage session and an authenticated non-admin
        // session all get the plain 404 an unknown path gives — never
        // a 401 or 403 that would confirm the route exists — and no
        // settings leak.
        let anon = h.call_full("GET", "/v1/admin/settings", None, None, None);
        assert_eq!(anon.status, 404, "{}", anon.body);
        let garbage = h.call_full(
            "GET",
            "/v1/admin/settings",
            None,
            Some("__Host-nntune-session=not-a-token".to_string()),
            None,
        );
        assert_eq!(garbage.status, 404, "{}", garbage.body);
        let denied = h.call_full(
            "GET",
            "/v1/admin/settings",
            None,
            Some(member_cookie.clone()),
            None,
        );
        assert_eq!(denied.status, 404, "{}", denied.body);
        assert_eq!(
            denied.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        // Byte-identical to the unknown-path answer: no oracle.
        let unknown = h.call_full("GET", "/v1/no-such-route", None, None, None);
        assert_eq!(denied.body, unknown.body);
        assert_eq!(anon.body, unknown.body);

        // A role-read outage is the same plain 404 for a member AND
        // for an admin session: until the role is confirmed, the
        // caller is not known to be an admin, so the guard never
        // answers the 500 an unknown path cannot produce.
        *h.store.fail_user_role_lookup.borrow_mut() = true;
        for cookie in [member_cookie, admin_cookie.clone()] {
            let resp = h.call_full("GET", "/v1/admin/settings", None, Some(cookie), None);
            assert_eq!(resp.status, 404, "{}", resp.body);
            assert_eq!(resp.body, unknown.body);
        }
        *h.store.fail_user_role_lookup.borrow_mut() = false;

        // Admin: every typed key at its built-in default while no rows
        // exist (the pre-seed posture).
        let ok = h.call_full("GET", "/v1/admin/settings", None, Some(admin_cookie), None);
        assert_eq!(ok.status, 200, "{}", ok.body);
        let body: serde_json::Value = serde_json::from_str(&ok.body).unwrap();
        assert_eq!(body["settings"]["invite_default_expiry_days"], 7);
        assert_eq!(body["settings"]["invite_member_min_paid_months"], 3);
        assert_eq!(body["settings"]["invite_member_per_window"], 2);
    }

    #[test]
    fn stale_sessions_hear_404_for_members_and_step_up_only_for_admins() {
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let member_cookie = magic_login(&h, "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let stale_ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            stripe: &h.stripe,
            now: NOW + session::PRIVILEGE_REAUTH_WINDOW_SECS + 1,
            source_ip: "198.51.100.9".to_string(),
        };
        let stale_put = |cookie: &str| {
            let req = Request {
                method: "PUT".to_string(),
                path: "/v1/admin/settings/invite_default_expiry_days".to_string(),
                cookie: Some(cookie.to_string()),
                origin: Some(ORIGIN.to_string()),
                authorization: None,
                stripe_signature: None,
                body: serde_json::json!({"value": 14}).to_string().into_bytes(),
            };
            block_on(handle(&stale_ctx, &req))
        };

        // A stale MEMBER session gets the plain 404, not the step-up
        // prompt: reauthenticating could never authorize them.
        let member = stale_put(&member_cookie);
        assert_eq!(member.status, 404, "{}", member.body);

        // A stale ADMIN session gets the step-up for a mutation...
        let admin = stale_put(&admin_cookie);
        assert_eq!(admin.status, 401, "{}", admin.body);
        assert_eq!(
            admin.body,
            r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
        );
        // ...and neither stale write stored anything.
        assert!(h.store.app_settings_rows.borrow().is_empty());

        // Reads never step up: the stale admin session still lists.
        let get_req = Request {
            method: "GET".to_string(),
            path: "/v1/admin/settings".to_string(),
            cookie: Some(admin_cookie),
            origin: None,
            authorization: None,
            stripe_signature: None,
            body: Vec::new(),
        };
        let list = block_on(handle(&stale_ctx, &get_req));
        assert_eq!(list.status, 200, "{}", list.body);
    }

    #[test]
    fn admin_routes_refuse_presented_bearer_credentials_with_404() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");

        // Bearer only, no cookie: the plain 404, like every other
        // non-admin probe of the namespace.
        let bearer_only = h.call_bearer(
            "GET",
            "/v1/admin/settings",
            Some("Bearer ntk_synthetic".to_string()),
        );
        assert_eq!(bearer_only.status, 404, "{}", bearer_only.body);
        assert_eq!(
            bearer_only.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );

        // A valid admin cookie does not rescue a presented bearer header.
        let req = Request {
            method: "GET".to_string(),
            path: "/v1/admin/settings".to_string(),
            cookie: Some(admin_cookie),
            origin: None,
            authorization: Some("Bearer ntk_synthetic".to_string()),
            stripe_signature: None,
            body: Vec::new(),
        };
        let both = block_on(handle(&h.ctx(), &req));
        assert_eq!(both.status, 404, "{}", both.body);
    }

    // --- Admin ingest (migration 0034) -------------------------------------

    /// Synthetic NZB with `file_count` files of `segments_per_file`
    /// contiguous segments. Test-only content: the marker strings below
    /// are what the redaction assertions hunt for in responses.
    fn ingest_nzb(
        file_count: usize,
        segments_per_file: usize,
        subject_pad: usize,
        bytes_each: u64,
    ) -> Vec<u8> {
        let mut out = b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n<head><meta type=\"title\">synthetic-ingest-fixture</meta></head>\n".to_vec();
        for file in 0..file_count {
            let pad = "x".repeat(subject_pad);
            out.extend_from_slice(
                format!(
                    "<file poster=\"poster@example.invalid\" date=\"1700000000\" subject=\"file-{file}-{pad}\">\n<groups><group>alt.binaries.synthetic</group></groups>\n<segments>\n"
                )
                .as_bytes(),
            );
            for segment in 1..=segments_per_file {
                out.extend_from_slice(
                    format!(
                        "<segment bytes=\"{bytes_each}\" number=\"{segment}\">mid-{file}-{segment}@example.invalid</segment>\n"
                    )
                    .as_bytes(),
                );
            }
            out.extend_from_slice(b"</segments>\n</file>\n");
        }
        out.extend_from_slice(b"</nzb>\n");
        out
    }

    fn minimal_ingest_nzb() -> Vec<u8> {
        ingest_nzb(1, 2, 0, 100)
    }

    fn post_ingest(h: &Harness, cookie: &str, body: &[u8]) -> Response {
        h.call_full(
            "POST",
            "/v1/admin/ingest",
            Some(String::from_utf8(body.to_vec()).expect("synthetic NZB is UTF-8")),
            Some(cookie.to_string()),
            Some(ORIGIN),
        )
    }

    fn ingest_audit_details(audit: &crate::vault::AuditEvent) -> serde_json::Value {
        assert_eq!(
            audit.action,
            crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION
        );
        assert!(audit.subject_user_id.is_none());
        serde_json::from_str(audit.details.as_ref().expect("details")).unwrap()
    }

    #[test]
    fn admin_ingest_guard_collapses_to_byte_identical_404() {
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let member_cookie = magic_login(&h, "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let unknown = h.call_full("GET", "/v1/no-such-route", None, None, None);
        assert_eq!(unknown.status, 404);

        // Reads: anonymous, garbage session and member probes all hear
        // the plain unknown-path 404 — byte-identical, never a distinct
        // code that would confirm the route exists.
        let anon = h.call_full("GET", "/v1/admin/ingest", None, None, None);
        assert_eq!(anon.body, unknown.body);
        let garbage = h.call_full(
            "GET",
            "/v1/admin/ingest",
            None,
            Some("__Host-nntune-session=not-a-token".to_string()),
            None,
        );
        assert_eq!(garbage.status, 404, "{}", garbage.body);
        assert_eq!(garbage.body, unknown.body);
        let denied = h.call_full(
            "GET",
            "/v1/admin/ingest",
            None,
            Some(member_cookie.clone()),
            None,
        );
        assert_eq!(denied.status, 404, "{}", denied.body);
        assert_eq!(denied.body, unknown.body);
        // Unknown job ids and malformed shapes 404 the same way for an
        // admin: the namespace never distinguishes miss reasons.
        let miss = h.call_full(
            "GET",
            "/v1/admin/ingest/adm_nosuchjob00000000000000000000000000000",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(miss.body, unknown.body);
        let deep = h.call_full(
            "GET",
            "/v1/admin/ingest/a/b",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(deep.body, unknown.body);
        // The mutation also 404s for non-admins (and the body never
        // varies with the payload: no size oracle).
        let nzb = minimal_ingest_nzb();
        let denied_post = post_ingest(&h, &member_cookie, &nzb);
        assert_eq!(denied_post.status, 404, "{}", denied_post.body);
        assert_eq!(denied_post.body, unknown.body);

        // Presented bearer credentials are refused with the same 404 —
        // the scope map keeps the admin namespace session-only — and a
        // valid cookie does not rescue a presented header.
        let bearer_only = h.call_bearer(
            "GET",
            "/v1/admin/ingest",
            Some("Bearer ntk_synthetic".to_string()),
        );
        assert_eq!(bearer_only.body, unknown.body);
        let bearer_post = h.call_bearer(
            "POST",
            "/v1/admin/ingest",
            Some("Bearer ntk_synthetic".to_string()),
        );
        assert_eq!(bearer_post.body, unknown.body);
        let req = Request {
            method: "POST".to_string(),
            path: "/v1/admin/ingest".to_string(),
            cookie: Some(admin_cookie.clone()),
            origin: None,
            authorization: Some("Bearer ntk_synthetic".to_string()),
            stripe_signature: None,
            body: nzb.clone(),
        };
        assert_eq!(block_on(handle(&h.ctx(), &req)).body, unknown.body);

        // A stale member session hears 404 on the mutation; a stale
        // admin session steps up (401) instead of writing.
        let stale_ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &h.dns,
            stripe: &h.stripe,
            now: NOW + session::PRIVILEGE_REAUTH_WINDOW_SECS + 1,
            source_ip: "198.51.100.9".to_string(),
        };
        let stale_post = |cookie: &str| {
            let req = Request {
                method: "POST".to_string(),
                path: "/v1/admin/ingest".to_string(),
                cookie: Some(cookie.to_string()),
                origin: Some(ORIGIN.to_string()),
                authorization: None,
                stripe_signature: None,
                body: nzb.clone(),
            };
            block_on(handle(&stale_ctx, &req))
        };
        assert_eq!(stale_post(&member_cookie).body, unknown.body);
        let step_up = stale_post(&admin_cookie);
        assert_eq!(step_up.status, 401, "{}", step_up.body);
        assert_eq!(
            step_up.body,
            r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
        );
        assert!(h.store.ingest_jobs.borrow().is_empty());

        // A role-read outage is the same plain 404 for reads and the
        // mutation alike: until the role is confirmed the caller is not
        // known to be an admin.
        *h.store.fail_user_role_lookup.borrow_mut() = true;
        for resp in [
            h.call_full("GET", "/v1/admin/ingest", None, Some(admin_cookie), None),
            post_ingest(&h, &member_cookie, &nzb),
        ] {
            assert_eq!(resp.status, 404, "{}", resp.body);
            assert_eq!(resp.body, unknown.body);
        }
        *h.store.fail_user_role_lookup.borrow_mut() = false;
        assert!(h.store.ingest_jobs.borrow().is_empty());
        // No guarded attempt ran, so no upload audit exists (the seeded
        // sessions audited their own logins only).
        assert!(h
            .store
            .audits
            .borrow()
            .iter()
            .all(|a| a.action != crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION));
    }

    #[test]
    fn admin_ingest_accepts_and_dedupes_by_content() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let nzb = minimal_ingest_nzb();

        let first = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(first.status, 201, "{}", first.body);
        let body: serde_json::Value = serde_json::from_str(&first.body).unwrap();
        assert_eq!(body["status"], "accepted");
        let job_id = body["job_id"].as_str().expect("job id").to_string();
        assert!(job_id.starts_with("adm_"));
        assert_eq!(body["nzb_bytes"], nzb.len() as u64);
        assert_eq!(body["file_count"], 1);
        assert_eq!(body["segment_count"], 2);
        assert_eq!(body["has_original"], true);
        assert_eq!(body["uploader_user_id"], "admin1");
        assert_eq!(body["deduplicated"], false);

        // The stored row is redacted by construction: counts and the
        // uploader only, no NZB text anywhere in the served body.
        for marker in [
            "poster@example",
            "alt.binaries.synthetic",
            "mid-0-1@",
            "synthetic-ingest-fixture",
        ] {
            assert!(!first.body.contains(marker), "{marker}");
        }
        let stored = h.store.ingest_jobs.borrow();
        let row = stored.get(&job_id).expect("stored row");
        assert_eq!(row.db_status, "queued");
        assert_eq!(row.accepted_at, NOW);
        assert_eq!(row.failure_reason, None);
        assert!(row.has_original);
        assert!(h.store.ingest_originals.borrow().contains_key(&job_id));
        drop(stored);

        // The audit row names the actor and the accepted outcome, with
        // no subject and no reason code. (Seeding the admin session
        // audited the login, so the ingest row is found by action,
        // never by index.)
        let find_uploads = || {
            h.store
                .audits
                .borrow()
                .iter()
                .filter(|a| a.action == crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION)
                .map(|a| (a.user_id.clone(), a.at, ingest_audit_details(a)))
                .collect::<Vec<_>>()
        };
        let uploads = find_uploads();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0, "admin1");
        assert_eq!(uploads[0].1, NOW);
        assert_eq!(uploads[0].2["job_id"], job_id);
        assert_eq!(uploads[0].2["outcome"], "accepted");
        assert!(uploads[0].2.get("reason_code").is_none());

        // A replay converges: 200, same job, `deduplicated: true`, no
        // second envelope, and the audit names the duplicate outcome.
        let replay = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(replay.status, 200, "{}", replay.body);
        let body: serde_json::Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(body["job_id"], job_id);
        assert_eq!(body["deduplicated"], true);
        assert_eq!(body["status"], "accepted");
        assert_eq!(h.store.ingest_jobs.borrow().len(), 1);
        let uploads = find_uploads();
        assert_eq!(uploads.len(), 2);
        assert_eq!(uploads[1].2["outcome"], "duplicate");

        // A second admin uploading the same bytes lands on the same job:
        // identity is content-derived, never the uploader.
        let admin2_cookie = seed_admin(&h, "admin2", "admin2@example.com");
        let second_admin = post_ingest(&h, &admin2_cookie, &nzb);
        assert_eq!(second_admin.status, 200, "{}", second_admin.body);
        let body: serde_json::Value = serde_json::from_str(&second_admin.body).unwrap();
        assert_eq!(body["job_id"], job_id);
        assert_eq!(body["deduplicated"], true);
        assert_eq!(body["uploader_user_id"], "admin1");
        assert_eq!(h.store.ingest_jobs.borrow().len(), 1);

        // Reads carry no `deduplicated` marker.
        let get = h.call_full(
            "GET",
            &format!("/v1/admin/ingest/{job_id}"),
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(get.status, 200, "{}", get.body);
        let body: serde_json::Value = serde_json::from_str(&get.body).unwrap();
        assert!(body.get("deduplicated").is_none());
        assert_eq!(body["status"], "accepted");
        let list = h.call_full(
            "GET",
            "/v1/admin/ingest",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(list.status, 200, "{}", list.body);
        let body: serde_json::Value = serde_json::from_str(&list.body).unwrap();
        assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
        assert!(body["jobs"][0].get("deduplicated").is_none());

        // A rejected upload converges the same way: same failed job id,
        // same reason, `deduplicated: true` on the replay.
        let bad = b"{\"not\": \"nzb\"}".to_vec();
        let rejected = post_ingest(&h, &admin2_cookie, &bad);
        assert_eq!(rejected.status, 400, "{}", rejected.body);
        let body: serde_json::Value = serde_json::from_str(&rejected.body).unwrap();
        let failed_id = body["job_id"].as_str().expect("failed job id").to_string();
        assert_eq!(body["reason"], "not_nzb");
        let replay_bad = post_ingest(&h, &admin_cookie, &bad);
        assert_eq!(replay_bad.status, 200, "{}", replay_bad.body);
        let body: serde_json::Value = serde_json::from_str(&replay_bad.body).unwrap();
        assert_eq!(body["job_id"], failed_id);
        assert_eq!(body["deduplicated"], true);
        assert_eq!(body["status"], "failed");
        assert_eq!(body["reason"], "not_nzb");
    }

    #[test]
    fn admin_ingest_rejects_empty_without_row_or_audit() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let resp = post_ingest(&h, &admin_cookie, &[]);
        assert_eq!(resp.status, 400, "{}", resp.body);
        assert_eq!(
            resp.body,
            r#"{"code":"invalid_request","job_id":null,"reason":"empty","status":"failed"}"#
        );
        assert!(h.store.ingest_jobs.borrow().is_empty());
        assert!(h.store.ingest_originals.borrow().is_empty());
        assert!(h
            .store
            .audits
            .borrow()
            .iter()
            .all(|a| a.action != crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION));
    }

    #[test]
    fn admin_ingest_rejection_legs_carry_fixed_vocabulary() {
        // (case, body, status, wire code, reason). The oversized leg is
        // built through `handle` so the in-handler pre-gate answers the
        // 413 null-job shape; the entry's collection seam answers the
        // uniform 400 instead (it runs before authentication).
        let oversize = vec![b'a'; crate::admin_ingest::ADMIN_INGEST_BODY_LIMIT_BYTES + 1];
        let mut truncated = minimal_ingest_nzb();
        truncated.truncate(truncated.len() / 2);
        let legs: Vec<(&str, Vec<u8>, u16, &str, &str)> = vec![
            ("malformed", truncated, 400, "invalid_request", "malformed"),
            (
                "not_nzb_json",
                b"{\"not\": \"nzb\"}".to_vec(),
                400,
                "invalid_request",
                "not_nzb",
            ),
            (
                "not_nzb_xml",
                b"<?xml version=\"1.0\"?><feed><entry/></feed>".to_vec(),
                400,
                "invalid_request",
                "not_nzb",
            ),
            ("too_large", oversize, 413, "payload_too_large", "too_large"),
            (
                "too_many_files",
                ingest_nzb(1025, 1, 0, 100),
                413,
                "payload_too_large",
                "too_many_files",
            ),
            (
                "too_many_segments_per_file",
                ingest_nzb(1, 4097, 0, 100),
                413,
                "payload_too_large",
                "too_many_segments",
            ),
            (
                "too_many_segments_total",
                ingest_nzb(61, 1000, 0, 100),
                413,
                "payload_too_large",
                "too_many_segments",
            ),
            (
                "declared_bytes_over_cap",
                ingest_nzb(1, 1, 0, (64 << 30) + 1),
                413,
                "payload_too_large",
                "declared_bytes_over_cap",
            ),
        ];
        for (case, body, status, code, reason) in legs {
            let h = harness();
            let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
            let resp = post_ingest(&h, &admin_cookie, &body);
            assert_eq!(resp.status, status, "{case}: {}", resp.body);
            let view: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(view["status"], "failed", "{case}");
            assert_eq!(view["code"], code, "{case}");
            assert_eq!(view["reason"], reason, "{case}");
            if reason == "too_large" {
                // Nothing was parsed: no row, no audit, null job.
                assert_eq!(view["job_id"], serde_json::Value::Null, "{case}");
                assert!(h.store.ingest_jobs.borrow().is_empty(), "{case}");
            } else {
                // A rejected body still writes its failed job row (same
                // transaction, same audit shape) with no original.
                let job_id = view["job_id"].as_str().expect("job id").to_string();
                let stored = h.store.ingest_jobs.borrow();
                let row = stored.get(&job_id).expect("failed row");
                assert_eq!(row.db_status, "failed", "{case}");
                assert_eq!(row.failure_reason.as_deref(), Some(reason), "{case}");
                assert!(!row.has_original, "{case}");
                assert!(
                    !h.store.ingest_originals.borrow().contains_key(&job_id),
                    "{case}"
                );
                drop(stored);
            }
            // No NZB text or parser detail ever reaches the body.
            for marker in ["poster@example", "alt.binaries", "mid-", "subject"] {
                assert!(!resp.body.contains(marker), "{case}: {marker}");
            }

            // The audit row for the attempt names the rejected outcome
            // plus the reason code; the oversized leg audits nothing.
            // (The seeded admin login audited too, so the attempt row
            // is found by action, never by index.)
            let uploads: Vec<_> = h
                .store
                .audits
                .borrow()
                .iter()
                .filter(|a| a.action == crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION)
                .map(ingest_audit_details)
                .collect();
            if reason == "too_large" {
                assert!(uploads.is_empty(), "{case}");
            } else {
                assert_eq!(uploads.len(), 1, "{case}");
                assert_eq!(uploads[0]["outcome"], "rejected", "{case}");
                assert_eq!(uploads[0]["reason_code"], reason, "{case}");
            }

            // A second admin replaying the same bytes converges on the
            // stored failure (or the oversized null shape again).
            let admin2_cookie = seed_admin(&h, "admin2", "admin2@example.com");
            let replay = post_ingest(&h, &admin2_cookie, &body);
            if reason == "too_large" {
                assert_eq!(replay.status, 413, "{case}: {}", replay.body);
            } else {
                assert_eq!(replay.status, 200, "{case}: {}", replay.body);
                let view: serde_json::Value = serde_json::from_str(&replay.body).unwrap();
                assert_eq!(view["deduplicated"], true, "{case}");
                assert_eq!(view["reason"], reason, "{case}");
            }
        }
    }

    #[test]
    fn admin_ingest_accepts_synthetic_document_over_two_mib() {
        // The admin cap (8 MiB) is deliberately above the user intake
        // gate (2 MiB): a lawful 2 MiB-plus NZB the user surface would
        // refuse must still ingest here.
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let nzb = ingest_nzb(1024, 1, 1900, 100);
        assert!(
            nzb.len() > 2 * 1024 * 1024,
            "fixture must clear the user gate: {}",
            nzb.len()
        );
        assert!(
            nzb.len() <= crate::admin_ingest::ADMIN_INGEST_BODY_LIMIT_BYTES,
            "fixture must fit the admin cap: {}",
            nzb.len()
        );
        let resp = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(resp.status, 201, "{}", resp.body);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["status"], "accepted");
        assert_eq!(body["file_count"], 1024);
        assert_eq!(body["segment_count"], 1024);
        assert_eq!(body["nzb_bytes"], nzb.len() as u64);
    }

    #[test]
    fn admin_ingest_serves_redacted_list_and_get_views() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let accepted_id = {
            let resp = post_ingest(&h, &admin_cookie, &minimal_ingest_nzb());
            assert_eq!(resp.status, 201, "{}", resp.body);
            serde_json::from_str::<serde_json::Value>(&resp.body).unwrap()["job_id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let failed_id = {
            let resp = post_ingest(&h, &admin_cookie, b"{\"not\": \"nzb\"}");
            assert_eq!(resp.status, 400, "{}", resp.body);
            serde_json::from_str::<serde_json::Value>(&resp.body).unwrap()["job_id"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // The full list serves both rows, newest first, with the bounded
        // default window — and no NZB text in any view.
        let list = h.call_full(
            "GET",
            "/v1/admin/ingest",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(list.status, 200, "{}", list.body);
        let body: serde_json::Value = serde_json::from_str(&list.body).unwrap();
        assert_eq!(body["limit"], 50);
        assert_eq!(body["offset"], 0);
        let jobs = body["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        let ids: Vec<&str> = jobs
            .iter()
            .map(|job| job["job_id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&accepted_id.as_str()));
        assert!(ids.contains(&failed_id.as_str()));
        for marker in [
            "poster@example",
            "alt.binaries.synthetic",
            "mid-",
            "@example.invalid",
        ] {
            assert!(!list.body.contains(marker), "{marker}");
        }

        // The status filter admits exactly the view vocabulary; anything
        // else is a 422, never a filtered read.
        for (query, want) in [
            ("?status=accepted", vec![accepted_id.as_str()]),
            ("?status=failed", vec![failed_id.as_str()]),
        ] {
            let resp = h.call_full(
                "GET",
                &format!("/v1/admin/ingest{query}"),
                None,
                Some(admin_cookie.clone()),
                None,
            );
            assert_eq!(resp.status, 200, "{query}: {}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            let ids: Vec<&str> = body["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|job| job["job_id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, want, "{query}");
        }
        let bad_status = h.call_full(
            "GET",
            "/v1/admin/ingest?status=queued",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(bad_status.status, 422, "{}", bad_status.body);
        assert_eq!(
            bad_status.body,
            r#"{"code":"validation_failed","message":"unknown status","retryable":false}"#
        );

        // The window clamps without erroring: unparsable falls back to
        // the default, oversize clamps to the cap, unknown keys are
        // ignored.
        let window = h.call_full(
            "GET",
            "/v1/admin/ingest?limit=1&offset=1",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        let body: serde_json::Value = serde_json::from_str(&window.body).unwrap();
        assert_eq!(body["limit"], 1);
        assert_eq!(body["offset"], 1);
        assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
        let junk = h.call_full(
            "GET",
            "/v1/admin/ingest?limit=junk&offset=junk&unknown=1",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        let body: serde_json::Value = serde_json::from_str(&junk.body).unwrap();
        assert_eq!(body["limit"], 50);
        assert_eq!(body["offset"], 0);
        assert_eq!(body["jobs"].as_array().unwrap().len(), 2);
        let capped = h.call_full(
            "GET",
            "/v1/admin/ingest?limit=9999",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        let body: serde_json::Value = serde_json::from_str(&capped.body).unwrap();
        assert_eq!(body["limit"], 100);

        // Single reads: the accepted view carries counts, the failed
        // view carries the fixed code and reason — neither carries NZB
        // text or the deduplicated marker.
        let get = h.call_full(
            "GET",
            &format!("/v1/admin/ingest/{accepted_id}"),
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(get.status, 200, "{}", get.body);
        let body: serde_json::Value = serde_json::from_str(&get.body).unwrap();
        assert_eq!(body["status"], "accepted");
        assert_eq!(body["file_count"], 1);
        assert_eq!(body["uploader_user_id"], "admin1");
        let get_failed = h.call_full(
            "GET",
            &format!("/v1/admin/ingest/{failed_id}"),
            None,
            Some(admin_cookie),
            None,
        );
        assert_eq!(get_failed.status, 200, "{}", get_failed.body);
        let body: serde_json::Value = serde_json::from_str(&get_failed.body).unwrap();
        assert_eq!(body["status"], "failed");
        assert_eq!(body["code"], "invalid_request");
        assert_eq!(body["reason"], "not_nzb");
        for marker in ["poster@example", "mid-", "@example.invalid"] {
            assert!(!get_failed.body.contains(marker), "{marker}");
        }
    }

    #[test]
    fn admin_ingest_accepts_ten_independent_uploads() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let mut ids = Vec::new();
        for pad in 0..10 {
            let resp = post_ingest(&h, &admin_cookie, &ingest_nzb(1, 1, pad, 100));
            assert_eq!(resp.status, 201, "upload {pad}: {}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(body["deduplicated"], false, "upload {pad}");
            ids.push(body["job_id"].as_str().unwrap().to_string());
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 10);
        let list = h.call_full(
            "GET",
            "/v1/admin/ingest?limit=100",
            None,
            Some(admin_cookie),
            None,
        );
        assert_eq!(list.status, 200, "{}", list.body);
        let body: serde_json::Value = serde_json::from_str(&list.body).unwrap();
        assert_eq!(body["jobs"].as_array().unwrap().len(), 10);
    }

    #[test]
    fn admin_ingest_delete_is_store_level_only() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let id_a = {
            let resp = post_ingest(&h, &admin_cookie, &minimal_ingest_nzb());
            serde_json::from_str::<serde_json::Value>(&resp.body).unwrap()["job_id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let nzb_b = ingest_nzb(1, 1, 7, 100);
        let id_b = {
            let resp = post_ingest(&h, &admin_cookie, &nzb_b);
            serde_json::from_str::<serde_json::Value>(&resp.body).unwrap()["job_id"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // No route serves deletion: every method on the job path that is
        // not a read 404s.
        for (method, path) in [
            ("DELETE", format!("/v1/admin/ingest/{id_a}")),
            ("PUT", format!("/v1/admin/ingest/{id_a}")),
            ("POST", format!("/v1/admin/ingest/{id_a}")),
        ] {
            let resp = h.call_full(method, &path, None, Some(admin_cookie.clone()), None);
            assert_eq!(resp.status, 404, "{method} {path}: {}", resp.body);
        }

        // The primitive deletes the original first, then the job, and
        // reports the deletion.
        assert_eq!(
            block_on(h.store.admin_ingest_delete("admin1", &id_a)).unwrap(),
            AdminIngestDeleteOutcome::Deleted
        );
        assert!(!h.store.ingest_originals.borrow().contains_key(&id_a));
        assert!(!h.store.ingest_jobs.borrow().contains_key(&id_a));
        let get = h.call_full(
            "GET",
            &format!("/v1/admin/ingest/{id_a}"),
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(get.status, 404, "{}", get.body);

        // A fresh upload of the same bytes lands a new row (200 becomes
        // 201 again): the delete fully released the identity.
        let fresh = post_ingest(&h, &admin_cookie, &minimal_ingest_nzb());
        assert_eq!(fresh.status, 201, "{}", fresh.body);
        let body: serde_json::Value = serde_json::from_str(&fresh.body).unwrap();
        assert_eq!(body["job_id"], id_a);
        assert_eq!(body["deduplicated"], false);
        assert!(h.store.ingest_originals.borrow().contains_key(&id_a));

        // Unknown ids (and pipeline-shaped ones, which the admin reads
        // never expose) report `Missing`, never a row.
        assert_eq!(
            block_on(
                h.store
                    .admin_ingest_delete("admin1", "adm_nosuchjob00000000000000000000")
            )
            .unwrap(),
            AdminIngestDeleteOutcome::Missing
        );

        // A demoted actor's delete is refused before any read: nothing
        // is removed and nothing is audited.
        h.store
            .user_roles
            .borrow_mut()
            .insert("admin1".to_string(), crate::admin::ROLE_MEMBER.to_string());
        let audits_before = h.store.audits.borrow().len();
        assert_eq!(
            block_on(h.store.admin_ingest_delete("admin1", &id_b)).unwrap(),
            AdminIngestDeleteOutcome::ActorNotAdmin
        );
        assert!(h.store.ingest_jobs.borrow().contains_key(&id_b));
        assert!(h.store.ingest_originals.borrow().contains_key(&id_b));
        assert_eq!(h.store.audits.borrow().len(), audits_before);

        // The surviving job still reads and opens.
        h.store
            .user_roles
            .borrow_mut()
            .insert("admin1".to_string(), crate::admin::ROLE_ADMIN.to_string());
        let get = h.call_full(
            "GET",
            &format!("/v1/admin/ingest/{id_b}"),
            None,
            Some(admin_cookie),
            None,
        );
        assert_eq!(get.status, 200, "{}", get.body);
    }

    #[test]
    fn admin_ingest_envelope_opens_only_under_row_identity() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let nzb = minimal_ingest_nzb();
        let resp = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(resp.status, 201, "{}", resp.body);
        let job_id = serde_json::from_str::<serde_json::Value>(&resp.body).unwrap()["job_id"]
            .as_str()
            .expect("job id")
            .to_string();
        let job_id: &str = &job_id;

        let stored = h.store.ingest_originals.borrow();
        let (envelope, hex, kek_version) = stored.get(job_id).expect("original").clone();
        drop(stored);
        let kek = *h
            .config
            .kek_by_version(h.config.kek_version)
            .expect("test KEK");
        let open = |envelope: &crate::admin_ingest::IngestOriginalEnvelope,
                    hex: &str,
                    job_id: &str,
                    version: i16,
                    format: i16,
                    kek_version: u32,
                    kek: &[u8; 32]| {
            crate::admin_ingest::decrypt_ingest_original(
                kek,
                hex,
                job_id,
                version,
                format,
                kek_version,
                envelope,
            )
        };
        assert_eq!(
            open(&envelope, &hex, job_id, 2, 1, kek_version, &kek).unwrap(),
            nzb
        );

        // A flipped ciphertext byte fails authentication — never
        // partial plaintext — under the generic crypto error.
        let mut tampered = envelope.clone();
        tampered.ciphertext[0] ^= 1;
        assert_eq!(
            open(&tampered, &hex, job_id, 2, 1, kek_version, &kek).unwrap_err(),
            crate::vault::VaultError::Crypto
        );
        // A flipped wrapped-DEK byte fails the wrap check the same way.
        let mut tampered_wrap = envelope.clone();
        tampered_wrap.dek_wrapped[0] ^= 1;
        assert_eq!(
            open(&tampered_wrap, &hex, job_id, 2, 1, kek_version, &kek).unwrap_err(),
            crate::vault::VaultError::Crypto
        );
        // Rows do not open under each other's identity: the AAD binds
        // the content hash and the job id.
        assert_eq!(
            open(
                &envelope,
                &hex,
                "adm_otheridentity00000000000000000000000000",
                2,
                1,
                kek_version,
                &kek
            )
            .unwrap_err(),
            crate::vault::VaultError::Crypto
        );
        assert!(open(&envelope, &"0".repeat(64), job_id, 2, 1, kek_version, &kek).is_err());
        // A caller-selected version the row was not wrapped under fails
        // the wrap check: no fallback loop.
        assert_eq!(
            open(&envelope, &hex, job_id, 2, 1, kek_version + 1, &kek).unwrap_err(),
            crate::vault::VaultError::Crypto
        );
        // Stored-shape failures (a short wrapped DEK, a negative
        // version) are corrupt-data errors, never crypto errors.
        let mut short = envelope.clone();
        short.dek_wrapped.truncate(47);
        assert_eq!(
            open(&short, &hex, job_id, 2, 1, kek_version, &kek).unwrap_err(),
            crate::vault::VaultError::CorruptStoredData
        );
        assert_eq!(
            open(&envelope, &hex, job_id, -1, 1, kek_version, &kek).unwrap_err(),
            crate::vault::VaultError::CorruptStoredData
        );
    }

    #[test]
    fn admin_ingest_demoted_actor_writes_nothing() {
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let nzb = minimal_ingest_nzb();
        let first = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(first.status, 201, "{}", first.body);

        // Demoted after passing the guard once: the write-time
        // re-check refuses with the plain non-admin 404 — no row, no
        // audit — even for bytes that already name a stored job.
        h.store
            .user_roles
            .borrow_mut()
            .insert("admin1".to_string(), crate::admin::ROLE_MEMBER.to_string());
        let unknown = h.call_full("GET", "/v1/no-such-route", None, None, None);
        let denied = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(denied.status, 404, "{}", denied.body);
        assert_eq!(denied.body, unknown.body);
        assert_eq!(h.store.ingest_jobs.borrow().len(), 1);
        let audits_before = h.store.audits.borrow().len();

        // The store primitive reports the refusal directly: no row,
        // no audit — even though the guard already passed for this
        // session before the demotion.
        let attempt = AdminIngestAttempt {
            actor_user_id: "admin1".to_string(),
            job_id: "adm_synthetic000000000000000000000000000".to_string(),
            idempotency_key: "admin:synthetic".to_string(),
            content_sha256_hex: "0".repeat(64),
            nzb_bytes: 10,
            file_count: None,
            segment_count: None,
            failure: Some(crate::admin_ingest::FailureReason::Malformed),
            original: None,
            kek_version: 1,
            source_ip: "198.51.100.9".to_string(),
            now: NOW,
        };
        assert_eq!(
            block_on(h.store.admin_ingest_upload(&attempt)).unwrap(),
            AdminIngestUploadOutcome::ActorNotAdmin
        );
        assert_eq!(h.store.ingest_jobs.borrow().len(), 1);
        assert_eq!(h.store.audits.borrow().len(), audits_before);
    }

    #[test]
    fn admin_ingest_deleting_actor_is_fenced() {
        // ADR 0002 rule 3 fences every writer of user-owned state: the
        // upload's job/original rows carry the actor's id under an
        // ON DELETE RESTRICT FK, so a deleting actor's upload is refused
        // with the typed `account_deleting` 409 — no job, no original,
        // no audit — even though the session is still valid and the
        // actor is still an admin through the grace window.
        let h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let nzb = minimal_ingest_nzb();
        h.store
            .deleting_users
            .borrow_mut()
            .insert("admin1".to_string());
        let audits_before = h.store.audits.borrow().len();

        let denied = post_ingest(&h, &admin_cookie, &nzb);
        assert_eq!(denied.status, 409, "{}", denied.body);
        assert_eq!(
            denied.body,
            r#"{"code":"account_deleting","message":"account is being deleted","retryable":false}"#,
            "{}",
            denied.body
        );
        assert!(h.store.ingest_jobs.borrow().is_empty());
        assert!(h.store.ingest_originals.borrow().is_empty());
        assert_eq!(h.store.audits.borrow().len(), audits_before);

        // The store primitive reports the refusal directly.
        let attempt = AdminIngestAttempt {
            actor_user_id: "admin1".to_string(),
            job_id: "adm_synthetic000000000000000000000000000".to_string(),
            idempotency_key: "admin:synthetic".to_string(),
            content_sha256_hex: "0".repeat(64),
            nzb_bytes: 10,
            file_count: None,
            segment_count: None,
            failure: Some(crate::admin_ingest::FailureReason::Malformed),
            original: None,
            kek_version: 1,
            source_ip: "198.51.100.9".to_string(),
            now: NOW,
        };
        assert_eq!(
            block_on(h.store.admin_ingest_upload(&attempt)).unwrap(),
            AdminIngestUploadOutcome::AccountDeleting
        );
        assert!(h.store.ingest_jobs.borrow().is_empty());
        assert_eq!(h.store.audits.borrow().len(), audits_before);
    }

    #[test]
    fn admin_preflight_never_maps_the_admin_namespace() {
        // Non-enumerability must hold on preflight too (TWO-70 A-AC1):
        // every path in the `/v1/admin` namespace — real, dynamic,
        // mistyped — answers OPTIONS with the plain 404, never the
        // empty 204 a served path gets.
        let h = harness();
        for path in [
            "/v1/admin",
            "/v1/admin/",
            "/v1/admin/settings",
            "/v1/admin/settings/invite_default_expiry_days",
            "/v1/admin/users/u_1/role",
            "/v1/admin/no-such-thing",
        ] {
            let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 404, "{path}: {}", resp.body);
            assert_eq!(
                resp.body, r#"{"code":"not_found","message":"not found","retryable":false}"#,
                "{path}"
            );
        }
        // The lookalike outside the namespace is simply unserved.
        let resp = h.call_full("OPTIONS", "/v1/adminx", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404);
        // And a genuinely served session path still clears preflight:
        // the exclusion is the namespace, not SessionOnly paths.
        let resp = h.call_full("OPTIONS", "/v1/me", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 204);
    }

    #[test]
    fn admin_write_store_recheck_refuses_a_demoted_actor() {
        // The write transactions re-read the actor's role under their
        // locks (production) / at write time (this fake): an actor
        // demoted after passing the guard writes and audits nothing.
        let h = harness();
        let _admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        seed_user(&h, "member1", "member@example.com");
        h.store
            .user_roles
            .borrow_mut()
            .insert("admin1".to_string(), crate::admin::ROLE_MEMBER.to_string());
        let audits_before = h.store.audits.borrow().len();

        let role = block_on(h.store.admin_set_user_role(
            "admin1",
            "member1",
            "admin",
            "198.51.100.9",
            NOW,
        ))
        .unwrap();
        assert_eq!(role, AdminWriteOutcome::ActorNotAdmin);
        let setting = block_on(h.store.admin_set_app_setting(
            "admin1",
            "invite_default_expiry_days",
            "14",
            "198.51.100.9",
            NOW,
        ))
        .unwrap();
        assert_eq!(setting, AdminWriteOutcome::ActorNotAdmin);
        assert_eq!(
            block_on(h.store.user_role("member1")).unwrap().as_deref(),
            Some("member")
        );
        assert!(h.store.app_settings_rows.borrow().is_empty());
        assert_eq!(h.store.audits.borrow().len(), audits_before);
    }

    #[test]
    fn admin_setting_write_is_typed_bounded_and_audited() {
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let member_cookie = magic_login(&h, "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let path = "/v1/admin/settings/invite_default_expiry_days";

        // A member write is refused before anything is stored or audited.
        let denied = h.call_full(
            "PUT",
            path,
            Some(serde_json::json!({"value": 14}).to_string()),
            Some(member_cookie),
            Some(ORIGIN),
        );
        assert_eq!(denied.status, 404, "{}", denied.body);
        assert!(h.store.app_settings_rows.borrow().is_empty());
        assert!(h
            .store
            .audits
            .borrow()
            .iter()
            .all(|a| a.action != crate::admin::SETTINGS_CHANGED_AUDIT_ACTION));

        // First admin write: the default was in force.
        let put = |cookie: &str, p: &str, v: serde_json::Value| {
            h.call_full(
                "PUT",
                p,
                Some(v.to_string()),
                Some(cookie.to_string()),
                Some(ORIGIN),
            )
        };
        let first = put(&admin_cookie, path, serde_json::json!({"value": 14}));
        assert_eq!(first.status, 200, "{}", first.body);
        let body: serde_json::Value = serde_json::from_str(&first.body).unwrap();
        assert_eq!(body["value"], 14);
        assert_eq!(body["previous_value"], 7);

        // The audit row names the actor and the old-to-new values, with
        // no subject (a settings write acts on no user).
        let audits = h.store.audits.borrow();
        let row = audits
            .iter()
            .find(|a| a.action == crate::admin::SETTINGS_CHANGED_AUDIT_ACTION)
            .expect("settings.changed audit row");
        assert_eq!(row.user_id, "admin1");
        assert_eq!(row.subject_user_id, None);
        let details: serde_json::Value =
            serde_json::from_str(row.details.as_deref().unwrap()).unwrap();
        assert_eq!(details["key"], "invite_default_expiry_days");
        assert_eq!(details["old"], serde_json::Value::Null);
        assert_eq!(details["new"], "14");
        drop(audits);

        // A second write reports the stored previous value.
        let second = put(&admin_cookie, path, serde_json::json!({"value": 30}));
        assert_eq!(second.status, 200, "{}", second.body);
        let body: serde_json::Value = serde_json::from_str(&second.body).unwrap();
        assert_eq!(body["previous_value"], 14);

        // Out-of-bounds and unknown keys write nothing.
        let audit_count = h.store.audits.borrow().len();
        let too_big = put(&admin_cookie, path, serde_json::json!({"value": 31}));
        assert_eq!(too_big.status, 422, "{}", too_big.body);
        let unknown = put(
            &admin_cookie,
            "/v1/admin/settings/not_a_setting",
            serde_json::json!({"value": 1}),
        );
        assert_eq!(unknown.status, 404, "{}", unknown.body);
        assert_eq!(
            h.store
                .app_settings_rows
                .borrow()
                .get("invite_default_expiry_days")
                .map(String::as_str),
            Some("30")
        );
        assert_eq!(h.store.audits.borrow().len(), audit_count);
    }

    #[test]
    fn admin_role_change_is_audited_and_never_self_served() {
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let member_cookie = magic_login(&h, "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        let put_role = |cookie: &str, subject: &str, role: &str| {
            h.call_full(
                "PUT",
                &format!("/v1/admin/users/{subject}/role"),
                Some(serde_json::json!({"role": role}).to_string()),
                Some(cookie.to_string()),
                Some(ORIGIN),
            )
        };

        // Self-promotion by a member never reaches the store.
        let self_serve = put_role(&member_cookie, "member1", "admin");
        assert_eq!(self_serve.status, 404, "{}", self_serve.body);
        assert_eq!(
            block_on(h.store.user_role("member1")).unwrap().as_deref(),
            Some("member")
        );

        // An admin promotes the member; the audit row carries actor,
        // subject and the old-to-new roles.
        let promoted = put_role(&admin_cookie, "member1", "admin");
        assert_eq!(promoted.status, 200, "{}", promoted.body);
        let body: serde_json::Value = serde_json::from_str(&promoted.body).unwrap();
        assert_eq!(body["previous_role"], "member");
        assert_eq!(body["role"], "admin");
        let audits = h.store.audits.borrow();
        let row = audits
            .iter()
            .find(|a| a.action == crate::admin::ROLE_CHANGED_AUDIT_ACTION)
            .expect("user.role_changed audit row");
        assert_eq!(row.user_id, "admin1");
        assert_eq!(row.subject_user_id.as_deref(), Some("member1"));
        let details: serde_json::Value =
            serde_json::from_str(row.details.as_deref().unwrap()).unwrap();
        assert_eq!(details["old"], "member");
        assert_eq!(details["new"], "admin");
        drop(audits);

        // The role is read fresh per request: the member's existing
        // cookie is an admin session from the next call on.
        let now_admin = h.call_full(
            "GET",
            "/v1/admin/settings",
            None,
            Some(member_cookie.clone()),
            None,
        );
        assert_eq!(now_admin.status, 200, "{}", now_admin.body);

        // Self-demotion is refused, unknown roles 422, unknown subjects 404.
        let audit_count = h.store.audits.borrow().len();
        let self_demote = put_role(&admin_cookie, "admin1", "member");
        assert_eq!(self_demote.status, 422, "{}", self_demote.body);
        let bogus = put_role(&admin_cookie, "member1", "owner");
        assert_eq!(bogus.status, 422, "{}", bogus.body);
        let ghost = put_role(&admin_cookie, "no-such-user", "member");
        assert_eq!(ghost.status, 404, "{}", ghost.body);
        assert_eq!(h.store.audits.borrow().len(), audit_count);
        assert_eq!(
            block_on(h.store.user_role("admin1")).unwrap().as_deref(),
            Some("admin")
        );

        // Demotion by another admin takes effect on the very next call.
        let demoted = put_role(&admin_cookie, "member1", "member");
        assert_eq!(demoted.status, 200, "{}", demoted.body);
        let no_longer = h.call_full("GET", "/v1/admin/settings", None, Some(member_cookie), None);
        assert_eq!(no_longer.status, 404, "{}", no_longer.body);
    }

    #[test]
    fn admin_role_write_on_a_deleting_subject_is_fenced() {
        // ADR 0002 rule 3 fences every writer once `deleting_at` is
        // set, and the admin role write is not exempt: a grant would
        // stay effective (roles are read fresh per request) until
        // erasure. The fenced write answers the typed
        // `account_deleting` 409 and writes/audits nothing.
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        h.store
            .deleting_users
            .borrow_mut()
            .insert("member1".to_string());
        let audits_before = h.store.audits.borrow().len();

        let fenced = h.call_full(
            "PUT",
            "/v1/admin/users/member1/role",
            Some(serde_json::json!({"role": "admin"}).to_string()),
            Some(admin_cookie),
            Some(ORIGIN),
        );
        assert_eq!(fenced.status, 409, "{}", fenced.body);
        assert_eq!(
            fenced.body,
            r#"{"code":"account_deleting","message":"account is being deleted","retryable":false}"#
        );
        assert_eq!(
            block_on(h.store.user_role("member1")).unwrap().as_deref(),
            Some("member")
        );
        assert_eq!(h.store.audits.borrow().len(), audits_before);
    }

    /// One full passkey signup (start + finish) presenting `invite_code`
    /// at finish only, mirroring the signup page's code step. Returns
    /// the finish response and the user id minted at start.
    fn signup_with_invite(
        h: &Harness,
        email: &str,
        cred_id: &[u8],
        cose_seed: u8,
        invite_code: Option<&str>,
    ) -> (Response, String) {
        let start = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": email}),
        );
        assert_eq!(start.status, 200, "{}", start.body);
        let start: serde_json::Value = serde_json::from_str(&start.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap().to_string();
        let user_id = block_on(h.store.user_by_email(email)).unwrap().unwrap().id;
        let emailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        let att = fx_none_attestation(cred_id, &fx_es256_cose(cose_seed));
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
                "magic_token": emailed,
                "invite_code": invite_code,
            }),
        );
        (resp, user_id)
    }

    fn audits_with_action(h: &Harness, action: &str) -> Vec<vault::AuditEvent> {
        h.store
            .audits
            .borrow()
            .iter()
            .filter(|event| event.action == action)
            .cloned()
            .collect()
    }

    /// No audit row may ever carry invite code material, plaintext or
    /// hash — rows reference invites by surrogate id only.
    fn assert_no_code_material_in_audits(h: &Harness, code: &str, code_hash: &str) {
        for event in h.store.audits.borrow().iter() {
            let details = event.details.clone().unwrap_or_default();
            assert!(
                !details.contains(code) && !details.contains(code_hash),
                "audit {} leaked code material: {details}",
                event.action
            );
        }
    }

    #[test]
    fn admin_invite_issue_list_revoke_round_trip_without_code_material() {
        let h = harness();
        seed_user(&h, "member1", "member@example.com");
        let member_cookie = magic_login(&h, "member@example.com");
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");

        // Non-admin probes of every invite route hear the plain 404 and
        // store nothing (AC-I3).
        for (method, path, cookie) in [
            ("POST", "/v1/admin/invites", Some(member_cookie.clone())),
            ("POST", "/v1/admin/invites", None),
            ("GET", "/v1/admin/invites", Some(member_cookie.clone())),
            (
                "POST",
                "/v1/admin/invites/1/revoke",
                Some(member_cookie.clone()),
            ),
        ] {
            let resp = h.call_full(
                method,
                path,
                Some(serde_json::json!({}).to_string()),
                cookie,
                Some(ORIGIN),
            );
            assert_eq!(resp.status, 404, "{method} {path}: {}", resp.body);
        }
        let bearer = h.call_bearer(
            "GET",
            "/v1/admin/invites",
            Some("Bearer ntk_synthetic".to_string()),
        );
        assert_eq!(bearer.status, 404, "{}", bearer.body);
        assert!(h.store.invites.borrow().is_empty());

        // Issue: the plaintext code is in this response exactly once.
        let issued = h.call_full(
            "POST",
            "/v1/admin/invites",
            Some(
                serde_json::json!({"email": "Invited@Example.com", "expires_in_days": 3})
                    .to_string(),
            ),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(issued.status, 201, "{}", issued.body);
        let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
        let code = body["code"].as_str().unwrap().to_string();
        assert!(!code.is_empty());
        let invite_id = body["invite"]["id"].as_i64().unwrap();
        assert_eq!(body["invite"]["status"], "active");
        assert_eq!(body["invite"]["kind"], "admin");
        assert_eq!(body["invite"]["email"], "invited@example.com");
        assert_eq!(body["invite"]["issued_by"], "admin1");
        assert_eq!(
            body["invite"]["expires_at"].as_u64().unwrap(),
            NOW + 3 * 86_400
        );
        // The hash is never in the response that carries the plaintext.
        let code_hash = auth::hash_invite_code(&code);
        assert!(!issued.body.contains(&code_hash));
        assert!(h.store.invites.borrow().contains_key(&code_hash));

        // The issue audit references the row id only.
        let issued_audits = audits_with_action(&h, auth::INVITE_ISSUED_AUDIT_ACTION);
        assert_eq!(issued_audits.len(), 1);
        assert_eq!(issued_audits[0].user_id, "admin1");
        assert_no_code_material_in_audits(&h, &code, &code_hash);

        // Out-of-range per-invite expiry is refused and stores nothing.
        let refused = h.call_full(
            "POST",
            "/v1/admin/invites",
            Some(serde_json::json!({"expires_in_days": 31}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(refused.status, 422, "{}", refused.body);
        assert_eq!(h.store.invites.borrow().len(), 1);

        // List + filters: lifecycle fields only, never code material.
        for (path, expected) in [
            ("/v1/admin/invites".to_string(), 1),
            ("/v1/admin/invites/status/active".to_string(), 1),
            ("/v1/admin/invites/status/revoked".to_string(), 0),
            ("/v1/admin/invites/issuer/admin1".to_string(), 1),
            ("/v1/admin/invites/issuer/nobody".to_string(), 0),
        ] {
            let resp = h.call_full("GET", &path, None, Some(admin_cookie.clone()), None);
            assert_eq!(resp.status, 200, "{path}: {}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(
                body["invites"].as_array().unwrap().len(),
                expected,
                "{path}"
            );
            assert!(!resp.body.contains(&code), "{path} leaked the code");
            assert!(!resp.body.contains(&code_hash), "{path} leaked the hash");
        }
        let bogus = h.call_full(
            "GET",
            "/v1/admin/invites/status/bogus",
            None,
            Some(admin_cookie.clone()),
            None,
        );
        assert_eq!(bogus.status, 404, "{}", bogus.body);

        // Revoke: audited, idempotent, and unknown ids are a plain 404.
        let revoke_path = format!("/v1/admin/invites/{invite_id}/revoke");
        let revoked = h.call_full(
            "POST",
            &revoke_path,
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(revoked.status, 200, "{}", revoked.body);
        let again = h.call_full(
            "POST",
            &revoke_path,
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(again.status, 200, "{}", again.body);
        assert_eq!(
            audits_with_action(&h, auth::INVITE_REVOKED_AUDIT_ACTION).len(),
            1,
            "the idempotent re-revoke must not audit twice"
        );
        assert_no_code_material_in_audits(&h, &code, &code_hash);
        let missing = h.call_full(
            "POST",
            "/v1/admin/invites/999/revoke",
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(missing.status, 404, "{}", missing.body);
        let list = h.call_full(
            "GET",
            "/v1/admin/invites/status/revoked",
            None,
            Some(admin_cookie),
            None,
        );
        let body: serde_json::Value = serde_json::from_str(&list.body).unwrap();
        assert_eq!(body["invites"].as_array().unwrap().len(), 1);
        assert_eq!(body["invites"][0]["revoked_by"], "admin1");
    }

    #[test]
    fn invite_redemption_enrolls_once_and_stamps_invited_by() {
        let mut h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        h.config.invites_required = true;

        let issued = h.call_full(
            "POST",
            "/v1/admin/invites",
            Some(serde_json::json!({"email": "new@example.com"}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(issued.status, 201, "{}", issued.body);
        let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
        let code = body["code"].as_str().unwrap().to_string();
        let invite_id = body["invite"]["id"].as_i64().unwrap();
        // No per-invite expiry: the typed default (7 days) applies.
        assert_eq!(
            body["invite"]["expires_at"].as_u64().unwrap(),
            NOW + 7 * 86_400
        );

        let (resp, user_id) =
            signup_with_invite(&h, "new@example.com", b"invited-cred-1", 81, Some(&code));
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some());

        let code_hash = auth::hash_invite_code(&code);
        {
            let invites = h.store.invites.borrow();
            let row = &invites[&code_hash];
            assert!(row.used);
            assert_eq!(row.redeemed_by.as_deref(), Some(user_id.as_str()));
            assert_eq!(row.redeemed_at, Some(NOW));
        }
        assert_eq!(
            h.store.invited_by.borrow().get(&user_id),
            Some(&Some("admin1".to_string()))
        );
        let redeemed = audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION);
        assert_eq!(redeemed.len(), 1);
        assert_eq!(
            redeemed[0].subject_user_id.as_deref(),
            Some(user_id.as_str())
        );
        assert_no_code_material_in_audits(&h, &code, &code_hash);

        // A redeemed invite cannot be revoked after the fact.
        let conflict = h.call_full(
            "POST",
            &format!("/v1/admin/invites/{invite_id}/revoke"),
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(conflict.status, 422, "{}", conflict.body);

        // Exactly once: a second signup with a spent (unbound) code
        // enrolls nothing and audits no second redemption.
        let issued = h.call_full(
            "POST",
            "/v1/admin/invites",
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie),
            Some(ORIGIN),
        );
        let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
        let shared = body["code"].as_str().unwrap().to_string();
        let (first, _) = signup_with_invite(
            &h,
            "first@example.com",
            b"invited-cred-2",
            82,
            Some(&shared),
        );
        assert_eq!(first.status, 200, "{}", first.body);
        let (second, second_user) = signup_with_invite(
            &h,
            "second@example.com",
            b"invited-cred-3",
            83,
            Some(&shared),
        );
        assert_eq!(second.status, 401, "{}", second.body);
        assert!(second.set_cookie.is_none());
        assert!(
            block_on(h.store.credentials_for_user(&second_user))
                .unwrap()
                .is_empty(),
            "the losing signup must not enroll a credential"
        );
        assert_eq!(
            audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION).len(),
            2,
            "one redemption audit per successful signup, no more"
        );
    }

    #[test]
    fn invite_invalid_classes_share_one_generic_error_at_signup_start() {
        let mut h = harness();
        let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
        h.config.invites_required = true;

        // One stored invite per invalid class (AC-I1).
        let seed_code = |code: &str, mutate: &dyn Fn(&mut InviteRecord)| {
            let hash = auth::hash_invite_code(code);
            let mut record = InviteRecord::new_issued(
                hash.clone(),
                Some("admin1".to_string()),
                auth::INVITE_KIND_ADMIN,
                None,
                NOW - 1_000,
                NOW + 604_800,
            );
            mutate(&mut record);
            h.store.invites.borrow_mut().insert(hash, record);
        };
        seed_code("expired-code", &|r| r.expires_at = NOW - 1);
        seed_code("revoked-code", &|r| r.revoked_at = Some(NOW - 10));
        seed_code("spent-code", &|r| {
            r.used = true;
            r.redeemed_at = Some(NOW - 10);
        });
        seed_code("bound-code", &|r| {
            r.email = Some("someone-else@example.com".to_string())
        });
        seed_code("self-code", &|_| {});

        let mail_before = h.mailer.sent.borrow().len();
        let mut bodies = Vec::new();
        for (index, code) in [
            "unknown-code",
            "expired-code",
            "revoked-code",
            "spent-code",
            "bound-code",
        ]
        .iter()
        .enumerate()
        {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({
                    "email": format!("probe-{index}@example.com"),
                    "invite_code": code,
                }),
            );
            assert_eq!(resp.status, 401, "{code}: {}", resp.body);
            assert!(resp.set_cookie.is_none());
            bodies.push(resp.body);
        }
        // Self-invite: the issuer's own email is refused the same way.
        let self_resp = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "admin@example.com", "invite_code": "self-code"}),
        );
        assert_eq!(self_resp.status, 401, "{}", self_resp.body);
        bodies.push(self_resp.body);
        assert!(
            bodies.iter().all(|body| body == &bodies[0]),
            "every invalid class must be byte-identical: {bodies:?}"
        );
        assert_eq!(
            h.mailer.sent.borrow().len(),
            mail_before,
            "a refused pre-check mints no signup and sends no mail"
        );

        // A valid code passes the pre-check WITHOUT being consumed:
        // abandoning this signup leaves it redeemable, and finishing a
        // later signup with it succeeds. Revoking it in between a start
        // and its finish makes that finish fail generically instead.
        let issued = h.call_full(
            "POST",
            "/v1/admin/invites",
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie.clone()),
            Some(ORIGIN),
        );
        let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
        let code = body["code"].as_str().unwrap().to_string();
        let invite_id = body["invite"]["id"].as_i64().unwrap();
        let start = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "racer@example.com", "invite_code": code}),
        );
        assert_eq!(start.status, 200, "{}", start.body);
        let start_body: serde_json::Value = serde_json::from_str(&start.body).unwrap();
        let challenge = start_body["challenge"].as_str().unwrap().to_string();
        let racer_id = block_on(h.store.user_by_email("racer@example.com"))
            .unwrap()
            .unwrap()
            .id;
        let emailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        let code_hash = auth::hash_invite_code(&code);
        assert!(
            !h.store.invites.borrow()[&code_hash].used,
            "the pre-check must never consume the code"
        );
        let revoked = h.call_full(
            "POST",
            &format!("/v1/admin/invites/{invite_id}/revoke"),
            Some(serde_json::json!({}).to_string()),
            Some(admin_cookie),
            Some(ORIGIN),
        );
        assert_eq!(revoked.status, 200, "{}", revoked.body);
        let att = fx_none_attestation(b"racer-cred", &fx_es256_cose(84));
        let finish = h.call(
            "POST",
            "/v1/auth/passkey/register/finish",
            serde_json::json!({
                "challenge": challenge,
                "credential_id": b64url_encode(b"racer-cred"),
                "attestation_object": b64url_encode(&att),
                "client_data_json": b64url_encode(
                    fx_client_json("webauthn.create", &challenge).as_bytes()
                ),
                "magic_token": emailed,
                "invite_code": code,
            }),
        );
        assert_eq!(finish.status, 401, "{}", finish.body);
        assert_eq!(finish.body, bodies[0]);
        assert!(block_on(h.store.credentials_for_user(&racer_id))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invite_per_email_budget_denies_sixth_attempt_generically() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let good_hash = auth::hash_invite_code("email-budget-code");
        h.store.invites.borrow_mut().insert(
            good_hash.clone(),
            InviteRecord::new_issued(
                good_hash.clone(),
                Some("issuer-admin".to_string()),
                auth::INVITE_KIND_ADMIN,
                None,
                NOW,
                NOW + 604_800,
            ),
        );
        let finish_with_code = |code: &str| {
            let challenge = auth::new_registration_challenge("example.com", ORIGIN, NOW).unwrap();
            let challenge_id = challenge.id.clone();
            block_on(
                h.store
                    .insert_challenge(&challenge, Some("u1"), PURPOSE_REGISTER),
            )
            .unwrap();
            let (token, record) =
                auth::issue_magic_link("u1", "a@example.com", NOW).expect("issue");
            block_on(h.store.insert_magic(&record)).unwrap();
            let att = fx_none_attestation(b"email-budget-cred", &fx_es256_cose(85));
            h.call(
                "POST",
                "/v1/auth/passkey/register/finish",
                serde_json::json!({
                    "challenge": challenge_id,
                    "credential_id": b64url_encode(b"email-budget-cred"),
                    "attestation_object": b64url_encode(&att),
                    "client_data_json": b64url_encode(
                        fx_client_json("webauthn.create", &challenge_id).as_bytes()
                    ),
                    "magic_token": token,
                    "invite_code": code,
                }),
            )
        };
        // Five wrong guesses spend the per-email budget (5/hour) while
        // the per-client budget (10/300s) still has room...
        for _ in 0..auth::INVITE_EMAIL_MAX_ATTEMPTS {
            assert_eq!(finish_with_code("wrong-code").status, 401);
        }
        // ...so the sixth attempt is denied generically even with the
        // correct code, and the code stays unspent.
        let denied = finish_with_code("email-budget-code");
        assert_eq!(denied.status, 401, "{}", denied.body);
        assert!(denied.set_cookie.is_none());
        assert!(!h.store.invites.borrow()[&good_hash].used);
        assert!(block_on(h.store.credentials_for_user("u1"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn magic_finish_cannot_bypass_invite_gate_for_credentialless_account() {
        let mut h = harness();
        h.config.invites_required = true;
        seed_user(&h, "u-nocred", "nocred@example.com");
        let start = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "nocred@example.com"}),
        );
        assert_eq!(start.status, 200, "{}", start.body);
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let token_hash = auth::hash_magic_token(&token);
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert!(
            !h.store.magic.borrow()[&token_hash].used,
            "the refused finish must not spend the token"
        );
        assert!(h.store.sessions.borrow().is_empty());

        // An established member (has a credential) is unaffected: the
        // gate is on account creation, not on sign-in.
        seed_user(&h, "u-member", "member2@example.com");
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(b"member2-cred"),
            user_id: "u-member".to_string(),
            public_key: fx_es256_cose(86),
            sign_count: 0,
            registered_at: NOW,
        }))
        .unwrap();
        let cookie = magic_login(&h, "member2@example.com");
        assert!(!cookie.is_empty());
    }

    #[test]
    fn magic_finish_invite_gate_keeps_established_magic_only_members_signed_in() {
        // Regression (review round 1): the gate must refuse only
        // never-admitted accounts. A magic-only member — no passkey,
        // but a prior session or a vault, the states the reaper
        // protects — keeps signing in by email once invites are
        // required; gating on credential-lessness alone locked them
        // out at the flip with no way back in.
        let mut h = harness();
        h.config.invites_required = true;

        // Established via a prior session (even a revoked one: a
        // completed sign-in happened, which is the admission proof).
        seed_user(&h, "u-magic-session", "magic-session@example.com");
        block_on(h.store.insert_session(&SessionRecord {
            id: "sess-prior".to_string(),
            user_id: "u-magic-session".to_string(),
            created_at: NOW,
            expires_at: NOW + 3600,
            rotation_counter: 0,
            revoked: true,
        }))
        .unwrap();
        let cookie = magic_login(&h, "magic-session@example.com");
        assert!(!cookie.is_empty());

        // Established via a vault, with no credential and no session.
        seed_user(&h, "u-magic-vault", "magic-vault@example.com");
        h.store
            .legacy_vault_users
            .borrow_mut()
            .insert("u-magic-vault".to_string());
        let cookie = magic_login(&h, "magic-vault@example.com");
        assert!(!cookie.is_empty());

        // The bypass stays closed in the same posture: a fresh
        // account with none of credential/session/vault is still
        // refused generically and its token stays unspent.
        seed_user(&h, "u-fresh", "fresh-signup@example.com");
        let start = h.call(
            "POST",
            "/v1/auth/magic/start",
            serde_json::json!({"email": "fresh-signup@example.com"}),
        );
        assert_eq!(start.status, 200, "{}", start.body);
        let token = h.mailer.sent.borrow().last().unwrap().1.clone();
        let token_hash = auth::hash_magic_token(&token);
        let resp = h.call(
            "POST",
            "/v1/auth/magic/finish",
            serde_json::json!({"token": token}),
        );
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert!(
            !h.store.magic.borrow()[&token_hash].used,
            "the refused finish must not spend the token"
        );
    }

    #[test]
    fn bearer_gate_enforces_scope_without_session_fallback() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let mint = |scopes: serde_json::Value| {
            let resp = mint_call(
                &h,
                &cookie,
                serde_json::json!({"scopes": scopes, "ttl_secs": 3600}),
            );
            assert_eq!(resp.status, 201, "{}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            mint_secret(&body)
        };
        let library_secret = mint(serde_json::json!(["library:read"]));
        let stream_secret = mint(serde_json::json!(["stream:read"]));

        // In-scope bearer passes the gate (and then 404s: data routes do
        // not exist yet, so default-deny holds). The 404 still proves the
        // gate ran — `last_used_at` is recorded.
        let ok = h.call_bearer(
            "GET",
            "/v1/search",
            Some(format!("Bearer {library_secret}")),
        );
        assert_eq!(ok.status, 404, "{}", ok.body);
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert!(
            rows.iter().any(|r| r.last_used_at == Some(NOW)),
            "successful bearer use records last_used_at"
        );

        // Wrong scope is the names-only 403, never the plain 404 (which
        // hides route existence for unauthenticated callers).
        let forbidden = h.call_bearer("GET", "/v1/search", Some(format!("Bearer {stream_secret}")));
        assert_eq!(forbidden.status, 403);
        assert_eq!(
            forbidden.body,
            r#"{"code":"forbidden","message":"insufficient scope","retryable":false}"#
        );

        // Presented-but-invalid bearers are the same plain 404 a
        // header-absent caller receives — including malformed shapes
        // (rejected before any HMAC/DB work) and a well-formed unknown
        // key (fails closed after the lookup). No distinct 401 may
        // disclose route existence.
        for header in [
            Some("Bearer ntk_no-such-credential-000000000000000000".to_string()),
            Some(format!("Bearer ntk_{}", "0".repeat(43))),
            Some("Bearer".to_string()),
            Some("Basic abc123".to_string()),
            Some("__Host-nntune-session=abc123".to_string()),
        ] {
            let resp = h.call_bearer("GET", "/v1/search", header);
            assert_eq!(resp.status, 404, "{}", resp.body);
            assert_eq!(
                resp.body,
                r#"{"code":"not_found","message":"not found","retryable":false}"#
            );
        }

        // No Authorization header skips the bearer gate entirely (ADR 0010:
        // session-cookie holders see no behavior change). With no data
        // routes yet this is the plain 404 — the same posture as before
        // this credential class existed — never a 401.
        let missing = h.call_bearer("GET", "/v1/search", None);
        assert_eq!(missing.status, 404, "{}", missing.body);

        // A valid session cookie with no bearer header likewise falls
        // through to the dispatch (plain 404 until the data routes land
        // with their session-or-bearer dispatch), never the bearer 401.
        let cookie_only = h.call_full(
            "GET",
            "/v1/search",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(cookie_only.status, 404, "{}", cookie_only.body);
        assert_eq!(
            cookie_only.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );

        // A presented bearer is never rescued by a session cookie: a valid
        // cookie plus an invalid bearer is still the plain 404 (uniform
        // with the header-absent posture), never a distinct 401.
        let no_rescue = Request {
            method: "GET".to_string(),
            path: "/v1/search".to_string(),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            authorization: Some("Bearer ntk_definitely-not-a-key".to_string()),
            stripe_signature: None,
            body: Vec::new(),
        };
        let no_rescue = block_on(handle(&h.ctx(), &no_rescue));
        assert_eq!(no_rescue.status, 404, "{}", no_rescue.body);

        // And a valid bearer can never mint: issuance stays session-only.
        let bearer_mint_req = Request {
            method: "POST".to_string(),
            path: "/v1/credentials".to_string(),
            cookie: None,
            origin: Some(ORIGIN.to_string()),
            authorization: Some(format!("Bearer {library_secret}")),
            stripe_signature: None,
            body: serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60})
                .to_string()
                .into_bytes(),
        };
        let bearer_mint = block_on(handle(&h.ctx(), &bearer_mint_req));
        assert_eq!(bearer_mint.status, 401, "{}", bearer_mint.body);
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert_eq!(rows.len(), 2, "bearer mint attempt stores nothing");
    }

    #[test]
    fn malformed_bearer_shape_costs_no_store_work() {
        // ADR 0010 DoS pin: the shape gate must run BEFORE the per-IP
        // budget claim and the HMAC/DB lookup. A malformed token answers
        // the plain 404 with zero `reserve_rate_limit_pair` calls and
        // zero `native_credential_by_hmac` lookups; a well-formed unknown
        // key reaches both and still fails closed. Short-circuiting the
        // `is_plausible_credential_shape` guard must fail here (both
        // counters would move on the malformed leg).
        let h = peppered_harness();
        let pair_calls = || *h.store.rate_limit_pair_calls.borrow();
        let lookups = || *h.store.native_credential_lookups.borrow();
        let (base_pairs, base_lookups) = (pair_calls(), lookups());
        // Each malformed shape: plain 404, no store work at all.
        for header in [
            Some("Bearer ntk_short".to_string()),
            Some(format!("Bearer ntk_{}", "0".repeat(42))),
            Some(format!("Bearer ntk_{}", "0".repeat(44))),
            Some("Bearer ntk_no-such-credential-000000000000000000".to_string()),
        ] {
            let resp = h.call_bearer("GET", "/v1/search", header);
            assert_eq!(resp.status, 404, "{}", resp.body);
            assert_eq!(
                resp.body,
                r#"{"code":"not_found","message":"not found","retryable":false}"#
            );
        }
        assert_eq!(
            pair_calls(),
            base_pairs,
            "malformed shapes must not claim the lookup budget"
        );
        assert_eq!(
            lookups(),
            base_lookups,
            "malformed shapes must not reach the credential lookup"
        );
        // A well-formed unknown key passes the gate and pays for the
        // lookup before failing closed — proving the counters observe the
        // gate rather than the harness.
        let unknown = h.call_bearer(
            "GET",
            "/v1/search",
            Some(format!("Bearer ntk_{}", "0".repeat(43))),
        );
        assert_eq!(unknown.status, 404, "{}", unknown.body);
        assert!(
            pair_calls() > base_pairs,
            "a well-formed key must claim the lookup budget"
        );
        assert!(
            lookups() > base_lookups,
            "a well-formed key must reach the credential lookup"
        );
    }

    #[test]
    fn bearer_store_error_collapses_to_the_plain_404() {
        // Uniform-404 pin, store-error leg: a database failure during the
        // bearer lookup must answer the same plain 404 a header-absent
        // caller receives — never a distinct 401 that reopens the
        // route-existence oracle on the error path. Reverting the lookup
        // `Err` arm to the pre-slice 401 must fail here.
        let h = peppered_harness();
        *h.store.fail_native_credential_lookup.borrow_mut() = true;
        let lookups = || *h.store.native_credential_lookups.borrow();
        let base = lookups();
        let header = Some(format!("Bearer ntk_{}", "0".repeat(43)));
        let refused = h.call_bearer("GET", "/v1/search", header);
        let absent = h.call_bearer("GET", "/v1/search", None);
        assert_eq!(refused.status, 404, "{}", refused.body);
        assert_eq!(absent.status, 404, "{}", absent.body);
        assert_eq!(
            refused.body, absent.body,
            "store-error body must equal the header-absent body"
        );
        assert_eq!(
            refused.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        assert!(refused.set_cookie.is_none(), "no session rescue/refresh");
        assert!(
            lookups() > base,
            "the failing lookup must execute (else the Err arm is unpinned)"
        );
        assert!(
            !refused.body.contains("ntk_"),
            "no credential material in refusal"
        );
    }

    #[test]
    fn bearer_lookup_budget_denial_collapses_to_the_plain_404() {
        // ADR 0010 DoS: a flood of well-formed random `ntk_` tokens shares
        // one per-IP pool claimed before any credential DB work. Exhausting
        // it denies with the SAME plain 404 as an unknown key (no
        // throttle oracle), and even a valid credential is denied while
        // the budget is exhausted.
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let minted = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 3600}),
        );
        assert_eq!(minted.status, 201, "{}", minted.body);
        let secret = mint_secret(&serde_json::from_str(&minted.body).unwrap());

        // A valid bearer passes before exhaustion (404: gate passed, no
        // data routes yet).
        let ok = h.call_bearer("GET", "/v1/search", Some(format!("Bearer {secret}")));
        assert_eq!(ok.status, 404, "{}", ok.body);

        // Exhaust the per-minute pool directly (same key/action the gate
        // claims). The passing call above already holds one slot, so fill
        // the remaining limit - 1.
        let ip_key = rate_limit_ip_key("198.51.100.9");
        for _ in 0..(BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_MIN - 1) {
            let admitted = block_on(h.store.reserve_rate_limit(
                &ip_key,
                "native_rest.bearer_lookup_ip_min",
                BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_MIN,
                NOW,
                BEARER_LOOKUP_WINDOW_SECS,
            ))
            .unwrap();
            assert!(admitted);
        }

        // Now even the valid credential is denied — with the plain 404,
        // never a 429 or any other shape.
        let denied = h.call_bearer("GET", "/v1/search", Some(format!("Bearer {secret}")));
        assert_eq!(denied.status, 404, "{}", denied.body);
        assert_eq!(
            denied.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
    }

    #[test]
    fn credentials_mint_for_a_deleting_account_is_the_typed_409() {
        // ADR 0002 rule 3: issuance is a fenced writer like `vault_put`, so
        // a fresh session during the 24 h deletion grace cannot mint a new
        // long-lived bearer path behind the marker.
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let first = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(first.status, 201, "{}", first.body);
        h.store.deleting_users.borrow_mut().insert("u1".to_string());
        let fenced = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(fenced.status, 409, "{}", fenced.body);
        assert_eq!(
            fenced.body,
            r#"{"code":"account_deleting","message":"account is being deleted","retryable":false}"#
        );
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert_eq!(rows.len(), 1, "fenced mint stores nothing");
    }

    #[test]
    fn credentials_mint_store_refusal_returns_no_secret_or_success_audit() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        *h.store.revoke_session_before_mint.borrow_mut() = true;
        let before = h.store.audits.borrow().len();
        let resp = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(resp.status, 500, "{}", resp.body);
        assert!(!resp.body.contains("secret"));
        assert!(!resp.body.contains("ntk_"));
        assert!(h.store.native_credentials.borrow().is_empty());
        assert_eq!(h.store.audits.borrow().len(), before);
    }

    #[test]
    fn session_revoke_all_failure_retains_cookie_and_writes_no_success_audit() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let minted = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(minted.status, 201, "{}", minted.body);
        let before = h.store.audits.borrow().len();
        *h.store.fail_bulk_revoke.borrow_mut() = true;
        let resp = h.call_full(
            "POST",
            "/v1/session/revoke-all",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 500, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert_eq!(h.store.audits.borrow().len(), before);
        assert!(h.store.sessions.borrow().values().all(|row| !row.revoked));
        assert!(h
            .store
            .native_credentials
            .borrow()
            .values()
            .all(|row| row.record.revoked_at.is_none()));
        *h.store.fail_bulk_revoke.borrow_mut() = false;
        let retry = h.call_full(
            "POST",
            "/v1/session/revoke-all",
            None,
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(retry.status, 200, "{}", retry.body);
        assert_eq!(retry.set_cookie, Some(session::clear_cookie_header()));
    }

    #[test]
    fn session_revoke_all_revokes_native_credentials_too() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        for scopes in [["library:read"], ["stream:read"]] {
            let resp = mint_call(
                &h,
                &cookie,
                serde_json::json!({"scopes": scopes, "ttl_secs": 3600}),
            );
            assert_eq!(resp.status, 201, "{}", resp.body);
        }
        // One row pre-revoked: the sweep transitions only the live one.
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        block_on(
            h.store
                .revoke_native_credential(&rows[0].record.id, "u1", NOW),
        )
        .unwrap();

        let resp = h.call_full(
            "POST",
            "/v1/session/revoke-all",
            None,
            Some(cookie),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert!(rows.iter().all(|r| r.record.revoked_at.is_some()));
        assert!(
            h.store
                .audits
                .borrow()
                .iter()
                .any(|e| e.action == "native_credentials_revoked_all"),
            "bulk revocation audits without key material"
        );

        // A second sweep is a quiet no-op (still 200, nothing new).
        let cookie2 = magic_login(&h, "a@example.com");
        let resp = h.call_full(
            "POST",
            "/v1/session/revoke-all",
            None,
            Some(cookie2),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
    }

    #[test]
    fn credential_diagnostics_carry_no_secret_ip_or_hmac() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let resp = mint_call(
            &h,
            &cookie,
            serde_json::json!({"label": "cli", "scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(resp.status, 201, "{}", resp.body);
        let minted: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let secret = mint_secret(&minted);
        // The mint body shows the secret exactly once, by design — and
        // nothing else sensitive travels with it.
        assert_eq!(resp.body.matches(&secret).count(), 1);
        assert!(!resp.body.contains("key_hmac"));

        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        let debug = format!("{:?}", rows[0]);
        assert!(debug.contains(&rows[0].record.id), "Debug names the id");
        assert!(debug.contains("cli"), "Debug names the label");
        for needle in [
            secret.as_str(),
            secret.trim_start_matches("ntk_"),
            rows[0].record.key_hmac.as_str(),
            "198.51.100.9",
        ] {
            assert!(!debug.contains(needle), "diagnostic leaked {needle:?}");
        }
    }

    #[test]
    fn credentials_preflight_covers_the_dynamic_revoke_path() {
        let h = peppered_harness();
        for path in ["/v1/credentials", "/v1/credentials/cred_1"] {
            let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 204, "{path}");
            assert!(resp.body.is_empty(), "{path}");
        }
        // Nested shapes are not credential ids: preflight stays 404 there.
        let resp = h.call_full("OPTIONS", "/v1/credentials/a/b", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn options_preflight_pins_session_served_and_trailing_slash_posture() {
        // Scope-map preflight pin, both legs the served-paths table leaves
        // open: session-only paths answer 204 (a lawful cookie read must
        // clear preflight), and the bare trailing-slash playlist shape
        // stays 404. Narrowing the predicate to Bearer-only must fail the
        // 204 legs; dropping the trailing-slash arm must fail the 404 leg
        // (GET would map `/v1/playlists/` via the collection prefix).
        let h = peppered_harness();
        for path in ["/v1/me", "/v1/indexers"] {
            let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 204, "{path}");
            assert!(resp.body.is_empty(), "{path}");
        }
        let resp = h.call_full("OPTIONS", "/v1/playlists/", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404, "{}", resp.body);
        assert_eq!(
            resp.body,
            r#"{"code":"not_found","message":"not found","retryable":false}"#
        );
        assert!(resp.set_cookie.is_none());
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

        let user = block_on(h.store.user_by_email("new@example.com"))
            .unwrap()
            .unwrap();
        let public_handle = start["user_id"].as_str().unwrap();
        assert_ne!(public_handle, user.id);
        assert_eq!(
            block_on(h.store.challenge_by_id(&challenge))
                .unwrap()
                .unwrap()
                .user_id
                .as_deref(),
            Some(user.id.as_str())
        );

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
        assert_eq!(
            block_on(h.store.credentials_for_user(&user.id))
                .unwrap()
                .len(),
            1
        );
        assert!(block_on(h.store.credentials_for_user(public_handle))
            .unwrap()
            .is_empty());
        assert_eq!(h.store.sessions.borrow().len(), 1);
        assert!(h
            .store
            .sessions
            .borrow()
            .values()
            .all(|session| session.user_id == user.id));

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
        let (token, record) =
            auth::issue_magic_link("victim", "victim@example.com", NOW).expect("issue");
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
    fn register_finish_insert_failure_preserves_invite_and_proofs_for_retry() {
        registration_insert_failure_retry(Some(true));
    }

    #[test]
    fn register_finish_insert_failure_preserves_optional_invite() {
        registration_insert_failure_retry(Some(false));
    }

    #[test]
    fn register_finish_insert_failure_preserves_proofs_without_invite() {
        registration_insert_failure_retry(None);
    }

    fn registration_insert_failure_retry(invite_mode: Option<bool>) {
        let mut h = harness();
        h.config.invites_required = invite_mode.unwrap_or(false);
        let invite_hash = auth::hash_invite_code("retry-invite");
        h.store.invites.borrow_mut().insert(
            invite_hash.clone(),
            InviteRecord::new_issued(
                invite_hash.clone(),
                Some("issuer-admin".to_string()),
                auth::INVITE_KIND_ADMIN,
                None,
                NOW,
                NOW + 604_800,
            ),
        );
        let start = h.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "retry@example.com"}),
        );
        assert_eq!(start.status, 200, "{}", start.body);
        let start: serde_json::Value = serde_json::from_str(&start.body).unwrap();
        let challenge = start["challenge"].as_str().unwrap();
        let emailed = h.mailer.sent.borrow().last().unwrap().1.clone();
        let magic_hash = auth::hash_magic_token(&emailed);
        let cred_id = b"colliding-credential";
        let encoded_id = b64url_encode(cred_id);
        // Another account owns this credential id: the real INSERT's
        // primary-key constraint fails after all three claims succeeded.
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: encoded_id.clone(),
            user_id: "other-user".to_string(),
            public_key: fx_es256_cose(47),
            sign_count: 0,
            registered_at: NOW,
        }))
        .unwrap();
        let att = fx_none_attestation(cred_id, &fx_es256_cose(48));
        let finish = serde_json::json!({
            "challenge": challenge,
            "credential_id": encoded_id,
            "attestation_object": b64url_encode(&att),
            "client_data_json": b64url_encode(
                fx_client_json("webauthn.create", challenge).as_bytes()
            ),
            "magic_token": emailed,
            "invite_code": invite_mode.map(|_| "retry-invite"),
        });
        let resp = h.call("POST", "/v1/auth/passkey/register/finish", finish.clone());
        assert_eq!(resp.status, 401, "{}", resp.body);
        assert!(resp.set_cookie.is_none());
        assert!(
            !h.store.invites.borrow()[&invite_hash].used,
            "failed credential insertion must not spend the invite"
        );
        assert!(!h.store.challenges.borrow()[challenge].challenge.consumed);
        assert!(!h.store.magic.borrow()[&magic_hash].used);
        assert!(h.store.sessions.borrow().is_empty());
        assert_eq!(
            h.store.credentials.borrow()[&encoded_id].user_id,
            "other-user"
        );

        // Once the insertion fault is gone, the SAME request can succeed.
        h.store.credentials.borrow_mut().remove(&encoded_id);
        let resp = h.call("POST", "/v1/auth/passkey/register/finish", finish.clone());
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(resp.set_cookie.is_some());
        assert_eq!(
            h.store.invites.borrow()[&invite_hash].used,
            invite_mode.is_some()
        );
        assert!(h.store.challenges.borrow()[challenge].challenge.consumed);
        assert!(h.store.magic.borrow()[&magic_hash].used);
        assert_eq!(h.store.credentials.borrow().len(), 1);
        let replay = h.call("POST", "/v1/auth/passkey/register/finish", finish);
        assert_eq!(replay.status, 401);
        assert!(replay.set_cookie.is_none());
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
            InviteRecord::new_issued(
                good_hash.clone(),
                Some("issuer-admin".to_string()),
                auth::INVITE_KIND_ADMIN,
                None,
                NOW,
                NOW + 604_800,
            ),
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

    #[test]
    fn register_start_public_handle_survives_account_cleanup() {
        let h = harness();
        seed_user(&h, "known", "known@example.com");
        seed_user(&h, "resumed", "resumed@example.com");
        block_on(h.store.insert_credential(&PasskeyCredential {
            cred_id_b64url: b64url_encode(b"known-cred"),
            user_id: "known".to_string(),
            public_key: fx_es256_cose(55),
            sign_count: 0,
            registered_at: NOW,
        }))
        .unwrap();
        let start_at = |email: &str, now| {
            let mut ctx = h.ctx();
            ctx.now = now;
            let req = Request {
                method: "POST".to_string(),
                path: "/v1/auth/passkey/register/start".to_string(),
                cookie: None,
                origin: Some(ORIGIN.to_string()),
                authorization: None,
                stripe_signature: None,
                body: serde_json::json!({"email": email}).to_string().into_bytes(),
            };
            let resp = block_on(handle(&ctx, &req));
            assert_eq!(resp.status, 200, "{}", resp.body);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(body.as_object().unwrap().len(), 3);
            assert_eq!(body["rp_id"], h.config.rp_id);
            assert_eq!(
                b64url_decode(body["user_id"].as_str().unwrap())
                    .unwrap()
                    .len(),
                16
            );
            assert!(valid_challenge_id(body["challenge"].as_str().unwrap()));
            body
        };
        let emails = [
            "fresh@example.com",
            "resumed@example.com",
            "known@example.com",
        ];
        let before: Vec<_> = emails.iter().map(|email| start_at(email, NOW)).collect();
        let internal_ids: Vec<_> = emails
            .iter()
            .map(|email| block_on(h.store.user_by_email(email)).unwrap().unwrap().id)
            .collect();
        for (body, internal_id) in before.iter().zip(&internal_ids) {
            assert_ne!(body["user_id"].as_str().unwrap(), internal_id);
        }
        assert_ne!(before[0]["user_id"], before[1]["user_id"]);
        assert_ne!(before[0]["user_id"], before[2]["user_id"]);
        assert_eq!(h.mailer.sent.borrow().len(), 2);

        let later = NOW + UNVERIFIED_USER_TTL_SECS + 1;
        start_at("maintenance@example.com", later);
        for email in &emails[..2] {
            assert!(block_on(h.store.user_by_email(email)).unwrap().is_none());
        }
        assert_eq!(
            block_on(h.store.user_by_email(emails[2]))
                .unwrap()
                .unwrap()
                .id,
            internal_ids[2]
        );
        for (index, email) in emails.iter().enumerate() {
            let after = start_at(email, later);
            assert_eq!(before[index]["user_id"], after["user_id"]);
            assert_ne!(before[index]["challenge"], after["challenge"]);
            let internal_id = block_on(h.store.user_by_email(email)).unwrap().unwrap().id;
            let row = block_on(
                h.store
                    .challenge_by_id(after["challenge"].as_str().unwrap()),
            )
            .unwrap();
            if index < 2 {
                assert_ne!(internal_id, internal_ids[index]);
                assert_eq!(row.unwrap().user_id.as_deref(), Some(internal_id.as_str()));
            } else {
                assert!(row.is_none());
            }
        }
        let normalized = start_at("  FRESH@EXAMPLE.COM  ", later);
        assert_eq!(normalized["user_id"], before[0]["user_id"]);
        assert!(h
            .mailer
            .sent
            .borrow()
            .iter()
            .all(|(email, _)| email != emails[2]));
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
                assert_eq!(
                    resp.body,
                    crate::api_error::ApiError::rate_limited("too many requests").body_json(),
                    "iteration {i}"
                );
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
    fn qa_bearer_skips_per_ip_start_and_mail_budgets_on_armed_staging() {
        // TWO-434: every journey spec shares one CI runner IP, so the
        // staged QA bearer on an armed staging posture is admitted past
        // the per-IP start budget and, for synthetic mailboxes, the
        // per-IP mail cap. Exempt starts spend nothing: an unexempt
        // caller from the same IP still gets its full budget afterwards.
        let h = qa_harness();
        let bearer = format!("Bearer {QA_TOKEN}");
        let total = AUTH_STARTS_PER_IP_PER_HOUR + 10;
        for i in 0..total {
            let resp = h.call_authorized(
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("journey{i}@example.test")}),
                Some(bearer.as_str()),
            );
            assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
        }
        for i in 0..total {
            let resp = h.call_authorized(
                "/v1/auth/magic/start",
                serde_json::json!({"email": format!("journey{i}@example.test")}),
                Some(bearer.as_str()),
            );
            assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
        }
        assert_eq!(
            h.mailer.sent.borrow().len(),
            2 * total as usize,
            "synthetic mailboxes are mailed past the per-IP mail cap"
        );
        for i in 0..=AUTH_STARTS_PER_IP_PER_HOUR {
            let resp = h.call(
                "POST",
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("plain{i}@example.test")}),
            );
            if i < AUTH_STARTS_PER_IP_PER_HOUR {
                assert_eq!(resp.status, 200, "iteration {i}: {}", resp.body);
            } else {
                assert_eq!(
                    resp.status, 429,
                    "unexempt budget still binds: {}",
                    resp.body
                );
            }
        }
    }

    #[test]
    fn qa_bearer_exemption_never_covers_routable_mailboxes() {
        // A leaked staging token must not create unbounded real-address
        // rows or email-bomb real addresses: outside `example.test` the
        // bearer spends the ordinary per-IP start budget, so user rows,
        // challenges and mail all stop at it.
        let h = qa_harness();
        let bearer = format!("Bearer {QA_TOKEN}");
        let total = AUTH_STARTS_PER_IP_PER_HOUR + 10;
        for i in 0..total {
            let resp = h.call_authorized(
                "/v1/auth/passkey/register/start",
                serde_json::json!({"email": format!("victim{i}@example.com")}),
                Some(bearer.as_str()),
            );
            let expected = if i < AUTH_STARTS_PER_IP_PER_HOUR {
                200
            } else {
                429
            };
            assert_eq!(resp.status, expected, "iteration {i}: {}", resp.body);
        }
        assert_eq!(
            h.store.users.borrow().len(),
            AUTH_STARTS_PER_IP_PER_HOUR as usize,
            "no routable user row past the per-IP start budget"
        );
        assert_eq!(
            h.store.challenges.borrow().len(),
            AUTH_STARTS_PER_IP_PER_HOUR as usize,
            "no routable challenge past the per-IP start budget"
        );
        assert!(
            h.mailer.sent.borrow().len() <= MAIL_PER_IP_PER_HOUR as usize,
            "routable mailboxes stay under the per-IP mail cap"
        );
    }

    #[test]
    fn qa_rate_limit_exemption_requires_armed_posture_and_exact_bearer() {
        // Production (flag off), a flag armed without a staged token, a
        // wrong or truncated bearer, and the right token without the
        // `Bearer ` scheme all keep the ordinary per-IP start budget.
        let right = format!("Bearer {QA_TOKEN}");
        let armed = || cfg().with_qa_code_seam(true).with_qa_auth_token(QA_TOKEN);
        let cases = [
            (
                "production",
                cfg().with_qa_auth_token(QA_TOKEN),
                right.as_str(),
            ),
            (
                "flag without token",
                cfg().with_qa_code_seam(true),
                right.as_str(),
            ),
            ("wrong bearer", armed(), "Bearer wrong-token"),
            ("truncated bearer", armed(), "Bearer test-qa-bearer-toke"),
            ("no scheme", armed(), QA_TOKEN),
        ];
        for (label, config, authorization) in cases {
            let mut h = harness();
            h.config = config;
            for i in 0..=AUTH_STARTS_PER_IP_PER_HOUR {
                let resp = h.call_authorized(
                    "/v1/auth/passkey/register/start",
                    serde_json::json!({"email": format!("journey{i}@example.test")}),
                    Some(authorization),
                );
                let expected = if i < AUTH_STARTS_PER_IP_PER_HOUR {
                    200
                } else {
                    429
                };
                assert_eq!(
                    resp.status, expected,
                    "{label}, iteration {i}: {}",
                    resp.body
                );
            }
        }
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
        assert_eq!(
            taken_denied.body,
            crate::api_error::ApiError::rate_limited("too many requests").body_json()
        );
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
        assert_eq!(
            denied.body,
            crate::api_error::ApiError::rate_limited("too many requests").body_json()
        );
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
        // unnamed `query_typed` (TWO-434: a named statement fails through
        // Hyperdrive), not a divergent literal or no guard at all. Deleting
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
        let sql_pos = code
            .find("handlers::RATE_LIMIT_EXHAUSTED_SQL")
            .expect("Pg passes the shared exhausted SQL");
        let call_pos = code[..sql_pos]
            .rfind(".query_typed(")
            .expect("Pg fast-fail runs via unnamed query_typed");
        assert!(
            sql_pos - call_pos < 120,
            "Pg fast-fail must run the shared SQL via unnamed query_typed"
        );
        assert!(
            !code.contains("SELECT COUNT(*) FROM auth_rate_limit_slots"),
            "Pg must not carry a divergent fast-fail literal"
        );
    }

    #[test]
    fn rate_limit_claim_sql_is_single_statement() {
        // Regression pin for the bearer hourly scan: the production claim
        // must select a free slot pool-wide in one statement
        // (`generate_series` anti-join), never one upsert per occupied
        // slot. A sequential per-slot loop makes the Nth admitted request
        // pay N conflicting upserts inside one transaction (up to ~3300
        // for the bearer pair near the hourly cap), hitting the store
        // deadline as a 404 before the budget is used up. Reintroducing
        // the loop leaves every behavioral test green (fakes claim
        // in-memory), so this shape test observes the production path
        // directly at compile time. Comments are stripped first: the
        // explanatory comment naming the technique must not satisfy the
        // pin on its own.
        let entry = include_str!("worker_entry.rs");
        let code = strip_line_comments(entry);
        let claim_pos = code
            .find("async fn claim_rate_limit_slot")
            .expect("Pg claim fn exists");
        let claim_end = code[claim_pos..]
            .find("\n}\n")
            .map(|i| claim_pos + i)
            .expect("Pg claim fn end");
        let body = &code[claim_pos..claim_end];
        assert!(
            body.contains("generate_series"),
            "Pg claim must select a free slot with a generate_series anti-join"
        );
        assert!(
            !body.contains("for slot in 0.."),
            "Pg claim must not scan slots one upsert at a time"
        );
        assert!(
            body.contains("RATE_LIMIT_CLAIM_ATTEMPTS"),
            "Pg claim retries must stay bounded and fail closed"
        );
        // Contention pin: overlapping claims must not all queue on the
        // lowest free slot (K simultaneous claims need K attempts for the
        // last finisher, so 5 parallel bearer lookups on a fresh pool
        // exhaust a bound of 4 with 2996 slots free). Each attempt must
        // rotate its search start. Reverting to `ORDER BY s LIMIT 1`
        // leaves every behavioral test green (fakes claim in-memory),
        // so this shape test observes the production path directly.
        assert!(
            body.contains("CLAIM_DISPERSAL"),
            "Pg claim must rotate the free-slot search start per attempt"
        );
        assert!(
            !body.contains("ORDER BY s LIMIT 1"),
            "Pg claim must not force every contender onto the lowest free slot"
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
        assert_eq!(
            denied.body,
            crate::api_error::ApiError::rate_limited("too many requests").body_json()
        );
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
    fn expired_magic_links_are_pruned_per_user_on_issue() {
        // Live accounts are never reaped, so a victim's expired links must not
        // accumulate: each issuance drops only that user's expired rows.
        // Fixtures distinguish both predicates: pruning by user alone would
        // drop `live_unexpired`, pruning all expired rows would drop
        // `expired_other`, and not pruning would leave the 50 spaced rows.
        let h = harness();
        let spacing = auth::MAGIC_LINK_TTL_SECS + 1;
        let (_, expired_other) =
            auth::issue_magic_link("other", "other@example.com", NOW).expect("issue");
        block_on(h.store.insert_magic(&expired_other)).unwrap();
        let mut first_live_hash = String::new();
        for i in 0..50u64 {
            let (_, record) = auth::issue_magic_link("live", "live@example.com", NOW + i * spacing)
                .expect("issue");
            if i == 0 {
                first_live_hash.clone_from(&record.token_hash);
            }
            block_on(h.store.insert_magic(&record)).unwrap();
        }
        let (_, live_unexpired) =
            auth::issue_magic_link("live", "live@example.com", NOW + 50 * spacing).expect("issue");
        block_on(h.store.insert_magic(&live_unexpired)).unwrap();
        let (_, newest_live) =
            auth::issue_magic_link("live", "live@example.com", NOW + 50 * spacing + 1)
                .expect("issue");
        block_on(h.store.insert_magic(&newest_live)).unwrap();
        let rows = h.store.magic.borrow();
        assert_eq!(
            rows.values().filter(|r| r.user_id == "live").count(),
            2,
            "only unexpired live links survive issuance"
        );
        assert!(
            !rows.contains_key(&first_live_hash),
            "the issuing user's expired link is pruned"
        );
        assert!(
            rows.contains_key(&live_unexpired.token_hash),
            "the issuing user's unexpired link is retained"
        );
        assert!(
            rows.contains_key(&newest_live.token_hash),
            "the newest live link is inserted"
        );
        assert!(
            rows.contains_key(&expired_other.token_hash),
            "another user's expired link is untouched"
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
            ("deleting", "deleting@example.com"),
            ("admin-stub", "admin-stub@example.com"),
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
        h.store
            .deleting_users
            .borrow_mut()
            .insert("deleting".to_string());
        // A promoted stub account (admin role, but no credential,
        // session, vault or live proof) is never unverified-signup
        // garbage: reaping it would delete an admin.
        h.store.user_roles.borrow_mut().insert(
            "admin-stub".to_string(),
            crate::admin::ROLE_ADMIN.to_string(),
        );

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
        assert!(block_on(h.store.user_by_email("deleting@example.com"))
            .unwrap()
            .is_some());
        assert!(
            block_on(h.store.user_by_email("admin-stub@example.com"))
                .unwrap()
                .is_some(),
            "an admin account must survive the unverified-user reaper"
        );
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
        assert!(
            sql.contains("deleting_at IS NULL"),
            "reap must never delete an account already scheduled for \
             deletion (its delete_operations RESTRICT FK owns it)"
        );
        assert!(
            sql.contains("users.role = 'member'"),
            "reap must never delete an admin account, however stub-like"
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
            auth::issue_magic_link("magic-only", "magic-only@example.com", NOW).expect("issue");
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
        h.store
            .legacy_vault_users
            .borrow_mut()
            .insert("magic-only".to_string());
        let (_, inbox_record) =
            auth::issue_magic_link("inbox-only", "inbox-only@example.com", NOW).expect("issue");
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
            h.store.legacy_vault_users.borrow().contains("magic-only"),
            "surviving legacy vault marker must not cascade-delete"
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
                stripe: &h.stripe,
            };
            let req = Request {
                method: "GET".to_string(),
                path: "/v1/plans".to_string(),
                cookie: Some(cookie.clone()),
                origin: None,
                authorization: None,
                stripe_signature: None,
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
            stripe: &h.stripe,
        };
        let req = Request {
            method: "POST".to_string(),
            path: "/v1/session/refresh".to_string(),
            cookie: Some(cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            stripe_signature: None,
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
            let (token, record) = auth::issue_magic_link(user_id, email, NOW).expect("issue");
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
            InviteRecord::new_issued(
                good_hash.clone(),
                Some("issuer-admin".to_string()),
                auth::INVITE_KIND_ADMIN,
                None,
                NOW,
                NOW + 604_800,
            ),
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
            InviteRecord::new_issued(
                good_hash_2,
                Some("issuer-admin".to_string()),
                auth::INVITE_KIND_ADMIN,
                None,
                NOW,
                NOW + 604_800,
            ),
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
        let (token, record) = auth::issue_magic_link("u1", "a@example.com", NOW).expect("issue");
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
        let (token2, record2) = auth::issue_magic_link("u1", "a@example.com", NOW).expect("issue");
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
        let (token, record) = auth::issue_magic_link("u1", "a@example.com", NOW).expect("issue");
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
    fn healthz_reports_only_ok_and_build_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        // Unset var: explicit marker, never a missing key.
        let h = harness();
        let resp = h.call_full("GET", "/healthz", None, None, None);
        assert_eq!(resp.status, 200);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"ok": true, "build_sha": "unknown"})
        );
        // Valid SHA passes through; the body carries exactly two keys.
        let mut h = harness();
        h.config = h.config.with_build_sha(Some(sha));
        let resp = h.call_full("GET", "/healthz", None, None, None);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body, serde_json::json!({"ok": true, "build_sha": sha}));
        // Anything else is dropped, never echoed.
        let upper = sha.to_uppercase();
        for raw in ["", "abc123", upper.as_str(), "<script>", "secret-value"] {
            let mut h = harness();
            h.config = h.config.with_build_sha(Some(raw));
            let resp = h.call_full("GET", "/healthz", None, None, None);
            let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(
                body,
                serde_json::json!({"ok": true, "build_sha": "unknown"}),
                "{raw}"
            );
        }
    }

    const CAT_REC_A: &str = "11111111-2222-3333-4444-555555555555";
    const CAT_REC_B: &str = "11111111-2222-3333-4444-555555555556";
    const CAT_RELEASE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const CAT_ARTIST: &str = "ffffffff-1111-2222-3333-444444444444";

    fn matched_library_row(job: &str, rec: &str, ord: i64) -> crate::catalogue::LibraryDbRow {
        crate::catalogue::LibraryDbRow {
            job_id: job.to_string(),
            release_mbid: Some(CAT_RELEASE.to_string()),
            recording_mbids: vec![CAT_REC_A.to_string(), CAT_REC_B.to_string()],
            recording_mbid: Some(rec.to_string()),
            recording_ord: ord,
            recording_title: Some(format!("Track {ord}")),
            duration_ms: Some(200_000),
            medium_no: Some(1),
            track_no: Some(ord as i16),
            release_title: Some("Catalogue Fixture".to_string()),
            artist_mbid: Some(CAT_ARTIST.to_string()),
            artist_name: Some("Fixture Artist".to_string()),
        }
    }

    fn unmatched_library_row(job: &str) -> crate::catalogue::LibraryDbRow {
        crate::catalogue::LibraryDbRow {
            job_id: job.to_string(),
            release_mbid: None,
            recording_mbids: Vec::new(),
            recording_mbid: None,
            recording_ord: 0,
            recording_title: None,
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: None,
            artist_mbid: None,
            artist_name: None,
        }
    }

    fn seed_catalogue(h: &Harness) {
        h.store
            .catalogue_artists
            .borrow_mut()
            .push(crate::catalogue::ArtistDbRow {
                mbid: CAT_ARTIST.to_string(),
                name: "Fixture Artist".to_string(),
                sort_name: "Artist, Fixture".to_string(),
                artist_type: Some("Group".to_string()),
                country: Some("GB".to_string()),
            });
        h.store
            .catalogue_releases
            .borrow_mut()
            .push(crate::catalogue::ReleaseDbRow {
                mbid: CAT_RELEASE.to_string(),
                title: "Catalogue Fixture".to_string(),
                release_type: Some("Album".to_string()),
                year: Some(1983),
                artist_mbid: Some(CAT_ARTIST.to_string()),
                artist_name: Some("Fixture Artist".to_string()),
            });
        h.store
            .catalogue_tracks
            .borrow_mut()
            .push(crate::catalogue::TrackDbRow {
                release_mbid: CAT_RELEASE.to_string(),
                recording_mbid: CAT_REC_A.to_string(),
                title: "Track 1".to_string(),
                duration_ms: Some(200_000),
                medium_no: Some(1),
                track_no: Some(1),
                release_title: "Catalogue Fixture".to_string(),
                artist_mbid: Some(CAT_ARTIST.to_string()),
                artist_name: Some("Fixture Artist".to_string()),
            });
        h.store.catalogue_release_tracks.borrow_mut().insert(
            CAT_RELEASE.to_string(),
            vec![crate::catalogue::ReleaseTrackDbRow {
                recording_mbid: CAT_REC_A.to_string(),
                title: "Track 1".to_string(),
                duration_ms: Some(200_000),
                medium_no: Some(1),
                track_no: Some(1),
            }],
        );
    }

    fn seed_library(h: &Harness, user: &str) {
        h.store.catalogue_library.borrow_mut().insert(
            user.to_string(),
            vec![
                matched_library_row("job-1", CAT_REC_A, 1),
                matched_library_row("job-1", CAT_REC_B, 2),
                unmatched_library_row("job-2"),
            ],
        );
    }

    #[test]
    fn catalogue_library_serves_m1_row_for_session_caller() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        seed_library(&h, "u1");
        let resp = h.call_full("GET", "/v1/library", None, Some(cookie), Some(ORIGIN));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let items: Vec<serde_json::Value> = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(items.len(), 3);
        // M1 row: string id, non-empty recording_mbids, release_mbid, plus
        // the rich enrichment the web library renders.
        assert_eq!(items[0]["id"], CAT_REC_A);
        assert_eq!(
            items[0]["recording_mbids"],
            serde_json::json!([CAT_REC_A, CAT_REC_B])
        );
        assert_eq!(items[0]["release_mbid"], CAT_RELEASE);
        assert_eq!(items[0]["title"], "Track 1");
        assert_eq!(items[0]["artist"], "Fixture Artist");
        // The unmatched upload stays visible under its job id with the
        // by-design empty array.
        assert_eq!(items[2]["id"], "job-2");
        assert_eq!(items[2]["recording_mbids"], serde_json::json!([]));
        assert!(items[2].get("release_mbid").is_none_or(|v| v.is_null()));
    }

    #[test]
    fn catalogue_library_alias_preserves_unenriched_app_fields() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let page: Vec<_> = [1, 2]
            .into_iter()
            .map(|ord| crate::catalogue::LibraryPageRow {
                job_id: "job-1".into(),
                release_mbid: Some(CAT_RELEASE.into()),
                recording_mbids: vec![CAT_REC_A.into(), CAT_REC_A.into()],
                recording_mbid: Some(CAT_REC_A.into()),
                recording_ord: ord,
            })
            .collect();
        h.store.catalogue_library.borrow_mut().insert(
            "u1".into(),
            crate::catalogue::assemble_library_rows(&page, &[], &[], &[]),
        );
        let v1 = h.call_full(
            "GET",
            "/v1/library",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        let alias = h.call_full("GET", "/api/library", None, Some(cookie), Some(ORIGIN));
        assert_eq!(v1.status, 200);
        assert_eq!(alias.status, 200);
        assert_eq!(v1.body, alias.body);
        let rows: Vec<serde_json::Value> = serde_json::from_str(&v1.body).unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0]["occurrence_key"], rows[1]["occurrence_key"]);
        for row in rows {
            assert_eq!(row["id"], CAT_REC_A);
            assert_eq!(row["release_mbid"], CAT_RELEASE);
            assert_eq!(
                row["recording_mbids"],
                serde_json::json!([CAT_REC_A, CAT_REC_A])
            );
            for field in [
                "title",
                "artist",
                "artist_mbid",
                "release",
                "duration_ms",
                "medium_no",
                "track_no",
            ] {
                assert!(
                    row.get(field).is_none_or(|value| value.is_null()),
                    "{field}"
                );
            }
        }
    }

    #[test]
    fn catalogue_library_alias_serves_identical_entries() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        seed_library(&h, "u1");
        let v1 = h.call_full(
            "GET",
            "/v1/library",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        let alias = h.call_full("GET", "/api/library", None, Some(cookie), Some(ORIGIN));
        assert_eq!(v1.status, 200, "{}", v1.body);
        assert_eq!(alias.status, 200, "{}", alias.body);
        assert_eq!(
            alias.body, v1.body,
            "alias must never reshape the M1 contract"
        );
    }

    #[test]
    fn catalogue_routes_require_authentication() {
        let h = harness();
        for path in [
            "/v1/library",
            "/api/library",
            "/v1/artists",
            "/v1/releases",
            "/v1/tracks",
            &format!("/v1/releases/{CAT_RELEASE}"),
        ] {
            let resp = h.call_full("GET", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 401, "{path}");
            assert_eq!(
                resp.body, r#"{"error":"check your email and try again"}"#,
                "{path}"
            );
        }
    }

    #[test]
    fn catalogue_library_bearer_serves_without_cookie() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let minted = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 3600}),
        );
        assert_eq!(minted.status, 201, "{}", minted.body);
        let secret = mint_secret(&serde_json::from_str(&minted.body).unwrap());
        seed_library(&h, "u1");
        // The gate-authorized bearer reaches the data route with no session
        // cookie and no second gate spend. (`/api/library` sits outside
        // the bearer scope map, so a native-REST bearer there is the
        // alias handler's 401 — pinned by
        // `catalogue_library_alias_session_transports` below.)
        let resp = h.call_bearer("GET", "/v1/library", Some(format!("Bearer {secret}")));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let items: Vec<serde_json::Value> = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["id"], CAT_REC_A);
        // Wrong scope stays the names-only 403 (the gate owns it).
        let stream_minted = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": ["stream:read"], "ttl_secs": 3600}),
        );
        let stream_secret = mint_secret(&serde_json::from_str(&stream_minted.body).unwrap());
        let forbidden = h.call_bearer(
            "GET",
            "/v1/library",
            Some(format!("Bearer {stream_secret}")),
        );
        assert_eq!(forbidden.status, 403, "{}", forbidden.body);
    }

    #[test]
    fn catalogue_library_alias_session_transports() {
        // The legacy alias is outside the bearer scope map (its handler
        // owns auth, the `/api/ingest` precedent): the session cookie and
        // the session-as-Bearer header both serve the same entries as
        // `/v1/library`, while a presented native-REST bearer fails closed
        // with the generic 401 — never the gate's 404, never a rescue by
        // the cookie.
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        seed_library(&h, "u1");
        let session = cookie
            .split_once('=')
            .map(|(_, value)| value)
            .expect("login sets a session pair");
        // Session-as-Bearer with no cookie serves the identical entries.
        let via_bearer = h.call_bearer("GET", "/api/library", Some(format!("Bearer {session}")));
        assert_eq!(via_bearer.status, 200, "{}", via_bearer.body);
        let via_cookie = h.call_full(
            "GET",
            "/api/library",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(via_cookie.status, 200, "{}", via_cookie.body);
        assert_eq!(
            via_bearer.body, via_cookie.body,
            "alias must serve one shape on both session transports"
        );
        let items: Vec<serde_json::Value> = serde_json::from_str(&via_bearer.body).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["id"], CAT_REC_A);
        // A native-REST credential is not a session: 401 even when the
        // shape is well-formed, even beside a valid cookie (no rescue).
        for authorization in [
            Some(format!("Bearer ntk_{}", "0".repeat(43))),
            Some("Bearer ntk_short".to_string()),
        ] {
            let refused = h.call_bearer("GET", "/api/library", authorization);
            assert_eq!(refused.status, 401, "{}", refused.body);
            assert_eq!(
                refused.body, r#"{"error":"check your email and try again"}"#,
                "alias keeps the generic session 401"
            );
        }
        let no_rescue = Request {
            method: "GET".to_string(),
            path: "/api/library".to_string(),
            cookie: Some(cookie),
            origin: Some(ORIGIN.to_string()),
            authorization: Some(format!("Bearer ntk_{}", "0".repeat(43))),
            stripe_signature: None,
            body: Vec::new(),
        };
        let no_rescue = block_on(handle(&h.ctx(), &no_rescue));
        assert_eq!(no_rescue.status, 401, "{}", no_rescue.body);
    }

    #[test]
    fn catalogue_library_is_per_user() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        seed_user(&h, "u2", "b@example.com");
        let cookie_b = magic_login(&h, "b@example.com");
        seed_library(&h, "u1");
        let resp = h.call_full("GET", "/v1/library", None, Some(cookie_b), Some(ORIGIN));
        assert_eq!(resp.status, 200, "{}", resp.body);
        let items: Vec<serde_json::Value> = serde_json::from_str(&resp.body).unwrap();
        assert!(
            items.is_empty(),
            "u1 rows must never reach u2: {}",
            resp.body
        );
    }

    #[test]
    fn catalogue_collections_serve_rich_entries() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        seed_catalogue(&h);
        seed_library(&h, "u1");
        let artists = h.call_full(
            "GET",
            "/v1/artists",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(artists.status, 200, "{}", artists.body);
        let artists: Vec<serde_json::Value> = serde_json::from_str(&artists.body).unwrap();
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0]["mbid"], CAT_ARTIST);
        assert_eq!(artists[0]["type"], "Group");
        let releases = h.call_full(
            "GET",
            "/v1/releases",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(releases.status, 200, "{}", releases.body);
        let releases: Vec<serde_json::Value> = serde_json::from_str(&releases.body).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0]["mbid"], CAT_RELEASE);
        assert_eq!(releases[0]["artist"], "Fixture Artist");
        assert_eq!(
            releases[0]["art_url"],
            format!("/v1/releases/{CAT_RELEASE}/art")
        );
        let tracks = h.call_full("GET", "/v1/tracks", None, Some(cookie), Some(ORIGIN));
        assert_eq!(tracks.status, 200, "{}", tracks.body);
        let tracks: Vec<serde_json::Value> = serde_json::from_str(&tracks.body).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["mbid"], CAT_REC_A);
        assert_eq!(tracks[0]["track_no"], 1);
        assert_eq!(tracks[0]["medium_no"], 1);
        assert_eq!(tracks[0]["duration_ms"], 200_000);
        // Own-origin art only: no third-party URL may ever serialize.
        for body in [&releases, &tracks] {
            assert!(
                !body[0].to_string().contains("http"),
                "art URL must stay own-origin: {}",
                body[0]
            );
        }
    }

    #[test]
    fn catalogue_nested_release_gets_spend_no_bearer_budget_or_lookup() {
        let h = peppered_harness();
        let before_pairs = *h.store.rate_limit_pair_calls.borrow();
        let before_lookups = *h.store.native_credential_lookups.borrow();
        let unknown = format!("Bearer ntk_{}", "0".repeat(43));
        for path in [
            "/v1/releases/".to_string(),
            format!("/v1/releases/{CAT_RELEASE}/art"),
            format!("/v1/releases/{CAT_RELEASE}/tracks/extra"),
            "/v1/releases//".to_string(),
        ] {
            for header in [
                None,
                Some("Bearer ntk_short".to_string()),
                Some(unknown.clone()),
            ] {
                let resp = h.call_bearer("GET", &path, header);
                assert_eq!(resp.status, 404, "{path}: {}", resp.body);
                assert_eq!(
                    resp.body,
                    r#"{"code":"not_found","message":"not found","retryable":false}"#
                );
            }
        }
        assert_eq!(*h.store.rate_limit_pair_calls.borrow(), before_pairs);
        assert_eq!(*h.store.native_credential_lookups.borrow(), before_lookups);
        // A served single-segment shape proves the harness observes the gate.
        let resp = h.call_bearer("GET", &format!("/v1/releases/{CAT_RELEASE}"), Some(unknown));
        assert_eq!(resp.status, 404);
        assert!(*h.store.rate_limit_pair_calls.borrow() > before_pairs);
        assert!(*h.store.native_credential_lookups.borrow() > before_lookups);
    }

    #[test]
    fn catalogue_release_detail_round_trip_and_404s() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        seed_catalogue(&h);
        seed_library(&h, "u1");
        let resp = h.call_full(
            "GET",
            &format!("/v1/releases/{CAT_RELEASE}"),
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let detail: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(detail["release"]["mbid"], CAT_RELEASE);
        assert_eq!(detail["tracks"].as_array().unwrap().len(), 1);
        assert_eq!(detail["tracks"][0]["mbid"], CAT_REC_A);
        // Unknown, malformed, empty, and nested shapes are one plain 404 —
        // no oracle distinguishes them.
        for path in [
            "/v1/releases/22222222-3333-4444-5555-666666666666".to_string(),
            "/v1/releases/not-an-mbid".to_string(),
            "/v1/releases/".to_string(),
            format!("/v1/releases/{CAT_RELEASE}/art"),
            format!("/v1/releases/{CAT_RELEASE}/tracks/extra"),
        ] {
            let resp = h.call_full("GET", &path, None, Some(cookie.clone()), Some(ORIGIN));
            assert_eq!(resp.status, 404, "{path}: {}", resp.body);
            assert_eq!(
                resp.body, r#"{"code":"not_found","message":"not found","retryable":false}"#,
                "{path}"
            );
        }
    }

    #[test]
    fn catalogue_collections_are_member_scoped() {
        const OTHER_RELEASE: &str = "bbbbbbbb-2222-3333-4444-555555555555";
        const OTHER_ARTIST: &str = "eeeeeeee-2222-3333-4444-555555555555";
        const OTHER_REC: &str = "22222222-3333-4444-5555-666666666666";
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        seed_user(&h, "u2", "b@example.com");
        let cookie = magic_login(&h, "a@example.com");
        let cookie_b = magic_login(&h, "b@example.com");
        seed_catalogue(&h);
        // A second global release the caller never ingested: the member
        // caller must never see it, and the caller with no ledger at all
        // sees empty pages plus a 404 detail.
        h.store
            .catalogue_artists
            .borrow_mut()
            .push(crate::catalogue::ArtistDbRow {
                mbid: OTHER_ARTIST.to_string(),
                name: "Other Artist".to_string(),
                sort_name: "Artist, Other".to_string(),
                artist_type: Some("Person".to_string()),
                country: Some("US".to_string()),
            });
        h.store
            .catalogue_releases
            .borrow_mut()
            .push(crate::catalogue::ReleaseDbRow {
                mbid: OTHER_RELEASE.to_string(),
                title: "Other Release".to_string(),
                release_type: Some("Album".to_string()),
                year: Some(1991),
                artist_mbid: Some(OTHER_ARTIST.to_string()),
                artist_name: Some("Other Artist".to_string()),
            });
        h.store
            .catalogue_tracks
            .borrow_mut()
            .push(crate::catalogue::TrackDbRow {
                release_mbid: OTHER_RELEASE.to_string(),
                recording_mbid: OTHER_REC.to_string(),
                title: "Other Track".to_string(),
                duration_ms: Some(180_000),
                medium_no: Some(1),
                track_no: Some(1),
                release_title: "Other Release".to_string(),
                artist_mbid: Some(OTHER_ARTIST.to_string()),
                artist_name: Some("Other Artist".to_string()),
            });
        // A same-recording cross-release occurrence: the recording is a
        // member recording, but the occurrence is not a member pair.
        h.store
            .catalogue_tracks
            .borrow_mut()
            .push(crate::catalogue::TrackDbRow {
                release_mbid: OTHER_RELEASE.to_string(),
                recording_mbid: CAT_REC_A.to_string(),
                title: "Other Occurrence".to_string(),
                duration_ms: Some(200_000),
                medium_no: Some(1),
                track_no: Some(2),
                release_title: "Other Release".to_string(),
                artist_mbid: Some(OTHER_ARTIST.to_string()),
                artist_name: Some("Other Artist".to_string()),
            });
        seed_library(&h, "u1");
        for path in ["/v1/artists", "/v1/releases", "/v1/tracks"] {
            let resp = h.call_full("GET", path, None, Some(cookie.clone()), Some(ORIGIN));
            assert_eq!(resp.status, 200, "{path}: {}", resp.body);
            let items: Vec<serde_json::Value> = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(items.len(), 1, "{path}: {}", resp.body);
            assert!(
                !resp.body.contains(OTHER_RELEASE) && !resp.body.contains(OTHER_ARTIST),
                "{path}: non-member rows must never reach a member: {}",
                resp.body
            );
        }
        let forbidden = h.call_full(
            "GET",
            &format!("/v1/releases/{OTHER_RELEASE}"),
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(forbidden.status, 404, "{}", forbidden.body);
        for path in ["/v1/artists", "/v1/releases", "/v1/tracks"] {
            let resp = h.call_full("GET", path, None, Some(cookie_b.clone()), Some(ORIGIN));
            assert_eq!(resp.status, 200, "{path}: {}", resp.body);
            assert_eq!(
                resp.body, "[]",
                "{path}: a caller with no ledger sees no rows"
            );
        }
        let stranger = h.call_full(
            "GET",
            &format!("/v1/releases/{CAT_RELEASE}"),
            None,
            Some(cookie_b),
            Some(ORIGIN),
        );
        assert_eq!(stranger.status, 404, "{}", stranger.body);
    }

    #[test]
    fn catalogue_store_outage_is_opaque_and_entities_are_unavailable() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        *h.store.fail_catalogue_reads.borrow_mut() = true;
        for path in [
            "/v1/library",
            "/api/library",
            "/v1/artists",
            "/v1/releases",
            "/v1/tracks",
            &format!("/v1/releases/{CAT_RELEASE}"),
        ] {
            let resp = h.call_full("GET", path, None, Some(cookie.clone()), Some(ORIGIN));
            if path.ends_with("/library") {
                assert_eq!(resp.status, 500, "{path}");
                assert_eq!(resp.body, r#"{"error":"internal error"}"#, "{path}");
            } else {
                assert_eq!(resp.status, 503, "{path}");
                let error: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
                assert_eq!(error["code"], "service_unavailable");
                assert_eq!(error["message"], "service unavailable");
                assert_eq!(error["retryable"], true);
                assert!(!resp.body.contains("mirror"));
            }
        }
        // The alias's bearer-session transport hits the same store read,
        // so it outages the same way (auth succeeds, the read fails).
        let session = cookie
            .split_once('=')
            .map(|(_, value)| value)
            .expect("login sets a session pair");
        let outage = h.call_bearer("GET", "/api/library", Some(format!("Bearer {session}")));
        assert_eq!(outage.status, 500, "{}", outage.body);
        assert_eq!(outage.body, r#"{"error":"internal error"}"#);
    }

    #[test]
    fn catalogue_release_track_outage_is_opaque_after_member_release_lookup() {
        let h = harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        seed_catalogue(&h);
        seed_library(&h, "u1");
        let path = format!("/v1/releases/{CAT_RELEASE}");
        let available = h.call_full("GET", &path, None, Some(cookie.clone()), Some(ORIGIN));
        assert_eq!(available.status, 200, "{}", available.body);

        *h.store.fail_catalogue_release_tracks.borrow_mut() = true;
        let outage = h.call_full("GET", &path, None, Some(cookie), Some(ORIGIN));
        assert_eq!(outage.status, 503, "{}", outage.body);
        let error: serde_json::Value = serde_json::from_str(&outage.body).unwrap();
        assert_eq!(
            error,
            serde_json::json!({
                "code": "service_unavailable",
                "message": "service unavailable",
                "retryable": true,
            })
        );
    }

    #[test]
    fn options_preflight_only_for_known_routes() {
        let h = harness();
        // Every served path answers the preflight with an empty 204 (the
        // entry adds credentialed CORS headers on exact-origin match).
        for path in [
            "/healthz",
            "/api/ingest",
            "/v1/auth/passkey/register/start",
            "/v1/auth/passkey/register/finish",
            "/v1/auth/passkey/login/start",
            "/v1/auth/passkey/login/finish",
            "/v1/auth/magic/start",
            "/v1/auth/magic/finish",
            "/v1/session/refresh",
            "/v1/session/logout",
            "/v1/session/revoke-all",
            "/v1/library",
            "/api/library",
            "/v1/artists",
            "/v1/releases",
            "/v1/tracks",
        ] {
            assert!(is_known_path(path), "{path}");
            let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 204, "{path}");
            assert!(resp.body.is_empty(), "{path}");
            assert!(resp.set_cookie.is_none(), "{path}");
        }
        // Dynamic release-detail paths preflight through the scope map
        // (like the single-segment credential revoke id): exact-path
        // `is_known_path` cannot name them, but a lawful bearer GET must
        // still clear preflight. Nested shapes stay hidden (they 404 in
        // dispatch, including the reserved art path).
        let detail = format!("/v1/releases/{CAT_RELEASE}");
        assert!(!is_known_path(&detail));
        let resp = h.call_full("OPTIONS", &detail, None, None, Some(ORIGIN));
        assert_eq!(resp.status, 204, "{detail}");
        for hidden in [
            "/v1/releases/".to_string(),
            format!("/v1/releases/{CAT_RELEASE}/art"),
        ] {
            let resp = h.call_full("OPTIONS", &hidden, None, None, Some(ORIGIN));
            assert_eq!(resp.status, 404, "{hidden}");
        }
        // Unknown paths still 404 on preflight: no surface mapping.
        assert!(!is_known_path("/v1/auth/magic-link"));
        let resp = h.call_full("OPTIONS", "/v1/auth/magic-link", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404);
        let resp = h.call_full("OPTIONS", "/nope", None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404);
    }

    /// Retired user-vault surface: every method and shape of these paths.
    const RETIRED_VAULT_PATHS: [&str; 9] = [
        "/v1/vault",
        "/v1/vault/",
        "/v1/vault/test",
        "/v1/vault/test/",
        "/v1/vault/test-connection",
        "/v1/vault/test-connection/",
        "/v1/vault/other",
        "/v1/vault/test/extra",
        "/V1/vault",
    ];
    const RETIRED_VAULT_METHODS: [&str; 9] = [
        "GET", "PUT", "DELETE", "POST", "PATCH", "HEAD", "OPTIONS", "TRACE", "get",
    ];

    #[test]
    fn retired_vault_paths_are_unknown_and_denied_by_the_scope_map() {
        for path in RETIRED_VAULT_PATHS {
            assert!(!is_known_path(path), "{path} must not be a known path");
            assert!(
                !options_preflight_served(path),
                "{path} must not clear a browser preflight"
            );
            for method in RETIRED_VAULT_METHODS {
                assert_eq!(
                    crate::native_rest::scope_for_operation(method, path),
                    crate::native_rest::RouteDecision::Deny,
                    "{method} {path}"
                );
            }
        }
    }

    #[test]
    fn retired_vault_routes_are_plain_not_found_without_side_effects() {
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        let cookie = magic_login(&h, "a@example.com");
        // Retired paths must not even look the session up: any store
        // authentication read would surface as a non-404 failure.
        *h.store.fail_session_lookup.borrow_mut() = true;
        let audits = h.store.audits.borrow().len();
        let sessions = h.store.sessions.borrow().len();
        let slots = h.store.rate_limit_slots.borrow().len();
        let mails = h.mailer.sent.borrow().len();
        let credential_lookups = *h.store.native_credential_lookups.borrow();
        let expected = crate::api_error::ApiError::not_found("not found").body_json();

        let provider_body = serde_json::json!({
            "provider": {
                "host": "news.example.com", "port": 563, "tls": true,
                "username": "synthetic-user", "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        })
        .to_string();
        let bodies: [Option<String>; 6] = [
            None,
            Some(String::new()),
            Some("{}".to_string()),
            Some(provider_body),
            Some(r#"{"provider": {"host": 7}}"#.to_string()),
            Some(r#"{"provider": {"host": "news.example.com""#.to_string()),
        ];
        let bearer = format!("Bearer ntk_{}", "0".repeat(43));
        // (cookie, authorization, origin): anonymous, valid cookie
        // session, presented bearer, and cookie plus bearer together.
        let callers: [(Option<String>, Option<String>, Option<&str>); 6] = [
            (None, None, None),
            (None, None, Some(ORIGIN)),
            (Some(cookie.clone()), None, None),
            (Some(cookie.clone()), None, Some(ORIGIN)),
            (None, Some(bearer.clone()), None),
            (Some(cookie), Some(bearer), Some(ORIGIN)),
        ];
        for path in RETIRED_VAULT_PATHS {
            for method in RETIRED_VAULT_METHODS {
                for (cookie, authorization, origin) in &callers {
                    for body in &bodies {
                        let req = Request {
                            method: method.to_string(),
                            path: path.to_string(),
                            cookie: cookie.clone(),
                            origin: origin.map(str::to_string),
                            authorization: authorization.clone(),
                            stripe_signature: None,
                            body: body.clone().unwrap_or_default().into_bytes(),
                        };
                        let resp = block_on(handle(&h.ctx(), &req));
                        let who = format!(
                            "{method} {path} cookie={} bearer={} body={body:?}",
                            cookie.is_some(),
                            authorization.is_some()
                        );
                        assert_eq!(resp.status, 404, "{who}: {}", resp.body);
                        assert_eq!(resp.body, expected, "{who}");
                        assert!(resp.set_cookie.is_none(), "{who}: no cookies");
                        assert!(resp.retry_after.is_none(), "{who}");
                    }
                }
            }
        }
        assert_eq!(h.store.audits.borrow().len(), audits, "no audit rows");
        assert_eq!(
            h.store.sessions.borrow().len(),
            sessions,
            "no session writes"
        );
        assert_eq!(h.store.rate_limit_slots.borrow().len(), slots, "no budgets");
        assert_eq!(h.mailer.sent.borrow().len(), mails);
        assert_eq!(
            *h.store.native_credential_lookups.borrow(),
            credential_lookups,
            "no bearer credential lookup"
        );
        assert!(h.dialer.seen.borrow().is_empty(), "no probe dial");
        assert!(
            h.store.legacy_vault_users.borrow().is_empty(),
            "no vault state is created"
        );
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
            // Exact dispatch paths plus the single-segment dynamic revoke
            // id (exact-path `is_known_path` cannot name it).
            let served = is_known_path(path)
                || path
                    .strip_prefix("/v1/credentials/")
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
            assert!(served, "{method} {path}");
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
                "credentials-mint" => {
                    serde_json::from_value::<CredentialsMintBody>(wire.clone()).map(|_| ())
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
            pinned, 9,
            "every body-bearing route (incl. the invite variant and the credentials mint) is pinned"
        );
    }

    #[test]
    fn auth_mail_send_deadline_is_five_seconds_both_directions() {
        // Deadline-value pin: the README promises at most five seconds, but the
        // acceptance suite only bounds it by a 30s watchdog, so 100ms (mail
        // never delivers) and 12s (documented bound drifts) both stayed green.
        // Pin the literal and the elapsed behavior here, in builder-owned unit
        // code — the acceptance file is untouched.
        assert_eq!(
            AUTH_MAIL_SEND_DEADLINE_SECS, 5,
            "auth-mail send deadline must stay exactly 5s; a new budget needs \
             README + caller updates plus this pin"
        );

        fn drive<F: Future>(mut future: F) -> F::Output {
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            // SAFETY: test-only poll loop; the future is stack-owned and never
            // moved after pinning.
            let mut future = unsafe { std::pin::Pin::new_unchecked(&mut future) };
            let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                    return output;
                }
                assert!(
                    std::time::Instant::now() < until,
                    "mail deadline helper still pending past its test ceiling"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        // Permanently pending send must settle at ~5s, not immediately and not
        // at the watchdog: catches both 100ms and 12s drift.
        let started = std::time::Instant::now();
        let outcome = drive(deliver_mail_with_deadline(std::future::pending()));
        assert!(outcome.is_err(), "pending mail must time out, not succeed");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_secs(4),
            "pending send finished in {elapsed:?}; the 5s deadline must actually wait"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "pending send took {elapsed:?}; the documented at-most-5s bound must hold"
        );

        // Fast sends must not wait for the deadline.
        let started = std::time::Instant::now();
        let outcome = drive(deliver_mail_with_deadline(async { Ok(()) }));
        assert!(outcome.is_ok(), "fast success must pass through");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "fast send must not wait for the deadline"
        );
    }

    // --- 404 not_found envelope (acceptance; Builder must not edit) -------
    //
    // Every served 404 must be the canonical error envelope
    // `{code: "not_found", message, retryable: false}` — exactly those
    // three keys, no legacy `error` key — so clients can branch on `code`
    // instead of matching prose. The three served sites are pinned
    // separately because each can regress alone: the shared `not_found()`
    // helper (reached here via credential revoke), the QA-seam guard, and
    // the router catch-all. The sites must also stay byte-indistinguishable
    // from one another: a distinct per-site body is an existence oracle.

    fn assert_not_found_envelope(status: u16, body: &str, site: &str) {
        assert_eq!(status, 404, "{site}: {body}");
        let parsed: serde_json::Value = serde_json::from_str(body)
            .unwrap_or_else(|e| panic!("{site}: 404 body is not JSON ({e}): {body}"));
        let obj = parsed
            .as_object()
            .unwrap_or_else(|| panic!("{site}: 404 body is not a JSON object: {body}"));
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["code", "message", "retryable"],
            "{site}: 404 body must be exactly the envelope keys, got: {body}"
        );
        assert_eq!(
            parsed["code"], "not_found",
            "{site}: envelope code must be not_found, got: {body}"
        );
        assert_eq!(
            parsed["retryable"], false,
            "{site}: not_found must be non-retryable, got: {body}"
        );
        let message = parsed["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{site}: envelope message must be a string: {body}"));
        assert!(
            !message.is_empty(),
            "{site}: envelope message must not be empty: {body}"
        );
    }

    #[test]
    fn not_found_envelope_shape_matches_the_api_error_registry() {
        // Control: the canonical constructor already produces the exact
        // shape the served sites must converge on. Green before and after
        // the build; if this fails, the registry itself drifted and the
        // acceptance expectation must be revisited, not the sites.
        let err = crate::api_error::ApiError::not_found("not found");
        assert_eq!(
            (err.code, err.status, err.retryable),
            ("not_found", 404, false)
        );
        assert_not_found_envelope(err.status, &err.body_json(), "ApiError::not_found");
    }

    #[test]
    fn not_found_envelope_on_the_router_catch_all() {
        let h = harness();
        let post = h.call(
            "POST",
            "/api/no-such-route-canary",
            serde_json::json!({"email": "a@example.test"}),
        );
        assert_not_found_envelope(post.status, &post.body, "catch-all POST");
        assert!(
            !post.body.contains("no-such-route-canary"),
            "catch-all 404 must not echo the probed path: {}",
            post.body
        );
        let pre = h.call_full(
            "OPTIONS",
            "/api/no-such-route-canary",
            None,
            None,
            Some(ORIGIN),
        );
        assert_not_found_envelope(pre.status, &pre.body, "catch-all OPTIONS");
        assert_eq!(
            pre.body, post.body,
            "catch-all bodies must not vary by method"
        );
    }

    #[test]
    fn not_found_envelope_on_the_qa_seam_guard() {
        // Armed seam, valid bearer, no issuance behind the mailbox: a miss.
        let h = qa_harness();
        let miss = h.call_seam(
            serde_json::json!({"email": "fresh-canary@example.test"}),
            Some(QA_TOKEN),
        );
        assert_not_found_envelope(miss.status, &miss.body, "qa seam miss");
        assert!(
            !miss.body.contains("fresh-canary"),
            "qa seam 404 must not echo the probed mailbox: {}",
            miss.body
        );
        // Malformed seam bodies miss the same enveloped way.
        for body in [serde_json::json!({}), serde_json::json!({"email": 1})] {
            let resp = h.call_seam(body, Some(QA_TOKEN));
            assert_not_found_envelope(resp.status, &resp.body, "qa seam malformed body");
            assert_eq!(resp.body, miss.body);
        }
        // Flag armed with no staged bearer: fail closed, same envelope.
        let mut unarmed = harness();
        unarmed.config = cfg().with_qa_code_seam(true);
        let closed = unarmed.call_seam(
            serde_json::json!({"email": "a@example.test"}),
            Some(QA_TOKEN),
        );
        assert_not_found_envelope(closed.status, &closed.body, "qa seam fail-closed");
        assert_eq!(closed.body, miss.body);
        // Production posture (seam off): the guard still answers, enveloped.
        let off = harness().call(
            "POST",
            QA_TEST_CODES_PATH,
            serde_json::json!({"email": "a@example.test"}),
        );
        assert_not_found_envelope(off.status, &off.body, "qa seam disabled");
        assert_eq!(off.body, miss.body);
    }

    #[test]
    fn not_found_envelope_on_credential_revoke_misses() {
        // The shared not_found() helper, served via DELETE /v1/credentials/:id.
        let h = peppered_harness();
        seed_user(&h, "u1", "a@example.com");
        seed_user(&h, "u2", "b@example.com");
        let cookie_a = magic_login(&h, "a@example.com");
        let cookie_b = magic_login(&h, "b@example.com");
        let minted = mint_call(
            &h,
            &cookie_a,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": 60}),
        );
        assert_eq!(minted.status, 201, "{}", minted.body);
        let id = mint_id(&serde_json::from_str(&minted.body).unwrap());

        // Another user's id and an unknown id: the same enveloped 404, and
        // neither body leaks the probed id.
        let foreign = h.call_full(
            "DELETE",
            &format!("/v1/credentials/{id}"),
            None,
            Some(cookie_b),
            Some(ORIGIN),
        );
        assert_not_found_envelope(foreign.status, &foreign.body, "revoke foreign id");
        assert!(
            !foreign.body.contains(&id),
            "revoke 404 must not echo the probed id: {}",
            foreign.body
        );
        let unknown = h.call_full(
            "DELETE",
            "/v1/credentials/cred_missing_canary",
            None,
            Some(cookie_a),
            Some(ORIGIN),
        );
        assert_not_found_envelope(unknown.status, &unknown.body, "revoke unknown id");
        assert!(
            !unknown.body.contains("cred_missing_canary"),
            "revoke 404 must not echo the probed id: {}",
            unknown.body
        );
        assert_eq!(
            foreign.body, unknown.body,
            "foreign and unknown revoke misses must be indistinguishable"
        );
        // The victim row is untouched by the foreign probe.
        let rows = block_on(h.store.native_credentials_for_user("u1")).unwrap();
        assert_eq!(rows[0].record.revoked_at, None);
    }

    #[test]
    fn not_found_envelope_is_indistinguishable_across_all_three_sites() {
        // One body per served site; all three must be byte-identical so no
        // site can be fingerprinted (and route/mailbox/row existence cannot
        // be read out of which 404 answered).
        let h = qa_harness();
        let catch_all = h.call(
            "POST",
            "/api/no-such-route-canary",
            serde_json::json!({"email": "a@example.test"}),
        );
        assert_not_found_envelope(catch_all.status, &catch_all.body, "catch-all");
        let seam = h.call_seam(
            serde_json::json!({"email": "fresh@example.test"}),
            Some(QA_TOKEN),
        );
        assert_not_found_envelope(seam.status, &seam.body, "qa seam");
        assert_eq!(seam.body, catch_all.body, "qa seam vs catch-all");

        let hp = peppered_harness();
        seed_user(&hp, "u1", "a@example.com");
        let cookie = magic_login(&hp, "a@example.com");
        let revoke = hp.call_full(
            "DELETE",
            "/v1/credentials/cred_missing_canary",
            None,
            Some(cookie),
            Some(ORIGIN),
        );
        assert_not_found_envelope(revoke.status, &revoke.body, "revoke");
        assert_eq!(revoke.body, catch_all.body, "revoke vs catch-all");
    }
}
