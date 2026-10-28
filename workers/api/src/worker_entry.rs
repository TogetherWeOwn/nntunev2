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

use tokio_postgres::GenericClient;
use worker::{
    Delay, Env, Fetch, Headers, Method, Request, RequestInit, Response, ResponseBody,
    Result as WorkerResult,
};

use crate::auth::{
    self, InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential, SingleUseClaim,
    SingleUseStore,
};
use crate::handlers::{
    self, AsyncDns, ChallengeRow, Config, Dialer, Mailer, NativeCredentialRow, PairAdmission,
    QaIssuedCode, RateLimitBudget, Store, StoreError, UserRecord, VaultRow,
};
use crate::providers::{
    insert_provider_sql, prune_superseded_providers_sql, purge_unreferenced_providers_sql,
    ProviderMintRow,
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
    env_string_raw(env, name).filter(|s| !s.is_empty())
}

/// Presence-preserving env lookup: `Some("")` for a present-but-empty
/// var, `None` only when neither secret nor var exists. Used solely by
/// the vault-KEK provisioning decision, where a blanked var (what a
/// dashboard "clear this var" action leaves behind) must fail closed
/// rather than read as absent.
fn env_string_raw(env: &Env, name: &str) -> Option<String> {
    env.secret(name)
        .ok()
        .map(|s| s.to_string())
        .or_else(|| env.var(name).ok().map(|s| s.to_string()))
}

/// Half-width of the versioned-KEK discovery window around
/// `VAULT_KEK_CURRENT` (see [`kek_set_from_lookup`]). The Worker's `Env`
/// offers no key listing, so provisioned `VAULT_KEK_V<N>` secrets are
/// discovered by probing version names; the rotation ceremony's live set
/// (`{N-1, N}` steady state, `{N, N+1}` pre-staged — `kek-rotation.md`
/// §5) fits with wide margin, and anything outside the window simply
/// stays unprovisioned (reads for it fail closed per-row, like any
/// unknown version).
const KEK_VERSION_WINDOW: u32 = 8;

/// Parsed vault KEK set: the current write key plus the retained priors
/// for mixed-version reads. Built by [`kek_set_from_lookup`], consumed
/// once by `config_from_env`.
struct KekSet {
    kek: [u8; 32],
    version: u32,
    priors: Vec<(u32, [u8; 32])>,
}

/// Strict KEK decode: unpadded base64url (`crate::b64`, F-HARD-01)
/// plus the exact 32-byte length. One helper so legacy and versioned
/// paths cannot drift apart.
fn decode_kek(raw: &str) -> Option<[u8; 32]> {
    crate::b64::decode(raw).ok()?.try_into().ok()
}

/// Pure vault-KEK provisioning decision over the env lookup, so native
/// tests drive the exact function `config_from_env` calls. Two
/// exclusive modes (`docs/security/kek-rotation.md` §3):
///
/// - `VAULT_KEK_CURRENT` present and non-blank: versioned mode. The
///   current write key is `VAULT_KEK_V{current}` (required — missing,
///   blank or invalid fails startup, even when legacy vars are also
///   present, so a half-cutover never serves the wrong write
///   version), and every other present `VAULT_KEK_V<N>` inside
///   `current ± KEK_VERSION_WINDOW` becomes a retained prior
///   (present-but-blank or present-but-invalid fails startup; absent
///   is simply unprovisioned). Legacy `VAULT_KEK_BASE64`/
///   `VAULT_KEK_VERSION` are ignored: the cutover reuses the legacy
///   key as `VAULT_KEK_V<L>` + `VAULT_KEK_CURRENT=<L>`, where L is
///   live `VAULT_KEK_VERSION`; delete legacy only after V<L> reads green.
/// - `VAULT_KEK_CURRENT` absent: legacy single-key mode —
///   `VAULT_KEK_BASE64` + `VAULT_KEK_VERSION`, both required, no
///   priors. Per-version secrets are ignored here, so provisioning
///   `V<L>` ahead of the cutover deploy never changes serving behavior.
///
/// A present-but-blank (empty or whitespace-only) or
/// present-but-unparsable `VAULT_KEK_CURRENT` fails startup rather
/// than silently falling back to legacy: the operator asked for a
/// versioned cutover, and serving legacy writes would split-brain
/// the version the ceremony believes is current.
/// Callers must pass a presence-preserving lookup (`env_string_raw`,
/// not `env_string`): empty-filtering the lookup would erase the
/// absent/blank distinction this decision depends on.
fn kek_set_from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<KekSet> {
    // Present-but-blank CURRENT (empty from a dashboard "clear this
    // var", or whitespace-only from a stray paste) fails closed: it
    // must never downgrade to legacy mode while retained
    // `VAULT_KEK_BASE64` is still set. The trim is load-bearing, not
    // cosmetic: without it a whitespace-only value would refuse only
    // incidentally via the u32 parse below, and a future trim of the
    // lookup could silently re-open the legacy downgrade.
    match get("VAULT_KEK_CURRENT") {
        None => {}
        Some(current_raw) => {
            if current_raw.trim().is_empty() {
                return None;
            }
            let current: u32 = current_raw.parse().ok()?;
            let kek = get(&format!("VAULT_KEK_V{current}"))
                .filter(|s| !s.is_empty())
                .and_then(|raw| decode_kek(&raw))?;
            let mut priors = Vec::new();
            let low = current.saturating_sub(KEK_VERSION_WINDOW);
            let high = current.saturating_add(KEK_VERSION_WINDOW);
            // Nested (not let-chained): this crate is edition 2021, where
            // `if a && let ...` does not compile.
            let mut version = low;
            loop {
                if version != current {
                    if let Some(raw) = get(&format!("VAULT_KEK_V{version}")) {
                        // A blanked prior is fail-closed, never silently
                        // treated as unprovisioned.
                        if raw.is_empty() {
                            return None;
                        }
                        priors.push((version, decode_kek(&raw)?));
                    }
                }
                if version == high {
                    break;
                }
                version = version.saturating_add(1);
            }
            return Some(KekSet {
                kek,
                version: current,
                priors,
            });
        }
    }
    let kek = get("VAULT_KEK_BASE64")
        .filter(|s| !s.is_empty())
        .and_then(|raw| decode_kek(&raw))?;
    let version: u32 = get("VAULT_KEK_VERSION")
        .filter(|s| !s.is_empty())?
        .parse()
        .ok()?;
    Some(KekSet {
        kek,
        version,
        priors: Vec::new(),
    })
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
    // Vault KEK set: legacy `VAULT_KEK_BASE64` + `VAULT_KEK_VERSION`
    // single-key mode, or the versioned `VAULT_KEK_V<N>` +
    // `VAULT_KEK_CURRENT` map — the pure `kek_set_from_lookup` decides,
    // and any half-provisioned versioned config refuses to start here.
    // The lookup is presence-preserving on purpose: a blanked var must
    // fail closed, not read as absent (see `env_string_raw`).
    let kek_set = kek_set_from_lookup(|name| env_string_raw(env, name))?;
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
    let mut config = Config::new(
        &rp_id,
        &origin,
        session_secret,
        kek_set.kek,
        kek_set.version,
        session_ttl_secs,
        truthy("INVITES_REQUIRED"),
        truthy("ALLOW_LOOPBACK_DEV"),
        session_previous_secret,
    )
    .ok()?
    .with_prior_keks(kek_set.priors);
    // Staging-only emailed-code retrieval seam (`POST /api/test-codes`):
    // armed ONLY by the explicit staging flag, and usable only with the
    // staged QA bearer. Both default off/absent, so production serves the
    // seam path as a plain 404. The token is read only when the flag is
    // armed — an unflagged env never holds it in config.
    if truthy("QA_CODE_SEAM") {
        config = config.with_qa_code_seam(true);
        if let Some(token) =
            env_string(env, "QA_AUTH_TOKEN").filter(|token| !token.trim().is_empty())
        {
            config = config.with_qa_auth_token(&token);
        }
    }
    // Dedicated native-REST bearer keyring (ADR 0010, migration 0019):
    // both halves present and valid, or neither (endpoints fail closed
    // per-request). A half-configured keyring refuses to start, like the
    // stray session-secret previous.
    let config = match (
        env_string(env, "NATIVE_REST_PEPPER_BASE64"),
        env_string(env, "NATIVE_REST_PEPPER_VERSION"),
    ) {
        (None, None) => config,
        (Some(raw), Some(version)) => {
            let bytes = crate::b64::decode(&raw).ok()?;
            if bytes.len() < crate::session::MIN_SECRET_LEN {
                return None;
            }
            let version: u32 = version.parse().ok()?;
            config.with_native_rest_pepper(bytes, version)
        }
        (None, Some(_)) | (Some(_), None) => return None,
    };
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
        // Exactly the methods the scope map serves, plus the OPTIONS
        // self-grant: omitting a served method (HEAD revalidation, PATCH
        // edits) would preflight-block lawful bearer calls, and naming an
        // unserved method would map fiction.
        headers.push((
            "access-control-allow-methods",
            "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS".to_string(),
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
    client: &impl GenericClient,
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
    client: &impl GenericClient,
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

/// Race `future` against a `timeout_ms` budget, returning the caller's
/// opaque `error` on expiry. [`PgStore::connect`] uses this with the vault
/// dial/total budgets; the timer is platform-appropriate (Worker `Delay`
/// on wasm32, `futures-timer` natively). Expiry wins ties; both futures
/// drop before the caller observes the result.
pub async fn with_deadline<T, E>(
    future: impl Future<Output = Result<T, E>>,
    timeout_ms: u64,
    error: E,
) -> Result<T, E> {
    let duration = Duration::from_millis(timeout_ms);
    #[cfg(target_arch = "wasm32")]
    let deadline = Delay::from(duration);
    #[cfg(not(target_arch = "wasm32"))]
    let deadline = futures_timer::Delay::new(duration);
    let mut deadline = std::pin::pin!(deadline);
    let mut future = std::pin::pin!(future);
    let mut error = Some(error);
    std::future::poll_fn(|cx| {
        // Worker Delay arms on first poll, so poll it before starting I/O.
        // Expiry wins if both become ready together; there is no late retry.
        if deadline.as_mut().poll(cx).is_ready() {
            let error = error.take().expect("deadline fires once");
            return std::task::Poll::Ready(Err(error));
        }
        future.as_mut().poll(cx)
    })
    .await
    // Both futures are dropped here, before the caller observes the result.
}

struct PgStore {
    env: Option<Env>,
    trace_id: String,
    #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
    test_client: std::cell::RefCell<Option<tokio_postgres::Client>>,
}

/// Inject only connection acquisition; the handler still executes the real
/// PgStore method, deadline, digest forwarding, transaction and decoder.
#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
pub fn session_retry_store_for_postgres_test(
    client: tokio_postgres::Client,
) -> impl Store + SingleUseStore {
    PgStore {
        env: None,
        trace_id: "postgres-refresh-test".into(),
        test_client: std::cell::RefCell::new(Some(client)),
    }
}

impl PgStore {
    /// Race a store operation against the caller's budget, expiring to the
    /// opaque `StoreError`. Every `Store` method runs its connect plus
    /// statements inside this seam with the vault total: Hyperdrive pools
    /// in transaction mode and RESETs the backend on return, so the
    /// connect-time `SET statement_timeout` cannot bound later statements
    /// and each method re-bounds where its statements run. The budget stays
    /// a caller parameter (never a new literal) so native tests can race a
    /// short deadline without a database.
    async fn bounded<T>(
        operation: impl Future<Output = Result<T, StoreError>>,
        timeout_ms: u64,
    ) -> Result<T, StoreError> {
        with_deadline(operation, timeout_ms, StoreError).await
    }

    async fn connect(&self) -> Result<tokio_postgres::Client, StoreError> {
        #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
        if let Some(client) = self.test_client.borrow_mut().take() {
            return Ok(client);
        }
        let env = self.env.as_ref().ok_or(StoreError)?;
        // One overall budget encloses dial, handshake, driver spawn, and
        // statement setup: the handshake and SET consume the time left
        // after dial rather than resetting the clock.
        with_deadline(
            async {
                let hyperdrive = env.hyperdrive(HYPERDRIVE_BINDING).map_err(|_| StoreError)?;
                let socket = worker::Socket::builder()
                    .secure_transport(worker::SecureTransport::StartTls)
                    .connect(hyperdrive.host(), hyperdrive.port())
                    .map_err(|_| StoreError)?;
                // The builder connect is synchronous; the actual dial
                // completes when the socket opens, bounded by the vault
                // dial budget shared with the test-connection probe.
                with_deadline(
                    async { socket.opened().await.map(|_| ()).map_err(|_| StoreError) },
                    DIAL_TIMEOUT_MS,
                    StoreError,
                )
                .await?;
                let config: tokio_postgres::Config = hyperdrive
                    .connection_string()
                    .parse()
                    .map_err(|_| StoreError)?;
                let (client, connection) = config
                    .connect_raw(socket, worker::postgres_tls::PassthroughTls)
                    .await
                    .map_err(|_| StoreError)?;
                // The driver outlives the handler but still belongs to this request.
                // Own its resolved trace and normalized build identity in the task.
                let trace_id = self.trace_id.clone();
                let build_sha = build_sha_from_env(env);
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = Self::connection_diagnostic(connection, &trace_id, &build_sha).await;
                });
                // Connect-time SET of the total budget; Hyperdrive RESETs
                // pooled backends, so each Store method additionally races
                // its statements through Self::bounded.
                client
                    .batch_execute(&format!("SET statement_timeout = {}", TOTAL_TIMEOUT_MS))
                    .await
                    .map_err(|_| StoreError)?;
                Ok(client)
            },
            TOTAL_TIMEOUT_MS,
            StoreError,
        )
        .await
    }

    async fn connection_diagnostic<F, E>(
        connection: F,
        trace_id: &str,
        build_sha: &str,
    ) -> Option<String>
    where
        F: std::future::Future<Output = Result<(), E>>,
    {
        // No detail in logs: driver errors can carry host/role strings.
        if connection.await.is_err() {
            emit_record(
                crate::logging::Level::Error,
                "api.db.connection_error",
                trace_id,
                serde_json::json!({"error_class": "service_unavailable", "build_sha": build_sha, "release": build_sha}),
            )
        } else {
            None
        }
    }

    /// Fenced vault-upsert statement (ADR 0002 rule 3), shared by
    /// [`Store::upsert_vault`] and
    /// [`Store::save_vault_and_mint_provider`] so the two call sites can
    /// never fork. Shape pinned in the fence test below.
    fn upsert_vault_sql() -> &'static str {
        "INSERT INTO provider_vaults (user_id, kek_version, dek_wrapped_nonce, dek_wrapped, \
         payload_nonce, payload, indexer_configured, updated_at) \
         SELECT $1, $2, $3, $4, $5, $6, $7, $8 \
         FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
         ON CONFLICT (user_id) DO UPDATE SET kek_version = EXCLUDED.kek_version, \
         dek_wrapped_nonce = EXCLUDED.dek_wrapped_nonce, dek_wrapped = EXCLUDED.dek_wrapped, \
         payload_nonce = EXCLUDED.payload_nonce, payload = EXCLUDED.payload, \
         indexer_configured = EXCLUDED.indexer_configured, updated_at = EXCLUDED.updated_at"
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

const NATIVE_REST_COLUMNS: &str = "id, user_id, label, key_prefix, key_hmac, pepper_version, \
     scopes, created_at, expires_at, last_used_at, revoked_at, created_from_ip";

/// Map a `native_rest_credentials` row (migration 0019). Stored scopes
/// that no longer parse are a data-corruption signal: the row fails
/// closed (`StoreError`, never an authenticated caller) rather than
/// authorizing a narrowed or widened set.
fn native_credential_from_row(
    row: &tokio_postgres::Row,
) -> Result<NativeCredentialRow, StoreError> {
    let scopes: Vec<String> = row.try_get("scopes").map_err(|_| StoreError)?;
    let mut parsed = Vec::with_capacity(scopes.len());
    for scope in &scopes {
        parsed.push(crate::native_rest::Scope::parse(scope).ok_or(StoreError)?);
    }
    let revoked_at: Option<i64> = row.try_get("revoked_at").map_err(|_| StoreError)?;
    let last_used_at: Option<i64> = row.try_get("last_used_at").map_err(|_| StoreError)?;
    Ok(NativeCredentialRow {
        record: crate::native_rest::NativeRestRecord {
            id: row.try_get("id").map_err(|_| StoreError)?,
            user_id: row.try_get("user_id").map_err(|_| StoreError)?,
            key_prefix: row.try_get("key_prefix").map_err(|_| StoreError)?,
            key_hmac: row.try_get("key_hmac").map_err(|_| StoreError)?,
            pepper_version: u32::try_from(
                row.try_get::<_, i32>("pepper_version")
                    .map_err(|_| StoreError)?,
            )
            .map_err(|_| StoreError)?,
            scopes: parsed,
            created_at: u64_of(row.try_get("created_at").map_err(|_| StoreError)?)?,
            expires_at: u64_of(row.try_get("expires_at").map_err(|_| StoreError)?)?,
            revoked_at: revoked_at.map(u64_of).transpose()?,
        },
        label: row.try_get("label").map_err(|_| StoreError)?,
        created_from_ip: row.try_get("created_from_ip").map_err(|_| StoreError)?,
        last_used_at: last_used_at.map(u64_of).transpose()?,
    })
}

impl Store for PgStore {
    async fn user_by_email(&self, email: &str) -> Result<Option<UserRecord>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let row = client
                    .query_opt(
                        "SELECT id, email, created_at FROM users WHERE email = $1",
                        &[&email],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                row.map(|r| user_from_row(&r)).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_user(&self, user: &UserRecord) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                client
                    .execute(
                        "INSERT INTO users (id, email, created_at) VALUES ($1, $2, $3)",
                        &[&user.id, &user.email, &i64_of(user.created_at)?],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_challenge(
        &self,
        challenge: &PasskeyChallenge,
        user_id: Option<&str>,
        purpose: &str,
    ) -> Result<(), StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn challenge_by_id(&self, id: &str) -> Result<Option<ChallengeRow>, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_credential(&self, credential: &PasskeyCredential) -> Result<(), StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn credential_by_id(
        &self,
        cred_id_b64url: &str,
    ) -> Result<Option<PasskeyCredential>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let sql = format!(
                    "SELECT {CREDENTIAL_COLUMNS} FROM webauthn_credentials WHERE cred_id = $1"
                );
                let row = client
                    .query_opt(&sql, &[&cred_id_b64url])
                    .await
                    .map_err(|_| StoreError)?;
                row.map(|r| credential_from_row(&r)).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<PasskeyCredential>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let sql = format!(
                    "SELECT {CREDENTIAL_COLUMNS} FROM webauthn_credentials WHERE user_id = $1"
                );
                let rows = client
                    .query(&sql, &[&user_id])
                    .await
                    .map_err(|_| StoreError)?;
                rows.iter().map(credential_from_row).collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn advance_sign_count(
        &self,
        cred_id_b64url: &str,
        expected: u32,
        new: u32,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    // Per-user prune: magic_link_tokens has no expires_at index, so a global sweep would scan.
    async fn insert_magic(&self, record: &MagicLinkRecord) -> Result<(), StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn magic_by_hash(&self, token_hash: &str) -> Result<Option<MagicLinkRecord>, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    // Staging-only code-seam outbox (migration 0018): one latest-token
    // row per synthetic mailbox. Read-only retrieval never touches this
    // table beyond the SELECT below.
    async fn record_qa_issued_code(
        &self,
        email: &str,
        token: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        Self::bounded(async {
        let client = self.connect().await?;
        client
            .execute(
                "INSERT INTO qa_issued_codes (email, token, issued_at) VALUES ($1, $2, $3) \
                 ON CONFLICT (email) DO UPDATE SET token = EXCLUDED.token, issued_at = EXCLUDED.issued_at",
                &[&email, &token, &i64_of(now)?],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn qa_issued_code(&self, email: &str) -> Result<Option<QaIssuedCode>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let row = client
                    .query_opt(
                        "SELECT token, issued_at FROM qa_issued_codes WHERE email = $1",
                        &[&email],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                row.map(|r| {
                    Ok(QaIssuedCode {
                        token: r.try_get("token").map_err(|_| StoreError)?,
                        issued_at: u64_of(r.try_get("issued_at").map_err(|_| StoreError)?)?,
                    })
                })
                .transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn invite_by_hash(&self, code_hash: &str) -> Result<Option<InviteRecord>, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_session(&self, record: &SessionRecord) -> Result<(), StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn session_by_id(&self, id: &str) -> Result<Option<SessionRecord>, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn rotate_session_row(
        &self,
        id: &str,
        expected_counter: u32,
        new_counter: u32,
        new_expires_at: u64,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query(
                        "UPDATE sessions SET rotation_counter = $3, expires_at = $4, \
                 refresh_retry_hash = NULL, refresh_issued_at = NULL, refresh_claimed_at = NULL \
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn rotate_session_with_retry(
        &self,
        previous: &crate::session::SessionClaims,
        next: &crate::session::SessionClaims,
        retry_hash: &str,
        _now: u64,
    ) -> Result<Option<crate::session::SessionClaims>, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                crate::session_retry_store::rotate_with_retry(
                    &mut client,
                    previous,
                    next,
                    retry_hash,
                )
                .await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn replace_session_after_reauthentication(
        &self,
        replacement: &crate::session::ReauthenticatedSession,
    ) -> Result<bool, StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn revoke_session(&self, id: &str) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                client
                    .execute("UPDATE sessions SET revoked = TRUE WHERE id = $1", &[&id])
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn revoke_single_session(&self, id: &str, user_id: &str) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let claim = crate::session::SessionRevokeClaim::Single {
                    session_id: id,
                    user_id,
                };
                let rows = client
                    .query(claim.statement(), &[&id, &user_id])
                    .await
                    .map_err(|_| StoreError)?;
                Ok(rows.len() == 1)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn revoke_all_sessions(&self, user_id: &str) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                client
                    .execute(
                        "UPDATE sessions SET revoked = TRUE WHERE user_id = $1",
                        &[&user_id],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    // --- Native REST credentials (ADR 0010, migration 0019) -------------------------------
    //
    // Every statement is parameterized; the secret itself never reaches the
    // database (only its HMAC plus the non-secret listing prefix). The
    // composite UNIQUE (pepper_version, key_hmac) is the lookup identity.

    async fn insert_native_credential(&self, row: &NativeCredentialRow) -> Result<(), StoreError> {
        Self::bounded(async {
        let scopes: Vec<String> = row
            .record
            .scopes
            .iter()
            .map(|scope| scope.as_str().to_string())
            .collect();
        let client = self.connect().await?;
        // Account-deletion fence (ADR 0002 rule 3): the insert only lands
        // while the owner's deleting_at is NULL, holding the user row
        // FOR SHARE through the statement, so a fresh session during the
        // 24 h grace cannot mint a new long-lived bearer path behind the
        // marker. A fenced (0-row) insert fails closed as StoreError; the
        // handler classifies it through `account_deleting` into the typed
        // 409. The owner binds $2 (the user_id column), never a value.
        let affected = client
            .execute(
                "INSERT INTO native_rest_credentials (id, user_id, label, key_prefix, key_hmac, \
                 pepper_version, scopes, created_at, expires_at, created_from_ip) \
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10 \
                 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence",
                &[
                    &row.record.id,
                    &row.record.user_id,
                    &row.label,
                    &row.record.key_prefix,
                    &row.record.key_hmac,
                    &i32::try_from(row.record.pepper_version).map_err(|_| StoreError)?,
                    &scopes,
                    &i64_of(row.record.created_at)?,
                    &i64_of(row.record.expires_at)?,
                    &row.created_from_ip,
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        if affected == 0 {
            return Err(StoreError);
        }
        Ok(())
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn native_credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<NativeCredentialRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let sql = format!(
                    "SELECT {NATIVE_REST_COLUMNS} FROM native_rest_credentials \
             WHERE user_id = $1 ORDER BY created_at, id"
                );
                let rows = client
                    .query(&sql, &[&user_id])
                    .await
                    .map_err(|_| StoreError)?;
                rows.iter().map(native_credential_from_row).collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn native_credential_by_hmac(
        &self,
        pepper_version: u32,
        key_hmac: &str,
    ) -> Result<Option<NativeCredentialRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let sql = format!(
                    "SELECT {NATIVE_REST_COLUMNS} FROM native_rest_credentials \
             WHERE pepper_version = $1 AND key_hmac = $2"
                );
                let row = client
                    .query_opt(
                        &sql,
                        &[
                            &i32::try_from(pepper_version).map_err(|_| StoreError)?,
                            &key_hmac,
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                row.map(|row| native_credential_from_row(&row)).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn revoke_native_credential(
        &self,
        id: &str,
        user_id: &str,
        now: u64,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Owner-scoped and sticky: only a live row of this user
                // transitions; an already-revoked row keeps its timestamp.
                let transitioned = client
                    .execute(
                        "UPDATE native_rest_credentials SET revoked_at = $3 \
                 WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
                        &[&id, &user_id, &i64_of(now)?],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if transitioned == 1 {
                    return Ok(true);
                }
                // No transition: report whether the row exists at all (unknown
                // and foreign ids both report false — no existence oracle).
                let exists = client
                    .query_opt(
                        "SELECT id FROM native_rest_credentials WHERE id = $1 AND user_id = $2",
                        &[&id, &user_id],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(exists.is_some())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn revoke_all_native_credentials(
        &self,
        user_id: &str,
        now: u64,
    ) -> Result<u64, StoreError> {
        Self::bounded(
            async {
                // An empty user id (upstream bug) must never become a mass
                // revocation: revoke nothing without a database round trip.
                if user_id.is_empty() {
                    return Ok(0);
                }
                let client = self.connect().await?;
                let revoked = client
                    .execute(
                        "UPDATE native_rest_credentials SET revoked_at = $2 \
                 WHERE user_id = $1 AND revoked_at IS NULL",
                        &[&user_id, &i64_of(now)?],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(revoked)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn record_native_credential_use(&self, id: &str, now: u64) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                client
                    .execute(
                        "UPDATE native_rest_credentials SET last_used_at = $2 WHERE id = $1",
                        &[&id, &i64_of(now)?],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn vault_by_user(&self, user_id: &str) -> Result<Option<VaultRow>, StoreError> {
        Self::bounded(
            async {
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
                            payload_nonce_b64url: r
                                .try_get("payload_nonce")
                                .map_err(|_| StoreError)?,
                            payload_b64url: r.try_get("payload").map_err(|_| StoreError)?,
                        },
                        indexer_configured: r
                            .try_get("indexer_configured")
                            .map_err(|_| StoreError)?,
                        updated_at: u64_of(r.try_get("updated_at").map_err(|_| StoreError)?)?,
                    })
                })
                .transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn upsert_vault(&self, user_id: &str, row: &VaultRow) -> Result<(), StoreError> {
        Self::bounded(
            async {
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
                        Self::upsert_vault_sql(),
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn delete_vault(&self, user_id: &str) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                let transaction = client.transaction().await.map_err(|_| StoreError)?;
                // Serialize with the save's users-row FOR SHARE fence,
                // including first saves when no vault row exists to lock.
                // Removal remains allowed for a deleting account; a gone
                // user makes this and the deletes harmless no-ops.
                transaction
                    .query_opt("SELECT id FROM users WHERE id = $1 FOR UPDATE", &[&user_id])
                    .await
                    .map_err(|_| StoreError)?;
                if transaction
                    .execute(
                        "DELETE FROM provider_vaults WHERE user_id = $1",
                        &[&user_id],
                    )
                    .await
                    .is_err()
                {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    return Err(StoreError);
                }
                // Purge after the vault delete: a save serialized before
                // us is visible to this READ COMMITTED statement; a later
                // save cannot start writing until our commit releases the
                // owner lock. Grant-referenced rows survive (RESTRICT).
                if transaction
                    .execute(purge_unreferenced_providers_sql(), &[&user_id])
                    .await
                    .is_err()
                {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    return Err(StoreError);
                }
                transaction.commit().await.map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_provider(&self, row: &ProviderMintRow) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Account-deletion fence (ADR 0002 rule 3): the mint only lands
                // while the owner's deleting_at is NULL, holding the user row
                // FOR SHARE through the statement, so saving provider
                // credentials cannot mint an identity behind the deletion
                // marker. A fenced (0-row) mint fails closed as StoreError; the
                // handler classifies it through `account_deleting` into the
                // typed 409. The owner binds $2 (the user_id column), never a
                // value; the statement shape (literals, fence) is pinned in
                // `crate::providers` and below.
                let affected = client
                    .execute(
                        insert_provider_sql(),
                        &[
                            &row.id,
                            &row.user_id,
                            &row.label,
                            &row.encrypted_creds.as_slice(),
                            &i32::try_from(row.kek_version).map_err(|_| StoreError)?,
                            &row.dek_wrapped_nonce.as_slice(),
                            &row.dek_wrapped.as_slice(),
                            &row.payload_nonce.as_slice(),
                            &i64_of(row.created_at)?,
                            &i64_of(row.last_verified_at)?,
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if affected == 0 {
                    return Err(StoreError);
                }
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn save_vault_and_mint_provider(
        &self,
        user_id: &str,
        vault: &VaultRow,
        mint: &ProviderMintRow,
    ) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                let transaction = client.transaction().await.map_err(|_| StoreError)?;
                // One atomic save: the vault upsert and the providers
                // mint land together or not at all, so a failed mint can
                // never leave a committed vault row the client was told
                // failed. Both statements carry the account-deletion
                // fence in-statement (ADR 0002 rule 3); a 0-row write on
                // either rolls everything back and fails closed as
                // StoreError, which the handler classifies through
                // `account_deleting` into the typed 409. The upsert runs
                // first and holds the owner's users row FOR SHARE to
                // commit, so the rotation prune below needs no fence of
                // its own and no deletion marker can slip between the
                // statements.
                let upserted = transaction
                    .execute(
                        Self::upsert_vault_sql(),
                        &[
                            &user_id,
                            &i32::try_from(vault.blob.kek_version).map_err(|_| StoreError)?,
                            &vault.blob.dek_wrapped_nonce_b64url,
                            &vault.blob.dek_wrapped_b64url,
                            &vault.blob.payload_nonce_b64url,
                            &vault.blob.payload_b64url,
                            &vault.indexer_configured,
                            &i64_of(vault.updated_at)?,
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if upserted == 0 {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    return Err(StoreError);
                }
                // The owner binds $2 (the user_id column), like
                // `insert_provider` above; the id binds $1 as text and
                // casts server-side (tokio-postgres takes Rust strings
                // for text, not uuid).
                let minted = transaction
                    .execute(
                        insert_provider_sql(),
                        &[
                            &mint.id,
                            &mint.user_id,
                            &mint.label,
                            &mint.encrypted_creds.as_slice(),
                            &i32::try_from(mint.kek_version).map_err(|_| StoreError)?,
                            &mint.dek_wrapped_nonce.as_slice(),
                            &mint.dek_wrapped.as_slice(),
                            &mint.payload_nonce.as_slice(),
                            &i64_of(mint.created_at)?,
                            &i64_of(mint.last_verified_at)?,
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if minted == 0 {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    return Err(StoreError);
                }
                // Rotation prune: superseded rows survive only while a
                // grant references them. Zero pruned rows is success.
                if transaction
                    .execute(prune_superseded_providers_sql(), &[&user_id, &mint.id])
                    .await
                    .is_err()
                {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    return Err(StoreError);
                }
                transaction.commit().await.map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn account_deleting(&self, user_id: &str) -> Result<bool, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn reserve_rate_limit(
        &self,
        scope_key: &str,
        action: &str,
        limit: u64,
        now: u64,
        window_secs: u64,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn reserve_rate_limit_pair(
        &self,
        first: RateLimitBudget<'_>,
        second: RateLimitBudget<'_>,
        now: u64,
    ) -> Result<PairAdmission, StoreError> {
        Self::bounded(
            async {
                if !rate_limit_budget_admissible(&first) {
                    return Ok(PairAdmission::DeniedFirst);
                }
                if !rate_limit_budget_admissible(&second) {
                    return Ok(PairAdmission::DeniedSecond);
                }
                // All-or-none in one transaction: the delete plus both
                // claims commit together, so deadline expiry — which drops
                // this future — rolls back instead of stranding the first
                // slot past an unreachable Err-arm release. A dropped
                // tokio-postgres transaction rolls back (and the driver
                // terminate aborts it server-side); `?` errors take the
                // same implicit rollback with no extra round-trip.
                let mut client = self.connect().await?;
                let now_i = i64_of(now)?;
                let transaction = client.transaction().await.map_err(|_| StoreError)?;
                delete_expired_rate_limit_slots(&transaction, now_i).await?;
                if claim_rate_limit_slot(&transaction, &first, now, now_i)
                    .await?
                    .is_none()
                {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    return Ok(PairAdmission::DeniedFirst);
                }
                if claim_rate_limit_slot(&transaction, &second, now, now_i)
                    .await?
                    .is_some()
                {
                    transaction.commit().await.map_err(|_| StoreError)?;
                    Ok(PairAdmission::Allowed)
                } else {
                    transaction.rollback().await.map_err(|_| StoreError)?;
                    Ok(PairAdmission::DeniedSecond)
                }
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn prune_expired_challenges(&self, now: u64) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                client
                    .execute(
                        "DELETE FROM auth_challenges WHERE expires_at <= $1",
                        &[&i64_of(now)?],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn reap_unverified_users(
        &self,
        now: u64,
        ttl_secs: u64,
        protected_user_id: Option<&str>,
    ) -> Result<(), StoreError> {
        Self::bounded(
            async {
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
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn acquire_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
        lease_secs: u64,
        max_concurrency: u32,
    ) -> Result<bool, StoreError> {
        Self::bounded(async {
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
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn release_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                client
                    .execute(
                        "DELETE FROM vault_test_leases WHERE user_id = $1 AND token = $2",
                        &[&user_id, &token],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }
}

/// Executes exactly the parameterized conditional UPDATE/RETURNING the
/// core prepared (`SingleUseClaim::statement`) and reports how many rows
/// it returned — autocommitted before the handler responds.
impl SingleUseStore for PgStore {
    async fn claim(&self, candidate: SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        Self::bounded(
            async {
                let statement = candidate.statement();
                let client = self.connect().await?;
                type Param<'a> = &'a (dyn tokio_postgres::types::ToSql + Sync);
                let rows = match &candidate {
                    SingleUseClaim::MagicLink { record, now } => {
                        let expires = i64_of(record.expires_at).map_err(|_| StoreError)?;
                        let now = i64_of(*now).map_err(|_| StoreError)?;
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
                        let expires = i64_of(record.expires_at).map_err(|_| StoreError)?;
                        let now = i64_of(*now).map_err(|_| StoreError)?;
                        let params: [Param<'_>; 5] =
                            [&record.id, &record.rp_id, &record.origin, &expires, &now];
                        client.query(statement, &params[..]).await
                    }
                    SingleUseClaim::Invite { record } => {
                        let params: [Param<'_>; 1] = [&record.code_hash];
                        client.query(statement, &params[..]).await
                    }
                }
                .map_err(|_| StoreError)?;
                Ok(rows.len() as u64)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
        .map_err(|_| auth::AuthError::Invalid)
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

/// A dropped JsFuture does not cancel fetch. Keep this controller alive through
/// response.json(), and abort on every exit (including deadline/caller drop).
/// The signal cancels both pending response headers and an unread response body.
struct AbortDohOnDrop(Option<worker::AbortController>);

impl Drop for AbortDohOnDrop {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.abort();
        }
    }
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
    let controller = worker::AbortController::default();
    let signal = controller.signal();
    let _abort = AbortDohOnDrop(Some(controller));
    let mut response = match Fetch::Request(request).send_with_signal(&signal).await {
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
            // The handler's single resolution deadline covers BOTH queries,
            // including headers + JSON bodies; it never restarts for AAAA.
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
    async fn probe(&self, target: &ValidatedTarget, tls: bool) -> bool {
        // The dialed port is the validated target's own allowlisted port —
        // never a caller-supplied substitute.
        let port = target.port();
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
    trace_id: String,
}

/// Native-testable request headers; the runtime supplies the resolved trace
/// header pair and operator-provisioned authorization, never inbound identity.
fn webhook_header_pairs(
    trace: (&'static str, &str),
    api_key: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut pairs = vec![
        ("content-type", "application/json".to_string()),
        (trace.0, trace.1.to_string()),
    ];
    if let Some(key) = api_key {
        pairs.push(("authorization", format!("Bearer {key}")));
    }
    pairs
}

// Dropping a Rust fetch future alone does not cancel its JavaScript promise.
// Keep the controller in the send future so the handler's deadline (or any
// enclosing cancellation) actually aborts the webhook. The guard also aborts
// on normal completion; only the response status is used by this mailer.
struct AbortMailOnDrop(Option<worker::AbortController>);

impl Drop for AbortMailOnDrop {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.abort();
        }
    }
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
        let api_key = env_string(&self.env, "EMAIL_API_KEY");
        for (name, value) in webhook_header_pairs(
            (crate::logging::TRACE_HEADER, &self.trace_id),
            api_key.as_deref(),
        ) {
            headers.set(name, &value).map_err(|_| StoreError)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(body.into()));
        let request = Request::new_with_init(&url, &init).map_err(|_| StoreError)?;
        let controller = worker::AbortController::default();
        let signal = controller.signal();
        let _abort = AbortMailOnDrop(Some(controller));
        let response = Fetch::Request(request)
            .send_with_signal(&signal)
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
    trace_id: &str,
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
        (crate::logging::TRACE_HEADER, trace_id.to_string()),
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
    trace_id: &str,
) -> WorkerResult<Response> {
    let headers = Headers::new();
    for (name, value) in render_header_pairs(response, cors, trace_id) {
        headers.append(name, &value)?;
    }
    Ok(
        Response::from_body(ResponseBody::Body(response.body.clone().into_bytes()))?
            .with_status(response.status)
            .with_headers(headers),
    )
}

fn internal_response() -> handlers::Response {
    handlers::Response {
        status: 500,
        body: r#"{"error":"internal error"}"#.to_string(),
        set_cookie: None,
        retry_after: None,
    }
}

fn internal_render(trace_id: &str) -> WorkerResult<Response> {
    render(&internal_response(), &[], trace_id)
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
/// 400, body `{"code":"invalid_request","message":"invalid request",
/// "retryable":false}`, no `set_cookie`), so the
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

/// Registry code for an error response: the envelope `code` when the body
/// carries one, else the status-implied code for statuses this worker owns
/// unambiguously (429 is only ever `rate_limited`, 404 only `not_found`,
/// and 500 falls back to `internal_error` per the taxonomy's unknown-code
/// rule in `docs/api-errors.md`). Other statuses stay unmapped: 400 has no
/// api-owned registry code and 401/403 each map to several codes, so
/// guessing would mislabel the group. Success bodies are never inspected:
/// callers only ask for 4xx/5xx responses.
fn response_error_code(status: u16, body: &str) -> Option<String> {
    let from_body = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("code")?.as_str().map(str::to_string));
    if from_body.is_some() {
        return from_body;
    }
    match status {
        429 => Some("rate_limited".to_string()),
        404 => Some("not_found".to_string()),
        500 => Some("internal_error".to_string()),
        _ => None,
    }
}

/// Log-safe display path: the bare path for static served routes, a fixed
/// template for credential revocation, and `unknown` otherwise. Raw paths
/// must never reach the
/// log: the catch-all 404 arm serves arbitrary probe paths that can carry
/// addresses or tokens, and unbounded distinct values would break log
/// grouping. Query strings are stripped before the known-path check for
/// the same reason.
fn served_display_path(path: &str) -> String {
    let bare = path.split('?').next().unwrap_or("");
    if handlers::is_known_path(bare) {
        bare.to_string()
    } else if bare
        .strip_prefix("/v1/credentials/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
    {
        "/v1/credentials/{id}".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Build identity for log lines: the 40-char lowercase hex commit supplied
/// at deploy time by the deploy lane via the public `BUILD_SHA` var (for
/// example `--var BUILD_SHA:$GITHUB_SHA`, wired for the api Worker by the
/// `staging-api` lane in `.github/workflows/deploy.yml`), or the explicit
/// `"unknown"` marker when absent or invalid (dev, preview without env,
/// unwired lane). Public non-secret config (see
/// `wrangler.toml.example`); never fail-closed, so a missing identity
/// degrades the line instead of the request. Normalized through the shared
/// validator, never logged raw.
fn build_sha_from_env(env: &Env) -> String {
    crate::logging::parse_build_sha(env_string(env, "BUILD_SHA").as_deref())
}

/// Wall-clock milliseconds for record timestamps.
#[cfg(target_arch = "wasm32")]
fn now_millis() -> u64 {
    worker::Date::now().as_millis()
}

/// Wall-clock milliseconds for record timestamps (native test builds).
#[cfg(not(target_arch = "wasm32"))]
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Emit one structured record through the shared emitter
/// (`docs/logging-standard.md` six-key envelope). Construction is
/// fail-closed: a rejected record (banned key, secret-like value, invalid
/// event) is dropped and `None` returned, never logged half-built. Every
/// value passed here is a code-controlled constant or a validated route
/// label, so a drop indicates a programming error. Returns the rendered
/// line so native tests can pin the shape without a Worker console.
fn emit_record(
    level: crate::logging::Level,
    event: &str,
    trace_id: &str,
    fields: serde_json::Value,
) -> Option<String> {
    let field_map = crate::logging::fields_from_json(fields).ok()?;
    let record = crate::logging::LogRecord::new(
        crate::logging::Worker::Api,
        level,
        trace_id,
        event,
        field_map,
        now_millis(),
    )
    .ok()?;
    let line = record.to_json_line();
    #[cfg(target_arch = "wasm32")]
    worker::console_log!("{line}");
    Some(line)
}

/// Worker `fetch` entry: adapts the runtime request into the handlers'
/// framework-agnostic shape. Misconfiguration (missing/invalid env)
/// answers the same generic 500 as any store outage.
pub async fn dispatch(mut req: Request, env: Env) -> WorkerResult<Response> {
    // Trace ID first: every record below carries it, including the
    // fail-closed paths. An invalid inbound value is dropped, never logged.
    let incoming_trace = req
        .headers()
        .get(crate::logging::TRACE_HEADER)
        .ok()
        .flatten();
    let trace_id = crate::logging::resolve_trace_id(incoming_trace.as_deref());
    let start_ms = now_millis();
    // Build identity first: every record below carries it, including the
    // fail-closed paths. An absent or invalid var becomes explicit
    // `"unknown"`, never a missing key.
    let build_sha = build_sha_from_env(&env);
    let Some(config) = config_from_env(&env) else {
        // Misconfiguration is an operator problem; clients only ever see
        // the generic internal error (never which variable is wrong).
        let _ = emit_record(
            crate::logging::Level::Error,
            "api.start.misconfigured",
            &trace_id,
            serde_json::json!({"error_class": "internal_error", "build_sha": build_sha, "release": build_sha}),
        );
        return internal_render(&trace_id);
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
    // Header-only bearer transports (ADR 0010 native keys, the QA seam
    // bearer): the raw `Authorization` value rides the request's
    // `authorization` field only, never the cookie or query shapes (which
    // the gates never read). Server logs redact this header the way
    // `/rest/*` logging redacts `u/p/t/s` material — nothing in this
    // module logs header values.
    let request = handlers::Request {
        method: req.method().to_string(),
        path: req.path(),
        cookie: headers_map.get("cookie").cloned(),
        origin: headers_map.get("origin").cloned(),
        authorization: headers_map.get("authorization").cloned(),
        // Template only: the gate replaces this with the collected bytes
        // on `Complete` and never dispatches it otherwise.
        body: Vec::new(),
    };
    let ctx = handlers::Ctx {
        cfg: &config,
        store: &PgStore {
            env: Some(env.clone()),
            trace_id: trace_id.clone(),
            #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
            test_client: std::cell::RefCell::new(None),
        },
        mailer: &WebhookMailer {
            origin: config.origin.clone(),
            env: env.clone(),
            trace_id: trace_id.clone(),
        },
        dialer: &SocketDialer,
        dns: &DohDns,
        now: worker::Date::now().as_millis() / 1000,
        source_ip: client_source_ip(&headers_map),
    };
    let served_method = request.method.clone();
    let served_path = served_display_path(&request.path);
    // Preserve the header marker independently of any truncated upload bytes.
    let refresh_content_type = headers_map.get("content-type").cloned();
    let response = dispatch_collected(collected, request, |collected_request| async move {
        if let Some(rejection) = handlers::refresh_transport_rejection(
            &collected_request,
            refresh_content_type.as_deref(),
        ) {
            return rejection;
        }
        handlers::handle(&ctx, &collected_request).await
    })
    .await;
    // One structured line per served request: route, status, latency, and
    // the error registry code when the response maps to one.
    let level = if response.status >= 500 {
        crate::logging::Level::Error
    } else {
        crate::logging::Level::Info
    };
    let mut fields = serde_json::json!({
        "route": format!("{served_method} {served_path}"),
        "status_code": response.status,
        "duration_ms": now_millis().saturating_sub(start_ms),
        "build_sha": build_sha,
        "release": build_sha,
    });
    if response.status >= 400 {
        if let Some(code) = response_error_code(response.status, &response.body) {
            fields["error_class"] = serde_json::Value::String(code);
        }
    }
    let _ = emit_record(level, "api.request.served", &trace_id, fields);
    // Exact match only: the configured origin is the one frontend origin
    // this Worker trusts with credentialed cross-origin calls. The raw
    // request Origin goes into the tested `cors_headers` decision — never
    // echoed directly.
    let cors = cors_headers(
        response.status,
        headers_map.get("origin").map(String::as_str),
        &config.origin,
    );
    render(&response, &cors, &trace_id)
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

    const REQUEST_TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    #[test]
    fn resolved_trace_reaches_response_mail_and_driver_diagnostics() {
        use futures_util::FutureExt;

        let hostile = "victim@example.com\r\nset-cookie: private-canary";
        for inbound in [Some(REQUEST_TRACE), None, Some(hostile)] {
            let trace_id = crate::logging::resolve_trace_id(inbound);
            assert!(crate::logging::is_valid_trace_id(&trace_id));
            if inbound == Some(REQUEST_TRACE) {
                assert_eq!(trace_id, REQUEST_TRACE);
            }
            for response in [
                handlers::Response {
                    status: 200,
                    body: r#"{"ok":true}"#.to_string(),
                    set_cookie: Some("session=synthetic; HttpOnly".to_string()),
                    retry_after: None,
                },
                handlers::Response {
                    status: 401,
                    body: r#"{"error":"unauthorized"}"#.to_string(),
                    set_cookie: None,
                    retry_after: None,
                },
                handlers::invalid_request(),
                handlers::preflight_response(),
                internal_response(),
            ] {
                let cors = cors_headers(
                    response.status,
                    Some("https://app.example"),
                    "https://app.example",
                );
                let pairs = render_header_pairs(&response, &cors, &trace_id);
                assert_eq!(
                    pairs
                        .iter()
                        .filter(|(name, _)| *name == crate::logging::TRACE_HEADER)
                        .count(),
                    1
                );
                assert!(pairs.contains(&(crate::logging::TRACE_HEADER, trace_id.clone())));
                assert!(cors.iter().all(|pair| pairs.contains(pair)));
                if let Some(cookie) = &response.set_cookie {
                    assert!(pairs.contains(&("set-cookie", cookie.clone())));
                }
                assert!(!format!("{pairs:?}").contains(hostile));
            }
            for api_key in [None, Some("synthetic-mail-key")] {
                let captured =
                    webhook_header_pairs((crate::logging::TRACE_HEADER, &trace_id), api_key);
                assert!(captured.contains(&("content-type", "application/json".to_string())));
                assert!(captured.contains(&(crate::logging::TRACE_HEADER, trace_id.clone())));
                assert_eq!(
                    captured
                        .iter()
                        .find(|(name, _)| *name == "authorization")
                        .map(|(_, value)| value.as_str()),
                    api_key.map(|_| "Bearer synthetic-mail-key")
                );
                assert_eq!(captured.len(), if api_key.is_some() { 3 } else { 2 });
                assert!(!format!("{captured:?}").contains(hostile));
            }
            let line = PgStore::connection_diagnostic(
                std::future::ready(Err::<(), _>(
                    "postgres://private-role:private-password@private-host",
                )),
                &trace_id,
                "unknown",
            )
            .now_or_never()
            .expect("synthetic driver is ready")
            .expect("failure emits a record");
            let record: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(record.as_object().unwrap().len(), 6);
            assert_eq!(record["trace_id"], trace_id);
            assert_eq!(record["event"], "api.db.connection_error");
            assert_eq!(
                record["fields"],
                serde_json::json!({"error_class": "service_unavailable", "build_sha": "unknown", "release": "unknown"})
            );
            assert!(!line.contains("private-"));
            assert!(!line.contains(hostile));
            assert!(PgStore::connection_diagnostic(
                std::future::ready(Ok::<(), ()>(())),
                &trace_id,
                "unknown"
            )
            .now_or_never()
            .unwrap()
            .is_none());
        }
    }

    #[test]
    fn dynamic_revoke_logs_only_the_fixed_template() {
        for path in [
            "/v1/credentials/private-id",
            "/v1/credentials/victim@example.com?token=private-token",
            "/v1/credentials/%2Fprivate-id?token=private-token",
        ] {
            let route = served_display_path(path);
            assert_eq!(route, "/v1/credentials/{id}");
            let line = emit_record(
                crate::logging::Level::Info,
                "api.request.served",
                REQUEST_TRACE,
                serde_json::json!({"route": format!("DELETE {route}"), "status_code": 200, "duration_ms": 0, "build_sha": "unknown"}),
            ).unwrap();
            assert!(!line.contains("private-"));
            assert!(!line.contains("victim@example.com"));
            assert!(!line.contains('?'));
        }
        for path in [
            "/v1/credentials/",
            "/v1/credentials/a/b?token=private-token",
            "/probe/private-id",
        ] {
            assert_eq!(served_display_path(path), "unknown");
        }
        assert_eq!(internal_response().body, r#"{"error":"internal error"}"#);
    }

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

    #[test]
    fn response_error_code_prefers_the_envelope_then_status() {
        assert_eq!(
            response_error_code(
                500,
                r#"{"code":"rate_limited","message":"x","retryable":true}"#
            ),
            Some("rate_limited".to_string())
        );
        // Legacy fixed bodies with no `code`: the status-implied mapping.
        assert_eq!(
            response_error_code(429, r#"{"error":"rate_limited"}"#),
            Some("rate_limited".to_string())
        );
        assert_eq!(
            response_error_code(404, r#"{"error":"not found"}"#),
            Some("not_found".to_string())
        );
        assert_eq!(
            response_error_code(500, r#"{"error":"internal error"}"#),
            Some("internal_error".to_string())
        );
        assert_eq!(
            response_error_code(500, "not json"),
            Some("internal_error".to_string())
        );
        assert_eq!(
            response_error_code(500, r#"{"code":42}"#),
            Some("internal_error".to_string())
        );
        // Ambiguous or unowned statuses stay unmapped rather than guessed.
        assert_eq!(
            response_error_code(400, r#"{"error":"invalid request"}"#),
            None
        );
        assert_eq!(response_error_code(401, r#"{"error":"x"}"#), None);
        assert_eq!(response_error_code(200, r#"{"ok":true}"#), None);
    }

    #[test]
    fn served_display_path_hides_unknown_paths_and_queries() {
        assert_eq!(served_display_path("/v1/vault"), "/v1/vault");
        assert_eq!(served_display_path("/healthz"), "/healthz");
        // A probe path carrying an address collapses to the sentinel ...
        assert_eq!(
            served_display_path("/v1/vault/victim@example.com"),
            "unknown"
        );
        assert_eq!(served_display_path("/probe/anything"), "unknown");
        // ... and query strings never reach the log either.
        assert_eq!(served_display_path("/v1/vault?token=abc"), "/v1/vault");
        assert_eq!(served_display_path("/probe/x?token=abc"), "unknown");
    }

    #[test]
    fn emit_record_renders_the_served_line_and_drops_rejected_fields() {
        let line = emit_record(
            crate::logging::Level::Info,
            "api.request.served",
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
            serde_json::json!({
                "route": "POST /v1/auth/magic/start",
                "status_code": 200,
                "duration_ms": 12,
            }),
        )
        .expect("served line must render");
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("record must be one JSON object");
        let object = parsed.as_object().expect("record must be an object");
        assert_eq!(object.len(), 6, "exactly the six envelope keys");
        assert_eq!(object["worker"], "api");
        assert_eq!(object["event"], "api.request.served");
        assert_eq!(object["fields"]["route"], "POST /v1/auth/magic/start");
        // Fail-closed: a banned key or secret-like value drops the line.
        assert!(emit_record(
            crate::logging::Level::Info,
            "api.request.served",
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
            serde_json::json!({"route": "GET unknown", "session_id": "s"}),
        )
        .is_none());
        assert!(
            emit_record(
                crate::logging::Level::Info,
                "api.request.served",
                "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
                serde_json::json!({"route": "GET /v1/vault/victim@example.com"}),
            )
            .is_none(),
            "address-carrying route value must not become a log line"
        );
    }

    #[test]
    fn served_line_carries_build_sha_known_or_unknown() {
        // The wiring (`dispatch` always sets the key) is pinned by the
        // source-presence test below; this pins the rendered shape: a known
        // SHA and the explicit unknown marker both survive validation and
        // keep the six-key envelope.
        for sha in [
            "0123456789abcdef0123456789abcdef01234567",
            crate::logging::BUILD_SHA_UNKNOWN,
        ] {
            let normalized = crate::logging::parse_build_sha(Some(sha));
            let line = emit_record(
                crate::logging::Level::Info,
                "api.request.served",
                "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
                serde_json::json!({
                    "route": "GET /healthz",
                    "status_code": 200,
                    "duration_ms": 3,
                    "build_sha": normalized,
                }),
            )
            .expect("served line with build_sha must render");
            let parsed: serde_json::Value =
                serde_json::from_str(&line).expect("record must be one JSON object");
            let object = parsed.as_object().expect("record must be an object");
            assert_eq!(object.len(), 6, "exactly the six envelope keys");
            assert_eq!(object["fields"]["build_sha"], sha);
        }
    }

    #[test]
    fn dispatch_wires_build_sha_into_every_emit_site() {
        // `build_sha_from_env` needs a wasm Env, so pin the call sites the
        // same way the keyring matrix pins `config_from_env`: slice the
        // pre-test source so this test's own literals cannot satisfy it.
        // Every api record (served, misconfigured, db connection_error)
        // must carry the normalized key — never a missing key or a raw var.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        assert!(
            production.contains("fn build_sha_from_env("),
            "the BUILD_SHA reader must exist"
        );
        assert!(
            production.contains("crate::logging::parse_build_sha"),
            "the reader must normalize through the shared validator"
        );
        // The served `build_sha` key is set on the `fields` value built
        // before the `emit_record` call, so a forward window from the event
        // name would miss it: pin the enclosing function bodies instead.
        let dispatch_body = production
            .split_once("pub async fn dispatch(")
            .expect("dispatch must exist")
            .1
            .split_once("\nfn ")
            .expect("a following fn must bound dispatch")
            .0;
        for event in ["api.request.served", "api.start.misconfigured"] {
            assert!(dispatch_body.contains(event), "dispatch must emit {event}");
        }
        // Match the key literal inside the `json!` fields, not the bare
        // substring: `let build_sha = …` alone must never satisfy this.
        // `dispatch` holds two emit sites (served + misconfigured), so a
        // dropped key at either site drops the count below two.
        assert_eq!(
            dispatch_body.matches("\"build_sha\": build_sha").count(),
            2,
            "served and misconfigured must each carry the build_sha key"
        );
        let store_body = production
            .split_once("impl PgStore")
            .expect("PgStore impl must exist")
            .1
            .split_once("\nfn user_from_row")
            .expect("a following fn must bound the impl")
            .0;
        assert!(
            store_body.contains("api.db.connection_error"),
            "PgStore::connect must emit api.db.connection_error"
        );
        assert!(
            store_body.contains("\"build_sha\": build_sha"),
            "the db connection_error site must carry the build_sha key"
        );
    }

    fn previous_43() -> String {
        // 32 bytes as unpadded base64url (43 chars), like the template.
        crate::b64::encode(&[7u8; 32])
    }

    #[cfg(target_arch = "wasm32")]
    fn assert_render_security_headers(response: &handlers::Response) {
        let rendered = render(response, &[], REQUEST_TRACE).expect("response renders");
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
        let headers = render_header_pairs(response, &[], REQUEST_TRACE);
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
        let headers = render_header_pairs(&response, &[], REQUEST_TRACE);

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
        // Render the real preflight, not a rebuilt literal, so a shape
        // change fails here instead of silently testing a stale copy.
        let response = handlers::preflight_response();
        assert_eq!(response.status, 204);
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
        let headers = render_header_pairs(&throttled, &[], REQUEST_TRACE);
        assert!(headers.contains(&(crate::rate_limit::RETRY_AFTER_HEADER, "3600".to_string())));
        let plain = handlers::Response {
            status: 200,
            body: r#"{"ok":true}"#.to_string(),
            set_cookie: None,
            retry_after: None,
        };
        let headers = render_header_pairs(&plain, &[], REQUEST_TRACE);
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

    fn kek_vars(pairs: &[(&str, String)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect()
    }

    fn kek_lookup(vars: &HashMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
        |name: &str| vars.get(name).cloned()
    }

    fn kek_b64(byte: u8) -> String {
        crate::b64::encode(&[byte; 32])
    }

    #[test]
    fn kek_set_legacy_single_key_mode_is_preserved() {
        // No VAULT_KEK_CURRENT: exactly the pre-versioned behavior —
        // VAULT_KEK_BASE64 + VAULT_KEK_VERSION, both required, no priors.
        let vars = kek_vars(&[
            ("VAULT_KEK_BASE64", kek_b64(9)),
            ("VAULT_KEK_VERSION", "41".to_string()),
        ]);
        let set = kek_set_from_lookup(kek_lookup(&vars)).expect("legacy config parses");
        assert_eq!(set.kek, [9u8; 32]);
        assert_eq!(set.version, 41);
        assert!(set.priors.is_empty());
        for missing in ["VAULT_KEK_BASE64", "VAULT_KEK_VERSION"] {
            let mut vars = vars.clone();
            vars.remove(missing);
            assert!(
                kek_set_from_lookup(kek_lookup(&vars)).is_none(),
                "legacy mode without {missing} must refuse to start"
            );
            // Present-but-empty legacy vars refuse like missing ones:
            // the raw lookup preserves presence, the legacy path
            // filters empties itself.
            let mut vars = vars.clone();
            vars.insert(missing.to_string(), String::new());
            assert!(
                kek_set_from_lookup(kek_lookup(&vars)).is_none(),
                "legacy mode with blanked {missing} must refuse to start"
            );
        }
    }

    #[test]
    fn kek_set_legacy_mode_ignores_versioned_vars() {
        // Provisioning V1 ahead of the cutover deploy must not change
        // serving behavior while VAULT_KEK_CURRENT is still unset.
        let vars = kek_vars(&[
            ("VAULT_KEK_BASE64", kek_b64(9)),
            ("VAULT_KEK_VERSION", "41".to_string()),
            ("VAULT_KEK_V1", kek_b64(1)),
        ]);
        let set = kek_set_from_lookup(kek_lookup(&vars)).expect("legacy config parses");
        assert_eq!(set.kek, [9u8; 32]);
        assert_eq!(set.version, 41);
        assert!(set.priors.is_empty());
    }

    #[test]
    fn kek_set_versioned_mode_selects_current_and_priors() {
        // VAULT_KEK_CURRENT=2: writes select V2, V1 is a retained prior,
        // and legacy vars are ignored (cutover deploy still has them set).
        let vars = kek_vars(&[
            ("VAULT_KEK_CURRENT", "2".to_string()),
            ("VAULT_KEK_V2", kek_b64(2)),
            ("VAULT_KEK_V1", kek_b64(1)),
            ("VAULT_KEK_BASE64", kek_b64(9)),
            ("VAULT_KEK_VERSION", "41".to_string()),
        ]);
        let set = kek_set_from_lookup(kek_lookup(&vars)).expect("versioned config parses");
        assert_eq!(set.kek, [2u8; 32]);
        assert_eq!(set.version, 2);
        assert_eq!(set.priors, vec![(1, [1u8; 32])]);
    }

    #[test]
    fn kek_set_versioned_mode_reads_prestaged_future_version() {
        // Ceremony phase 1: V3 is pre-staged while writes stay at 2 — the
        // Worker reads N+1 but still writes N.
        let vars = kek_vars(&[
            ("VAULT_KEK_CURRENT", "2".to_string()),
            ("VAULT_KEK_V2", kek_b64(2)),
            ("VAULT_KEK_V3", kek_b64(3)),
        ]);
        let set = kek_set_from_lookup(kek_lookup(&vars)).expect("pre-staged config parses");
        assert_eq!(set.kek, [2u8; 32]);
        assert_eq!(set.version, 2);
        assert_eq!(set.priors, vec![(3, [3u8; 32])]);
    }

    #[test]
    fn kek_set_versioned_mode_fails_closed() {
        let legacy = vec![
            ("VAULT_KEK_BASE64", kek_b64(9)),
            ("VAULT_KEK_VERSION", "41".to_string()),
        ];
        // Missing current key: a half-cutover never serves legacy writes.
        let mut vars = kek_vars(&legacy);
        vars.insert("VAULT_KEK_CURRENT".to_string(), "2".to_string());
        vars.insert("VAULT_KEK_V1".to_string(), kek_b64(1));
        assert!(kek_set_from_lookup(kek_lookup(&vars)).is_none());
        // Present-but-unparsable CURRENT: the operator asked for a
        // cutover, so silently serving legacy would split-brain the
        // ceremony's current version.
        let mut vars = kek_vars(&legacy);
        vars.insert("VAULT_KEK_CURRENT".to_string(), "two".to_string());
        assert!(kek_set_from_lookup(kek_lookup(&vars)).is_none());
        // Present-but-empty CURRENT (a dashboard "clear this var"):
        // fails closed, never downgrades to legacy mode while the
        // retained legacy vars are still set.
        let mut vars = kek_vars(&legacy);
        vars.insert("VAULT_KEK_CURRENT".to_string(), String::new());
        vars.insert("VAULT_KEK_V2".to_string(), kek_b64(2));
        assert!(
            kek_set_from_lookup(kek_lookup(&vars)).is_none(),
            "blanked CURRENT must refuse to start, not serve legacy writes"
        );
        // Blanked CURRENT with no legacy fallback also refuses.
        let vars = kek_vars(&[("VAULT_KEK_CURRENT", String::new())]);
        assert!(kek_set_from_lookup(kek_lookup(&vars)).is_none());
        // Whitespace-only CURRENT (a dashboard value of a single space,
        // or "2 " from a stray paste): refused by the blank guard, never
        // silently read as absent — and never parsed as a version.
        for blank in [" ", "  ", "2 ", "\t"] {
            let mut vars = kek_vars(&legacy);
            vars.insert("VAULT_KEK_CURRENT".to_string(), blank.to_string());
            vars.insert("VAULT_KEK_V2".to_string(), kek_b64(2));
            assert!(
                kek_set_from_lookup(kek_lookup(&vars)).is_none(),
                "whitespace-only CURRENT {blank:?} must refuse to start"
            );
        }
        // Present-but-invalid priors fail startup, never serve half-keyed.
        for bad in [
            "!!!not-base64!!!".to_string(),
            // Padded std-base64 is rejected like every other secret codec.
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
            // Short keys can neither wrap nor unwrap.
            crate::b64::encode(&[1u8; 16]),
        ] {
            let vars = kek_vars(&[
                ("VAULT_KEK_CURRENT", "2".to_string()),
                ("VAULT_KEK_V2", kek_b64(2)),
                ("VAULT_KEK_V1", bad),
            ]);
            assert!(
                kek_set_from_lookup(kek_lookup(&vars)).is_none(),
                "invalid retained prior must refuse to start"
            );
        }
        // Blanked staged prior: fails closed, never silently treated
        // as unprovisioned.
        let vars = kek_vars(&[
            ("VAULT_KEK_CURRENT", "2".to_string()),
            ("VAULT_KEK_V2", kek_b64(2)),
            ("VAULT_KEK_V1", String::new()),
        ]);
        assert!(
            kek_set_from_lookup(kek_lookup(&vars)).is_none(),
            "blanked retained prior must refuse to start"
        );
        // Blanked current key: fails closed like a missing one.
        let vars = kek_vars(&[
            ("VAULT_KEK_CURRENT", "2".to_string()),
            ("VAULT_KEK_V2", String::new()),
            ("VAULT_KEK_V1", kek_b64(1)),
        ]);
        assert!(
            kek_set_from_lookup(kek_lookup(&vars)).is_none(),
            "blanked current key must refuse to start"
        );
    }

    #[test]
    fn kek_set_discovery_window_bounds() {
        // CURRENT=20: the window is 12..=28 — boundary versions provision,
        // anything outside stays unprovisioned (reads for it fail closed
        // per-row, like any unknown version).
        let vars = kek_vars(&[
            ("VAULT_KEK_CURRENT", "20".to_string()),
            ("VAULT_KEK_V20", kek_b64(20)),
            ("VAULT_KEK_V12", kek_b64(12)),
            ("VAULT_KEK_V11", kek_b64(11)),
            ("VAULT_KEK_V28", kek_b64(28)),
            ("VAULT_KEK_V29", kek_b64(29)),
        ]);
        let set = kek_set_from_lookup(kek_lookup(&vars)).expect("window config parses");
        assert_eq!(set.version, 20);
        let versions: Vec<u32> = set.priors.iter().map(|(v, _)| *v).collect();
        assert_eq!(versions, vec![12, 28]);
        assert_eq!(
            KEK_VERSION_WINDOW,
            20 - 12,
            "pin the window the boundary test assumes"
        );
    }

    #[test]
    fn kek_set_discovery_window_saturates_at_zero() {
        // CURRENT=2: the low edge saturates instead of wrapping — V0
        // provisions when present.
        let vars = kek_vars(&[
            ("VAULT_KEK_CURRENT", "2".to_string()),
            ("VAULT_KEK_V2", kek_b64(2)),
            ("VAULT_KEK_V0", kek_b64(0)),
        ]);
        let set = kek_set_from_lookup(kek_lookup(&vars)).expect("saturating config parses");
        assert_eq!(set.priors, vec![(0, [0u8; 32])]);
    }

    #[test]
    fn config_from_env_provisions_the_kek_set() {
        // `config_from_env` needs a wasm Env, so pin its KEK wiring the
        // same way the keyring matrix pins its call site: slice ONLY the
        // fn body out of the pre-test source, so this test's own
        // literals cannot satisfy the assertions.
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
            body.contains("kek_set_from_lookup(|name| env_string_raw(env, name))"),
            "config_from_env must build the KEK set from the presence-preserving env lookup"
        );
        assert!(
            body.contains(".with_prior_keks(kek_set.priors)"),
            "config_from_env must provision the retained priors on the Config"
        );
        assert!(
            body.contains("kek_set.kek") && body.contains("kek_set.version"),
            "Config::new must take the selected current key and version"
        );
    }

    #[test]
    fn env_string_raw_preserves_presence_without_filtering() {
        // `env_string_raw` needs a wasm Env, so pin its
        // presence-preserving body the same way the KEK wiring pins its
        // call site: slice ONLY the fn out of the pre-test source, so
        // this test's own literals cannot satisfy the assertions.
        // Reverting the body to empty-filtering (the pre-PR production
        // behavior) would silently downgrade a blanked
        // VAULT_KEK_CURRENT to legacy mode with the suite green — the
        // KEK unit tests drive `kek_set_from_lookup` through a HashMap
        // closure, never through this fn. Each slice is bounded at the
        // fn's own closing brace: these are negative assertions, so the
        // looser next-fn bound would sweep in the neighboring
        // const/struct and go brittle.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let raw_body = production
            .split_once("fn env_string_raw(")
            .expect("env_string_raw must exist")
            .1
            .split_once("\n}\n")
            .expect("env_string_raw must close")
            .0;
        assert!(
            !raw_body.contains("is_empty"),
            "env_string_raw must not filter empty values: presence preservation is the fail-closed decision's input"
        );
        assert!(
            !raw_body.contains(".filter("),
            "env_string_raw must not filter: the only empty filter lives in env_string"
        );
        assert!(
            raw_body.contains("env.secret(name)") && raw_body.contains("env.var(name)"),
            "env_string_raw must stay the secret-then-var fallthrough, not delegate to the filtering lookup"
        );
        // `env_string` stays the single place that maps present-but-empty
        // to absent.
        let body = production
            .split_once("fn env_string(")
            .expect("env_string must exist")
            .1
            .split_once("\n}\n")
            .expect("env_string must close")
            .0;
        assert!(
            body.contains("env_string_raw(env, name).filter(|s| !s.is_empty())"),
            "env_string must stay exactly the raw lookup plus the single empty filter"
        );
    }

    #[test]
    fn native_rest_pepper_matrix_is_both_or_neither() {
        // `config_from_env` needs a wasm Env, so pin its keyring matrix
        // the same way the overlay test pins its call site: slice ONLY
        // the fn body out of the pre-test source, so this test's own
        // literals cannot satisfy the assertions. Unset means the
        // endpoints fail closed per-request (never a startup refusal, so
        // provisioning is never a flag-day); half-set (one half without
        // the other) refuses to start like the stray session-secret
        // previous; a short pepper refuses to start like every other
        // secret codec.
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
            body.contains("NATIVE_REST_PEPPER_BASE64"),
            "config_from_env must read the native REST pepper"
        );
        assert!(
            body.contains("NATIVE_REST_PEPPER_VERSION"),
            "config_from_env must read the native REST pepper version"
        );
        assert!(
            body.contains("with_native_rest_pepper("),
            "config_from_env must provision the keyring when both halves validate"
        );
        assert!(
            body.contains("MIN_SECRET_LEN"),
            "the pepper must meet the same minimum as every other secret"
        );
        // The half-configured refusal: one half present without the other
        // must fail startup, never serve half-keyed.
        assert!(
            body.contains("(None, Some(_)) | (Some(_), None)"),
            "a half-configured keyring must refuse to start"
        );
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
            "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS".to_string()
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
    fn config_from_env_arms_qa_seam_only_behind_the_flag() {
        // The staging-only code seam must never arm by default: slice ONLY
        // the `config_from_env` body out of the pre-test source (so this
        // test's own literals cannot satisfy the assertions) and require
        // the explicit-flag gate plus both builder calls. Dropping the
        // flag check — or staging the bearer unconditionally — fails here.
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
            body.contains("if truthy(\"QA_CODE_SEAM\")"),
            "config_from_env must gate the seam behind the explicit staging flag"
        );
        assert!(
            body.contains("with_qa_code_seam(true)"),
            "config_from_env must arm the seam inside the flag gate"
        );
        assert!(
            body.contains("with_qa_auth_token(") && body.contains("\"QA_AUTH_TOKEN\""),
            "config_from_env must stage the QA bearer from the secret inside the flag gate"
        );
        assert!(
            body.contains("trim().is_empty()"),
            "config_from_env must reject whitespace-only QA bearer values"
        );
    }

    #[test]
    fn qa_outbox_statements_key_by_email() {
        // `PgStore` QA methods only compile into the wasm Worker, so pin
        // their statement text natively: the upsert must target the
        // migration 0018 table with a per-mailbox conflict key (one
        // latest-token row per synthetic mailbox), and retrieval must
        // filter by that same mailbox — never a full-table read.
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
        };
        let record = fn_body("async fn record_qa_issued_code(");
        assert!(
            record.contains("INSERT INTO qa_issued_codes (email, token, issued_at)")
                && record.contains("ON CONFLICT (email) DO UPDATE"),
            "record_qa_issued_code must upsert one latest-token row per mailbox"
        );
        let lookup = fn_body("async fn qa_issued_code(");
        assert!(
            lookup.contains("SELECT token, issued_at FROM qa_issued_codes WHERE email = $1"),
            "qa_issued_code must read exactly one mailbox row"
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
        // upsert fence, its zero-row guard, the test-lease fence, and the
        // native-credential mint fence are the only things stopping
        // credential writes behind the deletion marker.
        // Pin them the same way `insert_magic_prune_is_per_user_and_expired_only`
        // pins its statement: slice ONLY each fn body out of the pre-test
        // source, so this test's own literals cannot satisfy the assertions.
        // Dropping `AND deleting_at IS NULL` from any statement, or the
        // `affected == 0` guard from a fenced write, must fail here.
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
        // Vault upsert: the shared statement carries the fence and the
        // owner binding; both call sites execute it (never an inline
        // fork) and fail closed on 0 rows. Slice the plain-fn helper to
        // its own closing brace — the async-fn bound would overrun into
        // unrelated items.
        let upsert_sql = production
            .split_once("fn upsert_vault_sql()")
            .expect("upsert_vault_sql must exist")
            .1
            .split_once("\n    }\n")
            .expect("upsert_vault_sql body must close")
            .0;
        assert!(
            upsert_sql.contains(FENCE),
            "upsert_vault_sql must select its row through the owner fence"
        );
        assert!(
            !upsert_sql.contains("WHERE id = $2"),
            "upsert_vault_sql fence binds the owner ($1), never a column value"
        );
        assert!(
            upsert_sql.contains("SELECT $1, $2"),
            "upsert_vault_sql must keep SELECT $1 bound to the owner id"
        );
        let upsert = fn_body("async fn upsert_vault(");
        assert!(
            upsert.contains("upsert_vault_sql()"),
            "upsert_vault must execute the shared statement"
        );
        assert!(
            !upsert.contains("INSERT INTO provider_vaults"),
            "upsert_vault must not inline a fork of the statement"
        );
        let save = fn_body("async fn save_vault_and_mint_provider(");
        assert!(
            save.contains("upsert_vault_sql()"),
            "the atomic save must execute the shared upsert statement"
        );
        assert!(
            !save.contains("INSERT INTO provider_vaults"),
            "the atomic save must not inline a fork of the upsert"
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
        let arm_end = upsert[guard..]
            .find('}')
            .expect("the zero-row guard arm must close");
        assert!(
            upsert[guard..guard + arm_end].contains("return Err"),
            "the zero-row guard must fail closed (return Err), not Ok"
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
        // Native-credential mint: the same deletion fence on the owner
        // parameter, so a fresh session during the 24 h grace cannot mint
        // a new long-lived bearer path behind the marker. The credential
        // id binds $1, so the owner fence binds $2 here (not $1 as in the
        // vault upsert above).
        const NATIVE_FENCE: &str = "FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence";
        let mint = fn_body("async fn insert_native_credential(");
        assert!(
            mint.contains(NATIVE_FENCE),
            "insert_native_credential must select its row through the owner fence"
        );
        assert!(
            !mint.contains("WHERE id = $1"),
            "insert_native_credential fence binds the owner ($2), never $1"
        );
        assert!(
            mint.contains("SELECT $1, $2"),
            "insert_native_credential must keep SELECT $1 bound to the credential id and $2 to the owner"
        );
        let mint_guard = mint
            .find("if affected == 0")
            .expect("insert_native_credential must fail closed on 0 rows");
        let mint_ok = mint
            .find("Ok(())")
            .expect("insert_native_credential must return Ok on the write path");
        assert!(
            mint_guard < mint_ok,
            "the zero-row guard must precede the success return"
        );
        let mint_arm_end = mint[mint_guard..]
            .find('}')
            .expect("the zero-row guard arm must close");
        assert!(
            mint[mint_guard..mint_guard + mint_arm_end].contains("return Err"),
            "the zero-row guard must fail closed (return Err), not Ok"
        );
    }

    #[test]
    fn fenced_provider_mint_uses_the_pinned_statement_and_zero_row_guard() {
        // ADR 0002 rule 3: the providers mint is a credential write behind
        // the same deletion fence as the vault upsert. The statement shape
        // (fence, literals, no ON CONFLICT) is pinned in
        // `crate::providers`; this pins the production call site: it must
        // call that statement (never an inline fork), bind the parameters
        // in `PROVIDER_MINT_PARAM_ORDER`, and fail closed on 0 rows.
        // Slice ONLY the fn body out of the pre-test source, so this
        // test's own literals cannot satisfy the assertions.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let mint = production
            .split_once("async fn insert_provider(")
            .expect("insert_provider must exist")
            .1
            .split_once("\n    async fn ")
            .expect("a following fn must bound the body")
            .0;
        assert!(
            mint.contains("insert_provider_sql()"),
            "insert_provider must execute the pinned statement"
        );
        assert!(
            !mint.contains("INSERT INTO providers"),
            "insert_provider must not inline a fork of the statement"
        );
        // Bind order follows PROVIDER_MINT_PARAM_ORDER: several swaps
        // (id/user_id/label, the two nonces, the two clocks) would still
        // compile and silently corrupt the row, so the order is pinned
        // positionally.
        let binds = [
            "&row.id",
            "&row.user_id",
            "&row.label",
            "&row.encrypted_creds.as_slice()",
            "try_from(row.kek_version)",
            "&row.dek_wrapped_nonce.as_slice()",
            "&row.dek_wrapped.as_slice()",
            "&row.payload_nonce.as_slice()",
            "i64_of(row.created_at)",
            "i64_of(row.last_verified_at)",
        ];
        let mut last = 0;
        for needle in binds {
            let pos = mint
                .find(needle)
                .unwrap_or_else(|| panic!("insert_provider must bind {needle}"));
            assert!(
                pos > last,
                "bind order must follow PROVIDER_MINT_PARAM_ORDER ({needle} out of order)"
            );
            last = pos;
        }
        let guard = mint
            .find("if affected == 0")
            .expect("insert_provider must fail closed on 0 rows");
        let ok = mint
            .find("Ok(())")
            .expect("insert_provider must return Ok on the write path");
        assert!(
            guard < ok,
            "the zero-row guard must precede the success return"
        );
        let arm_end = mint[guard..]
            .find('}')
            .expect("the zero-row guard arm must close");
        assert!(
            mint[guard..guard + arm_end].contains("return Err"),
            "the zero-row guard must fail closed (return Err), not Ok"
        );
        // The atomic save reuses the pinned statement with its own bind
        // list: the same positional pin applies there, or a swap in the
        // save path would corrupt rows the single-writer pin cannot see.
        let save = production
            .split_once("async fn save_vault_and_mint_provider(")
            .expect("save_vault_and_mint_provider must exist")
            .1
            .split_once("\n    async fn ")
            .expect("a following fn must bound the body")
            .0;
        assert!(
            save.contains("insert_provider_sql()"),
            "the atomic save must execute the pinned mint statement"
        );
        assert!(
            !save.contains("INSERT INTO providers"),
            "the atomic save must not inline a fork of the mint"
        );
        let save_binds = [
            "&mint.id",
            "&mint.user_id",
            "&mint.label",
            "&mint.encrypted_creds.as_slice()",
            "try_from(mint.kek_version)",
            "&mint.dek_wrapped_nonce.as_slice()",
            "&mint.dek_wrapped.as_slice()",
            "&mint.payload_nonce.as_slice()",
            "i64_of(mint.created_at)",
            "i64_of(mint.last_verified_at)",
        ];
        let mut last = 0;
        for needle in save_binds {
            let pos = save
                .find(needle)
                .unwrap_or_else(|| panic!("the atomic save must bind {needle}"));
            assert!(
                pos > last,
                "save bind order must follow PROVIDER_MINT_PARAM_ORDER ({needle} out of order)"
            );
            last = pos;
        }
    }

    #[test]
    fn atomic_vault_save_runs_upsert_mint_and_prune_in_one_transaction() {
        // The save is one unit: the fenced upsert, the fenced mint, and
        // the rotation prune run on one transaction; any failure rolls
        // back before the handler classifies it. Dropping the
        // transaction, a zero-row guard, or a rollback must fail here.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let save = production
            .split_once("async fn save_vault_and_mint_provider(")
            .expect("save_vault_and_mint_provider must exist")
            .1
            .split_once("\n    async fn ")
            .expect("a following fn must bound the body")
            .0;
        assert!(
            save.contains(".transaction()"),
            "the save must open a transaction"
        );
        assert!(
            save.contains("prune_superseded_providers_sql()"),
            "the save must run the pinned rotation prune"
        );
        assert!(
            !save.contains("DELETE FROM providers"),
            "the save must not inline a fork of the prune"
        );
        // Both fenced writes guard 0 rows (fence closed → rollback, never
        // a partial commit); the prune needs no guard (zero pruned rows
        // is success).
        for needle in ["if upserted == 0", "if minted == 0"] {
            assert!(
                save.contains(needle),
                "the save must fail closed on a 0-row fenced write ({needle})"
            );
        }
        let rollbacks = save.match_indices("rollback()").count();
        assert_eq!(
            rollbacks, 3,
            "upsert guard, mint guard, and prune error each roll back ({rollbacks})"
        );
        let commit = save.find("commit()").expect("the save must commit");
        let ok = save
            .find("Ok(())")
            .expect("the save must return Ok on the write path");
        assert!(commit < ok, "the commit must precede the success return");
        // Vault delete purges through the pinned statement too (never an
        // inline fork), in the same rollback-on-error shape.
        let delete = production
            .split_once("async fn delete_vault(")
            .expect("delete_vault must exist")
            .1
            .split_once("\n    async fn ")
            .expect("a following fn must bound the body")
            .0;
        assert!(
            delete.contains("purge_unreferenced_providers_sql()"),
            "delete_vault must execute the pinned purge statement"
        );
        assert!(
            delete.contains(".transaction()"),
            "delete_vault must purge and delete atomically"
        );
        let vault_delete = delete
            .find("\"DELETE FROM provider_vaults")
            .expect("delete_vault must remove the vault row");
        let purge = delete
            .find(".execute(purge_unreferenced_providers_sql()")
            .expect("delete_vault must purge unreferenced providers");
        assert!(
            vault_delete < purge,
            "delete the vault before purging, so a concurrent save cannot leave orphaned credentials"
        );
        let commit = delete
            .find("transaction.commit().await.map_err(|_| StoreError)?")
            .expect("delete_vault must commit and propagate commit failure");
        let ok = delete.find("Ok(())").expect("delete_vault must return Ok");
        assert!(
            purge < commit && commit < ok,
            "commit both deletes before reporting success"
        );
    }

    #[test]
    fn vault_delete_locks_the_owner_before_touching_credential_rows() {
        // Locking only an existing vault row misses first-save races. The
        // owner lock also conflicts with the save's in-statement FOR SHARE
        // fence when no vault exists; lock errors must fail closed.
        let source = include_str!("worker_entry.rs");
        let production = source.split_once("#[cfg(test)]").unwrap().0;
        let delete = production
            .split_once("async fn delete_vault(")
            .unwrap()
            .1
            .split_once("\n    async fn ")
            .unwrap()
            .0;
        let lock = delete
            .find("\"SELECT id FROM users WHERE id = $1 FOR UPDATE\"")
            .expect("delete_vault must lock the owner even when the vault is absent");
        let lock_call = delete[..lock]
            .rfind("transaction")
            .expect("the owner lock must use the delete transaction");
        let lock_end = delete[lock..]
            .find(".map_err(|_| StoreError)?")
            .expect("owner-lock failure must propagate before any deletes")
            + lock;
        let vault_delete = delete.find("\"DELETE FROM provider_vaults").unwrap();
        let purge = delete
            .find(".execute(purge_unreferenced_providers_sql()")
            .unwrap();
        assert!(delete[lock_call..lock].contains(".query_opt("));
        assert!(delete[lock..lock_end].contains("&[&user_id]"));
        assert!(delete[lock..lock_end].contains(".await"));
        assert!(lock < lock_end && lock_end < vault_delete && lock_end < purge);
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

    // Native contract for the per-statement bound: PgStore::bounded races
    // a store operation against the supplied budget and expires to the
    // opaque StoreError. No Worker sockets or database; the watchdog only
    // fails a stuck test. Mirrors hyperdrive_deadline.rs, which contracts
    // the shared with_deadline helper this seam delegates to.
    fn poll_bounded_to_ready<F: Future>(mut future: std::pin::Pin<&mut F>) -> F::Output {
        use std::task::{Context, Poll};
        use std::time::{Duration, Instant};
        let end = Instant::now() + Duration::from_secs(2);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
            assert!(
                Instant::now() < end,
                "bounded seam still pending at the independent 2s watchdog"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn bounded_statement_expires_a_stalled_operation_to_the_opaque_error() {
        use std::time::{Duration, Instant};
        let mut bounded = Box::pin(PgStore::bounded(
            std::future::pending::<Result<u8, StoreError>>(),
            25,
        ));
        let start = Instant::now();
        let outcome = poll_bounded_to_ready(bounded.as_mut());
        assert!(
            outcome.is_err(),
            "a stalled statement must fail closed, not hang"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(25),
            "expiry must not be an immediate unconditional error"
        );
    }

    #[test]
    fn bounded_statement_passes_a_ready_operation_through() {
        let mut bounded = Box::pin(PgStore::bounded(async { Ok::<_, StoreError>(7u8) }, 5_000));
        assert_eq!(poll_bounded_to_ready(bounded.as_mut()).unwrap(), 7);
    }

    #[test]
    fn bounded_expiry_drops_between_phases_before_in_future_compensation_runs() {
        use std::cell::Cell;
        // The rate-limit pair claims its first slot, then its second,
        // inside one bounded future. When the second phase stalls past
        // the budget, the seam drops the whole future — an Err-arm
        // release written inside the future can never run. The pair
        // therefore commits both claims in one database transaction (a
        // dropped tokio-postgres transaction rolls back) instead of
        // compensating in Rust; this test pins the drop that makes that
        // necessary, using the real seam with no database.
        let compensated = Cell::new(false);
        let mut raced = Box::pin(PgStore::bounded(
            async {
                match std::future::pending::<Result<(), StoreError>>().await {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        compensated.set(true);
                        Err(err)
                    }
                }
            },
            25,
        ));
        let outcome = poll_bounded_to_ready(raced.as_mut());
        assert!(outcome.is_err(), "a stalled second phase must fail closed");
        assert!(
            !compensated.get(),
            "expiry drops the future: in-future compensation never runs"
        );
    }
}
