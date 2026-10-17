//! Worker runtime wiring: env config, Hyperdrive Postgres store,
//! DNS-over-HTTPS resolution, pinned-address socket probing, and the
//! magic-link mailer webhook. Request security decisions live in
//! `handlers`; startup fail-closed gates (env config plus the Hyperdrive
//! caching-disabled attestation below) live here and refuse to serve
//! traffic when the operator has not attested a cache-disabled config.
//!
//! Failures are opaque by construction: driver/DB errors map to
//! [`handlers::StoreError`] without detail, and nothing in this module
//! logs hosts, emails, tokens, banners or provider material.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use worker::{
    console_log, Delay, Env, Fetch, Headers, Method, Request, RequestInit, Response, ResponseBody,
    Result as WorkerResult,
};

use crate::auth::{
    self, InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential, SingleUseClaim,
    SingleUseStore,
};
use crate::handlers::{
    self, AsyncDns, ChallengeRow, Config, Dialer, Mailer, PairAdmission, RateLimitBudget, Store,
    StoreError, UserRecord, VaultRow,
};
use crate::session::SessionRecord;
use crate::vault::{
    AuditEvent, ValidatedTarget, VaultBlob, DIAL_TIMEOUT_MS, MAX_DIAL_ADDRESSES, TOTAL_TIMEOUT_MS,
};

/// Hyperdrive binding name; must match `wrangler.toml.example`.
const HYPERDRIVE_BINDING: &str = "HYPERDRIVE";
/// Fixed public DNS-over-HTTPS endpoint (never user-controlled).
const DOH_ENDPOINT: &str = "https://cloudflare-dns.com/dns-query";

// --- Config ---------------------------------------------------------------------------

fn env_string(env: &Env, name: &str) -> Option<String> {
    env.secret(name)
        .ok()
        .map(|s| s.to_string())
        .or_else(|| env.var(name).ok().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
}

fn config_from_env(env: &Env) -> Option<Config> {
    // Runtime fail-closed gate for the Hyperdrive caching finding: auth
    // reads are plain SELECTs through the HYPERDRIVE binding, so a
    // caching-enabled config would serve stale sessions/credentials for
    // up to ~75s. The Worker cannot inspect the Hyperdrive config at
    // runtime, so the operator attests `HYPERDRIVE_CACHING_DISABLED=1`
    // (set only after `check_hyperdrive_cache.sh` passes for that env).
    // Without the attestation the Worker answers the same generic 500 as
    // any other misconfiguration and serves no auth traffic.
    if !hyperdrive_caching_disabled_attested(env_string(env, "HYPERDRIVE_CACHING_DISABLED")) {
        return None;
    }
    let rp_id = env_string(env, "RP_ID")?;
    let origin = env_string(env, "RP_ORIGIN")?;
    // Strict unpadded base64url for both secrets (`crate::b64`, F-HARD-01):
    // padded std-base64 input is rejected, matching the template contract.
    let session_secret = crate::b64::decode(&env_string(env, "SESSION_SECRET_BASE64")?).ok()?;
    let kek_bytes = crate::b64::decode(&env_string(env, "VAULT_KEK_BASE64")?).ok()?;
    let kek: [u8; 32] = kek_bytes.try_into().ok()?;
    let kek_version: u32 = env_string(env, "VAULT_KEK_VERSION")?.parse().ok()?;
    let session_ttl_secs: u64 = env_string(env, "SESSION_TTL_SECS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(crate::session::MAX_SESSION_SECS);
    let truthy = |name: &str| {
        matches!(
            env_string(env, name).as_deref(),
            Some("1") | Some("true") | Some("TRUE")
        )
    };
    // Rotation window ("recorded" per the runbook): the previous secret is
    // honored only when the operator arms the rotation AND stages the
    // previous value. A staged previous without the flag (stray secret)
    // fails closed at startup; an armed flag with nothing staged is plain
    // single-secret steady state, so ending a rotation (delete the
    // previous, then clear the flag) never takes a deploy down. The matrix
    // lives in `staged_previous_secret`, natively unit-tested below.
    let rotation_armed = truthy("SESSION_SECRET_ROTATION");
    let previous_raw = env_string(env, "SESSION_SECRET_PREVIOUS_BASE64");
    let session_previous_secret = match staged_previous_secret(previous_raw, rotation_armed) {
        Ok(previous) => previous,
        Err(()) => return None,
    };
    let config = Config::new(
        &rp_id,
        &origin,
        session_secret,
        kek,
        kek_version,
        session_ttl_secs,
        truthy("INVITES_REQUIRED"),
        truthy("ALLOW_LOOPBACK_DEV"),
        session_previous_secret,
    )
    .ok()?;
    Some(apply_rate_limit_overlay(config, |name| {
        env_string(env, name)
    }))
}

/// The [vars] names `RateLimitConfig::from_vars` reads. One const
/// list so the overlay test can pin every name against its field.
const RATE_LIMIT_VAR_NAMES: [&str; 5] = [
    "AUTH_RATE_LIMIT_MAX",
    "AUTH_RATE_LIMIT_WINDOW_SECS",
    "VAULT_TEST_RATE_LIMIT_MAX",
    "VAULT_TEST_RATE_LIMIT_WINDOW_SECS",
    "VAULT_TEST_IP_RATE_LIMIT_MAX",
];

/// Pure [vars] overlay: collect whichever of [`RATE_LIMIT_VAR_NAMES`]
/// the lookup resolves and hand them to `from_vars`, which fails safe
/// to the documented defaults on anything missing or invalid, so a
/// typo'd override keeps limiting on (never unlimited, never
/// deny-all). Unit-tested below; the wasm wrapper only supplies the
/// `env_string` closure.
fn rate_limits_from_lookup(
    get: impl Fn(&str) -> Option<String>,
) -> crate::rate_limit::RateLimitConfig {
    let vars: HashMap<String, String> = RATE_LIMIT_VAR_NAMES
        .iter()
        .filter_map(|name| get(name).map(|v| ((*name).to_string(), v)))
        .collect();
    crate::rate_limit::RateLimitConfig::from_vars(&vars)
}

/// Apply the [vars] overlay to a built `Config`: the one place the
/// operator overrides replace the compiled rate-limit defaults.
/// Pure over the lookup closure so a native test drives the exact
/// function `config_from_env` calls with a real `Config` and asserts
/// the resulting field — dropping the `with_rate_limits` call (or
/// feeding it defaults) fails behaviorally, not just textually.
fn apply_rate_limit_overlay(config: Config, get: impl Fn(&str) -> Option<String>) -> Config {
    config.with_rate_limits(rate_limits_from_lookup(get))
}

/// Pure Hyperdrive caching attestation decision: `Some("1"/"true"/"TRUE")`
/// in `HYPERDRIVE_CACHING_DISABLED` means the operator verified that env's
/// Hyperdrive config has query caching disabled; anything else (missing,
/// empty, any other value) refuses to start. Unit-tested below; the same
/// truthy spelling as the other feature flags.
fn hyperdrive_caching_disabled_attested(raw: Option<String>) -> bool {
    matches!(raw.as_deref(), Some("1") | Some("true") | Some("TRUE"))
}

/// Pure rotation-matrix decision: the staged `SESSION_SECRET_PREVIOUS_BASE64`
/// value plus the `SESSION_SECRET_ROTATION` flag to the verify-only previous
/// secret (or the fail-closed refusal).
///
/// - `(None, _)` — nothing staged: single-secret steady state, flag or not.
///   Clearing the flag after deleting the previous is therefore safe in any
///   deploy order; it can never 500 the Worker.
/// - `(Some(_), false)` — previous staged outside a recorded rotation:
///   unexpected second secret, refuse to start (fail closed).
/// - `(Some(raw), true)` — recorded window: strict unpadded-base64url decode
///   plus the 32-byte minimum, else refuse to start (fail closed).
fn staged_previous_secret(
    previous_raw: Option<String>,
    rotation_armed: bool,
) -> Result<Option<Vec<u8>>, ()> {
    match (previous_raw, rotation_armed) {
        (None, _) => Ok(None),
        (Some(_), false) => Err(()),
        (Some(raw), true) => {
            let bytes = crate::b64::decode(&raw).map_err(|_| ())?;
            if bytes.len() < crate::session::MIN_SECRET_LEN {
                return Err(());
            }
            Ok(Some(bytes))
        }
    }
}

/// Exact-origin CORS allow-list: the request `Origin` is echoed if and only
/// if it equals the configured origin. Never a reflection, never `*` (the
/// API answers credentialed requests, so a wildcard would be both invalid
/// and unsafe). Mutations additionally require the same exact match inside
/// the handlers' CSRF pin.
fn allowed_cors_origin<'a>(req_origin: Option<&'a str>, cfg_origin: &str) -> Option<&'a str> {
    match req_origin {
        Some(origin) if origin == cfg_origin => Some(origin),
        _ => None,
    }
}

/// The full credentialed-CORS header set for one response, decided purely
/// from the request `Origin` and the configured origin: `render` appends
/// exactly this list, so the trust decision is unit-testable off-wasm.
/// No match (or no `Origin`) yields no headers at all — never a reflection,
/// never `*`. A 204 is always the known-path OPTIONS preflight, which
/// additionally needs the method/header grant.
fn cors_headers(
    status: u16,
    req_origin: Option<&str>,
    cfg_origin: &str,
) -> Vec<(&'static str, String)> {
    let Some(origin) = allowed_cors_origin(req_origin, cfg_origin) else {
        return Vec::new();
    };
    let mut headers = vec![
        ("access-control-allow-origin", origin.to_string()),
        ("vary", "origin".to_string()),
        ("access-control-allow-credentials", "true".to_string()),
    ];
    if status == 204 {
        headers.push((
            "access-control-allow-methods",
            "GET, POST, PUT, DELETE, OPTIONS".to_string(),
        ));
        headers.push((
            "access-control-allow-headers",
            "content-type, authorization".to_string(),
        ));
        headers.push(("access-control-max-age", "86400".to_string()));
    }
    headers
}

/// Webhook payload for one magic-link email. The `magic_link` lands on the
/// web app's `/auth/magic` page (which redeems it via
/// `POST /v1/auth/magic/finish`); the raw `token` is included so the mail
/// template can also print it as the copy-paste code the passkey-signup
/// inbox-proof step asks for. Pure so the link shape is unit-testable.
fn magic_link_payload(origin: &str, email: &str, token: &str) -> serde_json::Value {
    serde_json::json!({
        "to": email,
        "magic_link": format!("{origin}/auth/magic?token={token}"),
        "token": token,
    })
}

// --- Postgres over Hyperdrive ------------------------------------------------------------

fn i64_of(v: u64) -> Result<i64, StoreError> {
    i64::try_from(v).map_err(|_| StoreError)
}

fn u64_of(v: i64) -> Result<u64, StoreError> {
    u64::try_from(v).map_err(|_| StoreError)
}

fn u32_of(v: i64) -> Result<u32, StoreError> {
    u32::try_from(v).map_err(|_| StoreError)
}

/// Durable-slot budgets are bounded: a zero limit/window admits
/// nothing, and the slot scan is capped at
/// [`crate::rate_limit::MAX_BUDGET_SLOTS`] (the largest in-tree
/// budget). Configured maxima clamp to that cap in `from_vars`, so
/// this gate only refuses hand-built budgets, never operator config.
fn rate_limit_budget_admissible(budget: &RateLimitBudget<'_>) -> bool {
    budget.limit != 0
        && budget.window_secs != 0
        && budget.limit <= u64::from(crate::rate_limit::MAX_BUDGET_SLOTS)
}

async fn delete_expired_rate_limit_slots(
    client: &tokio_postgres::Client,
    now: i64,
) -> Result<(), StoreError> {
    client
        .execute(
            "DELETE FROM auth_rate_limit_slots WHERE expires_at <= $1",
            &[&now],
        )
        .await
        .map_err(|_| StoreError)?;
    Ok(())
}

/// Claim one slot of `budget` on `client`; `Ok(Some(slot))` is the
/// claimed slot index (the pair path needs it for its compensating
/// release), `Ok(None)` is an exhausted budget. Caller has already
/// deleted expired rows for `now_i`.
async fn claim_rate_limit_slot(
    client: &tokio_postgres::Client,
    budget: &RateLimitBudget<'_>,
    now: u64,
    now_i: i64,
) -> Result<Option<i32>, StoreError> {
    let expires_at = i64_of(now.saturating_add(budget.window_secs))?;
    // O(1) fast-fail on the exhausted path (single source of truth:
    // `handlers::RATE_LIMIT_EXHAUSTED_SQL`, pinned by statement-shape
    // test): without this guard every denied request pays one DELETE
    // plus `limit` conflicting upserts (~101 statements with 100 row
    // locks for the 100-slot finish lookup budget). The slot loop below
    // still claims atomically, so a concurrent race that passes this
    // snapshot simply fails closed there.
    let exhausted = client
        .query_one(
            handlers::RATE_LIMIT_EXHAUSTED_SQL,
            &[&budget.scope_key, &budget.action, &now_i],
        )
        .await
        .map_err(|_| StoreError)?
        .get::<_, i64>(0);
    if exhausted >= i64::try_from(budget.limit).map_err(|_| StoreError)? {
        return Ok(None);
    }
    for slot in 0..budget.limit {
        let slot = i32::try_from(slot).map_err(|_| StoreError)?;
        let rows = client
            .query(
                "INSERT INTO auth_rate_limit_slots (scope_key, action, slot, expires_at) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (scope_key, action, slot) DO UPDATE \
                 SET expires_at = EXCLUDED.expires_at \
                 WHERE auth_rate_limit_slots.expires_at <= $5 \
                 RETURNING slot",
                &[
                    &budget.scope_key,
                    &budget.action,
                    &slot,
                    &expires_at,
                    &now_i,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        if !rows.is_empty() {
            return Ok(Some(slot));
        }
    }
    Ok(None)
}

/// Release the exact slot `claim_rate_limit_slot` took for `budget`
/// at `now` (same scope, action, slot and expiry), so the pair
/// path's compensation can never free another request's slot.
async fn release_rate_limit_slot(
    client: &tokio_postgres::Client,
    budget: &RateLimitBudget<'_>,
    slot: i32,
    now: u64,
) -> Result<(), StoreError> {
    let expires_at = i64_of(now.saturating_add(budget.window_secs))?;
    client
        .execute(
            "DELETE FROM auth_rate_limit_slots \
             WHERE scope_key = $1 AND action = $2 AND slot = $3 AND expires_at = $4",
            &[&budget.scope_key, &budget.action, &slot, &expires_at],
        )
        .await
        .map_err(|_| StoreError)?;
    Ok(())
}

struct PgStore {
    env: Env,
}

impl PgStore {
    async fn connect(&self) -> Result<tokio_postgres::Client, StoreError> {
        let hyperdrive = self
            .env
            .hyperdrive(HYPERDRIVE_BINDING)
            .map_err(|_| StoreError)?;
        let socket = worker::Socket::builder()
            .secure_transport(worker::SecureTransport::StartTls)
            .connect(hyperdrive.host(), hyperdrive.port())
            .map_err(|_| StoreError)?;
        let config: tokio_postgres::Config = hyperdrive
            .connection_string()
            .parse()
            .map_err(|_| StoreError)?;
        let (client, connection) = config
            .connect_raw(socket, worker::postgres_tls::PassthroughTls)
            .await
            .map_err(|_| StoreError)?;
        wasm_bindgen_futures::spawn_local(async move {
            // No detail in logs: driver errors can carry host/role strings.
            if connection.await.is_err() {
                console_log!("api db connection error");
            }
        });
        Ok(client)
    }
}

fn user_from_row(row: &tokio_postgres::Row) -> Result<UserRecord, StoreError> {
    Ok(UserRecord {
        id: row.try_get("id").map_err(|_| StoreError)?,
        email: row.try_get("email").map_err(|_| StoreError)?,
        created_at: u64_of(row.try_get("created_at").map_err(|_| StoreError)?)?,
    })
}

fn credential_from_row(row: &tokio_postgres::Row) -> Result<PasskeyCredential, StoreError> {
    Ok(PasskeyCredential {
        cred_id_b64url: row.try_get("cred_id").map_err(|_| StoreError)?,
        user_id: row.try_get("user_id").map_err(|_| StoreError)?,
        public_key: row.try_get("public_key").map_err(|_| StoreError)?,
        sign_count: u32_of(row.try_get("sign_count").map_err(|_| StoreError)?)?,
        registered_at: u64_of(row.try_get("registered_at").map_err(|_| StoreError)?)?,
    })
}

const CREDENTIAL_COLUMNS: &str = "cred_id, user_id, public_key, sign_count, registered_at";

impl Store for PgStore {
    async fn user_by_email(&self, email: &str) -> Result<Option<UserRecord>, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                "SELECT id, email, created_at FROM users WHERE email = $1",
                &[&email],
            )
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| user_from_row(&r)).transpose()
    }

    async fn insert_user(&self, user: &UserRecord) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "INSERT INTO users (id, email, created_at) VALUES ($1, $2, $3)",
                &[&user.id, &user.email, &i64_of(user.created_at)?],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn insert_challenge(
        &self,
        challenge: &PasskeyChallenge,
        user_id: Option<&str>,
        purpose: &str,
    ) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, consumed, purpose) \
                 VALUES ($1, $2, $3, $4, $5, FALSE, $6)",
                &[
                    &challenge.id,
                    &user_id,
                    &challenge.rp_id,
                    &challenge.origin,
                    &i64_of(challenge.expires_at)?,
                    &purpose,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn challenge_by_id(&self, id: &str) -> Result<Option<ChallengeRow>, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                "SELECT id, user_id, rp_id, origin, expires_at, consumed, purpose \
                 FROM auth_challenges WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| {
            let expires_at: i64 = r.try_get("expires_at").map_err(|_| StoreError)?;
            let expires_at = u64_of(expires_at)?;
            Ok(ChallengeRow {
                challenge: PasskeyChallenge {
                    id: r.try_get("id").map_err(|_| StoreError)?,
                    rp_id: r.try_get("rp_id").map_err(|_| StoreError)?,
                    origin: r.try_get("origin").map_err(|_| StoreError)?,
                    // The schema stores expiry only; nothing downstream
                    // authorizes on issue time.
                    issued_at: expires_at.saturating_sub(auth::CHALLENGE_TTL_SECS),
                    expires_at,
                    consumed: r.try_get("consumed").map_err(|_| StoreError)?,
                },
                user_id: r.try_get("user_id").map_err(|_| StoreError)?,
                purpose: r.try_get("purpose").map_err(|_| StoreError)?,
            })
        })
        .transpose()
    }

    async fn insert_credential(&self, credential: &PasskeyCredential) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "INSERT INTO webauthn_credentials (cred_id, user_id, public_key, sign_count, registered_at) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &credential.cred_id_b64url,
                    &credential.user_id,
                    &credential.public_key.as_slice(),
                    &i64::from(credential.sign_count),
                    &i64_of(credential.registered_at)?,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn credential_by_id(
        &self,
        cred_id_b64url: &str,
    ) -> Result<Option<PasskeyCredential>, StoreError> {
        let client = self.connect().await?;
        let sql =
            format!("SELECT {CREDENTIAL_COLUMNS} FROM webauthn_credentials WHERE cred_id = $1");
        let row = client
            .query_opt(&sql, &[&cred_id_b64url])
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| credential_from_row(&r)).transpose()
    }

    async fn credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<PasskeyCredential>, StoreError> {
        let client = self.connect().await?;
        let sql =
            format!("SELECT {CREDENTIAL_COLUMNS} FROM webauthn_credentials WHERE user_id = $1");
        let rows = client
            .query(&sql, &[&user_id])
            .await
            .map_err(|_| StoreError)?;
        rows.iter().map(credential_from_row).collect()
    }

    async fn advance_sign_count(
        &self,
        cred_id_b64url: &str,
        expected: u32,
        new: u32,
    ) -> Result<bool, StoreError> {
        let client = self.connect().await?;
        let rows = client
            .query(
                "UPDATE webauthn_credentials SET sign_count = $3 \
                 WHERE cred_id = $1 AND sign_count = $2 RETURNING cred_id",
                &[&cred_id_b64url, &i64::from(expected), &i64::from(new)],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(rows.len() == 1)
    }

    // Per-user prune: magic_link_tokens has no expires_at index, so a global sweep would scan.
    async fn insert_magic(&self, record: &MagicLinkRecord) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "WITH pruned AS (DELETE FROM magic_link_tokens WHERE user_id = $2 AND expires_at <= $4) \
                 INSERT INTO magic_link_tokens (token_hash, user_id, email, issued_at, expires_at, used) \
                 VALUES ($1, $2, $3, $4, $5, FALSE)",
                &[
                    &record.token_hash,
                    &record.user_id,
                    &record.email,
                    &i64_of(record.issued_at)?,
                    &i64_of(record.expires_at)?,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn magic_by_hash(&self, token_hash: &str) -> Result<Option<MagicLinkRecord>, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                "SELECT token_hash, user_id, email, issued_at, expires_at, used \
                 FROM magic_link_tokens WHERE token_hash = $1",
                &[&token_hash],
            )
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| {
            Ok(MagicLinkRecord {
                token_hash: r.try_get("token_hash").map_err(|_| StoreError)?,
                user_id: r.try_get("user_id").map_err(|_| StoreError)?,
                email: r.try_get("email").map_err(|_| StoreError)?,
                issued_at: u64_of(r.try_get("issued_at").map_err(|_| StoreError)?)?,
                expires_at: u64_of(r.try_get("expires_at").map_err(|_| StoreError)?)?,
                used: r.try_get("used").map_err(|_| StoreError)?,
            })
        })
        .transpose()
    }

    async fn invite_by_hash(&self, code_hash: &str) -> Result<Option<InviteRecord>, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                "SELECT code_hash, used, created_at FROM invite_codes WHERE code_hash = $1",
                &[&code_hash],
            )
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| {
            Ok(InviteRecord {
                code_hash: r.try_get("code_hash").map_err(|_| StoreError)?,
                used: r.try_get("used").map_err(|_| StoreError)?,
                created_at: u64_of(r.try_get("created_at").map_err(|_| StoreError)?)?,
            })
        })
        .transpose()
    }

    async fn insert_session(&self, record: &SessionRecord) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "INSERT INTO sessions (id, user_id, created_at, expires_at, rotation_counter, revoked) \
                 VALUES ($1, $2, $3, $4, $5, FALSE)",
                &[
                    &record.id,
                    &record.user_id,
                    &i64_of(record.created_at)?,
                    &i64_of(record.expires_at)?,
                    &i32::try_from(record.rotation_counter).map_err(|_| StoreError)?,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn session_by_id(&self, id: &str) -> Result<Option<SessionRecord>, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                "SELECT id, user_id, created_at, expires_at, rotation_counter, revoked \
                 FROM sessions WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| {
            let counter: i32 = r.try_get("rotation_counter").map_err(|_| StoreError)?;
            Ok(SessionRecord {
                id: r.try_get("id").map_err(|_| StoreError)?,
                user_id: r.try_get("user_id").map_err(|_| StoreError)?,
                created_at: u64_of(r.try_get("created_at").map_err(|_| StoreError)?)?,
                expires_at: u64_of(r.try_get("expires_at").map_err(|_| StoreError)?)?,
                rotation_counter: u32::try_from(counter).map_err(|_| StoreError)?,
                revoked: r.try_get("revoked").map_err(|_| StoreError)?,
            })
        })
        .transpose()
    }

    async fn rotate_session_row(
        &self,
        id: &str,
        expected_counter: u32,
        new_counter: u32,
        new_expires_at: u64,
    ) -> Result<bool, StoreError> {
        let client = self.connect().await?;
        let rows = client
            .query(
                "UPDATE sessions SET rotation_counter = $3, expires_at = $4 \
                 WHERE id = $1 AND rotation_counter = $2 AND revoked = FALSE RETURNING id",
                &[
                    &id,
                    &i32::try_from(expected_counter).map_err(|_| StoreError)?,
                    &i32::try_from(new_counter).map_err(|_| StoreError)?,
                    &i64_of(new_expires_at)?,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(rows.len() == 1)
    }

    async fn replace_session_after_reauthentication(
        &self,
        replacement: &crate::session::ReauthenticatedSession,
    ) -> Result<bool, StoreError> {
        let claim = replacement.revocation_claim();
        let params = claim.params();
        let rotation_counter =
            i32::try_from(claim.rotation_counter().ok_or(StoreError)?).map_err(|_| StoreError)?;
        let created_at = i64_of(replacement.new_record.created_at)?;
        let expires_at = i64_of(replacement.new_record.expires_at)?;
        let new_counter =
            i32::try_from(replacement.new_record.rotation_counter).map_err(|_| StoreError)?;
        let mut client = self.connect().await?;
        let transaction = client.transaction().await.map_err(|_| StoreError)?;
        let rows = transaction
            .query(
                claim.statement(),
                &[&params[0], &params[1], &rotation_counter],
            )
            .await
            .map_err(|_| StoreError)?;
        if rows.len() != 1 {
            transaction.rollback().await.map_err(|_| StoreError)?;
            return Ok(false);
        }
        if transaction
            .execute(
                "INSERT INTO sessions (id, user_id, created_at, expires_at, rotation_counter, revoked) \
                 VALUES ($1, $2, $3, $4, $5, FALSE)",
                &[
                    &replacement.new_record.id,
                    &replacement.new_record.user_id,
                    &created_at,
                    &expires_at,
                    &new_counter,
                ],
            )
            .await
            .is_err()
        {
            transaction.rollback().await.map_err(|_| StoreError)?;
            return Err(StoreError);
        }
        transaction.commit().await.map_err(|_| StoreError)?;
        Ok(true)
    }

    async fn revoke_session(&self, id: &str) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute("UPDATE sessions SET revoked = TRUE WHERE id = $1", &[&id])
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn revoke_all_sessions(&self, user_id: &str) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "UPDATE sessions SET revoked = TRUE WHERE user_id = $1",
                &[&user_id],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn vault_by_user(&self, user_id: &str) -> Result<Option<VaultRow>, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                "SELECT kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, \
                 indexer_configured, updated_at FROM provider_vaults WHERE user_id = $1",
                &[&user_id],
            )
            .await
            .map_err(|_| StoreError)?;
        row.map(|r| {
            let kek_version: i32 = r.try_get("kek_version").map_err(|_| StoreError)?;
            Ok(VaultRow {
                blob: VaultBlob {
                    kek_version: u32::try_from(kek_version).map_err(|_| StoreError)?,
                    dek_wrapped_nonce_b64url: r
                        .try_get("dek_wrapped_nonce")
                        .map_err(|_| StoreError)?,
                    dek_wrapped_b64url: r.try_get("dek_wrapped").map_err(|_| StoreError)?,
                    payload_nonce_b64url: r.try_get("payload_nonce").map_err(|_| StoreError)?,
                    payload_b64url: r.try_get("payload").map_err(|_| StoreError)?,
                },
                indexer_configured: r.try_get("indexer_configured").map_err(|_| StoreError)?,
                updated_at: u64_of(r.try_get("updated_at").map_err(|_| StoreError)?)?,
            })
        })
        .transpose()
    }

    async fn upsert_vault(&self, user_id: &str, row: &VaultRow) -> Result<(), StoreError> {
        let client = self.connect().await?;
        // Account-deletion fence (ADR 0002 rule 3): the upsert only lands
        // while the owner's deleting_at is NULL, holding the user row
        // FOR SHARE through the statement, so saving provider credentials
        // cannot repopulate user-owned state behind the deletion marker.
        // A fenced (0-row) upsert fails closed as StoreError; the
        // handler classifies it through `account_deleting` into the
        // typed 409. delete_vault below is deliberately not fenced:
        // removal cannot restore state, and the executor deletes the
        // row anyway.
        let affected = client
            .execute(
                "INSERT INTO provider_vaults (user_id, kek_version, dek_wrapped_nonce, dek_wrapped, \
                 payload_nonce, payload, indexer_configured, updated_at) \
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8 \
                 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
                 ON CONFLICT (user_id) DO UPDATE SET kek_version = EXCLUDED.kek_version, \
                 dek_wrapped_nonce = EXCLUDED.dek_wrapped_nonce, dek_wrapped = EXCLUDED.dek_wrapped, \
                 payload_nonce = EXCLUDED.payload_nonce, payload = EXCLUDED.payload, \
                 indexer_configured = EXCLUDED.indexer_configured, updated_at = EXCLUDED.updated_at",
                &[
                    &user_id,
                    &i32::try_from(row.blob.kek_version).map_err(|_| StoreError)?,
                    &row.blob.dek_wrapped_nonce_b64url,
                    &row.blob.dek_wrapped_b64url,
                    &row.blob.payload_nonce_b64url,
                    &row.blob.payload_b64url,
                    &row.indexer_configured,
                    &i64_of(row.updated_at)?,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        if affected == 0 {
            return Err(StoreError);
        }
        Ok(())
    }

    async fn delete_vault(&self, user_id: &str) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "DELETE FROM provider_vaults WHERE user_id = $1",
                &[&user_id],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn account_deleting(&self, user_id: &str) -> Result<bool, StoreError> {
        let client = self.connect().await?;
        let row = client
            .query_opt(
                crate::account_deletion::ACCOUNT_DELETING_PROBE_SQL,
                &[&user_id],
            )
            .await
            .map_err(|_| StoreError)?;
        match row {
            Some(row) => row.try_get("deleting").map_err(|_| StoreError),
            // The user row is gone (erased or reaped): not a fenced
            // write, so the handler keeps its generic failure.
            None => Ok(false),
        }
    }

    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "INSERT INTO audit_log (user_id, action, ok, source_ip, at) VALUES ($1, $2, $3, $4, $5)",
                &[
                    &event.user_id,
                    &event.action,
                    &event.ok,
                    &event.source_ip,
                    &i64_of(event.at)?,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn reserve_rate_limit(
        &self,
        scope_key: &str,
        action: &str,
        limit: u64,
        now: u64,
        window_secs: u64,
    ) -> Result<bool, StoreError> {
        let budget = RateLimitBudget {
            scope_key,
            action,
            limit,
            window_secs,
        };
        if !rate_limit_budget_admissible(&budget) {
            return Ok(false);
        }
        let client = self.connect().await?;
        let now_i = i64_of(now)?;
        delete_expired_rate_limit_slots(&client, now_i).await?;
        Ok(claim_rate_limit_slot(&client, &budget, now, now_i)
            .await?
            .is_some())
    }

    async fn reserve_rate_limit_pair(
        &self,
        first: RateLimitBudget<'_>,
        second: RateLimitBudget<'_>,
        now: u64,
    ) -> Result<PairAdmission, StoreError> {
        if !rate_limit_budget_admissible(&first) {
            return Ok(PairAdmission::DeniedFirst);
        }
        if !rate_limit_budget_admissible(&second) {
            return Ok(PairAdmission::DeniedSecond);
        }
        // All-or-none on one connection: claim the first slot, then the
        // second; if the second does not admit, release the first by
        // its exact identity (scope, action, slot, expiry). A live
        // slot cannot be overwritten by another claim (reuse requires
        // an expired row), so the compensating DELETE can only remove
        // the slot this call just took — a denied attempt ends up
        // consuming neither budget.
        let client = self.connect().await?;
        let now_i = i64_of(now)?;
        delete_expired_rate_limit_slots(&client, now_i).await?;
        let Some(first_slot) = claim_rate_limit_slot(&client, &first, now, now_i).await? else {
            return Ok(PairAdmission::DeniedFirst);
        };
        match claim_rate_limit_slot(&client, &second, now, now_i).await {
            Ok(Some(_)) => Ok(PairAdmission::Allowed),
            Ok(None) => {
                release_rate_limit_slot(&client, &first, first_slot, now).await?;
                Ok(PairAdmission::DeniedSecond)
            }
            Err(err) => {
                let _ = release_rate_limit_slot(&client, &first, first_slot, now).await;
                Err(err)
            }
        }
    }

    async fn prune_expired_challenges(&self, now: u64) -> Result<(), StoreError> {
        let client = self.connect().await?;
        client
            .execute(
                "DELETE FROM auth_challenges WHERE expires_at <= $1",
                &[&i64_of(now)?],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn reap_unverified_users(
        &self,
        now: u64,
        ttl_secs: u64,
        protected_user_id: Option<&str>,
    ) -> Result<(), StoreError> {
        // Single source of truth lives in `handlers` (statement-shape test
        // pins the purpose literal and the vault/session/magic exemptions):
        // magic-only accounts and vault owners are never reaped, so a vault
        // saved under a magic session cannot cascade-delete 24h later.
        let cutoff = i64_of(now.saturating_sub(ttl_secs))?;
        let now = i64_of(now)?;
        let client = self.connect().await?;
        client
            .execute(
                &handlers::reap_unverified_users_sql(),
                &[&cutoff, &protected_user_id, &now],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }

    async fn acquire_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
        lease_secs: u64,
        max_concurrency: u32,
    ) -> Result<bool, StoreError> {
        if max_concurrency == 0 || max_concurrency > 16 || lease_secs == 0 {
            return Ok(false);
        }
        let lease_secs = i64_of(lease_secs)?;
        let client = self.connect().await?;
        client
            .execute(
                "DELETE FROM vault_test_leases \
                 WHERE expires_at <= FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT",
                &[],
            )
            .await
            .map_err(|_| StoreError)?;
        for slot in 0..max_concurrency {
            let slot = i16::try_from(slot).map_err(|_| StoreError)?;
            // Fenced like the vault upsert (ADR 0002 rule 3): a deleting
            // account acquires no new lease rows; every slot probe then
            // returns no row and the lease is simply denied.
            let rows = client
                .query(
                    "INSERT INTO vault_test_leases (user_id, slot, token, expires_at) \
                     SELECT $1, $2, $3, FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT + $4 \
                     FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
                     ON CONFLICT (user_id, slot) DO UPDATE \
                     SET token = EXCLUDED.token, expires_at = EXCLUDED.expires_at \
                     WHERE vault_test_leases.expires_at <= FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT \
                     RETURNING slot",
                    &[&user_id, &slot, &token, &lease_secs],
                )
                .await
                .map_err(|_| StoreError)?;
            if !rows.is_empty() {
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
        let client = self.connect().await?;
        client
            .execute(
                "DELETE FROM vault_test_leases WHERE user_id = $1 AND token = $2",
                &[&user_id, &token],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
    }
}

/// Executes exactly the parameterized conditional UPDATE/RETURNING the
/// core prepared (`SingleUseClaim::statement`) and reports how many rows
/// it returned — autocommitted before the handler responds.
impl SingleUseStore for PgStore {
    async fn claim(&self, candidate: SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        let statement = candidate.statement();
        let client = self.connect().await.map_err(|_| auth::AuthError::Invalid)?;
        type Param<'a> = &'a (dyn tokio_postgres::types::ToSql + Sync);
        let rows = match &candidate {
            SingleUseClaim::MagicLink { record, now } => {
                let expires = i64_of(record.expires_at).map_err(|_| auth::AuthError::Invalid)?;
                let now = i64_of(*now).map_err(|_| auth::AuthError::Invalid)?;
                let params: [Param<'_>; 5] = [
                    &record.token_hash,
                    &record.user_id,
                    &record.email,
                    &expires,
                    &now,
                ];
                client.query(statement, &params[..]).await
            }
            SingleUseClaim::Challenge { record, now } => {
                let expires = i64_of(record.expires_at).map_err(|_| auth::AuthError::Invalid)?;
                let now = i64_of(*now).map_err(|_| auth::AuthError::Invalid)?;
                let params: [Param<'_>; 5] =
                    [&record.id, &record.rp_id, &record.origin, &expires, &now];
                client.query(statement, &params[..]).await
            }
            SingleUseClaim::Invite { record } => {
                let params: [Param<'_>; 1] = [&record.code_hash];
                client.query(statement, &params[..]).await
            }
        }
        .map_err(|_| auth::AuthError::Invalid)?;
        Ok(rows.len() as u64)
    }
}

// --- DNS-over-HTTPS --------------------------------------------------------------------------

struct DohDns;

#[derive(serde::Deserialize)]
struct DohResponse {
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
}

#[derive(serde::Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    rrtype: u16,
    data: String,
}

async fn doh_query(host: &str, rrtype: &str) -> Vec<IpAddr> {
    let mut url = match url::Url::parse(DOH_ENDPOINT) {
        Ok(url) => url,
        Err(_) => return Vec::new(),
    };
    url.query_pairs_mut()
        .append_pair("name", host)
        .append_pair("type", rrtype);
    let headers = Headers::new();
    if headers.set("accept", "application/dns-json").is_err() {
        return Vec::new();
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let request = match Request::new_with_init(url.as_str(), &init) {
        Ok(request) => request,
        Err(_) => return Vec::new(),
    };
    let mut response = match Fetch::Request(request).send().await {
        Ok(response) => response,
        Err(_) => return Vec::new(),
    };
    if response.status_code() != 200 {
        return Vec::new();
    }
    let parsed: DohResponse = match response.json().await {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };
    parsed
        .answer
        .iter()
        .filter(|a| a.rrtype == 1 || a.rrtype == 28)
        .filter_map(|a| a.data.trim().parse::<IpAddr>().ok())
        .collect()
}

impl AsyncDns for DohDns {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<IpAddr>> + 'a>> {
        Box::pin(async move {
            if host.is_empty() || host.len() > 253 {
                return Vec::new();
            }
            if let Ok(ip) = host.parse::<IpAddr>() {
                return vec![ip];
            }
            // Resolve ALL records (A + AAAA): one forbidden answer fails
            // the whole target inside the core guard.
            let mut addrs = doh_query(host, "A").await;
            addrs.extend(doh_query(host, "AAAA").await);
            addrs
        })
    }
}

// --- Pinned-address dialer ----------------------------------------------------------------------

struct SocketDialer;

async fn probe_one(addr: IpAddr, port: u16, tls: bool) -> bool {
    let secure = if tls {
        worker::SecureTransport::On
    } else {
        worker::SecureTransport::Off
    };
    // Dial the vetted IP literal only — the runtime must not re-resolve
    // the hostname. With TLS on, the runtime validates the certificate
    // against the dialed address; a hostname-only certificate therefore
    // fails closed (`ok:false`) rather than bypassing the pin.
    let mut socket = match worker::Socket::builder()
        .secure_transport(secure)
        .connect(addr.to_string(), port)
    {
        Ok(socket) => socket,
        Err(_) => return false,
    };
    let ok = {
        let mut opened = std::pin::pin!(socket.opened());
        let mut delay = std::pin::pin!(Delay::from(Duration::from_millis(DIAL_TIMEOUT_MS)));
        std::future::poll_fn(|cx| {
            use std::task::Poll;
            if let Poll::Ready(result) = opened.as_mut().poll(cx) {
                return Poll::Ready(result.is_ok());
            }
            if let Poll::Ready(()) = delay.as_mut().poll(cx) {
                return Poll::Ready(false);
            }
            Poll::Pending
        })
        .await
    };
    let _ = socket.close().await;
    ok
}

impl Dialer for SocketDialer {
    async fn probe(&self, target: &ValidatedTarget, port: u16, tls: bool) -> bool {
        let mut deadline = std::pin::pin!(Delay::from(Duration::from_millis(TOTAL_TIMEOUT_MS)));
        let mut attempts = std::pin::pin!(async {
            for addr in target.addresses().iter().take(MAX_DIAL_ADDRESSES) {
                if probe_one(*addr, port, tls).await {
                    return true;
                }
            }
            false
        });
        std::future::poll_fn(|cx| {
            use std::task::Poll;
            if let Poll::Ready(()) = deadline.as_mut().poll(cx) {
                return Poll::Ready(false);
            }
            attempts.as_mut().poll(cx)
        })
        .await
    }
}

// --- Mailer webhook --------------------------------------------------------------------------------

struct WebhookMailer {
    env: Env,
    origin: String,
}

impl Mailer for WebhookMailer {
    async fn send_magic_link(&self, email: &str, token: &str) -> Result<(), StoreError> {
        let url = env_string(&self.env, "EMAIL_WEBHOOK_URL").ok_or(StoreError)?;
        // The webhook is operator-provisioned config, must be https, and
        // is the email provider boundary: the link goes only there.
        if !url.starts_with("https://") {
            return Err(StoreError);
        }
        let body = magic_link_payload(&self.origin, email, token).to_string();
        let headers = Headers::new();
        headers
            .set("content-type", "application/json")
            .map_err(|_| StoreError)?;
        if let Some(key) = env_string(&self.env, "EMAIL_API_KEY") {
            headers
                .set("authorization", &format!("Bearer {key}"))
                .map_err(|_| StoreError)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(body.into()));
        let request = Request::new_with_init(&url, &init).map_err(|_| StoreError)?;
        let response = Fetch::Request(request)
            .send()
            .await
            .map_err(|_| StoreError)?;
        if (200..300).contains(&response.status_code()) {
            Ok(())
        } else {
            Err(StoreError)
        }
    }
}

// --- Fetch dispatch ------------------------------------------------------------------------------------

fn render_header_pairs(
    response: &handlers::Response,
    cors: &[(&'static str, String)],
) -> Vec<(&'static str, String)> {
    let mut pairs = vec![
        (
            "content-type",
            "application/json; charset=utf-8".to_string(),
        ),
        (
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'".to_string(),
        ),
        ("cache-control", "no-store".to_string()),
        ("x-content-type-options", "nosniff".to_string()),
    ];
    if let Some(cookie) = &response.set_cookie {
        pairs.push(("set-cookie", cookie.clone()));
    }
    if let Some(retry_after) = response.retry_after {
        pairs.push((
            crate::rate_limit::RETRY_AFTER_HEADER,
            crate::rate_limit::retry_after_header_value(retry_after),
        ));
    }
    // Credentialed CORS for the configured origin only: the exact header
    // set is decided by the unit-tested `cors_headers`; render only
    // appends it.
    pairs.extend(cors.iter().cloned());
    pairs
}

fn render(
    response: &handlers::Response,
    cors: &[(&'static str, String)],
) -> WorkerResult<Response> {
    let headers = Headers::new();
    for (name, value) in render_header_pairs(response, cors) {
        headers.append(name, &value)?;
    }
    Ok(
        Response::from_body(ResponseBody::Body(response.body.clone().into_bytes()))?
            .with_status(response.status)
            .with_headers(headers),
    )
}

fn internal_render() -> WorkerResult<Response> {
    render(
        &handlers::Response {
            status: 500,
            body: r#"{"error":"internal error"}"#.to_string(),
            set_cookie: None,
            retry_after: None,
        },
        &[],
    )
}

// --- Request-body collection & dispatch gate --------------------------------
//
// A body *transport* failure (stream setup or a chunk read) must never be
// converted into an empty or truncated body the handlers then act on: an
// empty body is meaningful downstream (stored-credential vault testing
// dials the saved provider for an empty `POST /v1/vault/test` body), so a
// swallowed failure could invoke the handler, store writes, the mailer or
// the vault dialer on input the client never successfully sent. Collection
// therefore returns an explicit decision and only `Complete` may dispatch.

/// Opaque chunk-read failure. Carries no message by construction, so no
/// internal/runtime error detail can ever reach a response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyReadError;

/// One pollable source of body chunks (std-only so fake streams are
/// native-testable; the Worker adapter wraps the runtime byte stream
/// and maps every runtime chunk error to `BodyReadError`).
pub trait BodyChunkStream {
    fn poll_next_chunk(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Vec<u8>, BodyReadError>>>;
}

/// Where the body comes from, classified by the Worker entry using the
/// actual workers-rs contract:
/// - `Bodyless`: the edge request has no body at all
///   (`req.inner().body().is_none()`) — a legitimate bodyless request
///   (e.g. GET/DELETE, or a POST sent without a body). `req.stream()`
///   errors for such requests too, so this check must come first.
/// - `SetupFailed`: a body IS present but obtaining its stream failed
///   (`req.stream()` returned `Err`).
/// - `Stream(s)`: a body is present and its stream was obtained.
pub enum BodySource<S> {
    Bodyless,
    SetupFailed,
    Stream(S),
}

/// The collection decision. Only `Complete` may reach the handlers.
#[derive(Debug, PartialEq, Eq)]
pub enum CollectedBody {
    Complete(Vec<u8>),
    Oversized,
    TransportFailure,
}

/// Adapter from the runtime byte stream to [`BodyChunkStream`]: every
/// runtime chunk error becomes the opaque [`BodyReadError`].
struct WorkerBodyStream<S> {
    inner: std::pin::Pin<Box<S>>,
}

impl<S> BodyChunkStream for WorkerBodyStream<S>
where
    S: futures_util::Stream<Item = WorkerResult<Vec<u8>>>,
{
    fn poll_next_chunk(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Vec<u8>, BodyReadError>>> {
        match self.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(chunk))) => std::task::Poll::Ready(Some(Ok(chunk))),
            std::task::Poll::Ready(Some(Err(_))) => {
                std::task::Poll::Ready(Some(Err(BodyReadError)))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Collect one request body with the existing hard cap
/// (`handlers::MAX_BODY_BYTES`, the single source of truth — no second
/// size constant). Rules:
/// - `declared_len` is the parsed `Content-Length` (`None` when absent
///   or unparsable). `Some(len) if len > MAX` is `Oversized` and the
///   stream is not polled at all.
/// - `Bodyless` is `Complete(vec![])`; an empty stream is too when no
///   positive length was declared (an explicitly sent empty body stays
///   valid vault-test input).
/// - `SetupFailed` is `TransportFailure`.
/// - Any chunk `Err` is `TransportFailure`, however many valid bytes
///   (even a complete valid JSON document) arrived before it. Partial
///   bytes are dropped, never returned.
/// - A stream that ends after fewer bytes than `declared_len` is
///   `TransportFailure`: the runtime adapter maps a client abort to a
///   clean end-of-stream below this seam, so the declared length is the
///   only abort signal available here. With no declared length
///   (chunked), such an abort is undetectable at this seam — a runtime
///   limit, not a case this function can distinguish from a body that
///   legitimately ends where it ends.
/// - Buffering is bounded: once the cap would be exceeded the result is
///   `Oversized` and collection stops pulling chunks.
/// - `Poll::Pending` is awaited, never treated as end-of-body.
pub async fn collect_body<S: BodyChunkStream>(
    declared_len: Option<usize>,
    source: BodySource<S>,
) -> CollectedBody {
    if declared_len.is_some_and(|len| len > handlers::MAX_BODY_BYTES) {
        return CollectedBody::Oversized;
    }
    let mut stream = match source {
        BodySource::Bodyless => return CollectedBody::Complete(Vec::new()),
        BodySource::SetupFailed => return CollectedBody::TransportFailure,
        BodySource::Stream(stream) => stream,
    };
    let mut body = Vec::new();
    loop {
        let next = std::future::poll_fn(|cx| stream.poll_next_chunk(cx)).await;
        match next {
            None => {
                if declared_len.is_some_and(|len| body.len() < len) {
                    return CollectedBody::TransportFailure;
                }
                return CollectedBody::Complete(body);
            }
            Some(Err(BodyReadError)) => return CollectedBody::TransportFailure,
            Some(Ok(chunk)) => {
                // `body.len() <= MAX` is invariant, so this never overflows
                // and never buffers past the cap.
                if chunk.len() > handlers::MAX_BODY_BYTES - body.len() {
                    return CollectedBody::Oversized;
                }
                body.extend_from_slice(&chunk);
            }
        }
    }
}

/// The dispatch gate. `Complete(bytes)` replaces `request.body` with
/// exactly `bytes` and invokes `handler` once. `Oversized` and
/// `TransportFailure` invoke NOTHING and answer the handlers' existing
/// uniform pre-routing rejection (`handlers::invalid_request`: status
/// 400, body `{"error":"invalid request"}`, no `set_cookie`), so the
/// gate can never drift from it. The Worker entry passes a handler that
/// calls `handlers::handle`, so a rejected body can cause no store,
/// mailer, DNS, or dialer side effect.
pub async fn dispatch_collected<F, Fut>(
    collected: CollectedBody,
    mut request: handlers::Request,
    handler: F,
) -> handlers::Response
where
    F: FnOnce(handlers::Request) -> Fut,
    Fut: std::future::Future<Output = handlers::Response>,
{
    match collected {
        CollectedBody::Complete(bytes) => {
            request.body = bytes;
            handler(request).await
        }
        CollectedBody::Oversized | CollectedBody::TransportFailure => handlers::invalid_request(),
    }
}

/// Worker `fetch` entry: adapts the runtime request into the handlers'
/// framework-agnostic shape. Misconfiguration (missing/invalid env)
/// answers the same generic 500 as any store outage.
pub async fn dispatch(mut req: Request, env: Env) -> WorkerResult<Response> {
    let Some(config) = config_from_env(&env) else {
        // Misconfiguration is an operator problem; clients only ever see
        // the generic internal error (never which variable is wrong).
        console_log!("api worker misconfigured");
        return internal_render();
    };
    // Classify the body source from the actual runtime request, then
    // collect through the tested seam: a legitimate bodyless request
    // (`inner().body()` is `None`; `stream()` would error for it too) is
    // dispatchable empty input, while a present body whose stream cannot
    // be obtained — or whose chunks fail mid-read — is a transport
    // failure that the gate below rejects before any handler runs. The
    // declared length short-circuits oversized bodies and exposes a
    // short read (the runtime adapter hides a client abort as a clean
    // end-of-stream); chunked (length-less) oversized bodies stop
    // buffering at the same cap, and a chunked abort the adapter hides
    // is undetectable at this seam.
    let declared_len = req
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok());
    let source = if req.inner().body().is_none() {
        BodySource::Bodyless
    } else {
        match req.stream() {
            Ok(stream) => BodySource::Stream(WorkerBodyStream {
                inner: Box::pin(stream),
            }),
            Err(_) => BodySource::SetupFailed,
        }
    };
    let collected = collect_body(declared_len, source).await;
    let headers_map: HashMap<String, String> = req
        .headers()
        .entries()
        .map(|(k, v)| (k.to_lowercase(), v))
        .collect();
    let request = handlers::Request {
        method: req.method().to_string(),
        path: req.path(),
        cookie: headers_map.get("cookie").cloned(),
        origin: headers_map.get("origin").cloned(),
        // Template only: the gate replaces this with the collected bytes
        // on `Complete` and never dispatches it otherwise.
        body: Vec::new(),
    };
    let ctx = handlers::Ctx {
        cfg: &config,
        store: &PgStore { env: env.clone() },
        mailer: &WebhookMailer {
            origin: config.origin.clone(),
            env: env.clone(),
        },
        dialer: &SocketDialer,
        dns: &DohDns,
        now: worker::Date::now().as_millis() / 1000,
        source_ip: client_source_ip(&headers_map),
    };
    let response = dispatch_collected(collected, request, |collected_request| async move {
        handlers::handle(&ctx, &collected_request).await
    })
    .await;
    // Exact match only: the configured origin is the one frontend origin
    // this Worker trusts with credentialed cross-origin calls. The raw
    // request Origin goes into the tested `cors_headers` decision — never
    // echoed directly.
    let cors = cors_headers(
        response.status,
        headers_map.get("origin").map(String::as_str),
        &config.origin,
    );
    render(&response, &cors)
}

/// Client address for audit rows and rate-limit keys. Header data is not
/// trusted: only a canonical IP literal survives, else `unknown`
/// (`vault::sanitize_source_ip`). `headers` keys are lowercased.
fn client_source_ip(headers: &HashMap<String, String>) -> String {
    crate::vault::sanitize_source_ip(headers.get("cf-connecting-ip").map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_source_ip_takes_only_a_clean_cf_connecting_ip() {
        let headers = |value: Option<&str>| -> HashMap<String, String> {
            value
                .map(|v| ("cf-connecting-ip".to_string(), v.to_string()))
                .into_iter()
                .collect()
        };
        assert_eq!(
            client_source_ip(&headers(Some("203.0.113.7"))),
            "203.0.113.7"
        );
        assert_eq!(client_source_ip(&headers(None)), "unknown");
        assert_eq!(
            client_source_ip(&headers(Some("203.0.113.7\nINJECTED"))),
            "unknown"
        );
        // Only the Cloudflare-set header counts, never a client-forgeable one.
        let mut forged = HashMap::new();
        forged.insert("x-forwarded-for".to_string(), "203.0.113.7".to_string());
        assert_eq!(client_source_ip(&forged), "unknown");
    }

    fn previous_43() -> String {
        // 32 bytes as unpadded base64url (43 chars), like the template.
        crate::b64::encode(&[7u8; 32])
    }

    #[cfg(target_arch = "wasm32")]
    fn assert_render_security_headers(response: &handlers::Response) {
        let rendered = render(response, &[]).expect("response renders");
        let headers = rendered.headers();
        assert_eq!(rendered.status_code(), response.status);
        assert_eq!(
            headers
                .get("content-type")
                .expect("content-type header is readable")
                .as_deref(),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(
            headers
                .get("content-security-policy")
                .expect("CSP header is readable")
                .as_deref(),
            Some("default-src 'none'; frame-ancestors 'none'")
        );
        assert_eq!(
            headers
                .get("x-content-type-options")
                .expect("nosniff header is readable")
                .as_deref(),
            Some("nosniff")
        );
        assert_eq!(
            headers
                .get("cache-control")
                .expect("cache-control header is readable")
                .as_deref(),
            Some("no-store")
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn assert_render_security_headers(response: &handlers::Response) {
        // wasm-bindgen Response accessors cannot run on native targets.
        let headers = render_header_pairs(response, &[]);
        assert!(headers.contains(&(
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'".to_string()
        )));
        assert!(headers.contains(&("x-content-type-options", "nosniff".to_string())));
        assert!(headers.contains(&("cache-control", "no-store".to_string())));
    }

    #[test]
    fn render_adds_security_headers_to_successful_json() {
        let response = handlers::Response {
            status: 200,
            body: r#"{"ok":true}"#.to_string(),
            set_cookie: None,
            retry_after: None,
        };
        let headers = render_header_pairs(&response, &[]);

        assert!(headers.contains(&(
            "content-type",
            "application/json; charset=utf-8".to_string()
        )));
        assert_render_security_headers(&response);
    }

    #[test]
    fn render_adds_security_headers_to_json_errors() {
        let response = handlers::Response {
            status: 400,
            body: r#"{"error":"invalid request"}"#.to_string(),
            set_cookie: None,
            retry_after: None,
        };

        assert_render_security_headers(&response);
    }

    #[test]
    fn render_preserves_empty_preflight_response() {
        // OPTIONS preflight is an empty 204, not a JSON body. It shares
        // render by design, so its response headers remain consistent too.
        let response = handlers::Response {
            status: 204,
            body: String::new(),
            set_cookie: None,
            retry_after: None,
        };
        assert!(response.body.is_empty());
        assert_render_security_headers(&response);
    }

    #[test]
    fn render_includes_retry_after_only_when_set() {
        let throttled = handlers::Response {
            status: crate::rate_limit::RATE_LIMIT_STATUS,
            body: r#"{"error":"rate_limited"}"#.to_string(),
            set_cookie: None,
            retry_after: Some(3600),
        };
        let headers = render_header_pairs(&throttled, &[]);
        assert!(headers.contains(&(crate::rate_limit::RETRY_AFTER_HEADER, "3600".to_string())));
        let plain = handlers::Response {
            status: 200,
            body: r#"{"ok":true}"#.to_string(),
            set_cookie: None,
            retry_after: None,
        };
        let headers = render_header_pairs(&plain, &[]);
        assert!(!headers
            .iter()
            .any(|(name, _)| *name == crate::rate_limit::RETRY_AFTER_HEADER));
    }

    #[test]
    fn hyperdrive_attestation_fails_closed_without_explicit_opt_in() {
        // The Worker must not serve auth reads unless the operator
        // attested a cache-disabled Hyperdrive config for that env.
        assert!(hyperdrive_caching_disabled_attested(Some("1".to_string())));
        assert!(hyperdrive_caching_disabled_attested(Some(
            "true".to_string()
        )));
        assert!(hyperdrive_caching_disabled_attested(Some(
            "TRUE".to_string()
        )));
        for raw in [
            None,
            Some(String::new()),
            Some("0".to_string()),
            Some("false".to_string()),
            Some("yes".to_string()),
        ] {
            assert!(!hyperdrive_caching_disabled_attested(raw));
        }
    }

    #[test]
    fn rotation_matrix_without_staged_previous_is_steady_state() {
        // Ending a rotation (previous deleted, flag removal racing the
        // deploy) must never fail startup.
        assert_eq!(staged_previous_secret(None, false), Ok(None));
        assert_eq!(staged_previous_secret(None, true), Ok(None));
    }

    #[test]
    fn rotation_matrix_stray_previous_fails_closed() {
        // A previous secret outside a recorded rotation is an unexpected
        // second secret: refuse to start rather than widen signing scope.
        assert_eq!(staged_previous_secret(Some(previous_43()), false), Err(()));
    }

    #[test]
    fn rotation_matrix_recorded_window_accepts_strict_previous() {
        let previous = staged_previous_secret(Some(previous_43()), true);
        assert_eq!(previous, Ok(Some(vec![7u8; 32])));
    }

    #[test]
    fn rotation_matrix_recorded_window_rejects_bad_previous() {
        // Padded std-base64 is rejected like every other secret codec.
        assert_eq!(
            staged_previous_secret(
                Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string()),
                true
            ),
            Err(())
        );
        // Short secrets can neither mint nor verify.
        assert_eq!(
            staged_previous_secret(Some(crate::b64::encode(&[7u8; 16])), true),
            Err(())
        );
        assert_eq!(staged_previous_secret(Some(String::new()), true), Err(()));
    }

    #[test]
    fn cors_allow_list_is_exact_match_only() {
        let cfg = "https://app.example.com";
        assert_eq!(
            allowed_cors_origin(Some("https://app.example.com"), cfg),
            Some("https://app.example.com")
        );
        assert_eq!(allowed_cors_origin(None, cfg), None);
        assert_eq!(
            allowed_cors_origin(Some("https://evil.example.com"), cfg),
            None
        );
        // Trailing slash, case and scheme differences are different origins.
        assert_eq!(
            allowed_cors_origin(Some("https://app.example.com/"), cfg),
            None
        );
        assert_eq!(
            allowed_cors_origin(Some("HTTPS://APP.EXAMPLE.COM"), cfg),
            None
        );
        assert_eq!(
            allowed_cors_origin(Some("http://app.example.com"), cfg),
            None
        );
    }

    #[test]
    fn cors_headers_exact_match_grants_credentialed_headers() {
        let cfg = "https://app.example.com";
        let headers = cors_headers(200, Some("https://app.example.com"), cfg);
        assert_eq!(
            headers,
            vec![
                (
                    "access-control-allow-origin",
                    "https://app.example.com".to_string()
                ),
                ("vary", "origin".to_string()),
                ("access-control-allow-credentials", "true".to_string()),
            ]
        );
        assert!(headers.iter().all(|(_, v)| v != "*"));
    }

    #[test]
    fn cors_headers_preflight_adds_method_and_header_grants() {
        let cfg = "https://app.example.com";
        let headers = cors_headers(204, Some("https://app.example.com"), cfg);
        assert_eq!(headers.len(), 6);
        assert!(headers.contains(&(
            "access-control-allow-methods",
            "GET, POST, PUT, DELETE, OPTIONS".to_string()
        )));
        assert!(headers.contains(&(
            "access-control-allow-headers",
            "content-type, authorization".to_string()
        )));
        assert!(headers.contains(&("access-control-max-age", "86400".to_string())));
        // Non-preflight statuses never carry the preflight grants.
        let headers = cors_headers(401, Some("https://app.example.com"), cfg);
        assert!(headers
            .iter()
            .all(|(name, _)| *name != "access-control-allow-methods"));
    }

    #[test]
    fn cors_headers_untrusted_origins_get_nothing() {
        let cfg = "https://app.example.com";
        for origin in [
            None,
            Some("https://evil.example.com"),
            Some("https://app.example.com/"),
            Some("HTTPS://APP.EXAMPLE.COM"),
            Some("http://app.example.com"),
            Some("*"),
        ] {
            assert!(cors_headers(200, origin, cfg).is_empty(), "{origin:?}");
            assert!(cors_headers(204, origin, cfg).is_empty(), "{origin:?}");
        }
    }

    #[test]
    fn rate_limit_overlay_maps_each_var_name_to_its_field() {
        // The overlay is the only link between operator [vars] and the
        // live budgets: a renamed or typo'd key must fail here, not
        // silently revert operators to defaults.
        use crate::rate_limit::RateLimitConfig;
        let only = |name: &'static str, value: &'static str| {
            rate_limits_from_lookup(|key| (key == name).then(|| value.to_string()))
        };
        let defaults = RateLimitConfig::default();
        assert_eq!(rate_limits_from_lookup(|_| None), defaults);
        assert_eq!(
            only("AUTH_RATE_LIMIT_MAX", "3"),
            RateLimitConfig {
                auth_max_attempts: 3,
                ..defaults
            }
        );
        assert_eq!(
            only("AUTH_RATE_LIMIT_WINDOW_SECS", "120"),
            RateLimitConfig {
                auth_window_secs: 120,
                ..defaults
            }
        );
        assert_eq!(
            only("VAULT_TEST_RATE_LIMIT_MAX", "4"),
            RateLimitConfig {
                vault_test_max_attempts: 4,
                // The unset IP cap scales at 5x the configured
                // account cap (see `from_vars`), so it moves too.
                vault_test_ip_max_attempts: 20,
                ..defaults
            }
        );
        assert_eq!(
            only("VAULT_TEST_RATE_LIMIT_WINDOW_SECS", "90"),
            RateLimitConfig {
                vault_test_window_secs: 90,
                ..defaults
            }
        );
        assert_eq!(
            only("VAULT_TEST_IP_RATE_LIMIT_MAX", "40"),
            RateLimitConfig {
                vault_test_ip_max_attempts: 40,
                ..defaults
            }
        );
        // Every name in the const list is one of the five pinned above:
        // growing the list without a field pin fails this count.
        assert_eq!(RATE_LIMIT_VAR_NAMES.len(), 5);
    }

    #[test]
    fn config_from_env_applies_the_rate_limit_overlay() {
        use crate::rate_limit::RateLimitConfig;
        // Behavioral half: drive the exact fn `config_from_env` calls
        // with a real Config and a stub lookup. Dropping the
        // `with_rate_limits` call inside `apply_rate_limit_overlay`
        // (or feeding it defaults) leaves `rate_limits` at the
        // compiled defaults and fails here.
        let base = Config::new(
            "example.com",
            "https://example.com",
            b"test-session-secret-32-bytes-long!!".to_vec(),
            [9u8; 32],
            1,
            3600,
            false,
            false,
            None,
        )
        .unwrap();
        assert_eq!(base.rate_limits, RateLimitConfig::default());
        let overlaid = apply_rate_limit_overlay(base, |key| {
            (key == "AUTH_RATE_LIMIT_MAX").then(|| "3".to_string())
        });
        assert_eq!(
            overlaid.rate_limits,
            RateLimitConfig {
                auth_max_attempts: 3,
                ..RateLimitConfig::default()
            }
        );
        // Call-site half: `config_from_env` itself needs a wasm Env,
        // so pin that it calls the overlay — but slice ONLY its body
        // out of the pre-test source. Searching the whole file would
        // match this test's own string literal and pass vacuously
        // even with the call dropped.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let body = production
            .split_once("fn config_from_env(")
            .expect("config_from_env must exist")
            .1
            .split_once("\nfn ")
            .expect("a following fn must bound the body")
            .0;
        assert!(
            body.contains("apply_rate_limit_overlay(config"),
            "config_from_env must apply the [vars] rate-limit overlay"
        );
        // Forbid the bare-config return outright: an early
        // `Some(config)` above the overlay call would keep the
        // substring in place while skipping the overlay.
        assert!(
            !body.contains("Some(config)"),
            "config_from_env must not return the un-overlaid config"
        );
    }

    #[test]
    fn insert_magic_prune_is_per_user_and_expired_only() {
        // `PgStore::insert_magic` only compiles into the wasm Worker, so
        // pin its statement text natively the same way the
        // `config_from_env` test pins its call site: slice ONLY the fn
        // body out of the pre-test source, so this test's own literals
        // cannot satisfy the assertions. Keyed to `$5` (the new row's
        // expiry) or missing either predicate, the prune would delete
        // still-valid links or other users' rows and fail here.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let body = production
            .split_once("async fn insert_magic(")
            .expect("insert_magic must exist")
            .1
            .split_once("\n    async fn ")
            .expect("a following fn must bound the body")
            .0;
        assert!(
            body.contains("DELETE FROM magic_link_tokens WHERE user_id = $2 AND expires_at <= $4"),
            "insert_magic must prune only the issuing user's expired rows"
        );
        // `$4` is only issued_at if the binds stay in column order:
        // token_hash, user_id, email, issued_at, expires_at.
        let bind_pos = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("insert_magic must bind {needle}"))
        };
        assert!(
            bind_pos("&record.token_hash") < bind_pos("&record.user_id")
                && bind_pos("&record.user_id") < bind_pos("&record.email")
                && bind_pos("&record.email") < bind_pos("&i64_of(record.issued_at)?")
                && bind_pos("&i64_of(record.issued_at)?") < bind_pos("&i64_of(record.expires_at)?"),
            "insert_magic binds must keep issued_at as $4 and expires_at as $5"
        );
    }

    #[test]
    fn fenced_vault_statements_pin_the_owner_fence_and_zero_row_guard() {
        // ADR 0002 rule 3: PUT /v1/vault is reachable today, and the vault
        // upsert fence, its zero-row guard, and the test-lease fence are the
        // only things stopping credential writes behind the deletion marker.
        // Pin them the same way `insert_magic_prune_is_per_user_and_expired_only`
        // pins its statement: slice ONLY each fn body out of the pre-test
        // source, so this test's own literals cannot satisfy the assertions.
        // Dropping `AND deleting_at IS NULL` from either statement, or the
        // `affected == 0` guard from the upsert, must fail here.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let fn_body = |name: &str| {
            production
                .split_once(name)
                .unwrap_or_else(|| panic!("{name} must exist"))
                .1
                .split_once("\n    async fn ")
                .expect("a following fn must bound the body")
                .0
                .to_string()
        };
        const FENCE: &str = "FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence";
        // Vault upsert: the fence, the owner binding, and the fail-closed guard.
        let upsert = fn_body("async fn upsert_vault(");
        assert!(
            upsert.contains(FENCE),
            "upsert_vault must select its row through the owner fence"
        );
        assert!(
            !upsert.contains("WHERE id = $2"),
            "upsert_vault fence binds the owner ($1), never a column value"
        );
        assert!(
            upsert.contains("SELECT $1, $2"),
            "upsert_vault must keep SELECT $1 bound to the owner id"
        );
        let guard = upsert
            .find("if affected == 0")
            .expect("upsert_vault must fail closed on 0 rows");
        let ok = upsert
            .find("Ok(())")
            .expect("upsert_vault must return Ok on the write path");
        assert!(
            guard < ok,
            "the zero-row guard must precede the success return"
        );
        // Test-connection lease: the same fence on the same owner parameter,
        // denied (not granted) when the fence returns no row.
        let lease = fn_body("async fn acquire_test_connection_lease(");
        assert!(
            lease.contains(FENCE),
            "acquire_test_connection_lease must select its row through the owner fence"
        );
        assert!(
            lease.contains("SELECT $1, $2, $3"),
            "lease must keep SELECT $1 bound to the owner id"
        );
        assert!(
            lease.contains("RETURNING slot"),
            "lease must only succeed on a returned slot row"
        );
        assert!(
            lease.contains("if !rows.is_empty()"),
            "lease must deny when the fence returns no row"
        );
    }

    #[test]
    fn magic_link_payload_links_the_web_route_and_carries_the_code() {
        let payload = magic_link_payload(
            "https://app.example.com",
            "ada@example.com",
            "fixture-token",
        );
        assert_eq!(payload["to"], "ada@example.com");
        // The link must land on the page that redeems it
        // (apps/web/src/routes/auth/magic) and the template must be able
        // to print the raw token as the signup inbox-proof code.
        assert_eq!(
            payload["magic_link"],
            "https://app.example.com/auth/magic?token=fixture-token"
        );
        assert_eq!(payload["token"], "fixture-token");
    }
}
