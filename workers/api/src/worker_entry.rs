//! Worker runtime wiring: env config, Hyperdrive Postgres store,
//! DNS-over-HTTPS resolution, vetted-peer socket probing, and the
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
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio_postgres::types::Type;
use tokio_postgres::GenericClient;
use worker::{
    Delay, Env, Fetch, Headers, Method, Request, RequestInit, Response, ResponseBody,
    Result as WorkerResult,
};

use crate::admin_ingest::{
    self, AdminIngestAttempt, AdminIngestDeleteOutcome, AdminIngestUploadOutcome,
    StoredAdminIngestJob,
};
use crate::auth::{
    self, InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential, SingleUseClaim,
    SingleUseStore,
};
use crate::handlers::{
    self, AdminInviteIssueOutcome, AdminInviteRevokeOutcome, AdminWriteOutcome, AsyncDns,
    ChallengeRow, Config, Dialer, Mailer, NativeCredentialRow, PairAdmission, PlanRow,
    QaIssuedCode, RateLimitBudget, Store, StoreError, SubscriptionRow, UserRecord,
};
use crate::session::SessionRecord;
use crate::vault::{
    AuditEvent, ValidatedTarget, DIAL_TIMEOUT_MS, MAX_DIAL_ADDRESSES, TOTAL_TIMEOUT_MS,
};

/// Hyperdrive binding name; must match `wrangler.toml.example`.
const HYPERDRIVE_BINDING: &str = "HYPERDRIVE";
/// Mirror Hyperdrive binding name for the MusicBrainz mirror cluster
/// (ADR 0002 §1): the mirror lives on its own cluster, so catalogue reads
/// split the app-side page (above) from mirror enrichment (this binding).
/// Must match `wrangler.toml.example`; the shared `nntunev2-mb` config id
/// is inventoried in docs/ops.md like the mb worker's `MB_DB_BINDING`.
const MIRROR_HYPERDRIVE_BINDING: &str = "MB_DB";
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
    let mut config = config;
    // Dedicated Worker secret only: ordinary vars and the native pepper may
    // never supply compatibility key material. Missing/invalid stays disabled
    // per request, without taking unrelated native endpoints down.
    config.subsonic_index = env.secret("SUBSONIC_INDEX_PEPPER").ok().and_then(|raw| {
        let raw = zeroize::Zeroizing::new(raw.to_string());
        crate::subsonic_config::SubsonicIndexConfig::parse(&raw).ok()
    });
    // Stripe test-mode secrets (Checkout-shape slice): both optional at
    // startup — unprovisioned paths fail closed per-request (the webhook
    // 500s so Stripe retries it; purchases refuse without a Stripe call)
    // while every other route keeps serving. A staged bearer must be a
    // test-mode key: `sk_live_` or any other shape refuses the whole
    // startup before any traffic is served. Blank reads as absent.
    let stripe_secret = env_string(env, "STRIPE_SECRET_KEY");
    if let Some(key) = stripe_secret.as_deref() {
        if !crate::stripe::valid_secret_key(key) {
            return None;
        }
    }
    let stripe_webhook = env_string(env, "STRIPE_WEBHOOK_SECRET");
    let config = config.with_stripe(stripe_secret, stripe_webhook);
    // Play-grant signing secret, shared with the stream Worker: raw
    // string bytes, never base64-decoded (see the helper). Absent (or
    // the minter's minimum length not met at issue time) fails the
    // play routes closed per-request; every other route keeps serving.
    let config = match play_grant_secret_from_lookup(|name| env_string(env, name)) {
        Some(secret) => config.with_play_grant_secret(secret),
        None => config,
    };
    // Meter-sink HMAC key, shared with the stream Worker the same
    // raw-bytes way (see the helper). Absent (or under the sink's
    // minimum length at verify time) fails the internal endpoint
    // closed per-request; the stream DO keeps windows pending and
    // retries on its alarm.
    let config = match meter_sink_secret_from_lookup(|name| env_string(env, name)) {
        Some(secret) => config.with_meter_sink_secret(secret),
        None => config,
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

/// Play-grant secret overlay: the RAW STRING BYTES of the env value.
/// This is the one secret never base64-decoded here — the stream
/// Worker keys its HMAC with the raw bytes of the same string, so
/// decoding would silently desynchronize the two sides and refuse
/// every grant stream-side. Pure over a lookup so the no-decode rule
/// is unit-pinned below.
fn play_grant_secret_from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Vec<u8>> {
    get(crate::play_grant::PLAY_GRANT_SECRET_VAR).map(String::into_bytes)
}

/// Meter-sink secret overlay: same raw-string-bytes rule as the
/// play-grant secret — the stream Worker's sink signer keys its HMAC
/// with the raw bytes of the same env string, so decoding here would
/// silently refuse every flush. Unit-pinned below alongside the
/// play-grant probe.
fn meter_sink_secret_from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Vec<u8>> {
    get(crate::meter_sink::METER_SINK_SECRET_VAR).map(String::into_bytes)
}

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

/// The user's `playback_leases` row, if any: shared by the lease read
/// and the acquire-refusal path (which reads the surviving holder for
/// the 429's `Retry-After`). More than one row is impossible (user PK)
/// and errors rather than picking one.
async fn query_playback_lease_row(
    client: &impl GenericClient,
    user_id: &str,
) -> Result<Option<crate::playback::LeaseRow>, StoreError> {
    let rows = client
        .query_typed(
            crate::playback::playback_lease_select_statement(),
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() > 1 {
        return Err(StoreError);
    }
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(crate::playback::LeaseRow {
            session_id: row.try_get("session_id").map_err(|_| StoreError)?,
            track_id: row.try_get("track_id").map_err(|_| StoreError)?,
            play_id: row.try_get("play_id").map_err(|_| StoreError)?,
            epoch: u64_of(row.try_get("epoch").map_err(|_| StoreError)?)?,
            expires_at: u64_of(row.try_get("expires_at").map_err(|_| StoreError)?)?,
        })),
    }
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
        .query_typed(
            "DELETE FROM auth_rate_limit_slots WHERE expires_at <= $1",
            &[(&now, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    Ok(())
}

/// Claimed-slot attempts per [`claim_rate_limit_slot`] call. Each attempt
/// is one statement; a retry only happens when a concurrent claim stole
/// the selected slot between our snapshot and the insert (the conflict
/// clause then matches nothing). Past this bound the caller fails closed
/// as exhausted, so a contended pool degrades to denials, never to a
/// hung gate: every attempt shares the caller's `TOTAL_TIMEOUT_MS`
/// deadline via `Self::bounded`.
const RATE_LIMIT_CLAIM_ATTEMPTS: u32 = 4;

/// Per-attempt dispersal for [`claim_rate_limit_slot`]: successive attempts
/// start their free-slot search at successive offsets, so K overlapping
/// claims on one scope (parallel page loads sharing an IP budget on a
/// fresh hourly pool) aim at K distinct slots instead of all colliding on
/// slot 0 and spending the [`RATE_LIMIT_CLAIM_ATTEMPTS`] budget on each
/// other. Monotonic, not random: decorrelation is all the anti-join
/// needs, and a 32-bit counter keeps wasm32 (no OS RNG at this layer)
/// and unit fakes deterministic.
static CLAIM_DISPERSAL: AtomicU32 = AtomicU32::new(0);

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
    // plus a full-pool anti-join claim that finds nothing. A concurrent
    // race that passes this snapshot simply fails closed below.
    let exhausted = client
        .query_typed(
            handlers::RATE_LIMIT_EXHAUSTED_SQL,
            &[
                (&budget.scope_key, Type::TEXT),
                (&budget.action, Type::TEXT),
                (&now_i, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?
        .first()
        .ok_or(StoreError)?
        .try_get::<_, i64>(0)
        .map_err(|_| StoreError)?;
    if exhausted >= i64::try_from(budget.limit).map_err(|_| StoreError)? {
        return Ok(None);
    }
    // One statement per attempt, never one per occupied slot: the
    // `generate_series` anti-join selects a free-or-expired slot pool-wide
    // from a per-attempt rotated start, so an admitted request costs O(1)
    // round trips no matter how full the pool is. A sequential per-slot
    // loop would make the Nth admitted request pay N conflicting upserts
    // inside one transaction (up to ~3300 for the bearer pair near the
    // hourly cap), hitting the store deadline as a 404 before the budget
    // is used up.
    let limit_hi = i32::try_from(budget.limit.saturating_sub(1)).map_err(|_| StoreError)?;
    for _ in 0..RATE_LIMIT_CLAIM_ATTEMPTS {
        // Fresh offset per attempt, not per call: a loser re-enters at a
        // different start instead of queueing behind the winner's slot.
        // Masked to the budget so the modulo below always lands in range
        // (the exhausted fast-fail above already returned for limit 0).
        let start =
            u64::from(CLAIM_DISPERSAL.fetch_add(1, Ordering::Relaxed)) % budget.limit.max(1);
        let start_i = i64_of(start)?;
        let rows = client
            .query_typed(
                "INSERT INTO auth_rate_limit_slots (scope_key, action, slot, expires_at) \
                 SELECT $1, $2, s, $3 FROM generate_series(0, $4) AS s \
                 LEFT JOIN auth_rate_limit_slots AS t \
                 ON t.scope_key = $1 AND t.action = $2 AND t.slot = s AND t.expires_at > $5 \
                 WHERE t.slot IS NULL ORDER BY ((s::BIGINT + $6) % ($4::BIGINT + 1)) LIMIT 1 \
                 ON CONFLICT (scope_key, action, slot) DO UPDATE \
                 SET expires_at = EXCLUDED.expires_at \
                 WHERE auth_rate_limit_slots.expires_at <= $5 \
                 RETURNING slot",
                &[
                    (&budget.scope_key, Type::TEXT),
                    (&budget.action, Type::TEXT),
                    (&expires_at, Type::INT8),
                    (&limit_hi, Type::INT4),
                    (&now_i, Type::INT8),
                    (&start_i, Type::INT8),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        if let Some(row) = rows.first() {
            return Ok(Some(row.try_get::<_, i32>(0).map_err(|_| StoreError)?));
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MirrorBinding {
    Unconfigured,
    Configured,
}

impl MirrorBinding {
    fn from_env(env: &Env) -> Self {
        // Only a genuinely absent binding enables no-enrichment mode. A
        // present invalid binding (or a lookup exception) still fails through
        // connect_mirror's opaque mirror_binding diagnostic, never degrades.
        match worker::js_sys::Reflect::get(
            env,
            &worker::wasm_bindgen::JsValue::from_str(MIRROR_HYPERDRIVE_BINDING),
        ) {
            Ok(value) if value.is_undefined() => Self::Unconfigured,
            _ => Self::Configured,
        }
    }
}

#[path = "subsonic_pg.rs"]
mod subsonic_pg;

struct PgStore {
    env: Option<Env>,
    trace_id: String,
    mirror_binding: MirrorBinding,
    #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
    test_client: std::cell::RefCell<Option<tokio_postgres::Client>>,
    #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
    test_mirror: Option<CatalogueTestMirror>,
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
struct CatalogueTestMirror {
    client: std::cell::RefCell<Option<Result<tokio_postgres::Client, StoreError>>>,
    attempts: std::rc::Rc<std::cell::Cell<usize>>,
}

/// Test-only seam for the Subsonic mount's snapshot read: the postgres
/// harness drives the production `PgStore::subsonic_member_catalogue`
/// through the injected connections (see
/// `catalogue_store_for_postgres_test`).
#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
pub trait SubsonicSnapshotCatalogueStore {
    async fn subsonic_snapshot_catalogue_with_cap(
        &self,
        user_id: &str,
        cap: i64,
    ) -> Result<
        (
            Vec<crate::catalogue::ArtistDbRow>,
            Vec<crate::catalogue::ReleaseDbRow>,
            Vec<crate::catalogue::TrackDbRow>,
        ),
        StoreError,
    >;

    async fn subsonic_snapshot_catalogue(
        &self,
        user_id: &str,
    ) -> Result<
        (
            Vec<crate::catalogue::ArtistDbRow>,
            Vec<crate::catalogue::ReleaseDbRow>,
            Vec<crate::catalogue::TrackDbRow>,
        ),
        StoreError,
    > {
        self.subsonic_snapshot_catalogue_with_cap(user_id, SUBSONIC_SNAPSHOT_CAP)
            .await
    }
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
impl SubsonicSnapshotCatalogueStore for PgStore {
    async fn subsonic_snapshot_catalogue_with_cap(
        &self,
        user_id: &str,
        cap: i64,
    ) -> Result<
        (
            Vec<crate::catalogue::ArtistDbRow>,
            Vec<crate::catalogue::ReleaseDbRow>,
            Vec<crate::catalogue::TrackDbRow>,
        ),
        StoreError,
    > {
        self.subsonic_member_catalogue_with_cap(user_id, cap).await
    }
}

/// Inject connection acquisition only; execute the production catalogue SQL,
/// decoders, early returns and error propagation against disposable PostgreSQL.
#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
pub fn catalogue_store_for_postgres_test(
    app: tokio_postgres::Client,
    mirror_configured: bool,
    mirror: Result<tokio_postgres::Client, StoreError>,
    attempts: std::rc::Rc<std::cell::Cell<usize>>,
) -> impl Store + SubsonicSnapshotCatalogueStore {
    PgStore {
        env: None,
        trace_id: "postgres-catalogue-test".into(),
        mirror_binding: if mirror_configured {
            MirrorBinding::Configured
        } else {
            MirrorBinding::Unconfigured
        },
        test_client: std::cell::RefCell::new(Some(app)),
        test_mirror: Some(CatalogueTestMirror {
            client: std::cell::RefCell::new(Some(mirror)),
            attempts,
        }),
    }
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
        mirror_binding: MirrorBinding::Configured,
        test_client: std::cell::RefCell::new(Some(client)),
        test_mirror: None,
    }
}

/// The log-safe shape of a dead driver task: the 5-char Postgres `SqlState`
/// code only, never the message/detail (which can carry host/role strings).
/// `tokio_postgres::Error` extracts the real code; test stubs return fixed
/// shapes so the leak-proof test can feed hostile strings through the same
/// path production uses.
trait DriverErrorCode {
    fn sqlstate(&self) -> Option<String>;
}

impl DriverErrorCode for tokio_postgres::Error {
    fn sqlstate(&self) -> Option<String> {
        PgStore::sqlstate_of(self.code())
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

    /// One account row for the Subsonic mount's identity: email drives
    /// `getLicense`, role drives the `getUser` admin/manager flags. A
    /// single query keeps one Hyperdrive connect per request.
    #[allow(dead_code)] // Used by the mount in the next slice.
    async fn subsonic_account(
        &self,
        user_id: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        "SELECT email, role FROM users WHERE id = $1",
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.subsonic_account")
                    })?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|row| {
                        Ok::<(String, String), StoreError>((
                            row.try_get("email").map_err(|_| StoreError)?,
                            row.try_get("role").map_err(|_| StoreError)?,
                        ))
                    })
                    .transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    /// Member catalogue for the Subsonic mount's per-request snapshot:
    /// one membership probe feeds all three mirror reads, bound by
    /// `SUBSONIC_SNAPSHOT_CAP` instead of the native page size.
    #[allow(dead_code)] // Used by the mount in the next slice.
    async fn subsonic_member_catalogue(
        &self,
        user_id: &str,
    ) -> Result<
        (
            Vec<crate::catalogue::ArtistDbRow>,
            Vec<crate::catalogue::ReleaseDbRow>,
            Vec<crate::catalogue::TrackDbRow>,
        ),
        StoreError,
    > {
        self.subsonic_member_catalogue_with_cap(user_id, SUBSONIC_SNAPSHOT_CAP)
            .await
    }

    async fn subsonic_member_catalogue_with_cap(
        &self,
        user_id: &str,
        cap: i64,
    ) -> Result<
        (
            Vec<crate::catalogue::ArtistDbRow>,
            Vec<crate::catalogue::ReleaseDbRow>,
            Vec<crate::catalogue::TrackDbRow>,
        ),
        StoreError,
    > {
        Self::bounded(
            async {
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let client = self.connect().await?;
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_member_releases_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.subsonic_member_catalogue")
                    })?;
                let member: Vec<crate::catalogue::MembershipReleaseRow> = member
                    .iter()
                    .map(catalogue_member_release_from_row)
                    .collect::<Result<_, _>>()?;
                let member_releases = crate::catalogue::member_release_mbids(&member);
                let pairs = client
                    .query_typed(
                        crate::catalogue::catalogue_member_track_pairs_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.subsonic_member_catalogue")
                    })?;
                let pairs: Vec<crate::catalogue::MembershipTrackPairRow> = pairs
                    .iter()
                    .map(catalogue_member_pair_from_row)
                    .collect::<Result<_, _>>()?;
                let member_pairs = crate::catalogue::member_pair_arrays(&pairs);
                if member_releases.is_empty() && member_pairs.is_empty() {
                    return Ok((Vec::new(), Vec::new(), Vec::new()));
                }
                self.require_mirror()?;
                let mirror = self.connect_mirror().await?;
                let statement_failed =
                    || self.statement_failed("api.db.statement_failed.subsonic_member_catalogue");
                let artist_rows = mirror
                    .query_typed(
                        crate::catalogue::artists_list_statement(),
                        &[(&member_releases, Type::TEXT_ARRAY), (&cap, Type::INT8)],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                require_complete_snapshot_page(artist_rows.len(), cap)?;
                let artist_rows: Vec<crate::catalogue::ArtistDbRow> = artist_rows
                    .iter()
                    .map(catalogue_artist_from_row)
                    .collect::<Result<_, _>>()?;
                let release_rows = mirror
                    .query_typed(
                        crate::catalogue::releases_list_statement(),
                        &[(&member_releases, Type::TEXT_ARRAY), (&cap, Type::INT8)],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                require_complete_snapshot_page(release_rows.len(), cap)?;
                let release_rows: Vec<crate::catalogue::ReleaseDbRow> = release_rows
                    .iter()
                    .map(catalogue_release_from_row)
                    .collect::<Result<_, _>>()?;
                let (pair_releases, pair_recordings) =
                    crate::catalogue::member_pair_columns(&member_pairs);
                let track_rows = mirror
                    .query_typed(
                        crate::catalogue::tracks_list_statement(),
                        &[
                            (&pair_releases, Type::TEXT_ARRAY),
                            (&pair_recordings, Type::TEXT_ARRAY),
                            (&cap, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                require_complete_snapshot_page(track_rows.len(), cap)?;
                let track_rows: Vec<crate::catalogue::TrackDbRow> = track_rows
                    .iter()
                    .map(catalogue_track_from_row)
                    .collect::<Result<_, _>>()?;
                Ok((
                    artist_rows,
                    release_rows,
                    crate::catalogue::retain_member_track_occurrences(track_rows, &member_pairs),
                ))
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn connect(&self) -> Result<tokio_postgres::Client, StoreError> {
        #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
        if let Some(client) = self.test_client.borrow_mut().take() {
            return Ok(client);
        }
        let env = self.env.as_ref().ok_or(StoreError)?;
        // The phase in progress when the connect fails (or the deadline
        // fires) names the `api.db.connect_failed.*` record below; the
        // caller still sees only the opaque `StoreError`.
        let reached = std::cell::Cell::new("api.db.connect_failed.binding");
        // One overall budget encloses dial, handshake, driver spawn, and
        // statement setup: the handshake and SET consume the time left
        // after dial rather than resetting the clock.
        let connected = with_deadline(
            async {
                let hyperdrive = env.hyperdrive(HYPERDRIVE_BINDING).map_err(|_| StoreError)?;
                reached.set("api.db.connect_failed.socket");
                let socket = worker::Socket::builder()
                    .secure_transport(worker::SecureTransport::StartTls)
                    .connect(hyperdrive.host(), hyperdrive.port())
                    .map_err(|_| StoreError)?;
                // The builder connect is synchronous; the actual dial
                // completes when the socket opens, bounded by the vault
                // dial budget shared with the test-connection probe.
                reached.set("api.db.connect_failed.dial");
                with_deadline(
                    async { socket.opened().await.map(|_| ()).map_err(|_| StoreError) },
                    DIAL_TIMEOUT_MS,
                    StoreError,
                )
                .await?;
                reached.set("api.db.connect_failed.handshake");
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
                reached.set("api.db.connect_failed.set_timeout");
                client
                    .batch_execute(&format!("SET statement_timeout = {}", TOTAL_TIMEOUT_MS))
                    .await
                    .map_err(|_| StoreError)?;
                Ok(client)
            },
            TOTAL_TIMEOUT_MS,
            StoreError,
        )
        .await;
        if connected.is_err() {
            let _ = Self::db_failure(
                reached.get(),
                &self.trace_id,
                &build_sha_from_env(env),
                None,
            );
        }
        connected
    }

    fn require_mirror(&self) -> Result<(), StoreError> {
        match self.mirror_binding {
            MirrorBinding::Configured => Ok(()),
            MirrorBinding::Unconfigured => Err(self.statement_failed("api.db.mirror_unconfigured")),
        }
    }

    /// Mirror-cluster connection for the MusicBrainz enrichment reads (ADR
    /// 0002 §1): same dial/handshake/timeout shape as [`Self::connect`],
    /// against the `MB_DB` binding instead of `HYPERDRIVE`. Failure phases
    /// are the `mirror_*` literals so an incident names the cluster that
    /// refused; empty app results never need a mirror connection.
    async fn connect_mirror(&self) -> Result<tokio_postgres::Client, StoreError> {
        #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
        if let Some(mirror) = &self.test_mirror {
            mirror.attempts.set(mirror.attempts.get() + 1);
            return mirror.client.borrow_mut().take().ok_or(StoreError)?;
        }
        let env = self.env.as_ref().ok_or(StoreError)?;
        // The phase in progress when the connect fails (or the deadline
        // fires) names the `api.db.connect_failed.mirror_*` record below;
        // the caller still sees only the opaque `StoreError`.
        let reached = std::cell::Cell::new("api.db.connect_failed.mirror_binding");
        let connected = with_deadline(
            async {
                let hyperdrive = env
                    .hyperdrive(MIRROR_HYPERDRIVE_BINDING)
                    .map_err(|_| StoreError)?;
                reached.set("api.db.connect_failed.mirror_socket");
                let socket = worker::Socket::builder()
                    .secure_transport(worker::SecureTransport::StartTls)
                    .connect(hyperdrive.host(), hyperdrive.port())
                    .map_err(|_| StoreError)?;
                reached.set("api.db.connect_failed.mirror_dial");
                with_deadline(
                    async { socket.opened().await.map(|_| ()).map_err(|_| StoreError) },
                    DIAL_TIMEOUT_MS,
                    StoreError,
                )
                .await?;
                reached.set("api.db.connect_failed.mirror_handshake");
                let config: tokio_postgres::Config = hyperdrive
                    .connection_string()
                    .parse()
                    .map_err(|_| StoreError)?;
                let (client, connection) = config
                    .connect_raw(socket, worker::postgres_tls::PassthroughTls)
                    .await
                    .map_err(|_| StoreError)?;
                let trace_id = self.trace_id.clone();
                let build_sha = build_sha_from_env(env);
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = Self::connection_diagnostic(connection, &trace_id, &build_sha).await;
                });
                reached.set("api.db.connect_failed.mirror_set_timeout");
                client
                    .batch_execute(&format!("SET statement_timeout = {}", TOTAL_TIMEOUT_MS))
                    .await
                    .map_err(|_| StoreError)?;
                Ok(client)
            },
            TOTAL_TIMEOUT_MS,
            StoreError,
        )
        .await;
        if connected.is_err() {
            let _ = Self::db_failure(
                reached.get(),
                &self.trace_id,
                &build_sha_from_env(env),
                None,
            );
        }
        connected
    }

    /// Read the recording-duration rung from a configured mirror. An
    /// absent binding or key needs no probe; configured failures propagate.
    async fn mb_duration_for(&self, mbid: Option<&str>) -> Result<Option<u64>, StoreError> {
        let Some(mbid) = mbid else {
            return Ok(None);
        };
        if self.mirror_binding == MirrorBinding::Unconfigured {
            return Ok(None);
        }
        let mirror = self.connect_mirror().await?;
        let rows = mirror
            .query_typed(
                crate::meter_sink::track_meter_meta_mirror_statement(),
                &[(&mbid, Type::TEXT)],
            )
            .await
            .map_err(|_| StoreError)?;
        if rows.len() > 1 {
            return Err(StoreError);
        }
        match rows.first() {
            None => Ok(None),
            Some(row) => {
                let duration: Option<i32> =
                    row.try_get("mb_duration_ms").map_err(|_| StoreError)?;
                Ok(duration.and_then(|v| u64::try_from(v).ok()))
            }
        }
    }

    /// Map one `billing_operations` row to its single-flight shape: the
    /// payload fields parse tolerantly (a corrupt payload degrades to no
    /// reuse, never to a blocked flight).
    fn checkout_op_from_row(
        row: &tokio_postgres::Row,
    ) -> Result<crate::billing_reconcile::StoredCheckoutOp, StoreError> {
        let payload: String = row.try_get("request_payload").map_err(|_| StoreError)?;
        let (price_id, session_id) =
            crate::billing_reconcile::parse_checkout_operation_payload(&payload);
        let subscription: Option<String> = row
            .try_get("stripe_subscription_id")
            .map_err(|_| StoreError)?;
        Ok(crate::billing_reconcile::StoredCheckoutOp {
            id: row.try_get("id").map_err(|_| StoreError)?,
            user_id: row.try_get("user_id").map_err(|_| StoreError)?,
            state: row.try_get("state").map_err(|_| StoreError)?,
            price_id,
            session_id,
            subscription_id: subscription,
        })
    }

    /// Opaque `StoreError` for a failed statement, after an
    /// `api.db.statement_failed.<operation>` record (`event` names it).
    fn statement_failed(&self, event: &str) -> StoreError {
        let build_sha = self
            .env
            .as_ref()
            .map(build_sha_from_env)
            .unwrap_or_else(|| crate::logging::BUILD_SHA_UNKNOWN.to_string());
        let _ = Self::db_failure(event, &self.trace_id, &build_sha, None);
        StoreError
    }

    /// Fixed-shape Postgres error class for a dead driver task: the 5-char
    /// `SqlState` code only, never the message/detail (which can carry host,
    /// role or other identifiers). Anything that is not exactly five ASCII
    /// alphanumerics maps to `None` so only the fixed shape can reach logs.
    fn sqlstate_of(code: Option<&tokio_postgres::error::SqlState>) -> Option<String> {
        let code = code?.code().to_string();
        (code.len() == 5 && code.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(code)
    }

    async fn connection_diagnostic<F, E>(
        connection: F,
        trace_id: &str,
        build_sha: &str,
    ) -> Option<String>
    where
        F: std::future::Future<Output = Result<(), E>>,
        E: DriverErrorCode,
    {
        match connection.await {
            Ok(()) => None,
            Err(err) => Self::db_failure(
                "api.db.connection_error",
                trace_id,
                build_sha,
                err.sqlstate(),
            ),
        }
    }

    /// The one database-failure record shape. No detail in logs: driver
    /// errors can carry host/role strings, so only the fixed event name
    /// says where it failed, plus the optional fixed-shape `sqlstate`
    /// 5-char code (never message/detail).
    fn db_failure(
        event: &str,
        trace_id: &str,
        build_sha: &str,
        sqlstate: Option<String>,
    ) -> Option<String> {
        let mut fields = serde_json::json!({"error_class": "service_unavailable", "build_sha": build_sha, "release": build_sha});
        if let Some(code) = sqlstate {
            fields["sqlstate"] = serde_json::Value::String(code);
        }
        emit_record(crate::logging::Level::Error, event, trace_id, fields)
    }
}

fn user_from_row(row: &tokio_postgres::Row) -> Result<UserRecord, StoreError> {
    Ok(UserRecord {
        id: row.try_get("id").map_err(|_| StoreError)?,
        email: row.try_get("email").map_err(|_| StoreError)?,
        created_at: u64_of(row.try_get("created_at").map_err(|_| StoreError)?)?,
    })
}

/// One `invite_codes` row (migration 0033). Expects the extended
/// column set; `issuer_email` is present only in the by-hash lookup's
/// join, so a missing column mappers to `None` there.
fn invite_from_row(row: &tokio_postgres::Row) -> Result<InviteRecord, StoreError> {
    let opt_u64 = |name: &str| -> Result<Option<u64>, StoreError> {
        row.try_get::<_, Option<i64>>(name)
            .map_err(|_| StoreError)?
            .map(u64_of)
            .transpose()
    };
    Ok(InviteRecord {
        id: row.try_get("id").map_err(|_| StoreError)?,
        code_hash: row.try_get("code_hash").map_err(|_| StoreError)?,
        used: row.try_get("used").map_err(|_| StoreError)?,
        created_at: u64_of(row.try_get("created_at").map_err(|_| StoreError)?)?,
        issued_by: row.try_get("issued_by").map_err(|_| StoreError)?,
        kind: row.try_get("kind").map_err(|_| StoreError)?,
        email: row.try_get("email").map_err(|_| StoreError)?,
        expires_at: u64_of(row.try_get("expires_at").map_err(|_| StoreError)?)?,
        redeemed_by: row.try_get("redeemed_by").map_err(|_| StoreError)?,
        redeemed_at: opt_u64("redeemed_at")?,
        revoked_at: opt_u64("revoked_at")?,
        revoked_by: row.try_get("revoked_by").map_err(|_| StoreError)?,
        window_id: row.try_get("window_id").map_err(|_| StoreError)?,
        issuer_email: row.try_get("issuer_email").ok().flatten(),
    })
}

const INVITE_COLUMNS: &str = "invite_codes.id, invite_codes.code_hash, invite_codes.used, \
    invite_codes.created_at, invite_codes.issued_by, invite_codes.kind, invite_codes.email, \
    invite_codes.expires_at, invite_codes.redeemed_by, invite_codes.redeemed_at, \
    invite_codes.revoked_at, invite_codes.revoked_by, invite_codes.window_id";

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

/// Map one app-side library page row (migration 0035, no mirror data) to
/// the catalogue page row. Selected ids arrive as `text`; the unnest
/// ordinal arrives as bigint, the job array as `TEXT[]`.
fn catalogue_library_page_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::LibraryPageRow, StoreError> {
    Ok(crate::catalogue::LibraryPageRow {
        job_id: row.try_get("job_id").map_err(|_| StoreError)?,
        release_mbid: row.try_get("release_mbid").map_err(|_| StoreError)?,
        recording_mbids: row.try_get("recording_mbids").map_err(|_| StoreError)?,
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
        recording_ord: row.try_get("recording_ord").map_err(|_| StoreError)?,
    })
}

/// Map one mirror recording hit (title, duration by recording MBID) to the
/// catalogue hit. `mbid::text` arrives lowercase; assembly normalizes both
/// sides through `enrichment_key` anyway.
fn catalogue_library_recording_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::LibraryRecordingHit, StoreError> {
    Ok(crate::catalogue::LibraryRecordingHit {
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
        title: row.try_get("recording_title").map_err(|_| StoreError)?,
        duration_ms: row.try_get("duration_ms").map_err(|_| StoreError)?,
    })
}

/// Map one mirror occurrence hit (medium/track position for a
/// (release, recording) pair) to the catalogue hit.
fn catalogue_library_track_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::LibraryTrackHit, StoreError> {
    Ok(crate::catalogue::LibraryTrackHit {
        release_mbid: row.try_get("release_mbid").map_err(|_| StoreError)?,
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
        medium_no: row.try_get("medium_no").map_err(|_| StoreError)?,
        track_no: row.try_get("track_no").map_err(|_| StoreError)?,
    })
}

/// Map one mirror release hit (title with artist) to the catalogue hit.
fn catalogue_library_release_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::LibraryReleaseHit, StoreError> {
    Ok(crate::catalogue::LibraryReleaseHit {
        release_mbid: row.try_get("release_mbid").map_err(|_| StoreError)?,
        title: row.try_get("release_title").map_err(|_| StoreError)?,
        artist_mbid: row.try_get("artist_mbid").map_err(|_| StoreError)?,
        artist_name: row.try_get("artist_name").map_err(|_| StoreError)?,
    })
}

/// Map an `mb_artist` row to the catalogue row (`country` is `CHAR(2)`,
/// cast to text in SQL like the mirror worker's lookup).
fn catalogue_artist_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::ArtistDbRow, StoreError> {
    Ok(crate::catalogue::ArtistDbRow {
        mbid: row.try_get("mbid").map_err(|_| StoreError)?,
        name: row.try_get("name").map_err(|_| StoreError)?,
        sort_name: row.try_get("sort_name").map_err(|_| StoreError)?,
        artist_type: row.try_get("type").map_err(|_| StoreError)?,
        country: row.try_get("country").map_err(|_| StoreError)?,
    })
}

/// Map an artist-joined `mb_release` row to the catalogue row.
fn catalogue_release_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::ReleaseDbRow, StoreError> {
    Ok(crate::catalogue::ReleaseDbRow {
        mbid: row.try_get("mbid").map_err(|_| StoreError)?,
        title: row.try_get("title").map_err(|_| StoreError)?,
        release_type: row.try_get("release_type").map_err(|_| StoreError)?,
        year: row.try_get("year").map_err(|_| StoreError)?,
        artist_mbid: row.try_get("artist_mbid").map_err(|_| StoreError)?,
        artist_name: row.try_get("artist_name").map_err(|_| StoreError)?,
    })
}

/// Map a fully joined `mb_track` occurrence row to the catalogue row.
fn catalogue_track_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::TrackDbRow, StoreError> {
    Ok(crate::catalogue::TrackDbRow {
        release_mbid: row.try_get("release_mbid").map_err(|_| StoreError)?,
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
        title: row.try_get("title").map_err(|_| StoreError)?,
        duration_ms: row.try_get("duration_ms").map_err(|_| StoreError)?,
        medium_no: row.try_get("medium_no").map_err(|_| StoreError)?,
        track_no: row.try_get("track_no").map_err(|_| StoreError)?,
        release_title: row.try_get("release_title").map_err(|_| StoreError)?,
        artist_mbid: row.try_get("artist_mbid").map_err(|_| StoreError)?,
        artist_name: row.try_get("artist_name").map_err(|_| StoreError)?,
    })
}

/// Map one app-side member release row to the catalogue row.
fn catalogue_member_release_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::MembershipReleaseRow, StoreError> {
    Ok(crate::catalogue::MembershipReleaseRow {
        release_mbid: row.try_get("release_mbid").map_err(|_| StoreError)?,
    })
}

/// Map one app-side member (release, recording) pair row to the catalogue
/// row.
fn catalogue_member_pair_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::MembershipTrackPairRow, StoreError> {
    Ok(crate::catalogue::MembershipTrackPairRow {
        release_mbid: row.try_get("release_mbid").map_err(|_| StoreError)?,
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
    })
}

/// Map one app-side member recording row (one release's recordings for the
/// release detail's member-only track read) to the catalogue row.
fn catalogue_member_recording_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::MembershipReleaseRecordingRow, StoreError> {
    Ok(crate::catalogue::MembershipReleaseRecordingRow {
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
    })
}

/// Map one release-detail track row (recording columns only; the release
/// context joins in [`crate::catalogue::assemble_release_detail`]).
fn catalogue_release_track_from_row(
    row: &tokio_postgres::Row,
) -> Result<crate::catalogue::ReleaseTrackDbRow, StoreError> {
    Ok(crate::catalogue::ReleaseTrackDbRow {
        recording_mbid: row.try_get("recording_mbid").map_err(|_| StoreError)?,
        title: row.try_get("title").map_err(|_| StoreError)?,
        duration_ms: row.try_get("duration_ms").map_err(|_| StoreError)?,
        medium_no: row.try_get("medium_no").map_err(|_| StoreError)?,
        track_no: row.try_get("track_no").map_err(|_| StoreError)?,
    })
}

/// Shared by standalone claims and registration's transaction: keep every
/// single-use predicate and its bind order identical across both paths.
async fn claim_single_use_on(
    client: &impl GenericClient,
    candidate: SingleUseClaim<'_>,
) -> Result<u64, StoreError> {
    let statement = candidate.statement();
    // Unnamed via `query_typed` (TWO-434): registration finish runs these
    // claims through Hyperdrive, where a named `sN` statement can fail.
    type Param<'a> = (&'a (dyn tokio_postgres::types::ToSql + Sync), Type);
    let rows = match &candidate {
        SingleUseClaim::MagicLink { record, now } => {
            let expires = i64_of(record.expires_at).map_err(|_| StoreError)?;
            let now = i64_of(*now).map_err(|_| StoreError)?;
            let params: [Param<'_>; 5] = [
                (&record.token_hash, Type::TEXT),
                (&record.user_id, Type::TEXT),
                (&record.email, Type::TEXT),
                (&expires, Type::INT8),
                (&now, Type::INT8),
            ];
            client.query_typed(statement, &params[..]).await
        }
        SingleUseClaim::Challenge { record, now } => {
            let expires = i64_of(record.expires_at).map_err(|_| StoreError)?;
            let now = i64_of(*now).map_err(|_| StoreError)?;
            let params: [Param<'_>; 5] = [
                (&record.id, Type::TEXT),
                (&record.rp_id, Type::TEXT),
                (&record.origin, Type::TEXT),
                (&expires, Type::INT8),
                (&now, Type::INT8),
            ];
            client.query_typed(statement, &params[..]).await
        }
        SingleUseClaim::Invite { record } => {
            // The spend stamps come from `prepare_invite_redemption`;
            // a record without them was never prepared and must not
            // claim (bind would otherwise spend as user "" at 0).
            let (Some(redeemed_by), Some(redeemed_at)) =
                (record.redeemed_by.as_deref(), record.redeemed_at)
            else {
                return Err(StoreError);
            };
            let redeemed_at = i64_of(redeemed_at).map_err(|_| StoreError)?;
            let params: [Param<'_>; 3] = [
                (&record.code_hash, Type::TEXT),
                (&redeemed_by, Type::TEXT),
                (&redeemed_at, Type::INT8),
            ];
            client.query_typed(statement, &params[..]).await
        }
    }
    .map_err(|_| StoreError)?;
    Ok(rows.len() as u64)
}

async fn insert_credential_on(
    client: &impl GenericClient,
    credential: &PasskeyCredential,
) -> Result<(), StoreError> {
    client.query_typed(
        "INSERT INTO webauthn_credentials (cred_id, user_id, public_key, sign_count, registered_at) \
         VALUES ($1, $2, $3, $4, $5)",
        &[
            (&credential.cred_id_b64url, Type::TEXT),
            (&credential.user_id, Type::TEXT),
            (&credential.public_key.as_slice(), Type::BYTEA),
            (&i64::from(credential.sign_count), Type::INT8),
            (&i64_of(credential.registered_at)?, Type::INT8),
        ],
    ).await.map_err(|_| StoreError)?;
    Ok(())
}

async fn complete_registration_on(
    client: &mut tokio_postgres::Client,
    challenge: &PasskeyChallenge,
    magic: &MagicLinkRecord,
    invite: Option<&InviteRecord>,
    invite_audit: Option<&AuditEvent>,
    credential: &PasskeyCredential,
    now: u64,
) -> Result<(), StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    // Reaping locks parents before cascading to proofs. Take the compatible
    // enrollment lock first, so a reaper skips us even after our proofs expire.
    let parent = transaction
        .query_typed(
            "SELECT id FROM users WHERE id = $1 FOR KEY SHARE",
            &[(&credential.user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    // `query_opt`'s rule: exactly one parent row, else the claim fails.
    if parent.len() != 1 {
        return Err(StoreError);
    }
    let mut claims = vec![
        SingleUseClaim::Challenge {
            record: challenge,
            now,
        },
        SingleUseClaim::MagicLink { record: magic, now },
    ];
    if let Some(record) = invite {
        claims.push(SingleUseClaim::Invite { record });
    }
    for claim in claims {
        if claim_single_use_on(&transaction, claim).await? != 1 {
            return Err(StoreError);
        }
    }
    if let Some(record) = invite {
        // Stamp the invite tree on the new account and write the
        // `invite.redeemed` audit in this same transaction: an audit
        // failure rolls the claims back, leaving the invite unspent
        // and the signup retryable. `invited_by` is read from the
        // CLAIMED invite row inside this transaction, never from the
        // pre-claim snapshot: if the issuer was deleted after
        // `invite_by_hash`, the row's `issued_by` is already NULL
        // (ON DELETE SET NULL), while the snapshot still names the
        // deleted user and the `users.invited_by` FK would reject it,
        // rolling back an otherwise valid enrollment forever.
        transaction
            .query_typed(
                "UPDATE users SET invited_by = (SELECT issued_by FROM invite_codes WHERE code_hash = $2), invite_code_hash = $2 WHERE id = $1",
                &[
                    (&credential.user_id, Type::TEXT),
                    (&record.code_hash, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        let audit = invite_audit.ok_or(StoreError)?;
        transaction
            .query_typed(
                "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)",
                &[
                    (&audit.user_id, Type::TEXT),
                    (&audit.action, Type::TEXT),
                    (&audit.ok, Type::BOOL),
                    (&audit.source_ip, Type::TEXT),
                    (&i64_of(audit.at)?, Type::INT8),
                    (&audit.subject_user_id, Type::TEXT),
                    (&audit.details, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
    }
    insert_credential_on(&transaction, credential).await?;
    // Any earlier error or deadline drops the transaction and rolls back
    // every claim before another request can observe a partial enrollment.
    transaction.commit().await.map_err(|_| StoreError)
}

async fn reap_unverified_users_on(
    client: &mut tokio_postgres::Client,
    now: u64,
    ttl_secs: u64,
    protected_user_id: Option<&str>,
) -> Result<(), StoreError> {
    let cutoff = i64_of(now.saturating_sub(ttl_secs))?;
    let now = i64_of(now)?;
    let transaction = client
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| StoreError)?;
    // Signup-path statements run unnamed via `query_typed` (TWO-434, same
    // Hyperdrive reason as the mint path): no `sN` prepared statement can
    // collide on, or go missing from, a pooled backend between round-trips.
    let candidates = transaction
        .query_typed(
            &handlers::lock_unverified_users_sql(),
            &[
                (&cutoff, Type::INT8),
                (&protected_user_id, Type::TEXT),
                (&now, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    let ids = candidates
        .iter()
        .map(|row| row.try_get::<_, String>("id").map_err(|_| StoreError))
        .collect::<Result<Vec<_>, _>>()?;
    #[cfg(all(test, not(target_arch = "wasm32")))]
    reaper_selection_pause().await;
    if !ids.is_empty() {
        // Enrollment may commit between the selection snapshot and its parent
        // lock. A second READ COMMITTED statement sees that credential before
        // deleting; holding the parent locks prevents a new enrollment racing it.
        transaction
            .query_typed(
                &handlers::reap_unverified_users_sql(),
                &[
                    (&cutoff, Type::INT8),
                    (&protected_user_id, Type::TEXT),
                    (&now, Type::INT8),
                    (&ids, Type::TEXT_ARRAY),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
    }
    transaction.commit().await.map_err(|_| StoreError)
}

async fn prune_expired_challenges_on(
    client: &impl GenericClient,
    now: u64,
) -> Result<(), StoreError> {
    // Enrollment may hold a challenge while waiting on a later claim. Do not
    // stall unrelated auth starts once it expires; a later prune retries it.
    client
        .query_typed(
            "DELETE FROM auth_challenges WHERE id IN (\
             SELECT id FROM auth_challenges WHERE expires_at <= $1 \
             FOR UPDATE SKIP LOCKED)",
            &[(&i64_of(now)?, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    Ok(())
}

/// Admin role write (migration 0028): serialize every admin write,
/// re-read the actor's role locked, lock the subject row, then the
/// UPDATE and its extended audit row commit together or not at all.
/// The subject lock is what makes the audited old value the value
/// actually replaced: a concurrent writer either committed before the
/// lock (and the locked read sees its value) or waits behind it.
///
/// The actor re-check closes the guard race: the handler's fresh role
/// read happened outside this transaction, so two admins could pass
/// the guard concurrently and demote each other to zero admins. The
/// shared `admin_writes` advisory lock runs admin writes one at a
/// time in a fixed lock order (serialization, then actor row, then
/// subject row — settings writes take their per-key lock last), so
/// opposing demotions cannot deadlock: the first commits, and the
/// second then re-reads its actor role as `member` and applies
/// nothing. The actor row lock additionally fences against any other
/// writer of the actor's role until commit. A missing subject writes
/// and audits nothing, and so does a subject scheduled for deletion:
/// ADR 0002 rule 3 fences every writer once `deleting_at` is set, and
/// a role granted here would stay effective (the admin guard reads
/// the role fresh per request) until erasure.
async fn admin_set_user_role_on(
    client: &mut tokio_postgres::Client,
    actor_user_id: &str,
    subject_user_id: &str,
    new_role: &str,
    source_ip: &str,
    now: u64,
) -> Result<AdminWriteOutcome, StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    let actor = transaction
        .query_typed(
            "SELECT role FROM users WHERE id = $1 FOR UPDATE",
            &[(&actor_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if actor.len() > 1 {
        return Err(StoreError);
    }
    let actor_role: Option<String> = actor
        .first()
        .map(|row| row.try_get::<_, String>("role").map_err(|_| StoreError))
        .transpose()?;
    if !crate::admin::is_admin_role(actor_role.as_deref()) {
        return Ok(AdminWriteOutcome::ActorNotAdmin);
    }
    let locked = transaction
        .query_typed(
            "SELECT role, deleting_at IS NOT NULL AS deleting \
             FROM users WHERE id = $1 FOR UPDATE",
            &[(&subject_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if locked.len() > 1 {
        return Err(StoreError);
    }
    let Some(previous) = locked
        .first()
        .map(|row| row.try_get::<_, String>("role").map_err(|_| StoreError))
        .transpose()?
    else {
        return Ok(AdminWriteOutcome::SubjectMissing);
    };
    // The fence is read under the subject row lock, so a deletion
    // scheduled concurrently either committed first (and is seen
    // here) or waits behind this transaction.
    if locked[0]
        .try_get::<_, bool>("deleting")
        .map_err(|_| StoreError)?
    {
        return Ok(AdminWriteOutcome::AccountDeleting);
    }
    transaction
        .query_typed(
            "UPDATE users SET role = $2 WHERE id = $1",
            &[(&subject_user_id, Type::TEXT), (&new_role, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, $2, TRUE, $3, $4, $5, jsonb_build_object('old', $6, 'new', $7))",
            &[
                (&actor_user_id, Type::TEXT),
                (&crate::admin::ROLE_CHANGED_AUDIT_ACTION, Type::TEXT),
                (
                    &crate::vault::sanitize_source_ip(Some(source_ip)),
                    Type::TEXT,
                ),
                (&i64_of(now)?, Type::INT8),
                (&subject_user_id, Type::TEXT),
                (&previous, Type::TEXT),
                (&new_role, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(AdminWriteOutcome::Applied(Some(previous)))
}

/// Admin settings write (migration 0028): one transaction, like the
/// role write, under the same fixed lock order — the shared
/// `admin_writes` serialization lock first (so a demotion committed
/// before this write holds the lock is visible to the actor re-check,
/// and any later demotion queues behind this commit), then the actor
/// row, then the per-key advisory lock. That per-key lock serializes
/// writers of the same key — including concurrent first writes of an
/// absent key, where there is no row for `FOR UPDATE` to serialize
/// on — so the subsequent locked read returns the value this write
/// actually replaces (including a just-committed one) for the audit
/// row and the response. An actor demoted before the re-check writes
/// and audits nothing.
async fn admin_set_app_setting_on(
    client: &mut tokio_postgres::Client,
    actor_user_id: &str,
    key: &str,
    new_value: &str,
    source_ip: &str,
    now: u64,
) -> Result<AdminWriteOutcome, StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    let actor = transaction
        .query_typed(
            "SELECT role FROM users WHERE id = $1 FOR UPDATE",
            &[(&actor_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if actor.len() > 1 {
        return Err(StoreError);
    }
    let actor_role: Option<String> = actor
        .first()
        .map(|row| row.try_get::<_, String>("role").map_err(|_| StoreError))
        .transpose()?;
    if !crate::admin::is_admin_role(actor_role.as_deref()) {
        return Ok(AdminWriteOutcome::ActorNotAdmin);
    }
    transaction
        .query_typed(
            "SELECT pg_advisory_xact_lock(hashtext('app_settings:' || $1))",
            &[(&key, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    let locked = transaction
        .query_typed(
            "SELECT value FROM app_settings WHERE key = $1 FOR UPDATE",
            &[(&key, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if locked.len() > 1 {
        return Err(StoreError);
    }
    let previous: Option<String> = locked
        .first()
        .map(|row| row.try_get::<_, String>("value").map_err(|_| StoreError))
        .transpose()?;
    transaction
        .query_typed(
            "INSERT INTO app_settings (key, value, updated_at, updated_by) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, \
               updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by",
            &[
                (&key, Type::TEXT),
                (&new_value, Type::TEXT),
                (&i64_of(now)?, Type::INT8),
                (&actor_user_id, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, $2, TRUE, $3, $4, NULL, jsonb_build_object('key', $5, 'old', $6, 'new', $7))",
            &[
                (&actor_user_id, Type::TEXT),
                (&crate::admin::SETTINGS_CHANGED_AUDIT_ACTION, Type::TEXT),
                (
                    &crate::vault::sanitize_source_ip(Some(source_ip)),
                    Type::TEXT,
                ),
                (&i64_of(now)?, Type::INT8),
                (&key, Type::TEXT),
                (&previous, Type::TEXT),
                (&new_value, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(AdminWriteOutcome::Applied(previous))
}

/// Read one stored admin job row (migration 0034) from a locked SELECT:
/// `None` for unknown ids and pipeline rows (the readers fence on
/// `source = 'admin'`, so the pipeline ledger never leaks through).
/// `file_count`/`segment_count` are INTEGER columns, so they decode as
/// `i32` — tokio-postgres rejects an INT4 column for `i64`, even on
/// NULL, which would fail every accepted-job read closed.
fn stored_admin_ingest_job_from(
    row: &tokio_postgres::Row,
    has_original: bool,
) -> Result<StoredAdminIngestJob, StoreError> {
    let file_count: Option<i64> = row
        .try_get::<_, Option<i32>>("file_count")
        .map_err(|_| StoreError)?
        .map(i64::from);
    let segment_count: Option<i64> = row
        .try_get::<_, Option<i32>>("segment_count")
        .map_err(|_| StoreError)?
        .map(i64::from);
    Ok(StoredAdminIngestJob {
        job_id: row.try_get("id").map_err(|_| StoreError)?,
        db_status: row.try_get("status").map_err(|_| StoreError)?,
        accepted_at: u64_of(row.try_get("accepted_at").map_err(|_| StoreError)?)?,
        nzb_bytes: u64_of(row.try_get("nzb_bytes").map_err(|_| StoreError)?)?,
        file_count: file_count.map(u64_of).transpose()?,
        segment_count: segment_count.map(u64_of).transpose()?,
        failure_reason: row.try_get("failure_reason").map_err(|_| StoreError)?,
        has_original,
        uploader_user_id: row.try_get("user_id").map_err(|_| StoreError)?,
    })
}

/// Admin ingest upload (migration 0034): one transaction, like the role
/// and settings writes, under the same fixed lock order — the shared
/// `admin_writes` serialization lock first (so a demotion committed
/// before this upload holds the lock is visible to the actor re-check,
/// and concurrent same-content uploads serialize instead of racing),
/// then the actor row. The content-derived id converges duplicates:
/// the locked re-select finds the pre-existing row, so the replay
/// writes nothing new except its own audit row; only the first attempt
/// inserts the job, the original and the accepted audit shape.
async fn admin_ingest_upload_on(
    client: &mut tokio_postgres::Client,
    attempt: &AdminIngestAttempt,
) -> Result<AdminIngestUploadOutcome, StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    // The actor row locks here so a deletion scheduled concurrently
    // either committed first (and is seen below) or waits behind this
    // transaction. ADR 0002 rule 3 fences every writer of user-owned
    // state: the upload's job/original rows carry the actor's id under
    // an ON DELETE RESTRICT FK, so a deleting actor is refused like any
    // other fenced writer (typed 409, nothing written or audited),
    // exactly like the role write fences its deleting subject.
    let actor = transaction
        .query_typed(
            "SELECT role, deleting_at IS NOT NULL AS deleting FROM users WHERE id = $1 FOR UPDATE",
            &[(&attempt.actor_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if actor.len() > 1 {
        return Err(StoreError);
    }
    let actor_role: Option<String> = actor
        .first()
        .map(|row| row.try_get::<_, String>("role").map_err(|_| StoreError))
        .transpose()?;
    if !crate::admin::is_admin_role(actor_role.as_deref()) {
        return Ok(AdminIngestUploadOutcome::ActorNotAdmin);
    }
    if actor
        .first()
        .map(|row| row.try_get::<_, bool>("deleting"))
        .transpose()
        .map_err(|_| StoreError)?
        .unwrap_or(false)
    {
        return Ok(AdminIngestUploadOutcome::AccountDeleting);
    }
    let existing = transaction
        .query_typed(
            "SELECT id, status, accepted_at, nzb_bytes, file_count, segment_count, \
             failure_reason, user_id FROM ingest_jobs WHERE id = $1 AND source = 'admin'",
            &[(&attempt.job_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if existing.len() > 1 {
        return Err(StoreError);
    }
    // The audit outcome for this attempt: fresh acceptances and
    // rejections name themselves, replays converge. A duplicate of a
    // rejected upload re-attaches the stored reason below.
    let outcome: &str = match (&attempt.failure, existing.first()) {
        (None, None) => "accepted",
        (Some(_), None) => "rejected",
        (_, Some(_)) => "duplicate",
    };
    if let Some(row) = existing.first() {
        let has_original = transaction
            .query_typed(
                "SELECT 1 FROM ingest_originals WHERE job_id = $1",
                &[(&attempt.job_id, Type::TEXT)],
            )
            .await
            .map_err(|_| StoreError)?;
        let job = stored_admin_ingest_job_from(row, !has_original.is_empty())?;
        let reason_code = job.failure_reason.as_deref();
        if let Some(reason) = reason_code {
            transaction
                .query_typed(
                    "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
                     VALUES ($1, $2, TRUE, $3, $4, NULL, jsonb_build_object('job_id', $5, 'outcome', $6, 'reason_code', $7))",
                    &[
                        (&attempt.actor_user_id, Type::TEXT),
                        (
                            &crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION,
                            Type::TEXT,
                        ),
                        (
                            &crate::vault::sanitize_source_ip(Some(&attempt.source_ip)),
                            Type::TEXT,
                        ),
                        (&i64_of(attempt.now)?, Type::INT8),
                        (&attempt.job_id, Type::TEXT),
                        (&outcome, Type::TEXT),
                        (&reason, Type::TEXT),
                    ],
                )
                .await
                .map_err(|_| StoreError)?;
        } else {
            transaction
                .query_typed(
                    "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
                     VALUES ($1, $2, TRUE, $3, $4, NULL, jsonb_build_object('job_id', $5, 'outcome', $6))",
                    &[
                        (&attempt.actor_user_id, Type::TEXT),
                        (
                            &crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION,
                            Type::TEXT,
                        ),
                        (
                            &crate::vault::sanitize_source_ip(Some(&attempt.source_ip)),
                            Type::TEXT,
                        ),
                        (&i64_of(attempt.now)?, Type::INT8),
                        (&attempt.job_id, Type::TEXT),
                        (&outcome, Type::TEXT),
                    ],
                )
                .await
                .map_err(|_| StoreError)?;
        }
        transaction.commit().await.map_err(|_| StoreError)?;
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
    // INTEGER columns bind `i32` (like every other INTEGER write in
    // this file, e.g. the KEK version below): an `i64` param against an
    // INTEGER column depends on assignment-cast tolerance the reads
    // above already refuse, so both sides speak the column width.
    let file_count = attempt
        .file_count
        .map(|v| i32::try_from(v).map_err(|_| StoreError))
        .transpose()?;
    let segment_count = attempt
        .segment_count
        .map(|v| i32::try_from(v).map_err(|_| StoreError))
        .transpose()?;
    let failure_reason = attempt.failure.map(|reason| reason.as_str().to_string());
    transaction
        .query_typed(
            "INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status, \
             source, content_sha256, nzb_bytes, file_count, segment_count, failure_reason) \
             VALUES ($1, $2, $3, $4, $5, 'admin', $6, $7, $8, $9, $10)",
            &[
                (&attempt.job_id, Type::TEXT),
                (&attempt.actor_user_id, Type::TEXT),
                (&attempt.idempotency_key, Type::TEXT),
                (&i64_of(attempt.now)?, Type::INT8),
                (&db_status, Type::TEXT),
                (&attempt.content_sha256_hex, Type::TEXT),
                (&i64_of(attempt.nzb_bytes)?, Type::INT8),
                (&file_count, Type::INT4),
                (&segment_count, Type::INT4),
                (&failure_reason, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    if let Some(envelope) = &attempt.original {
        transaction
            .query_typed(
                "INSERT INTO ingest_originals (job_id, content_sha256, nzb_bytes, \
                 nzb_ref_encrypted, nzb_ref_envelope_version, nzb_ref_kek_version, \
                 nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce, \
                 nzb_ref_payload_format, created_at) \
                 VALUES ($1, $2, $3, $4, 2, $5, $6, $7, $8, 1, $9)",
                &[
                    (&attempt.job_id, Type::TEXT),
                    (&attempt.content_sha256_hex, Type::TEXT),
                    (&i64_of(attempt.nzb_bytes)?, Type::INT8),
                    (&envelope.ciphertext.as_slice(), Type::BYTEA),
                    (
                        &i32::try_from(attempt.kek_version).map_err(|_| StoreError)?,
                        Type::INT4,
                    ),
                    (&envelope.dek_wrapped_nonce.as_slice(), Type::BYTEA),
                    (&envelope.dek_wrapped.as_slice(), Type::BYTEA),
                    (&envelope.payload_nonce.as_slice(), Type::BYTEA),
                    (&i64_of(attempt.now)?, Type::INT8),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
    }
    if let Some(reason) = failure_reason.as_deref() {
        transaction
            .query_typed(
                "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
                 VALUES ($1, $2, TRUE, $3, $4, NULL, jsonb_build_object('job_id', $5, 'outcome', $6, 'reason_code', $7))",
                &[
                    (&attempt.actor_user_id, Type::TEXT),
                    (
                        &crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION,
                        Type::TEXT,
                    ),
                    (
                        &crate::vault::sanitize_source_ip(Some(&attempt.source_ip)),
                        Type::TEXT,
                    ),
                    (&i64_of(attempt.now)?, Type::INT8),
                    (&attempt.job_id, Type::TEXT),
                    (&outcome, Type::TEXT),
                    (&reason, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
    } else {
        transaction
            .query_typed(
                "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
                 VALUES ($1, $2, TRUE, $3, $4, NULL, jsonb_build_object('job_id', $5, 'outcome', $6))",
                &[
                    (&attempt.actor_user_id, Type::TEXT),
                    (
                        &crate::admin_ingest::INGEST_UPLOAD_AUDIT_ACTION,
                        Type::TEXT,
                    ),
                    (
                        &crate::vault::sanitize_source_ip(Some(&attempt.source_ip)),
                        Type::TEXT,
                    ),
                    (&i64_of(attempt.now)?, Type::INT8),
                    (&attempt.job_id, Type::TEXT),
                    (&outcome, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
    }
    let stored = transaction
        .query_typed(
            "SELECT id, status, accepted_at, nzb_bytes, file_count, segment_count, \
             failure_reason, user_id FROM ingest_jobs WHERE id = $1 AND source = 'admin'",
            &[(&attempt.job_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if stored.len() != 1 {
        return Err(StoreError);
    }
    let job = stored_admin_ingest_job_from(&stored[0], attempt.original.is_some())?;
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(AdminIngestUploadOutcome::Stored {
        job,
        deduplicated: false,
    })
}

/// Store-level admin ingest delete (migration 0034, no route serves
/// it): the original goes first, then the job, under the actor
/// re-check — the `RESTRICT` order. A second delete of the same id
/// reports `Missing`; a re-upload afterwards lands a fresh row. No
/// audit row: deletion writes no new observable state.
async fn admin_ingest_delete_on(
    client: &mut tokio_postgres::Client,
    actor_user_id: &str,
    job_id: &str,
) -> Result<AdminIngestDeleteOutcome, StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    let actor = transaction
        .query_typed(
            "SELECT role FROM users WHERE id = $1 FOR UPDATE",
            &[(&actor_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if actor.len() > 1 {
        return Err(StoreError);
    }
    let actor_role: Option<String> = actor
        .first()
        .map(|row| row.try_get::<_, String>("role").map_err(|_| StoreError))
        .transpose()?;
    if !crate::admin::is_admin_role(actor_role.as_deref()) {
        return Ok(AdminIngestDeleteOutcome::ActorNotAdmin);
    }
    transaction
        .query_typed(
            "DELETE FROM ingest_originals WHERE job_id IN \
             (SELECT id FROM ingest_jobs WHERE id = $1 AND source = 'admin')",
            &[(&job_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    // `RETURNING id`: a DELETE without RETURNING yields no result
    // rows, so the emptiness check below would always report Missing
    // even when the transaction just removed the job.
    let deleted = transaction
        .query_typed(
            "DELETE FROM ingest_jobs WHERE id = $1 AND source = 'admin' RETURNING id",
            &[(&job_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if deleted.len() > 1 {
        return Err(StoreError);
    }
    transaction.commit().await.map_err(|_| StoreError)?;
    if deleted.is_empty() {
        return Ok(AdminIngestDeleteOutcome::Missing);
    }
    Ok(AdminIngestDeleteOutcome::Deleted)
}
/// Re-read the actor's role under the admin-write serialization lock;
/// shared first step of every admin write transaction. The actor row
/// lock fences against any other writer of the actor's role until
/// commit (see `admin_set_user_role_on` for the full lock order).
async fn admin_actor_locked(
    transaction: &tokio_postgres::Transaction<'_>,
    actor_user_id: &str,
) -> Result<bool, StoreError> {
    transaction
        .query_typed(
            "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    let actor = transaction
        .query_typed(
            "SELECT role FROM users WHERE id = $1 FOR UPDATE",
            &[(&actor_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if actor.len() > 1 {
        return Err(StoreError);
    }
    let actor_role: Option<String> = actor
        .first()
        .map(|row| row.try_get::<_, String>("role").map_err(|_| StoreError))
        .transpose()?;
    Ok(crate::admin::is_admin_role(actor_role.as_deref()))
}

/// Admin invite issue (migration 0033): the INSERT and its
/// `invite.issued` audit row commit together under the shared
/// admin-write locks. The audit row references the invite by the
/// returned row id only — code plaintext and hash never appear in it.
async fn admin_issue_invite_on(
    client: &mut tokio_postgres::Client,
    actor_user_id: &str,
    record: &InviteRecord,
    source_ip: &str,
    now: u64,
) -> Result<AdminInviteIssueOutcome, StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    if !admin_actor_locked(&transaction, actor_user_id).await? {
        return Ok(AdminInviteIssueOutcome::ActorNotAdmin);
    }
    let rows = transaction
        .query_typed(
            "INSERT INTO invite_codes (code_hash, used, created_at, issued_by, kind, email, expires_at) \
             VALUES ($1, FALSE, $2, $3, $4, $5, $6) RETURNING id",
            &[
                (&record.code_hash, Type::TEXT),
                (&i64_of(record.created_at)?, Type::INT8),
                (&actor_user_id, Type::TEXT),
                (&record.kind, Type::TEXT),
                (&record.email, Type::TEXT),
                (&i64_of(record.expires_at)?, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() != 1 {
        return Err(StoreError);
    }
    let invite_id: i64 = rows[0].try_get("id").map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, $2, TRUE, $3, $4, NULL, \
               jsonb_build_object('invite_id', $5, 'kind', $6, 'email_bound', $7, 'expires_at', $8))",
            &[
                (&actor_user_id, Type::TEXT),
                (&crate::auth::INVITE_ISSUED_AUDIT_ACTION, Type::TEXT),
                (
                    &crate::vault::sanitize_source_ip(Some(source_ip)),
                    Type::TEXT,
                ),
                (&i64_of(now)?, Type::INT8),
                (&invite_id, Type::INT8),
                (&record.kind, Type::TEXT),
                (&record.email.is_some(), Type::BOOL),
                (&i64_of(record.expires_at)?, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(AdminInviteIssueOutcome::Issued(invite_id))
}

/// Admin invite revoke (migration 0033): the conditional UPDATE and
/// its `invite.revoked` audit row commit together. The invite row is
/// locked first, so a redemption claim executing concurrently either
/// committed first (and the locked read sees the spend) or waits and
/// then loses to the committed `revoked_at` — revocation wins the
/// race whenever it commits before the claim's UPDATE.
async fn admin_revoke_invite_on(
    client: &mut tokio_postgres::Client,
    actor_user_id: &str,
    invite_id: i64,
    source_ip: &str,
    now: u64,
) -> Result<AdminInviteRevokeOutcome, StoreError> {
    let transaction = client.transaction().await.map_err(|_| StoreError)?;
    if !admin_actor_locked(&transaction, actor_user_id).await? {
        return Ok(AdminInviteRevokeOutcome::ActorNotAdmin);
    }
    let locked = transaction
        .query_typed(
            "SELECT used, redeemed_at, revoked_at FROM invite_codes WHERE id = $1 FOR UPDATE",
            &[(&invite_id, Type::INT8)],
        )
        .await
        .map_err(|_| StoreError)?;
    if locked.len() > 1 {
        return Err(StoreError);
    }
    let Some(row) = locked.first() else {
        return Ok(AdminInviteRevokeOutcome::Missing);
    };
    let used: bool = row.try_get("used").map_err(|_| StoreError)?;
    let redeemed_at: Option<i64> = row.try_get("redeemed_at").map_err(|_| StoreError)?;
    let revoked_at: Option<i64> = row.try_get("revoked_at").map_err(|_| StoreError)?;
    if used || redeemed_at.is_some() {
        return Ok(AdminInviteRevokeOutcome::AlreadyRedeemed);
    }
    if revoked_at.is_some() {
        return Ok(AdminInviteRevokeOutcome::AlreadyRevoked);
    }
    transaction
        .query_typed(
            "UPDATE invite_codes SET revoked_at = $2, revoked_by = $3 WHERE id = $1",
            &[
                (&invite_id, Type::INT8),
                (&i64_of(now)?, Type::INT8),
                (&actor_user_id, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction
        .query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, $2, TRUE, $3, $4, NULL, jsonb_build_object('invite_id', $5))",
            &[
                (&actor_user_id, Type::TEXT),
                (&crate::auth::INVITE_REVOKED_AUDIT_ACTION, Type::TEXT),
                (
                    &crate::vault::sanitize_source_ip(Some(source_ip)),
                    Type::TEXT,
                ),
                (&i64_of(now)?, Type::INT8),
                (&invite_id, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    transaction.commit().await.map_err(|_| StoreError)?;
    Ok(AdminInviteRevokeOutcome::Revoked)
}

impl Store for PgStore {
    async fn user_by_email(&self, email: &str) -> Result<Option<UserRecord>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // The mint-path statements (here, insert_user,
                // insert_session) run unnamed via `query_typed`: one
                // Parse/Bind/Execute/Sync round-trip, so no `sN` prepared
                // statement name can collide on, or go missing from, a
                // Hyperdrive-pooled backend between round-trips.
                let rows = client
                    .query_typed(
                        "SELECT id, email, created_at FROM users WHERE email = $1",
                        &[(&email, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| self.statement_failed("api.db.statement_failed.user_by_email"))?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first().map(user_from_row).transpose()
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
                    .query_typed(
                        "INSERT INTO users (id, email, created_at) VALUES ($1, $2, $3)",
                        &[
                            (&user.id, Type::TEXT),
                            (&user.email, Type::TEXT),
                            (&i64_of(user.created_at)?, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| self.statement_failed("api.db.statement_failed.insert_user"))?;
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
            .query_typed(
                "INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, consumed, purpose) \
                 VALUES ($1, $2, $3, $4, $5, FALSE, $6)",
                &[
                    (&challenge.id, Type::TEXT),
                    (&user_id, Type::TEXT),
                    (&challenge.rp_id, Type::TEXT),
                    (&challenge.origin, Type::TEXT),
                    (&i64_of(challenge.expires_at)?, Type::INT8),
                    (&purpose, Type::TEXT),
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
                let rows = client
                    .query_typed(
                        "SELECT id, user_id, rp_id, origin, expires_at, consumed, purpose \
                 FROM auth_challenges WHERE id = $1",
                        &[(&id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: more than one row is an error.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|r| {
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
        Self::bounded(
            async {
                let client = self.connect().await?;
                insert_credential_on(&client, credential).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
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
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                complete_registration_on(
                    &mut client,
                    challenge,
                    magic,
                    invite,
                    invite_audit,
                    credential,
                    now,
                )
                .await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
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
                    .query_typed(&sql, &[(&user_id, Type::TEXT)])
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

    // Per-user prune avoids a global scan; enrollment-held tokens retry on a later issuance.
    async fn insert_magic(&self, record: &MagicLinkRecord) -> Result<(), StoreError> {
        Self::bounded(async {
        let client = self.connect().await?;
        client
            .query_typed(
                "WITH pruned AS (DELETE FROM magic_link_tokens WHERE user_id = $2 AND expires_at <= $4 \
                 AND token_hash IN (SELECT token_hash FROM magic_link_tokens \
                 WHERE user_id = $2 AND expires_at <= $4 FOR UPDATE SKIP LOCKED)) \
                 INSERT INTO magic_link_tokens (token_hash, user_id, email, issued_at, expires_at, used) \
                 VALUES ($1, $2, $3, $4, $5, FALSE)",
                &[
                    (&record.token_hash, Type::TEXT),
                    (&record.user_id, Type::TEXT),
                    (&record.email, Type::TEXT),
                    (&i64_of(record.issued_at)?, Type::INT8),
                    (&i64_of(record.expires_at)?, Type::INT8),
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
                let rows = client
                    .query_typed(
                        "SELECT token_hash, user_id, email, issued_at, expires_at, used \
                 FROM magic_link_tokens WHERE token_hash = $1",
                        &[(&token_hash, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: more than one row is an error.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|r| {
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
            .query_typed(
                "INSERT INTO qa_issued_codes (email, token, issued_at) VALUES ($1, $2, $3) \
                 ON CONFLICT (email) DO UPDATE SET token = EXCLUDED.token, issued_at = EXCLUDED.issued_at",
                &[
                    (&email, Type::TEXT),
                    (&token, Type::TEXT),
                    (&i64_of(now)?, Type::INT8),
                ],
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
                let rows = client
                    .query_typed(
                        "SELECT token, issued_at FROM qa_issued_codes WHERE email = $1",
                        &[(&email, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: more than one row is an error.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|r| {
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

    async fn ingest_jobs_today(&self, user_id: &str, now: u64) -> Result<u64, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Daily count derived from accepted `ingest_jobs` rows
                // (the daily fold table was dropped in migration 0026).
                // `accepted_at` is BIGINT epoch seconds (the 0022 converged
                // shape), so the UTC day is epoch/86400 — exactly the
                // fake's epoch-day key; no local-time skew is possible.
                // Pipeline rows only: admin-upload rows (migration 0034)
                // carry the uploading admin's id for the FK/fence, and
                // counting them here would burn the ordinary daily
                // intake allowance on admin activity.
                let rows = client
                    .query_typed(
                        "SELECT COUNT(*)::BIGINT AS ingest_jobs FROM ingest_jobs WHERE user_id = $1 \
                         AND accepted_at / 86400 = $2 / 86400 AND source = 'pipeline'",
                        &[(&user_id, Type::TEXT), (&i64_of(now)?, Type::INT8)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: more than one row is an error.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                match rows.first() {
                    None => Ok(0),
                    Some(row) => u64_of(row.try_get("ingest_jobs").map_err(|_| StoreError)?),
                }
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn seed_qa_ingest_usage(
        &self,
        user_id: &str,
        now: u64,
        ingest_jobs: u64,
    ) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Seed as synthetic accepted `ingest_jobs` rows for the day (the
                // count above derives from them): deterministic QA idempotency
                // keys make a reseed a no-op that never lowers the count.
                client
                    .query_typed(
                        "INSERT INTO ingest_jobs (user_id, idempotency_key, accepted_at, status) \
                 SELECT $1, 'qa-seed:' || g, $2, 'done' FROM generate_series(1, $3) AS g \
                 ON CONFLICT (user_id, idempotency_key) DO NOTHING",
                        &[
                            (&user_id, Type::TEXT),
                            (&i64_of(now)?, Type::INT8),
                            (&i64_of(ingest_jobs)?, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn plans(&self) -> Result<Vec<PlanRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(crate::billing::plans_select_statement(), &[])
                    .await
                    .map_err(|_| StoreError)?;
                rows.iter()
                    .map(|r| {
                        let price_cents: i32 = r.try_get("price_cents").map_err(|_| StoreError)?;
                        let stream_hours: i32 =
                            r.try_get("stream_hours_month").map_err(|_| StoreError)?;
                        let concurrent: i32 = r
                            .try_get("max_concurrent_streams")
                            .map_err(|_| StoreError)?;
                        Ok(PlanRow {
                            id: r.try_get("id").map_err(|_| StoreError)?,
                            price_cents: u64_of(i64::from(price_cents))?,
                            stream_hours_month: u64_of(i64::from(stream_hours))?,
                            max_concurrent_streams: u64_of(i64::from(concurrent))?,
                        })
                    })
                    .collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn subscription_snapshot(
        &self,
        user_id: &str,
    ) -> Result<(Option<SubscriptionRow>, bool), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::billing::subscription_live_select_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: the live-slot partial unique allows
                // at most one row; more is an error, never a guess.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                let live = rows
                    .first()
                    .map(|r| {
                        Ok::<SubscriptionRow, StoreError>(SubscriptionRow {
                            plan_id: r.try_get("plan_id").map_err(|_| StoreError)?,
                            status: r.try_get("status").map_err(|_| StoreError)?,
                            cancel_at_period_end: r
                                .try_get("cancel_at_period_end")
                                .map_err(|_| StoreError)?,
                            paid_through_at_unix: r
                                .try_get("paid_through_at_unix")
                                .map_err(|_| StoreError)?,
                            grace_until_unix: r
                                .try_get("grace_until_unix")
                                .map_err(|_| StoreError)?,
                            current_period_end_unix: r
                                .try_get("current_period_end_unix")
                                .map_err(|_| StoreError)?,
                        })
                    })
                    .transpose()?;
                // The ever-existed probe only matters when the live slot
                // is empty (a live row already proves history), so skip
                // the second query for subscribed users.
                let ever = if live.is_some() {
                    true
                } else {
                    let any = client
                        .query_typed(
                            crate::billing::subscription_any_select_statement(),
                            &[(&user_id, Type::TEXT)],
                        )
                        .await
                        .map_err(|_| StoreError)?;
                    !any.is_empty()
                };
                Ok((live, ever))
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn live_subscription(
        &self,
        user_id: &str,
    ) -> Result<Option<crate::playback::SubscriptionRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::billing::subscription_live_select_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // The live-slot partial unique guarantees at most one
                // row; more is a schema violation, never "pick one".
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                match rows.first() {
                    None => Ok(None),
                    Some(row) => Ok(Some(crate::playback::SubscriptionRow {
                        plan_id: row.try_get("plan_id").map_err(|_| StoreError)?,
                        status: row.try_get("status").map_err(|_| StoreError)?,
                        cancel_at_period_end: row
                            .try_get("cancel_at_period_end")
                            .map_err(|_| StoreError)?,
                        paid_through_at_unix: row
                            .try_get("paid_through_at_unix")
                            .map_err(|_| StoreError)?,
                        grace_until_unix: row
                            .try_get("grace_until_unix")
                            .map_err(|_| StoreError)?,
                        current_period_end_unix: row
                            .try_get("current_period_end_unix")
                            .map_err(|_| StoreError)?,
                    })),
                }
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn ever_subscribed(&self, user_id: &str) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::billing::subscription_ever_paid_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() != 1 {
                    return Err(StoreError);
                }
                rows[0].try_get("ever_paid").map_err(|_| StoreError)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn plan_caps(
        &self,
        plan_id: &str,
    ) -> Result<Option<crate::plan_limits::PlanCaps>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::plan_limits::plan_caps_select_statement(),
                        &[(&plan_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                match rows.first() {
                    None => Ok(None),
                    Some(row) => {
                        let hours = u64::try_from(
                            row.try_get::<_, i32>("stream_hours_month")
                                .map_err(|_| StoreError)?,
                        )
                        .map_err(|_| StoreError)?;
                        let concurrent = u32::try_from(
                            row.try_get::<_, i32>("max_concurrent_streams")
                                .map_err(|_| StoreError)?,
                        )
                        .map_err(|_| StoreError)?;
                        Ok(Some(crate::plan_limits::caps_from_plan_row(
                            hours, concurrent,
                        )))
                    }
                }
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn stream_seconds_month(
        &self,
        user_id: &str,
        period_month: &str,
    ) -> Result<u64, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::ledger::ledger_monthly_total_statement(),
                        &[(&user_id, Type::TEXT), (&period_month, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // The aggregate always yields exactly one row (COALESCE 0).
                if rows.len() != 1 {
                    return Err(StoreError);
                }
                u64_of(rows[0].try_get("stream_seconds").map_err(|_| StoreError)?)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn monthly_stream_seconds(
        &self,
        user_id: &str,
        period_month: &str,
    ) -> Result<u64, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::ledger::ledger_monthly_total_statement(),
                        &[(&user_id, Type::TEXT), (&period_month, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // The aggregate always yields exactly one row (COALESCE 0).
                if rows.len() != 1 {
                    return Err(StoreError);
                }
                u64_of(rows[0].try_get("stream_seconds").map_err(|_| StoreError)?)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn playback_lease(
        &self,
        user_id: &str,
    ) -> Result<Option<crate::playback::LeaseRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let row = query_playback_lease_row(&client, user_id).await?;
                Ok(row)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
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
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::playback::playback_lease_acquire_statement(),
                        &[
                            (&user_id, Type::TEXT),
                            (&session_id, Type::TEXT),
                            (&track_id, Type::TEXT),
                            (&play_id, Type::TEXT),
                            (&i64_of(expires_at)?, Type::INT8),
                            (&i64_of(now)?, Type::INT8),
                            (&takeover, Type::BOOL),
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                match rows.first() {
                    Some(row) => Ok(crate::playback::LeaseAcquire::Acquired {
                        epoch: u64_of(row.try_get("epoch").map_err(|_| StoreError)?)?,
                    }),
                    // Zero rows: another session holds a live lease.
                    // Read it (same connection, same budget) so the
                    // 429 carries its real remaining TTL. A vanished
                    // row (deleted mid-race) reads as retry-now.
                    None => {
                        let retry_after_secs =
                            match query_playback_lease_row(&client, user_id).await? {
                                Some(row) => row.retry_after_secs(now),
                                None => 0,
                            };
                        Ok(crate::playback::LeaseAcquire::Held { retry_after_secs })
                    }
                }
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn touch_playback_lease(
        &self,
        user_id: &str,
        session_id: &str,
        play_id: &str,
        epoch: u64,
        now: u64,
        expires_at: u64,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::playback::playback_lease_touch_statement(),
                        &[
                            (&user_id, Type::TEXT),
                            (&session_id, Type::TEXT),
                            (&play_id, Type::TEXT),
                            (&i64_of(epoch)?, Type::INT8),
                            (&i64_of(expires_at)?, Type::INT8),
                            (&i64_of(now)?, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                Ok(rows.len() == 1)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn track_meter_meta(
        &self,
        user_id: &str,
        track_id: &str,
    ) -> Result<crate::meter_sink::TrackMeterMeta, StoreError> {
        // A non-UUID track has no `tracks` row to read (and the cast
        // would fail the query besides): meter it on the fallback
        // rung via the all-None meta, without a database round trip.
        if !crate::meter_sink::is_uuid_shaped(track_id) {
            return Ok(crate::meter_sink::TrackMeterMeta::default());
        }
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::meter_sink::track_meter_meta_app_statement(),
                        &[(&track_id, Type::TEXT), (&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                let Some(row) = rows.first() else {
                    return Ok(crate::meter_sink::TrackMeterMeta {
                        mb_duration_ms: self.mb_duration_for(Some(track_id)).await?,
                        ..Default::default()
                    });
                };
                let u64_of_i32 = |v: Option<i32>| v.and_then(|v| u64::try_from(v).ok());
                let u64_of_i64 = |v: Option<i64>| v.and_then(|v| u64::try_from(v).ok());
                let track_mbid: Option<String> =
                    row.try_get("track_mbid").map_err(|_| StoreError)?;
                let ambiguous: bool = row
                    .try_get("occurrence_ambiguous")
                    .map_err(|_| StoreError)?;
                // A recording-only match cannot identify one of several artifacts.
                let (track_duration_ms, bitrate_kbps, file_bytes) = if ambiguous {
                    (None, None, None)
                } else {
                    (
                        u64_of_i32(row.try_get("track_duration_ms").map_err(|_| StoreError)?),
                        u64_of_i32(row.try_get("bitrate_kbps").map_err(|_| StoreError)?),
                        u64_of_i64(row.try_get("file_bytes").map_err(|_| StoreError)?),
                    )
                };
                Ok(crate::meter_sink::TrackMeterMeta {
                    track_duration_ms,
                    mb_duration_ms: self.mb_duration_for(track_mbid.as_deref()).await?,
                    bitrate_kbps,
                    file_bytes,
                })
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn record_play_window(
        &self,
        window: &crate::ledger::PlayWindow,
    ) -> Result<crate::meter_sink::PlayWindowOutcome, StoreError> {
        use crate::meter_sink::PlayWindowOutcome;
        Self::bounded(
            async {
                let client = self.connect().await?;
                let file_bytes = window.file_bytes.map(i64_of).transpose()?;
                let duration_ms = window.duration_ms.map(i64_of).transpose()?;
                let affected = client
                    .execute(
                        crate::ledger::ledger_play_window_upsert_statement(),
                        &[
                            &window.idempotency_key,
                            &window.user_id,
                            &window.track_id,
                            &window.period_month,
                            &i64_of(window.stream_seconds)?,
                            &i64_of(window.bytes_served)?,
                            &i64_of(window.recorded_at)?,
                            &file_bytes,
                            &duration_ms,
                        ],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if affected == 1 {
                    return Ok(PlayWindowOutcome::Written);
                }
                if affected > 1 {
                    return Err(StoreError);
                }
                // Zero rows is ambiguous by design (statement docs):
                // a stored row already covering this union is a
                // convergent duplicate; anything else means the writer
                // fence refused the write (deleting/nonexistent user)
                // — terminal for the sink, never retried behind a
                // deletion. Disambiguate with the select-by-key probe
                // the statement contract prescribes.
                let rows = client
                    .query_typed(
                        crate::ledger::ledger_select_by_key_statement(),
                        &[(&window.idempotency_key, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                match rows.first() {
                    Some(row) => {
                        let stored_bytes: i64 =
                            row.try_get("bytes_served").map_err(|_| StoreError)?;
                        if u64_of(stored_bytes)? >= window.bytes_served {
                            Ok(PlayWindowOutcome::Covered)
                        } else {
                            Ok(PlayWindowOutcome::Fenced)
                        }
                    }
                    None => Ok(PlayWindowOutcome::Fenced),
                }
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn invite_by_hash(&self, code_hash: &str) -> Result<Option<InviteRecord>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // The issuer's current email rides along so the pure
                // prepare can refuse self-invites before any claim.
                let statement = format!(
                    "SELECT {INVITE_COLUMNS}, issuer.email AS issuer_email \
                     FROM invite_codes \
                     LEFT JOIN users issuer ON issuer.id = invite_codes.issued_by \
                     WHERE invite_codes.code_hash = $1"
                );
                let rows = client
                    .query_typed(&statement, &[(&code_hash, Type::TEXT)])
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: more than one row is an error.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first().map(invite_from_row).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_session(&self, record: &SessionRecord) -> Result<(), StoreError> {
        Self::bounded(async {
        let client = self.connect().await?;
        client
            .query_typed(
                "INSERT INTO sessions (id, user_id, created_at, expires_at, rotation_counter, revoked) \
                 VALUES ($1, $2, $3, $4, $5, FALSE)",
                &[
                    (&record.id, Type::TEXT),
                    (&record.user_id, Type::TEXT),
                    (&i64_of(record.created_at)?, Type::INT8),
                    (&i64_of(record.expires_at)?, Type::INT8),
                    (
                        &i32::try_from(record.rotation_counter).map_err(|_| StoreError)?,
                        Type::INT4,
                    ),
                ],
            )
            .await
            .map_err(|_| self.statement_failed("api.db.statement_failed.insert_session"))?;
        Ok(())
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn redeem_magic_link(
        &self,
        magic: &MagicLinkRecord,
        // Admission time is intentionally unused here: the claim statement
        // re-evaluates expiry against the database clock when it executes
        // (see `crate::magic_redemption`), so a slow admission/lookup path
        // cannot redeem a link after its stated expiry.
        _now: u64,
        session: &SessionRecord,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                let transaction = client.transaction().await.map_err(|_| StoreError)?;
                crate::magic_redemption::redeem(transaction, magic, session, None).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn redeem_magic_link_and_audit(
        &self,
        magic: &MagicLinkRecord,
        _now: u64,
        session: &SessionRecord,
        audit: &AuditEvent,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                let transaction = client.transaction().await.map_err(|_| StoreError)?;
                crate::magic_redemption::redeem(transaction, magic, session, Some(audit)).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn session_by_id(&self, id: &str) -> Result<Option<SessionRecord>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539): every authenticated
                // request runs this lookup through Hyperdrive, where a
                // named `sN` statement can fail on a pooled backend.
                let rows = client
                    .query_typed(
                        "SELECT id, user_id, created_at, expires_at, rotation_counter, revoked \
                 FROM sessions WHERE id = $1",
                        &[(&id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                // `query_opt`'s rule: more than one row is an error.
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|r| {
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

    async fn revoke_sessions_and_native_credentials(
        &self,
        user_id: &str,
        now: u64,
    ) -> Result<u64, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                crate::credential_session_store::revoke_sessions_and_native_credentials(
                    &mut client,
                    user_id,
                    now,
                )
                .await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_subsonic_credential(
        &self,
        row: &crate::subsonic_lifecycle::PreparedCredential,
        session: &crate::session::SessionClaims,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async { subsonic_pg::insert(&mut self.connect().await?, row, session).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }
    async fn subsonic_credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::subsonic_lifecycle::ListedCredential>, StoreError> {
        Self::bounded(
            async { subsonic_pg::list(&mut self.connect().await?, user_id).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }
    async fn revoke_subsonic_credential(
        &self,
        id: &str,
        user_id: &str,
        now: u64,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                subsonic_pg::revoke(&mut self.connect().await?, Some(id), user_id, now)
                    .await
                    .map(|count| count == 1)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }
    async fn revoke_all_subsonic_credentials(
        &self,
        user_id: &str,
        now: u64,
    ) -> Result<u64, StoreError> {
        Self::bounded(
            async { subsonic_pg::revoke(&mut self.connect().await?, None, user_id, now).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    // --- Native REST credentials (ADR 0010, migration 0019) -------------------------------
    //
    // Every statement is parameterized; the secret itself never reaches the
    // database (only its HMAC plus the non-secret listing prefix). The
    // composite UNIQUE (pepper_version, key_hmac) is the lookup identity.

    async fn insert_native_credential(
        &self,
        row: &NativeCredentialRow,
        session_id: &str,
    ) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                crate::credential_session_store::insert_native_credential(
                    &mut client,
                    row,
                    session_id,
                )
                .await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
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

    /// Catalogue slice-1 reads. The statements live in
    /// `crate::catalogue` (shape-pinned there); these impls only bind and
    /// map. Collection bounds come from the catalogue constants, never
    /// literals, so the bound and the statement cannot drift apart. The
    /// pure-mirror reads below run on the mirror connection (ADR 0002 §1);
    /// the library read splits across both databases (see below).
    ///
    /// The library read cannot join across databases, so it runs as four
    /// statements: the app-side page (the `library` ledger is app-target,
    /// migration 0035) plus three bounded mirror probes over the page's
    /// well-formed MBIDs only — poisoned stored values never bind (the
    /// probes' `$n::uuid[]` casts see only `enrichment_key` output, so they
    /// cannot fail). [`crate::catalogue::assemble_library_rows`] joins the
    /// four results with the old listing's per-field NULL degradation.
    async fn catalogue_library(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::catalogue::LibraryDbRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let limit = i64::try_from(crate::catalogue::CATALOGUE_MAX_PAGE_SIZE)
                    .map_err(|_| StoreError)?;
                let page = client
                    .query_typed(
                        crate::catalogue::library_page_statement(),
                        &[(&user_id, Type::TEXT), (&limit, Type::INT8)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_library")
                    })?;
                let page: Vec<crate::catalogue::LibraryPageRow> = page
                    .iter()
                    .map(catalogue_library_page_from_row)
                    .collect::<Result<_, _>>()?;
                if page.is_empty() {
                    return Ok(Vec::new());
                }
                if self.mirror_binding == MirrorBinding::Unconfigured {
                    return Ok(crate::catalogue::assemble_library_rows(
                        &page,
                        &[],
                        &[],
                        &[],
                    ));
                }
                // Well-formed page MBIDs only, deduplicated: malformed
                // stored values never reach a mirror probe.
                let mut recording_mbids: Vec<String> = page
                    .iter()
                    .filter_map(|row| {
                        row.recording_mbid
                            .as_deref()
                            .and_then(crate::catalogue::enrichment_key)
                    })
                    .collect();
                recording_mbids.sort();
                recording_mbids.dedup();
                let mut release_mbids: Vec<String> = page
                    .iter()
                    .filter_map(|row| {
                        row.release_mbid
                            .as_deref()
                            .and_then(crate::catalogue::enrichment_key)
                    })
                    .collect();
                release_mbids.sort();
                release_mbids.dedup();
                let mirror = self.connect_mirror().await?;
                let statement_failed =
                    || self.statement_failed("api.db.statement_failed.catalogue_library");
                let rows = mirror
                    .query_typed(
                        crate::catalogue::library_recordings_statement(),
                        &[(&recording_mbids, Type::TEXT_ARRAY)],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                let recordings = rows
                    .iter()
                    .map(catalogue_library_recording_from_row)
                    .collect::<Result<Vec<_>, _>>()?;
                let rows = mirror
                    .query_typed(
                        crate::catalogue::library_tracks_statement(),
                        &[
                            (&release_mbids, Type::TEXT_ARRAY),
                            (&recording_mbids, Type::TEXT_ARRAY),
                        ],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                let tracks = rows
                    .iter()
                    .map(catalogue_library_track_from_row)
                    .collect::<Result<Vec<_>, _>>()?;
                let rows = mirror
                    .query_typed(
                        crate::catalogue::library_releases_statement(),
                        &[(&release_mbids, Type::TEXT_ARRAY)],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                let releases = rows
                    .iter()
                    .map(catalogue_library_release_from_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(crate::catalogue::assemble_library_rows(
                    &page,
                    &recordings,
                    &tracks,
                    &releases,
                ))
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    /// Member-scoped artist collection: the caller's distinct member
    /// releases from the app ledger (the whole ledger, uncapped — capping
    /// the input at the page size drops artists whose releases sort past
    /// the cap), then the first page of exactly those releases' artists
    /// from the mirror. Shared deadline.
    async fn catalogue_artists(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::catalogue::ArtistDbRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let limit = i64::try_from(crate::catalogue::CATALOGUE_MAX_PAGE_SIZE)
                    .map_err(|_| StoreError)?;
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_member_releases_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_artists")
                    })?;
                let member: Vec<crate::catalogue::MembershipReleaseRow> = member
                    .iter()
                    .map(catalogue_member_release_from_row)
                    .collect::<Result<_, _>>()?;
                let member_releases = crate::catalogue::member_release_mbids(&member);
                if member_releases.is_empty() {
                    return Ok(Vec::new());
                }
                self.require_mirror()?;
                let mirror = self.connect_mirror().await?;
                let statement_failed =
                    || self.statement_failed("api.db.statement_failed.catalogue_artists");
                let rows = mirror
                    .query_typed(
                        crate::catalogue::artists_list_statement(),
                        &[(&member_releases, Type::TEXT_ARRAY), (&limit, Type::INT8)],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                rows.iter().map(catalogue_artist_from_row).collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    /// Member-scoped release collection: same membership input as
    /// [`Self::catalogue_artists`], then the first page of the member
    /// releases themselves.
    async fn catalogue_releases(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::catalogue::ReleaseDbRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let limit = i64::try_from(crate::catalogue::CATALOGUE_MAX_PAGE_SIZE)
                    .map_err(|_| StoreError)?;
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_member_releases_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_releases")
                    })?;
                let member: Vec<crate::catalogue::MembershipReleaseRow> = member
                    .iter()
                    .map(catalogue_member_release_from_row)
                    .collect::<Result<_, _>>()?;
                let member_releases = crate::catalogue::member_release_mbids(&member);
                if member_releases.is_empty() {
                    return Ok(Vec::new());
                }
                self.require_mirror()?;
                let mirror = self.connect_mirror().await?;
                let statement_failed =
                    || self.statement_failed("api.db.statement_failed.catalogue_releases");
                let rows = mirror
                    .query_typed(
                        crate::catalogue::releases_list_statement(),
                        &[(&member_releases, Type::TEXT_ARRAY), (&limit, Type::INT8)],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                rows.iter().map(catalogue_release_from_row).collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    /// Member-scoped track-occurrence collection: the caller's distinct
    /// member (release, recording) pairs from the app ledger (the whole
    /// ledger, uncapped), then the mirror occurrences for exactly those
    /// pairs — the pair join admits no cross-release rows, so the bound
    /// applies to member occurrences, with exact-pair retention as the
    /// safety net.
    async fn catalogue_tracks(
        &self,
        user_id: &str,
    ) -> Result<Vec<crate::catalogue::TrackDbRow>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let limit = i64::try_from(crate::catalogue::CATALOGUE_MAX_PAGE_SIZE)
                    .map_err(|_| StoreError)?;
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_member_track_pairs_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_tracks")
                    })?;
                let member: Vec<crate::catalogue::MembershipTrackPairRow> = member
                    .iter()
                    .map(catalogue_member_pair_from_row)
                    .collect::<Result<_, _>>()?;
                let member_pairs = crate::catalogue::member_pair_arrays(&member);
                if member_pairs.is_empty() {
                    return Ok(Vec::new());
                }
                self.require_mirror()?;
                let (pair_releases, pair_recordings) =
                    crate::catalogue::member_pair_columns(&member_pairs);
                let mirror = self.connect_mirror().await?;
                let statement_failed =
                    || self.statement_failed("api.db.statement_failed.catalogue_tracks");
                let rows = mirror
                    .query_typed(
                        crate::catalogue::tracks_list_statement(),
                        &[
                            (&pair_releases, Type::TEXT_ARRAY),
                            (&pair_recordings, Type::TEXT_ARRAY),
                            (&limit, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| statement_failed())?;
                let rows: Vec<crate::catalogue::TrackDbRow> = rows
                    .iter()
                    .map(catalogue_track_from_row)
                    .collect::<Result<_, _>>()?;
                Ok(crate::catalogue::retain_member_track_occurrences(
                    rows,
                    &member_pairs,
                ))
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    /// Release detail, member-scoped: the app-side membership probe runs
    /// first (case-insensitive, like the mirror's `::uuid` cast) and a
    /// non-member id returns `None` before any mirror probe. The handler
    /// answers 404 for `None`, so unknown and non-member releases stay
    /// indistinguishable (no oracle).
    async fn catalogue_release(
        &self,
        user_id: &str,
        mbid: &str,
    ) -> Result<Option<crate::catalogue::ReleaseDbRow>, StoreError> {
        Self::bounded(
            async {
                let Some(key) = crate::catalogue::enrichment_key(mbid) else {
                    return Ok(None);
                };
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_release_membership_statement(),
                        &[(&user_id, Type::TEXT), (&key, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_release")
                    })?;
                if member.is_empty() {
                    return Ok(None);
                }
                self.require_mirror()?;
                let mirror = self.connect_mirror().await?;
                let rows = mirror
                    .query_typed(
                        crate::catalogue::release_read_statement(),
                        &[(&mbid, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_release")
                    })?;
                // `query_opt`'s rule: more than one row is an error (the
                // read is by primary key, so this is unreachable).
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first().map(catalogue_release_from_row).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    /// Release tracks, member-scoped: same gate as [`Self::catalogue_release`]
    /// so the tracks read is self-sufficient — a non-member id yields the
    /// empty list (the handler only reaches this read for a member release).
    /// A member release yields only the caller's own recordings on it, in
    /// album order: the read returns catalog ids, so each one resolves
    /// through the caller's memberships (ADR 0006), and non-member
    /// recordings on a partly owned release stay out.
    async fn catalogue_release_tracks(
        &self,
        user_id: &str,
        mbid: &str,
    ) -> Result<Vec<crate::catalogue::ReleaseTrackDbRow>, StoreError> {
        Self::bounded(
            async {
                let Some(key) = crate::catalogue::enrichment_key(mbid) else {
                    return Ok(Vec::new());
                };
                let client = self.connect().await?;
                // Unnamed via `query_typed` (TWO-539, Hyperdrive pooled
                // backends).
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_release_membership_statement(),
                        &[(&user_id, Type::TEXT), (&key, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_release_tracks")
                    })?;
                if member.is_empty() {
                    return Ok(Vec::new());
                }
                let member = client
                    .query_typed(
                        crate::catalogue::catalogue_member_release_recordings_statement(),
                        &[(&user_id, Type::TEXT), (&key, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_release_tracks")
                    })?;
                let member: Vec<crate::catalogue::MembershipReleaseRecordingRow> = member
                    .iter()
                    .map(catalogue_member_recording_from_row)
                    .collect::<Result<_, _>>()?;
                let member_recordings = crate::catalogue::member_recording_mbids(&member);
                if member_recordings.is_empty() {
                    return Ok(Vec::new());
                }
                self.require_mirror()?;
                let mirror = self.connect_mirror().await?;
                let limit = i64::from(crate::catalogue::CATALOGUE_RELEASE_TRACKS_MAX);
                let rows = mirror
                    .query_typed(
                        crate::catalogue::release_tracks_statement(),
                        &[
                            (&mbid, Type::TEXT),
                            (&member_recordings, Type::TEXT_ARRAY),
                            (&limit, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.catalogue_release_tracks")
                    })?;
                rows.iter().map(catalogue_release_track_from_row).collect()
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

    async fn claim_webhook_event(&self, event_id: &str) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // First claim wins: one row on insert, none on a retried
                // delivery (`ON CONFLICT DO NOTHING`), so duplicates ack
                // 200 with no state change.
                let rows = client
                    .query_typed(
                        crate::billing_reconcile::claim_webhook_event_statement(),
                        &[(&event_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.claim_webhook_event")
                    })?;
                Ok(rows.len() == 1)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn release_webhook_event(&self, event_id: &str) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Best-effort release of a claim whose handling failed
                // mid-reconcile: deleting the row lets the retried delivery
                // reprocess instead of acking 200 on a consumed claim.
                client
                    .query_typed(
                        crate::billing_reconcile::release_webhook_event_statement(),
                        &[(&event_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.release_webhook_event")
                    })?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn load_reconcile_input(
        &self,
        user_id: &str,
        target_subscription_id: Option<&str>,
    ) -> Result<crate::billing_reconcile::ReconcileInput, StoreError> {
        Self::bounded(
            async {
                let failed =
                    || self.statement_failed("api.db.statement_failed.load_reconcile_input");
                let client = self.connect().await?;
                let account_rows = client
                    .query_typed(
                        crate::billing_reconcile::reconcile_account_select_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| failed())?;
                if account_rows.len() > 1 {
                    return Err(StoreError);
                }
                let stripe_customer_id = account_rows
                    .first()
                    .map(|row| {
                        row.try_get::<_, Option<String>>("stripe_customer_id")
                            .map_err(|_| StoreError)
                    })
                    .transpose()?
                    .flatten();
                let live_rows = match target_subscription_id {
                    Some(target) => {
                        client
                            .query_typed(
                                crate::billing_reconcile::reconcile_target_select_statement(),
                                &[(&user_id, Type::TEXT), (&target, Type::TEXT)],
                            )
                            .await
                    }
                    None => {
                        client
                            .query_typed(
                                crate::billing_reconcile::reconcile_live_select_statement(),
                                &[(&user_id, Type::TEXT)],
                            )
                            .await
                    }
                }
                .map_err(|_| failed())?;
                if live_rows.len() > 1 {
                    return Err(StoreError);
                }
                let live = live_rows
                    .first()
                    .map(|row| {
                        Ok::<_, StoreError>(crate::billing_reconcile::LiveRow {
                            stripe_subscription_id: row
                                .try_get("stripe_subscription_id")
                                .map_err(|_| StoreError)?,
                            stripe_customer_id: row
                                .try_get("stripe_customer_id")
                                .map_err(|_| StoreError)?,
                            stripe_item_id: row
                                .try_get("stripe_subscription_item_id")
                                .map_err(|_| StoreError)?,
                            stripe_price_id: row
                                .try_get("stripe_price_id")
                                .map_err(|_| StoreError)?,
                            plan_id: row.try_get("plan_id").map_err(|_| StoreError)?,
                            status: row.try_get("status").map_err(|_| StoreError)?,
                            paid_through_unix: row
                                .try_get::<_, Option<i64>>("paid_through_unix")
                                .map_err(|_| StoreError)?,
                            paid_invoice_id: row
                                .try_get::<_, Option<String>>("paid_invoice_id")
                                .map_err(|_| StoreError)?,
                            grace_until_unix: row
                                .try_get::<_, Option<i64>>("grace_until_unix")
                                .map_err(|_| StoreError)?,
                        })
                    })
                    .transpose()?;
                let binding_rows = client
                    .query_typed(
                        crate::billing_reconcile::price_bindings_select_statement(),
                        &[],
                    )
                    .await
                    .map_err(|_| failed())?;
                let bindings = binding_rows
                    .iter()
                    .map(|row| {
                        Ok::<_, StoreError>(crate::billing_reconcile::PriceBinding {
                            price_id: row.try_get("stripe_price_id").map_err(|_| StoreError)?,
                            plan_id: row.try_get("plan_id").map_err(|_| StoreError)?,
                            selectable: row.try_get("selectable").map_err(|_| StoreError)?,
                            livemode: row.try_get("livemode").map_err(|_| StoreError)?,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(crate::billing_reconcile::ReconcileInput {
                    stripe_customer_id,
                    live,
                    bindings,
                })
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn user_for_stripe_customer(
        &self,
        customer_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::billing_reconcile::reconcile_user_by_customer_select_statement(),
                        &[(&customer_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.user_for_stripe_customer")
                    })?;
                // `query_opt`'s rule: more than one row is an error (the
                // UNIQUE on `stripe_customer_id` says this never happens).
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|row| row.try_get("user_id").map_err(|_| StoreError))
                    .transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn store_reconcile_outcome(
        &self,
        user_id: &str,
        outcome: &crate::billing_reconcile::ReconcileOutcome,
    ) -> Result<(), StoreError> {
        Self::bounded(
            async {
                let failed =
                    || self.statement_failed("api.db.statement_failed.store_reconcile_outcome");
                let mut client = self.connect().await?;
                let transaction = client.transaction().await.map_err(|_| failed())?;
                // One transaction under the per-user advisory lock: coverage
                // advances monotonically while history inserts converge on
                // the invoice id — duplicates and out-of-order replays land
                // the same stored state.
                transaction
                    .query_typed(
                        crate::billing_reconcile::reconcile_lock_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| failed())?;
                let row = &outcome.row;
                transaction
                    .query_typed(
                        crate::billing_reconcile::store_reconciled_subscription_statement(),
                        &[
                            (&user_id, Type::TEXT),
                            (&row.stripe_subscription_id, Type::TEXT),
                            (&row.stripe_customer_id, Type::TEXT),
                            (&row.stripe_item_id, Type::TEXT),
                            (&row.stripe_price_id, Type::TEXT),
                            (&row.plan_id, Type::TEXT),
                            (&row.status, Type::TEXT),
                            (&row.current_period_end_unix, Type::INT8),
                            (&row.cancel_at_period_end, Type::BOOL),
                            (&row.paid_through_unix, Type::INT8),
                            (&row.paid_invoice_id, Type::TEXT),
                            (&row.grace_until_unix, Type::INT8),
                            (&outcome.read_anchor, Type::TEXT),
                            (&outcome.read_plan, Type::TEXT),
                            (&outcome.explicit_stale, Type::BOOL),
                        ],
                    )
                    .await
                    .map_err(|_| failed())?;
                for history in &outcome.history {
                    transaction
                        .query_typed(
                            crate::billing_reconcile::insert_paid_invoice_history_statement(),
                            &[
                                (&user_id, Type::TEXT),
                                (&history.stripe_invoice_id, Type::TEXT),
                                (&history.stripe_subscription_id, Type::TEXT),
                                (&history.plan_id, Type::TEXT),
                                (&history.period_start_unix, Type::INT8),
                                (&history.period_end_unix, Type::INT8),
                                (&history.amount_cents, Type::INT8),
                                (&history.currency, Type::TEXT),
                                (&history.paid_at_unix, Type::INT8),
                            ],
                        )
                        .await
                        .map_err(|_| failed())?;
                }
                transaction.commit().await.map_err(|_| failed())?;
                Ok(())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn open_checkout_operation(
        &self,
        user_id: &str,
    ) -> Result<Option<crate::billing_reconcile::StoredCheckoutOp>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::billing_reconcile::open_checkout_operation_select_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.open_checkout_operation")
                    })?;
                // `query_opt`'s rule: more than one row is an error (the
                // partial unique admits at most one open flight per user).
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first().map(Self::checkout_op_from_row).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn create_checkout_operation(
        &self,
        op_id: &str,
        user_id: &str,
        price_id: &str,
        customer_id: Option<&str>,
    ) -> Result<crate::billing_reconcile::CreateOpOutcome, StoreError> {
        Self::bounded(
            async {
                let failed =
                    || self.statement_failed("api.db.statement_failed.create_checkout_operation");
                let client = self.connect().await?;
                // The operation id is its own request key (fresh UUID per
                // attempt, never reused across attempts); the partial
                // unique — not this key — arbitrates the race.
                let payload = crate::billing_reconcile::checkout_operation_payload(price_id, None);
                let rows = client
                    .query_typed(
                        crate::billing_reconcile::create_checkout_operation_statement(),
                        &[
                            (&op_id, Type::TEXT),
                            (&user_id, Type::TEXT),
                            (&op_id, Type::TEXT),
                            (&payload, Type::TEXT),
                        ],
                    )
                    .await
                    .map_err(|_| failed())?;
                if rows.len() == 1 {
                    // Winner: bind the Stripe customer to the account, but
                    // only when unbound — an existing binding is never
                    // overwritten by a racing second customer create.
                    if let Some(customer) = customer_id {
                        client
                            .query_typed(
                                crate::billing_reconcile::bind_account_customer_statement(),
                                &[(&user_id, Type::TEXT), (&customer, Type::TEXT)],
                            )
                            .await
                            .map_err(|_| failed())?;
                    }
                    return Ok(crate::billing_reconcile::CreateOpOutcome::Created(
                        crate::billing_reconcile::StoredCheckoutOp {
                            id: op_id.to_string(),
                            user_id: user_id.to_string(),
                            state: "pending".to_string(),
                            price_id: Some(price_id.to_string()),
                            session_id: None,
                            subscription_id: None,
                        },
                    ));
                }
                // Loser: converge on the winner's row (same operation id,
                // hence the same Stripe idempotency key) instead of
                // minting a second subscription.
                let winner = client
                    .query_typed(
                        crate::billing_reconcile::open_checkout_operation_select_statement(),
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| failed())?;
                if winner.len() > 1 {
                    return Err(StoreError);
                }
                winner
                    .first()
                    .map(Self::checkout_op_from_row)
                    .transpose()?
                    .map(crate::billing_reconcile::CreateOpOutcome::Existing)
                    .ok_or(StoreError)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn checkout_operation_by_id(
        &self,
        op_id: &str,
    ) -> Result<Option<crate::billing_reconcile::StoredCheckoutOp>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        crate::billing_reconcile::checkout_operation_by_id_select_statement(),
                        &[(&op_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.checkout_operation_by_id")
                    })?;
                // `query_opt`'s rule: more than one row is an error (the
                // operation id is the primary key).
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first().map(Self::checkout_op_from_row).transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn update_checkout_operation(
        &self,
        op_id: &str,
        state: &str,
        price_id: &str,
        session_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // Conditional mark (the statement only touches unresolved
                // rows): a mark that lands after a deliberate supersede
                // reports `false` instead of resurrecting the retired row.
                // The session already exists under the operation's
                // idempotency key, so a lost mark only costs a retrieve on
                // the next attempt — never a duplicate.
                let payload =
                    crate::billing_reconcile::checkout_operation_payload(price_id, session_id);
                let subscription: Option<String> = None;
                let rows = client
                    .query_typed(
                        crate::billing_reconcile::update_checkout_operation_statement(),
                        &[
                            (&op_id, Type::TEXT),
                            (&state, Type::TEXT),
                            (&payload, Type::TEXT),
                            (&subscription, Type::TEXT),
                        ],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.update_checkout_operation")
                    })?;
                Ok(!rows.is_empty())
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn account_admitted(&self, user_id: &str) -> Result<bool, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // One EXISTS probe over the same established states
                // the reaper protects (`unverified_user_filter`):
                // credential, any session row (a completed sign-in
                // leaves one, revoked or not), vault, or a promoted
                // (non-member) role. A missing user row is simply
                // never-admitted.
                let sql = format!(
                    "SELECT EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id = $1) \
                     OR EXISTS (SELECT 1 FROM sessions WHERE user_id = $1) \
                     OR EXISTS (SELECT 1 FROM provider_vaults WHERE user_id = $1) \
                     OR EXISTS (SELECT 1 FROM users WHERE id = $1 AND role <> '{member}') AS admitted",
                    member = crate::admin::ROLE_MEMBER
                );
                let rows = client
                    .query_typed(&sql, &[(&user_id, Type::TEXT)])
                    .await
                    .map_err(|_| StoreError)?;
                if rows.len() != 1 {
                    return Err(StoreError);
                }
                rows[0].try_get("admitted").map_err(|_| StoreError)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), StoreError> {
        Self::bounded(async {
        let client = self.connect().await?;
        client
            .query_typed(
                "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)",
                &[
                    (&event.user_id, Type::TEXT),
                    (&event.action, Type::TEXT),
                    (&event.ok, Type::BOOL),
                    (&event.source_ip, Type::TEXT),
                    (&i64_of(event.at)?, Type::INT8),
                    (&event.subject_user_id, Type::TEXT),
                    (&event.details, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        Ok(())
        }, TOTAL_TIMEOUT_MS).await
    }

    async fn user_role(&self, user_id: &str) -> Result<Option<String>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        "SELECT role FROM users WHERE id = $1",
                        &[(&user_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| self.statement_failed("api.db.statement_failed.user_role"))?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                rows.first()
                    .map(|row| row.try_get("role").map_err(|_| StoreError))
                    .transpose()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_set_user_role(
        &self,
        actor_user_id: &str,
        subject_user_id: &str,
        new_role: &str,
        source_ip: &str,
        now: u64,
    ) -> Result<AdminWriteOutcome, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                admin_set_user_role_on(
                    &mut client,
                    actor_user_id,
                    subject_user_id,
                    new_role,
                    source_ip,
                    now,
                )
                .await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn app_settings(&self) -> Result<Vec<(String, String)>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed("SELECT key, value FROM app_settings", &[])
                    .await
                    .map_err(|_| self.statement_failed("api.db.statement_failed.app_settings"))?;
                rows.iter()
                    .map(|row| {
                        Ok((
                            row.try_get("key").map_err(|_| StoreError)?,
                            row.try_get("value").map_err(|_| StoreError)?,
                        ))
                    })
                    .collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_set_app_setting(
        &self,
        actor_user_id: &str,
        key: &str,
        new_value: &str,
        source_ip: &str,
        now: u64,
    ) -> Result<AdminWriteOutcome, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                admin_set_app_setting_on(&mut client, actor_user_id, key, new_value, source_ip, now)
                    .await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_ingest_upload(
        &self,
        attempt: &AdminIngestAttempt,
    ) -> Result<AdminIngestUploadOutcome, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                admin_ingest_upload_on(&mut client, attempt).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_ingest_get(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredAdminIngestJob>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = client
                    .query_typed(
                        "SELECT id, status, accepted_at, nzb_bytes, file_count, segment_count, \
                         failure_reason, user_id FROM ingest_jobs WHERE id = $1 AND source = 'admin'",
                        &[(&job_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| self.statement_failed("api.db.statement_failed.admin_ingest_get"))?;
                if rows.len() > 1 {
                    return Err(StoreError);
                }
                let Some(row) = rows.first() else {
                    return Ok(None);
                };
                let originals = client
                    .query_typed(
                        "SELECT 1 FROM ingest_originals WHERE job_id = $1",
                        &[(&job_id, Type::TEXT)],
                    )
                    .await
                    .map_err(|_| self.statement_failed("api.db.statement_failed.admin_ingest_get"))?;
                stored_admin_ingest_job_from(row, !originals.is_empty()).map(Some)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_ingest_list(
        &self,
        status: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredAdminIngestJob>, StoreError> {
        // The handler bounds the window; the store re-clamps
        // defensively so a direct caller cannot page unboundedly.
        let limit = limit.clamp(1, admin_ingest::LIST_MAX_LIMIT);
        Self::bounded(
            async {
                let client = self.connect().await?;
                let rows = match status {
                    None => client
                        .query_typed(
                            "SELECT id, status, accepted_at, nzb_bytes, file_count, segment_count, \
                             failure_reason, user_id FROM ingest_jobs WHERE source = 'admin' \
                             ORDER BY accepted_at DESC, id ASC LIMIT $1 OFFSET $2",
                            &[
                                (&i64_of(limit)?, Type::INT8),
                                (&i64_of(offset)?, Type::INT8),
                            ],
                        )
                        .await
                        .map_err(|_| {
                            self.statement_failed("api.db.statement_failed.admin_ingest_list")
                        })?,
                    Some(status) => client
                        .query_typed(
                            "SELECT id, status, accepted_at, nzb_bytes, file_count, segment_count, \
                             failure_reason, user_id FROM ingest_jobs \
                             WHERE source = 'admin' AND status = $1 \
                             ORDER BY accepted_at DESC, id ASC LIMIT $2 OFFSET $3",
                            &[
                                (&status, Type::TEXT),
                                (&i64_of(limit)?, Type::INT8),
                                (&i64_of(offset)?, Type::INT8),
                            ],
                        )
                        .await
                        .map_err(|_| {
                            self.statement_failed("api.db.statement_failed.admin_ingest_list")
                        })?,
                };
                let mut jobs = Vec::with_capacity(rows.len());
                for row in &rows {
                    let job_id: String = row.try_get("id").map_err(|_| StoreError)?;
                    let originals = client
                        .query_typed(
                            "SELECT 1 FROM ingest_originals WHERE job_id = $1",
                            &[(&job_id, Type::TEXT)],
                        )
                        .await
                        .map_err(|_| {
                            self.statement_failed("api.db.statement_failed.admin_ingest_list")
                        })?;
                    jobs.push(stored_admin_ingest_job_from(row, !originals.is_empty())?);
                }
                Ok(jobs)
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_ingest_delete(
        &self,
        actor_user_id: &str,
        job_id: &str,
    ) -> Result<AdminIngestDeleteOutcome, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                admin_ingest_delete_on(&mut client, actor_user_id, job_id).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }
    async fn admin_issue_invite(
        &self,
        actor_user_id: &str,
        record: &InviteRecord,
        source_ip: &str,
        now: u64,
    ) -> Result<AdminInviteIssueOutcome, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                admin_issue_invite_on(&mut client, actor_user_id, record, source_ip, now).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_list_invites(
        &self,
        issuer: Option<&str>,
        status: Option<&str>,
        now: u64,
        limit: usize,
    ) -> Result<Vec<InviteRecord>, StoreError> {
        Self::bounded(
            async {
                let client = self.connect().await?;
                // The status filter mirrors `InviteRecord::status`
                // exactly (redeemed > revoked > expired > active).
                let status_predicate = match status {
                    Some("redeemed") => "(redeemed_at IS NOT NULL OR used)",
                    Some("revoked") => {
                        "(redeemed_at IS NULL AND NOT used AND revoked_at IS NOT NULL)"
                    }
                    Some("expired") => {
                        "(redeemed_at IS NULL AND NOT used AND revoked_at IS NULL AND expires_at <= $2)"
                    }
                    Some("active") => {
                        "(redeemed_at IS NULL AND NOT used AND revoked_at IS NULL AND expires_at > $2)"
                    }
                    _ => "TRUE",
                };
                let statement = format!(
                    "SELECT {INVITE_COLUMNS} FROM invite_codes \
                     WHERE ($1::text IS NULL OR issued_by = $1) AND {status_predicate} \
                     ORDER BY id DESC LIMIT $3"
                );
                let rows = client
                    .query_typed(
                        &statement,
                        &[
                            (&issuer, Type::TEXT),
                            (&i64_of(now)?, Type::INT8),
                            (&i64::try_from(limit).map_err(|_| StoreError)?, Type::INT8),
                        ],
                    )
                    .await
                    .map_err(|_| {
                        self.statement_failed("api.db.statement_failed.admin_list_invites")
                    })?;
                rows.iter().map(invite_from_row).collect()
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
    }

    async fn admin_revoke_invite(
        &self,
        actor_user_id: &str,
        invite_id: i64,
        source_ip: &str,
        now: u64,
    ) -> Result<AdminInviteRevokeOutcome, StoreError> {
        Self::bounded(
            async {
                let mut client = self.connect().await?;
                admin_revoke_invite_on(&mut client, actor_user_id, invite_id, source_ip, now).await
            },
            TOTAL_TIMEOUT_MS,
        )
        .await
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
                prune_expired_challenges_on(&client, now).await
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
                let mut client = self.connect().await?;
                reap_unverified_users_on(&mut client, now, ttl_secs, protected_user_id).await
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
                let client = self.connect().await?;
                claim_single_use_on(&client, candidate).await
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

// --- Hostname dialer ----------------------------------------------------------------------------------
///
/// The probe dials the validated hostname (never a second DNS lookup of
/// our own): the runtime resolves it for routing and uses the same name
/// for TLS SNI and certificate verification. Dialing a vetted IP literal
/// instead verifies the certificate against the address, so a
/// hostname-only certificate fails closed (`ok:false`) against every
/// real provider.
///
/// There is deliberately no connected-peer check: workerd reports the
/// dial string itself as `SocketInfo.remote_address` for outbound
/// sockets, so a hostname dial can never yield a peer IP to vet. SSRF
/// containment is the handler's fresh dial-time validation (resolve ALL
/// DNS records, reject non-public ranges) immediately before this probe,
/// the allowlisted port, the dial/total timeouts, and no application
/// bytes ever exchanged. A validate-to-dial DNS flip is an accepted
/// residual (threat model §3). See the threat model §3 pinning note.
struct SocketDialer;

/// The dial host for one probe attempt: the validated hostname — never
/// an IP literal (TLS would verify the certificate against the address).
/// A seam so tests pin the root-cause line: the `connect` target must be
/// the hostname.
fn probe_dial_host(target: &ValidatedTarget) -> &str {
    target.host()
}

async fn probe_attempt(host: &str, port: u16, tls: bool) -> bool {
    let secure = if tls {
        worker::SecureTransport::On
    } else {
        worker::SecureTransport::Off
    };
    let mut socket = match worker::Socket::builder()
        .secure_transport(secure)
        .connect(host, port)
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
        let host = probe_dial_host(target);
        let mut deadline = std::pin::pin!(Delay::from(Duration::from_millis(TOTAL_TIMEOUT_MS)));
        let mut attempts = std::pin::pin!(async {
            // Bounded attempts: each dial lets the runtime resolve the
            // validated hostname fresh (routing plus TLS SNI/verify, with
            // runtime address fallback across answers). No application
            // bytes are ever exchanged — the probe proves TCP+TLS
            // reachability only.
            for _ in 0..MAX_DIAL_ADDRESSES {
                if probe_attempt(host, port, tls).await {
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

// --- Stripe transport ----------------------------------------------------------------------------------

// Stripe HTTPS transport over `worker::Fetch`: bearer test secret plus
// the pinned `Stripe-Version` on every call, `Idempotency-Key` on the two
// creations (derived from the billing operation id, so retried creations
// replay instead of duplicating). Unprovisioned (no secret configured)
// every call fails closed to a transport error — the handlers map that to
// the same generic 500 as any store outage, never to an entitlement
// change. Secret material rides the `Authorization` header only; nothing
// here logs header values or response bodies.
struct WorkerStripeTransport {
    secret_key: Option<String>,
}

impl WorkerStripeTransport {
    async fn send<T>(&self, req: &crate::stripe::StripeRequest) -> crate::stripe::StripeResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        use crate::stripe::{StripeError, STRIPE_API_BASE, STRIPE_API_VERSION};
        let key = self
            .secret_key
            .as_deref()
            .ok_or_else(StripeError::transport)?;
        let headers = Headers::new();
        headers
            .set("Authorization", &format!("Bearer {key}"))
            .map_err(|_| StripeError::transport())?;
        headers
            .set("Stripe-Version", STRIPE_API_VERSION)
            .map_err(|_| StripeError::transport())?;
        if let Some(idempotency) = req.idempotency_key.as_deref() {
            headers
                .set("Idempotency-Key", idempotency)
                .map_err(|_| StripeError::transport())?;
        }
        let method = match req.method {
            "POST" => Method::Post,
            "DELETE" => Method::Delete,
            _ => Method::Get,
        };
        if req.method == "POST" {
            headers
                .set("Content-Type", "application/x-www-form-urlencoded")
                .map_err(|_| StripeError::transport())?;
        }
        let url = format!("{STRIPE_API_BASE}{}", req.path);
        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if req.method == "POST" {
            init.with_body(Some(req.body.clone().into()));
        }
        let request = Request::new_with_init(&url, &init).map_err(|_| StripeError::transport())?;
        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|_| StripeError::transport())?;
        let status = response.status_code();
        let body = response
            .text()
            .await
            .map_err(|_| StripeError::transport())?;
        if (200..300).contains(&status) {
            serde_json::from_str(&body).map_err(|_| StripeError::transport())
        } else {
            Err(StripeError::parse(status, body.as_bytes()))
        }
    }
}

impl crate::stripe::StripeTransport for WorkerStripeTransport {
    fn create_customer(
        &self,
        req: crate::stripe::CreateCustomer,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Customer>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::create_customer_request(
                &req.operation_id,
                req.user_id.as_deref(),
                req.test_clock.as_deref(),
            ))
            .await
        })
    }
    fn create_checkout_session(
        &self,
        req: crate::stripe::CreateCheckoutSession,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::CheckoutSession>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::create_checkout_session_request(
                &req.operation_id,
                &req.customer_id,
                &req.price_id,
                &req.success_url,
                &req.cancel_url,
            ))
            .await
        })
    }
    fn retrieve_checkout_session(
        &self,
        session_id: String,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::CheckoutSession>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::retrieve_checkout_session_request(
                &session_id,
            ))
            .await
        })
    }
    fn expire_checkout_session(
        &self,
        session_id: String,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::CheckoutSession>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::expire_checkout_session_request(&session_id))
                .await
        })
    }
    fn retrieve_subscription(
        &self,
        subscription_id: String,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Subscription>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::retrieve_subscription_request(
                &subscription_id,
            ))
            .await
        })
    }
    fn retrieve_invoice(
        &self,
        invoice_id: String,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Invoice>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::retrieve_invoice_request(&invoice_id))
                .await
        })
    }
    fn list_invoice_lines(
        &self,
        invoice_id: String,
        starting_after: Option<String>,
        limit: u8,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::InvoiceLines>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::list_invoice_lines_request(
                &invoice_id,
                starting_after.as_deref(),
                limit,
            ))
            .await
        })
    }
    fn list_invoices(
        &self,
        req: crate::stripe::ListInvoices,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<Vec<crate::stripe::Invoice>>> + '_>,
    > {
        Box::pin(async move {
            // An unfiltered list is never issued: without a customer or a
            // subscription the builder refuses, and the transport fails
            // closed instead of listing the account's invoices. The
            // endpoint returns a list envelope (`object: "list"`), never a
            // bare array, and reconciliation needs every page: follow
            // `has_more` with `starting_after` until exhausted.
            let mut invoices = Vec::new();
            let mut cursor: Option<String> = req.starting_after.clone();
            loop {
                let request = crate::stripe::list_invoices_request(
                    req.customer_id.as_deref(),
                    req.subscription_id.as_deref(),
                    req.paid_only,
                    req.limit,
                    cursor.as_deref(),
                )
                .ok_or_else(crate::stripe::StripeError::transport)?;
                let page: crate::stripe::InvoiceList = self.send(&request).await?;
                let last = page.data.last().map(|invoice| invoice.id.clone());
                invoices.extend(page.data);
                if !page.has_more {
                    break;
                }
                // A `has_more` page with no cursor to continue from cannot
                // advance: fail closed rather than loop on the same page.
                cursor = last;
                if cursor.is_none() {
                    return Err(crate::stripe::StripeError::transport());
                }
            }
            Ok(invoices)
        })
    }
    fn cancel_subscription(
        &self,
        subscription_id: String,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = crate::stripe::StripeResult<crate::stripe::Subscription>> + '_>,
    > {
        Box::pin(async move {
            self.send(&crate::stripe::cancel_subscription_request(
                &subscription_id,
            ))
            .await
        })
    }
}

// --- Fetch dispatch ------------------------------------------------------------------------------------

fn render_header_pairs(
    response: &handlers::Response,
    cors: &[(&'static str, String)],
    trace_id: &str,
) -> Vec<(String, String)> {
    let mut pairs = vec![
        (
            "content-type".to_string(),
            "application/json; charset=utf-8".to_string(),
        ),
        (
            "content-security-policy".to_string(),
            "default-src 'none'; frame-ancestors 'none'".to_string(),
        ),
        ("cache-control".to_string(), "no-store".to_string()),
        ("x-content-type-options".to_string(), "nosniff".to_string()),
        (
            crate::logging::TRACE_HEADER.to_string(),
            trace_id.to_string(),
        ),
    ];
    if let Some(cookie) = &response.set_cookie {
        pairs.push(("set-cookie".to_string(), cookie.clone()));
    }
    if let Some(retry_after) = response.retry_after {
        pairs.push((
            crate::rate_limit::RETRY_AFTER_HEADER.to_string(),
            crate::rate_limit::retry_after_header_value(retry_after),
        ));
    }
    // Caller-supplied extra headers (plan-limit denials): rendered
    // verbatim after the security set, so they can neither suppress nor
    // precede it.
    for (name, value) in &response.extra_headers {
        pairs.push((name.clone(), value.clone()));
    }
    // Credentialed CORS for the configured origin only: the exact header
    // set is decided by the unit-tested `cors_headers`; render only
    // appends it.
    pairs.extend(
        cors.iter()
            .map(|(name, value)| ((*name).to_string(), value.clone())),
    );
    pairs
}

fn render(
    response: &handlers::Response,
    cors: &[(&'static str, String)],
    trace_id: &str,
) -> WorkerResult<Response> {
    let headers = Headers::new();
    for (name, value) in render_header_pairs(response, cors, trace_id) {
        headers.append(&name, &value)?;
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
        extra_headers: Vec::new(),
    }
}

fn internal_render(trace_id: &str) -> WorkerResult<Response> {
    render(&internal_response(), &[], trace_id)
}

// --- Request-body collection & dispatch gate --------------------------------
//
// A body *transport* failure (stream setup or a chunk read) must never be
// converted into an empty or truncated body the handlers then act on: an
// swallowed failure could invoke handlers, store writes or the mailer on
// input the client never successfully sent. Collection
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

/// Collect one request body with an explicit byte cap. The default-cap
/// wrapper below is the single source of truth for API routes
/// (`handlers::MAX_BODY_BYTES`); per-route overrides (the NZB intake)
/// pass their own cap so the collection seam and the in-handler gate
/// stay on the same bound. Rules (in terms of `cap_bytes`):
/// - `declared_len` is the parsed `Content-Length` (`None` when absent
///   or unparsable). `Some(len) if len > MAX` is `Oversized` and the
///   stream is not polled at all.
/// - `Bodyless` is `Complete(vec![])`; an empty stream is too when no
///   positive length was declared (an explicitly sent empty body is not
///   a transport failure).
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
    collect_body_with_cap(declared_len, source, handlers::MAX_BODY_BYTES).await
}

/// [`collect_body`] with a caller-chosen cap: the entry passes the
/// per-route bound (`handlers::max_body_bytes`), so document-upload
/// routes accept their documents while API routes keep the pinned
/// default. Behavior is otherwise identical; the default-cap suite
/// pins `collect_body` unchanged.
pub async fn collect_body_with_cap<S: BodyChunkStream>(
    declared_len: Option<usize>,
    source: BodySource<S>,
    cap_bytes: usize,
) -> CollectedBody {
    if declared_len.is_some_and(|len| len > cap_bytes) {
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
                // `body.len() <= cap` is invariant, so this never overflows
                // and never buffers past the cap.
                if chunk.len() > cap_bytes - body.len() {
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
/// gate can never drift from it. Oversized answers stay route-blind on
/// purpose: this seam runs before authentication and cannot tell an
/// admin from a probe, so a route-distinctive 413 here would disclose
/// the admin ingest path to unauthenticated callers (a bare
/// Content-Length suffices — the stream is never polled). The route's
/// own 413 (`admin_ingest_oversized`) lives only in the in-handler
/// pre-gate, for direct-handle callers that reach it. The Worker
/// entry passes a handler that calls `handlers::handle`, so a rejected
/// body can cause no store, mailer, DNS, or dialer side effect.
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

/// Shared runtime composition for the collected request: the bound-refresh
/// transport guard runs before `handler`, so a marked request whose keyed
/// body was lost in transport is rejected instead of downgrading to the
/// ordinary empty-body rotation. Production dispatch and the transport
/// tests both call this seam, so disabling the guard fails those tests
/// behaviorally rather than only a source-shape pin.
pub async fn handle_with_refresh_transport_guard<F, Fut>(
    request: handlers::Request,
    refresh_content_type: Option<&str>,
    handler: F,
) -> handlers::Response
where
    F: FnOnce(handlers::Request) -> Fut,
    Fut: std::future::Future<Output = handlers::Response>,
{
    if let Some(rejection) = handlers::refresh_transport_rejection(&request, refresh_content_type) {
        return rejection;
    }
    handler(request).await
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
    } else if bare
        .strip_prefix("/v1/releases/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
    {
        // Single-segment release ids only: nested shapes stay `unknown`
        // (they 404 in dispatch), so probe paths never reach the log.
        "/v1/releases/{mbid}".to_string()
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
/// Member-collection cap for the Subsonic mount's per-request snapshot.
/// The native paged reads bind `CATALOGUE_MAX_PAGE_SIZE` (50), which would
/// truncate every library past the first page and make those songs
/// unstreamable; the snapshot reads the whole member collection under this
/// far larger but still bounded cap (rows are in-memory only).
const SUBSONIC_SNAPSHOT_CAP: i64 = 100_000;

// A full page may have been truncated. Refuse it rather than serve a
// potentially incomplete catalogue; the check precedes membership filtering.
fn require_complete_snapshot_page(len: usize, cap: i64) -> Result<(), StoreError> {
    if cap <= 0 || len >= cap as usize {
        Err(StoreError)
    } else {
        Ok(())
    }
}

async fn after_auth<A, T, E, F, S, R>(auth: A, snapshot: F) -> Result<R, E>
where
    A: std::future::Future<Output = Result<T, E>>,
    F: FnOnce(T) -> S,
    S: std::future::Future<Output = Result<R, E>>,
{
    snapshot(auth.await?).await
}

fn pre_auth_subsonic_response(
    method: &str,
    path: &str,
    query: &str,
    body: &str,
) -> Option<nntune_subsonic::HttpResponse> {
    use nntune_subsonic::routes::{route, Route};
    use nntune_subsonic::{MemStore, SubsonicApp};

    matches!(
        route(path),
        Route::PublicEndpoint(_) | Route::Hls | Route::Unknown | Route::Unsupported(_)
    )
    .then(|| SubsonicApp::production(MemStore::empty()).handle(method, path, query, body))
}

fn respond_document(res: &nntune_subsonic::HttpResponse) -> WorkerResult<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", &res.content_type)?;
    headers.set("Cache-Control", "private, no-store")?;
    let mut response = Response::from_bytes(res.body.clone())?.with_status(res.status);
    *response.headers_mut() = headers;
    Ok(response)
}

/// Authenticate before constructing the member-scoped dispatch store.
/// Playback remains unavailable until an occurrence has a trusted binding
/// to an application track; the dispatcher never receives audio bytes.
pub async fn subsonic_mount(
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    env: Env,
    trace_id: Option<String>,
) -> WorkerResult<Response> {
    use crate::subsonic_dispatch as core;
    use nntune_subsonic::envelope::code;
    use nntune_subsonic::{Identity, SubsonicApp, UserAccount};

    if let Some(response) = pre_auth_subsonic_response(method, path, query, body) {
        return respond_document(&response);
    }
    let merged = core::merged_query_params(method, query, body);
    let config = match config_from_env(&env) {
        Some(config) => config,
        None => {
            return respond_document(&core::render_error(
                &merged,
                code::GENERIC,
                "Internal server error.",
            ))
        }
    };
    let keyring = match config
        .subsonic_index
        .as_ref()
        .and_then(|cfg| cfg.keyring().ok())
    {
        Some(keyring) => keyring,
        None => {
            return respond_document(&core::auth_error_response(
                &merged,
                crate::subsonic_auth::AuthError::Unavailable,
            ))
        }
    };
    let form_body = (!body.is_empty()).then_some(body.as_bytes());
    let credentials = match crate::subsonic_http::parse_credentials(Some(query), form_body) {
        Ok(credentials) => credentials,
        Err(err) => return respond_document(&core::parse_error_response(&merged, err)),
    };
    let store = PgStore {
        env: Some(env.clone()),
        trace_id: crate::logging::resolve_trace_id(trace_id.as_deref()),
        mirror_binding: MirrorBinding::from_env(&env),
        #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
        test_client: std::cell::RefCell::new(None),
        #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
        test_mirror: None,
    };
    let now_millis = worker::Date::now().as_millis();
    let auth = async {
        crate::subsonic_auth::authenticate(
            &store,
            &credentials,
            &keyring,
            &config,
            now_millis / 1000,
        )
        .await
        .map_err(|err| core::auth_error_response(&merged, err))
    };
    let (principal, (artist_rows, release_rows, track_rows)) = match after_auth(auth, |principal| {
        let store = &store;
        let merged = &merged;
        async move {
            let rows = store
                .subsonic_member_catalogue(principal.user_id())
                .await
                .map_err(|_| core::render_error(merged, code::GENERIC, "Internal server error."))?;
            Ok((principal, rows))
        }
    })
    .await
    {
        Ok(result) => result,
        Err(response) => return respond_document(&response),
    };
    let (email, role) = match store.subsonic_account(principal.user_id()).await {
        Ok(Some(pair)) => pair,
        _ => {
            return respond_document(&core::render_error(
                &merged,
                code::GENERIC,
                "Internal server error.",
            ))
        }
    };
    let is_admin = role == "admin";
    let identity = Identity {
        username: principal.username().to_string(),
        email: email.clone(),
        is_admin,
    };
    let account = UserAccount {
        email,
        is_admin,
        is_manager: role == "manager",
    };
    let (artists, albums, songs) = core::build_snapshot(&artist_rows, &release_rows, &track_rows);
    let app = SubsonicApp::production(core::ProductionStore::new(
        identity,
        account,
        artists,
        albums,
        songs,
        track_rows,
        now_millis as i64,
    ));
    respond_document(&app.handle(method, path, query, body))
}

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
    // `GET /healthz` reports the same normalized identity the log lines carry.
    let config = config.with_build_sha(Some(build_sha.as_str()));
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
    // Per-route collection bound (the NZB intake accepts documents; API
    // routes keep the pinned default): the in-handler gate re-checks the
    // same bound as defense in depth, so the two cannot drift apart.
    let collected = collect_body_with_cap(
        declared_len,
        source,
        handlers::max_body_bytes(req.method().as_ref(), &req.path()),
    )
    .await;
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
    // The runtime path strips the query string everywhere; the admin
    // ingest list route needs its `?status=&limit=&offset=` filters in
    // production, so the entry reattaches the raw query for exactly
    // `GET /v1/admin/ingest` (non-empty query only). Every other route
    // keeps the bare path, so no new query surface appears elsewhere.
    let bare_path = req.path();
    let handler_path = if req.method() == Method::Get && bare_path == "/v1/admin/ingest" {
        match req.url().ok().and_then(|url| {
            url.query()
                .filter(|query| !query.is_empty())
                .map(|query| format!("{bare_path}?{query}"))
        }) {
            Some(with_query) => with_query,
            None => bare_path,
        }
    } else {
        bare_path
    };
    let request = handlers::Request {
        method: req.method().to_string(),
        path: handler_path,
        cookie: headers_map.get("cookie").cloned(),
        origin: headers_map.get("origin").cloned(),
        authorization: headers_map.get("authorization").cloned(),
        // Raw Stripe signature for the billing webhook carve-out (header
        // keys are lowercased above): the signature IS the webhook's auth,
        // so it rides its own field, never the bearer gate.
        stripe_signature: headers_map.get("stripe-signature").cloned(),
        // Template only: the gate replaces this with the collected bytes
        // on `Complete` and never dispatches it otherwise.
        body: Vec::new(),
    };
    // Stripe transport for this request: the provisioned test secret, or
    // fail-closed per call when unprovisioned (the handlers gate on the
    // same config before any Stripe call).
    let stripe = WorkerStripeTransport {
        secret_key: config.stripe_secret_key.clone(),
    };
    let ctx = handlers::Ctx {
        cfg: &config,
        store: &PgStore {
            env: Some(env.clone()),
            trace_id: trace_id.clone(),
            mirror_binding: MirrorBinding::from_env(&env),
            #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
            test_client: std::cell::RefCell::new(None),
            #[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
            test_mirror: None,
        },
        mailer: &WebhookMailer {
            origin: config.origin.clone(),
            env: env.clone(),
            trace_id: trace_id.clone(),
        },
        dialer: &SocketDialer,
        dns: &DohDns,
        stripe: &stripe,
        now: worker::Date::now().as_millis() / 1000,
        source_ip: client_source_ip(&headers_map),
    };
    let served_method = request.method.clone();
    let served_path = served_display_path(&request.path);
    // Preserve the header marker independently of any truncated upload bytes.
    let refresh_content_type = headers_map.get("content-type").cloned();
    let response = dispatch_collected(collected, request, |collected_request| async move {
        handle_with_refresh_transport_guard(
            collected_request,
            refresh_content_type.as_deref(),
            |guarded_request| async move { handlers::handle(&ctx, &guarded_request).await },
        )
        .await
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

    #[test]
    fn full_snapshot_page_fails_closed() {
        let cap = SUBSONIC_SNAPSHOT_CAP as usize;
        assert!(require_complete_snapshot_page(0, SUBSONIC_SNAPSHOT_CAP).is_ok());
        assert!(require_complete_snapshot_page(cap - 1, SUBSONIC_SNAPSHOT_CAP).is_ok());
        assert!(require_complete_snapshot_page(cap, SUBSONIC_SNAPSHOT_CAP).is_err());
        assert!(require_complete_snapshot_page(cap + 1, SUBSONIC_SNAPSHOT_CAP).is_err());
    }

    #[test]
    fn mount_routes_terminal_paths_before_authentication() {
        for (path, status, body) in [
            ("/rest/hls.m3u8", 410, "Gone"),
            ("/rest/not-a-route.view", 404, "Not Found"),
            ("/rest/jukeboxControl.view", 200, "not supported"),
        ] {
            let response = pre_auth_subsonic_response("GET", path, "f=json", "").unwrap();
            assert_eq!(response.status, status, "{path}");
            assert!(String::from_utf8(response.body).unwrap().contains(body));
        }
        for endpoint in [
            "ping",
            "createPlaylist",
            "updatePlaylist",
            "deletePlaylist",
            "scrobble",
            "star",
            "unstar",
            "setRating",
            "savePlayQueue",
            "refreshPodcasts",
            "createPodcastChannel",
            "deletePodcastChannel",
        ] {
            let path = format!("/rest/{endpoint}.view");
            assert!(pre_auth_subsonic_response("GET", &path, "f=json", "").is_none());
        }
        assert!(pre_auth_subsonic_response(
            "GET",
            "/rest/getOpenSubsonicExtensions.view",
            "f=json",
            ""
        )
        .is_some());
    }

    #[tokio::test]
    async fn snapshot_requires_completed_authentication() {
        let calls = std::cell::Cell::new(0);
        let rejected = after_auth(async { Err::<u8, _>("denied") }, |_| {
            calls.set(calls.get() + 1);
            async { Ok(()) }
        })
        .await;
        assert_eq!(rejected, Err("denied"));
        assert_eq!(calls.get(), 0);
        let calls_ref = &calls;
        let accepted = after_auth(async { Ok::<_, &str>(42) }, |user| async move {
            calls_ref.set(calls_ref.get() + 1);
            Ok(user)
        })
        .await;
        assert_eq!(accepted, Ok(42));
        assert_eq!(calls.get(), 1);
    }

    const REQUEST_TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    /// Test-only driver errors: the hostile string models a driver error
    /// whose message/detail carries identifiers (never logged — no code to
    /// extract), while the coded stub models one with a fixed-shape code.
    impl DriverErrorCode for &str {
        fn sqlstate(&self) -> Option<String> {
            None
        }
    }

    impl DriverErrorCode for () {
        fn sqlstate(&self) -> Option<String> {
            None
        }
    }

    struct CodedDriverError;
    impl DriverErrorCode for CodedDriverError {
        fn sqlstate(&self) -> Option<String> {
            Some("53300".to_string())
        }
    }

    /// Every names-only database failure event PgStore can emit beyond
    /// `api.db.connection_error`.
    const DB_FAILURE_EVENTS: [&str; 25] = [
        "api.db.mirror_unconfigured",
        "api.db.connect_failed.binding",
        "api.db.connect_failed.socket",
        "api.db.connect_failed.dial",
        "api.db.connect_failed.handshake",
        "api.db.connect_failed.set_timeout",
        "api.db.connect_failed.mirror_binding",
        "api.db.connect_failed.mirror_socket",
        "api.db.connect_failed.mirror_dial",
        "api.db.connect_failed.mirror_handshake",
        "api.db.connect_failed.mirror_set_timeout",
        "api.db.statement_failed.user_by_email",
        "api.db.statement_failed.insert_user",
        "api.db.statement_failed.insert_session",
        "api.db.statement_failed.claim_webhook_event",
        "api.db.statement_failed.release_webhook_event",
        "api.db.statement_failed.load_reconcile_input",
        "api.db.statement_failed.user_for_stripe_customer",
        "api.db.statement_failed.store_reconcile_outcome",
        "api.db.statement_failed.open_checkout_operation",
        "api.db.statement_failed.create_checkout_operation",
        "api.db.statement_failed.checkout_operation_by_id",
        "api.db.statement_failed.update_checkout_operation",
        "api.db.statement_failed.subsonic_account",
        "api.db.statement_failed.subsonic_member_catalogue",
    ];

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
                    extra_headers: Vec::new(),
                },
                handlers::Response {
                    status: 401,
                    body: r#"{"error":"unauthorized"}"#.to_string(),
                    set_cookie: None,
                    retry_after: None,
                    extra_headers: Vec::new(),
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
                assert!(
                    pairs.contains(&(crate::logging::TRACE_HEADER.to_string(), trace_id.clone()))
                );
                assert!(cors.iter().all(|pair| pairs
                    .iter()
                    .any(|(name, value)| name == pair.0 && value == &pair.1)));
                if let Some(cookie) = &response.set_cookie {
                    assert!(pairs.contains(&("set-cookie".to_string(), cookie.clone())));
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
            // A coded driver failure logs the fixed-shape code and nothing
            // else: exact field equality proves no message/detail leaked.
            let line = PgStore::connection_diagnostic(
                std::future::ready(Err::<(), _>(CodedDriverError)),
                &trace_id,
                "unknown",
            )
            .now_or_never()
            .expect("synthetic driver is ready")
            .expect("failure emits a record");
            let record: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(record["event"], "api.db.connection_error");
            assert_eq!(
                record["fields"],
                serde_json::json!({"error_class": "service_unavailable", "build_sha": "unknown", "release": "unknown", "sqlstate": "53300"})
            );
            assert!(!line.contains(hostile));
            // The code gate keeps only the 5-char shape: a real SqlState
            // constant passes, anything else (or nothing) maps to no field.
            assert_eq!(
                PgStore::sqlstate_of(Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)),
                Some("23505".to_string())
            );
            assert_eq!(PgStore::sqlstate_of(None), None);
            assert!(PgStore::connection_diagnostic(
                std::future::ready(Ok::<(), ()>(())),
                &trace_id,
                "unknown"
            )
            .now_or_never()
            .unwrap()
            .is_none());
            for event in DB_FAILURE_EVENTS {
                let line = PgStore::db_failure(event, &trace_id, "unknown", None)
                    .expect("every db failure event must pass the event validator");
                let record: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(record["event"], event);
                assert_eq!(
                    record["fields"],
                    serde_json::json!({"error_class": "service_unavailable", "build_sha": "unknown", "release": "unknown"})
                );
            }
        }
    }

    #[test]
    fn every_db_failure_event_is_emitted_from_its_store_path() {
        // The renderer test above calls `db_failure` directly; pin that the
        // production store paths actually name each event, so dropping an
        // emission site fails here. Slice the pre-test source so this
        // test's own literals cannot satisfy it.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let connect_body = production
            .split_once("async fn connect(&self)")
            .expect("PgStore::connect must exist")
            .1
            .split_once("\n    }\n")
            .expect("a closing brace must bound connect")
            .0;
        let mirror_body = production
            .split_once("async fn connect_mirror(&self)")
            .expect("PgStore::connect_mirror must exist")
            .1
            .split_once("\n    }\n")
            .expect("a closing brace must bound connect_mirror")
            .0;
        for body in [connect_body, mirror_body] {
            assert!(
                body.contains("Self::db_failure(") && body.contains("reached.get()"),
                "a failed connect must emit the phase it reached"
            );
        }
        for event in DB_FAILURE_EVENTS {
            if event == "api.db.mirror_unconfigured" {
                let gate = production
                    .split_once("fn require_mirror(&self)")
                    .unwrap()
                    .1
                    .split_once("\n    }\n")
                    .unwrap()
                    .0;
                assert!(gate.contains("self.statement_failed(\"api.db.mirror_unconfigured\")"));
                continue;
            }
            if let Some(phase) = event.strip_prefix("api.db.connect_failed.") {
                let literal = format!("\"api.db.connect_failed.{phase}\"");
                // Mirror phases live in the mirror connect; the app
                // phases in the app connect. Either body may not name
                // the other's phases, so an incident names its cluster.
                let body = if phase.starts_with("mirror_") {
                    mirror_body
                } else {
                    connect_body
                };
                assert!(body.contains(&literal), "connect must reach {event}");
                continue;
            }
            let (kind, operation) = event
                .strip_prefix("api.db.")
                .and_then(|rest| rest.split_once('.'))
                .expect("db failure events are api.db.<kind>.<operation>");
            let method = production
                .split_once(&format!("async fn {operation}(&self"))
                .or_else(|| {
                    production.split_once(&format!("async fn {operation}(\n        &self,"))
                })
                .unwrap_or_else(|| panic!("store method {operation} must exist"))
                .1
                .split_once("\n    async fn ")
                .expect("a following method must bound it")
                .0;
            let method = if operation == "subsonic_member_catalogue" {
                assert!(method.contains(
                    "self.subsonic_member_catalogue_with_cap(user_id, SUBSONIC_SNAPSHOT_CAP)"
                ));
                let (_, helper) = production
                    .split_once("async fn subsonic_member_catalogue_with_cap(")
                    .unwrap();
                helper.split_once("\n    async fn ").unwrap().0
            } else {
                method
            };
            assert_eq!(kind, "statement_failed", "unexpected db failure kind");
            let site = format!("self.statement_failed(\"{event}\")");
            assert!(method.contains(&site), "{operation} must emit {event}");
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
    fn dynamic_release_detail_logs_only_the_fixed_template() {
        // Catalogue roots log bare (they are `is_known_path`); the release
        // id itself never reaches the log — MBIDs are caller data, and
        // unbounded distinct values would break log grouping.
        for path in [
            "/v1/library",
            "/api/library",
            "/v1/artists",
            "/v1/releases",
            "/v1/tracks",
        ] {
            assert_eq!(served_display_path(path), path);
        }
        let mbid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        assert_eq!(
            served_display_path(&format!("/v1/releases/{mbid}")),
            "/v1/releases/{mbid}"
        );
        for path in [
            "/v1/releases/".to_string(),
            format!("/v1/releases/{mbid}/art"),
            format!("/v1/releases/{mbid}/tracks/extra"),
        ] {
            assert_eq!(served_display_path(&path), "unknown", "{path}");
        }
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
        assert_eq!(served_display_path("/v1/vault"), "unknown");
        assert_eq!(served_display_path("/healthz"), "/healthz");
        // A probe path carrying an address collapses to the sentinel ...
        assert_eq!(
            served_display_path("/v1/vault/victim@example.com"),
            "unknown"
        );
        assert_eq!(served_display_path("/probe/anything"), "unknown");
        // ... and query strings never reach the log either.
        assert_eq!(served_display_path("/v1/vault?token=abc"), "unknown");
        assert_eq!(served_display_path("/probe/x?token=abc"), "unknown");
    }

    /// Drive one ready future to completion on a noop waker (the gate
    /// answers without awaiting: the handler runs inline on `Complete`
    /// and never runs otherwise).
    fn drive_gate<F: Future>(future: F) -> F::Output {
        use std::task::{Context, Poll};
        let mut future = Box::pin(future);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the dispatch gate must answer without awaiting"),
        }
    }

    fn gate_request(method: &str, path: &str) -> handlers::Request {
        handlers::Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie: None,
            origin: None,
            authorization: None,
            stripe_signature: None,
            body: Vec::new(),
        }
    }

    #[test]
    fn dispatch_gate_answers_oversize_uniformly_without_dispatching() {
        // The handler must never run on a rejected body: reaching it
        // would let a truncated upload act.
        let probe = |collected: CollectedBody, method: &str, path: &str| {
            drive_gate(dispatch_collected(
                collected,
                gate_request(method, path),
                |_| async { panic!("rejected bodies never dispatch") },
            ))
        };
        // Oversized stays route-blind: the seam runs before
        // authentication, so a route-distinctive 413 for the admin
        // ingest path would disclose it to unauthenticated callers.
        // The route's own 413 lives only in the in-handler pre-gate,
        // for direct-handle callers that reach it.
        let admin = probe(CollectedBody::Oversized, "POST", "/v1/admin/ingest");
        assert_eq!(admin.status, 400);
        assert_eq!(
            admin.body,
            r#"{"code":"invalid_request","message":"invalid request","retryable":false}"#
        );
        // Every other oversized route answers the same generic 400, and
        // a transport failure is never an upload-shaped answer.
        let other = probe(CollectedBody::Oversized, "POST", "/api/ingest");
        assert_eq!(other.status, 400);
        assert_eq!(other.body, admin.body);
        let failed = probe(CollectedBody::TransportFailure, "POST", "/v1/admin/ingest");
        assert_eq!(failed.status, 400);
        assert_eq!(failed.body, other.body);
        // A complete body still dispatches with the bytes in place.
        let seen = drive_gate(dispatch_collected(
            CollectedBody::Complete(vec![1, 2, 3]),
            gate_request("POST", "/v1/admin/ingest"),
            |request| async move {
                assert_eq!(request.body, vec![1, 2, 3]);
                handlers::invalid_request()
            },
        ));
        assert_eq!(seen.status, 400);
    }

    #[test]
    fn admin_ingest_pg_statements_hold_their_contracts() {
        // The FakeStore route suite cannot see these Postgres-only
        // faults, so pin the statement shapes the same way the fence
        // tests pin theirs: slice the pre-test source so this test's
        // own literals cannot satisfy it.
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        // INTEGER columns decode as `i32`: tokio-postgres rejects an
        // INT4 column for `i64` (even NULL), which failed every
        // accepted-job read, and the insert binds the same width.
        for column in ["file_count", "segment_count"] {
            assert!(
                production.contains(&format!("try_get::<_, Option<i32>>(\"{column}\")")),
                "{column} must decode as i32"
            );
        }
        assert!(
            !production.contains("Option<i64> = row.try_get(\"file_count\")"),
            "file_count must not decode as i64"
        );
        assert!(
            !production.contains("Option<i64> = row.try_get(\"segment_count\")"),
            "segment_count must not decode as i64"
        );
        // A DELETE without RETURNING yields no result rows, so the
        // emptiness check would always report Missing.
        assert!(
            production.contains(
                "DELETE FROM ingest_jobs WHERE id = $1 AND source = 'admin' RETURNING id"
            ),
            "the admin job delete must RETURN its id"
        );
        // Admin-upload rows carry the uploading admin's id for the
        // FK/fence; counting them would burn the ordinary daily intake
        // allowance on admin activity.
        let today_body = production
            .split_once("async fn ingest_jobs_today(")
            .expect("ingest_jobs_today must exist")
            .1
            .split_once("\n    }\n")
            .expect("a closing brace must bound ingest_jobs_today")
            .0;
        assert!(
            today_body.contains("AND source = 'pipeline'"),
            "the daily intake count must fence out admin-source rows"
        );
        // The upload writes user-owned rows under a RESTRICT FK, so
        // ADR 0002 rule 3 fences a deleting actor like every other
        // writer — the locked actor re-select must read the marker.
        let upload_body = production
            .split_once("async fn admin_ingest_upload_on(")
            .expect("admin_ingest_upload_on must exist")
            .1
            .split_once("async fn admin_ingest_delete_on(")
            .expect("the delete fn must bound the upload fn")
            .0;
        assert!(
            upload_body.contains("deleting_at IS NOT NULL AS deleting"),
            "the upload actor re-check must read the deletion marker"
        );
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
            "content-security-policy".to_string(),
            "default-src 'none'; frame-ancestors 'none'".to_string()
        )));
        assert!(headers.contains(&("x-content-type-options".to_string(), "nosniff".to_string())));
        assert!(headers.contains(&("cache-control".to_string(), "no-store".to_string())));
    }

    #[test]
    fn render_adds_security_headers_to_successful_json() {
        let response = handlers::Response {
            status: 200,
            body: r#"{"ok":true}"#.to_string(),
            set_cookie: None,
            retry_after: None,
            extra_headers: Vec::new(),
        };
        let headers = render_header_pairs(&response, &[], REQUEST_TRACE);

        assert!(headers.contains(&(
            "content-type".to_string(),
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
            extra_headers: Vec::new(),
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
            extra_headers: Vec::new(),
        };
        let headers = render_header_pairs(&throttled, &[], REQUEST_TRACE);
        assert!(headers.contains(&(
            crate::rate_limit::RETRY_AFTER_HEADER.to_string(),
            "3600".to_string()
        )));
        let plain = handlers::Response {
            status: 200,
            body: r#"{"ok":true}"#.to_string(),
            set_cookie: None,
            retry_after: None,
            extra_headers: Vec::new(),
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
    fn play_grant_secret_overlay_keeps_the_raw_string_bytes() {
        // Absent stays absent (the play routes fail closed per-request).
        assert_eq!(play_grant_secret_from_lookup(|_| None), None);
        // The probe value IS valid base64 of different bytes: if the
        // overlay ever grows a decode (copying the other secrets), the
        // output changes and every grant would fail stream-side
        // verification. Raw bytes must come back byte-identical.
        let probe = crate::b64::encode(b"synthetic-secret-material-012345");
        assert!(
            crate::b64::decode(&probe).is_ok(),
            "probe must be valid base64"
        );
        let got = play_grant_secret_from_lookup(|key| {
            (key == crate::play_grant::PLAY_GRANT_SECRET_VAR).then(|| probe.clone())
        });
        assert_eq!(got.as_deref(), Some(probe.as_bytes()));
    }

    #[test]
    fn meter_sink_secret_overlay_keeps_the_raw_string_bytes() {
        // Absent stays absent (the sink fails closed per-request and
        // the stream DO retries behind its alarm).
        assert_eq!(meter_sink_secret_from_lookup(|_| None), None);
        // Valid-base64 probe, same as the play-grant test: a decode in
        // this overlay would refuse every stream flush. Raw bytes
        // must come back byte-identical.
        let probe = crate::b64::encode(b"synthetic-meter-sink-material-01");
        assert!(
            crate::b64::decode(&probe).is_ok(),
            "probe must be valid base64"
        );
        let got = meter_sink_secret_from_lookup(|key| {
            (key == crate::meter_sink::METER_SINK_SECRET_VAR).then(|| probe.clone())
        });
        assert_eq!(got.as_deref(), Some(probe.as_bytes()));
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
    fn bearer_lookup_budgets_pass_the_store_admissibility_gate() {
        // Regression: the bearer gate's hardcoded pools (300/min, 3000/hr)
        // once exceeded MAX_BUDGET_SLOTS (then 100), so the production
        // pair claim denied EVERY bearer before any credential lookup —
        // uniform 404, never the 403 the scope check returns — while the
        // FakeStore-based unit tests (no cap) stayed green and staging
        // showed mint-201 plus all-404 bearers. Pin the real gate against
        // the real consts so the two can never drift apart again.
        let min = RateLimitBudget {
            scope_key: "198.51.100.9",
            action: "native_rest.bearer_lookup_ip_min",
            limit: crate::handlers::BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_MIN,
            window_secs: crate::handlers::BEARER_LOOKUP_WINDOW_SECS,
        };
        let hour = RateLimitBudget {
            scope_key: "198.51.100.9",
            action: "native_rest.bearer_lookup_ip_hour",
            limit: crate::handlers::BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_HOUR,
            window_secs: crate::handlers::BEARER_LOOKUP_HOUR_WINDOW_SECS,
        };
        assert!(rate_limit_budget_admissible(&min));
        assert!(rate_limit_budget_admissible(&hour));
        // And the cap still bounds the slot scan: the largest live budget
        // is the hourly pool above.
        assert_eq!(
            u64::from(crate::rate_limit::MAX_BUDGET_SLOTS),
            crate::handlers::BEARER_LOOKUP_ATTEMPTS_PER_IP_PER_HOUR
        );
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
    fn config_from_env_refuses_non_test_stripe_keys_at_load() {
        // The bearer gate (`valid_secret_key`) is unit-tested pure, but the
        // load path that enforces it needs a wasm Env — so pin the call
        // site the same way the KEK/overlay tests pin theirs: slice ONLY
        // the `config_from_env` body out of the pre-test source, so this
        // test's own literals cannot satisfy the assertions. Dropping the
        // gate (or reading the key without validating) fails here, and a
        // staged `sk_live_` key would then reach traffic.
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
            body.contains("STRIPE_SECRET_KEY"),
            "config_from_env must read the Stripe bearer secret"
        );
        assert!(
            body.contains("valid_secret_key(key)"),
            "config_from_env must validate the staged bearer through the test-mode gate"
        );
        assert!(
            body.contains("return None"),
            "config_from_env must refuse startup when the staged key fails validation"
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
    fn native_credential_writes_use_the_postgres_tested_transactions() {
        let source = include_str!("worker_entry.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        for name in [
            "insert_native_credential",
            "revoke_sessions_and_native_credentials",
        ] {
            let body = production
                .split_once(&format!("async fn {name}("))
                .unwrap_or_else(|| panic!("{name} must exist"))
                .1
                .split_once("\n    async fn ")
                .expect("a following fn must bound the body")
                .0;
            assert!(
                body.contains(&format!("crate::credential_session_store::{name}(")),
                "the runtime must use the same transaction exercised by PostgreSQL tests"
            );
            assert!(body.contains("TOTAL_TIMEOUT_MS"));
        }
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

    #[test]
    fn probe_dials_the_validated_hostname_not_an_ip_literal() {
        // Root-cause pin: the `connect` target must be the validated
        // hostname (runtime SNI/verify), never a vetted IP literal (TLS
        // would verify the certificate against the address and fail every
        // hostname-only certificate). A peer-IP check is impossible here:
        // workerd reports the dial string itself as
        // `SocketInfo.remote_address`, so it can never vet the peer.
        struct ListDns(Vec<IpAddr>);
        impl crate::vault::DnsResolver for ListDns {
            fn resolve(&self, _host: &str) -> Vec<IpAddr> {
                self.0.clone()
            }
        }
        let target = crate::vault::ProviderConfig {
            host: "news.example.com".into(),
            port: 563,
            tls: true,
            username: "u".into(),
            password: "p".into(),
            max_connections: 2,
            allow_insecure_plaintext: false,
        }
        .validate(&ListDns(vec!["93.184.216.34".parse().unwrap()]))
        .expect("public fixture address validates");
        let host = probe_dial_host(&target);
        assert_eq!(host, "news.example.com");
        assert!(
            host.parse::<IpAddr>().is_err(),
            "dial target must stay a hostname, never an IP literal"
        );
    }
}

// Native acceptance can pause the real reaper between selection and deletion.
// Each single-threaded test runtime owns its hook; production has no hook.
#[cfg(all(test, not(target_arch = "wasm32")))]
type ReaperSelectionPause = std::pin::Pin<Box<dyn Future<Output = ()>>>;

#[cfg(all(test, not(target_arch = "wasm32")))]
std::thread_local! {
    static REAPER_SELECTION_PAUSE: std::cell::RefCell<Option<ReaperSelectionPause>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
async fn reaper_selection_pause() {
    let pause = REAPER_SELECTION_PAUSE.with(|slot| slot.borrow_mut().take());
    if let Some(pause) = pause {
        pause.await;
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "../tests/support/atomic_registration_store_acceptance.rs"]
mod atomic_registration_store_acceptance;
