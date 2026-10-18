//! NNTune v2 MusicBrainz mirror read API.
//!
//! Routes (all JSON, all read-only):
//! - `GET /healthz` — unauthenticated Worker health check; no bindings.
//! - `GET /v1/mb/artist/:mbid` — artist lookup by MBID.
//! - `GET /v1/mb/release/:mbid` — release lookup by MBID.
//! - `GET /v1/mb/recording/:mbid` — recording lookup by MBID.
//! - `GET /v1/mb/release/:mbid/recordings` — a release's recordings, in
//!   track order.
//! - `GET /v1/mb/search?q=&type=artist|release|recording&limit=&offset=` — trigram search,
//!   paged (`limit` defaults to `MAX_PAGE_SIZE`, over-limit clamped; empty `q` is 400
//!   `MB_SEARCH_EMPTY_QUERY`).
//!
//! Data comes from PlanetScale Postgres (`nntunev2-mb`) via the Hyperdrive
//! binding `MB_DB`; successful responses are cached in KV (`MB_CACHE`) and
//! at the edge via the Cache API / `Cache-Control`. Failures (400/404/500)
//! are `no-store` and are never written to either cache. Interactive reads
//! never call musicbrainz.org. Cover art bytes are intended for the
//! per-environment R2 buckets (`nntunev2-art-staging` / `nntunev2-art-prod`,
//! see `docs/ops.md`). Public release URLs are deferred until bucket
//! bindings and public hosts are activated. See `README.md` and
//! `migrations/0001_mb_mirror.sql`.
//!
//! Structure: [`handle_lookup`] / [`handle_search`] are the testable core
//! over the [`MbRepo`] and [`SuccessCache`] traits. The HTTP layer at the bottom wires them to a per-request
//! `tokio-postgres` client over the Hyperdrive socket and to the KV binding.

use nntune_logging::{Level, LogRecord, Worker as LogWorker};
use serde::Serialize;

mod api_error;
use api_error::ApiError;

pub mod replication;

/// Cache TTLs: lookups are immutable-ish (long TTL), search is short TTL.
/// They apply to successful responses only; failures serve `no-store`.
const LOOKUP_CACHE_TTL_S: u32 = 86_400;
const SEARCH_CACHE_TTL_S: u32 = 300;

/// Search returns at most this many hits (SQL `LIMIT` and a defensive
/// truncation in [`handle_search`]).
/// Kept alongside the paged path as the unpaged entry point; production
/// routes use [`handle_search_paged`], unit tests pin this one, hence the
/// allowance in non-test builds.
#[allow(dead_code)]
const SEARCH_LIMIT: usize = 25;

/// A release's recordings listing returns at most this many rows (SQL
/// `LIMIT` and a defensive truncation in [`handle_release_recordings`]).
/// Sized for box sets while keeping one response bounded; the ingest
/// library write accepts up to 10k recording MBIDs, so this cap is the
/// tighter of the two.
const RELEASE_RECORDINGS_LIMIT: usize = 1000;

/// Documented max page size for `GET /v1/mb/search` (`limit`/`offset`).
/// Matches the trigram `LIMIT 25` and the README's documented limit:
/// `None` defaults to this, `1..=MAX_PAGE_SIZE` is honored, anything above
/// is clamped (never rejected), and `Some(0)` is a 400.
const MAX_PAGE_SIZE: usize = 25;

/// Upper bound on `q` length, in chars. Bounds trigram work and KV keys.
const MAX_QUERY_CHARS: usize = 100;

/// Hyperdrive / KV binding names: must match `wrangler.toml.example`.
const MB_DB_BINDING: &str = "MB_DB";
const MB_CACHE_BINDING: &str = "MB_CACHE";

/// R2 cover-art buckets per `docs/ops.md` ("R2 buckets", "Environments
/// matrix"): one bucket per concern per environment. There is no shared
/// unsuffixed art bucket; always use one of the two names below.
pub const ART_BUCKET_STAGING: &str = "nntunev2-art-staging";
pub const ART_BUCKET_PROD: &str = "nntunev2-art-prod";

/// Resolve the cover-art bucket for an environment name: `"staging"` maps
/// to the staging bucket; `"prod"` and `"production"` (the wrangler env
/// name) map to the prod bucket. Anything else (including empty) resolves
/// to `None` so a missing or mistyped env can never silently target prod
/// art. Release responses do not use this mapping until a public host is
/// configured.
pub fn art_bucket_for_env(env: &str) -> Option<&'static str> {
    match env {
        "staging" => Some(ART_BUCKET_STAGING),
        "prod" | "production" => Some(ART_BUCKET_PROD),
        _ => None,
    }
}

/// Build an internal object URI for an art key (for example
/// `art/ab/cd/<sha>-500`, ADR 0003) in the given bucket. This `r2://` URI
/// is not a public HTTP URL and must not be sent to clients. This helper is
/// not used by runtime response handling; release reads pass the stored
/// `caa_url` through unchanged until a custom domain or r2.dev host is configured.
pub fn caa_url_for_art(bucket: &str, key: &str) -> String {
    let key = key.trim_start_matches('/');
    format!("r2://{bucket}/{key}")
}

/// Entity selector. The only route into SQL: each variant maps to a
/// static, parameterized statement (see [`lookup_sql`]/[`search_sql`]), so
/// no user input ever reaches a table or column name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Artist,
    Release,
    Recording,
}

impl Kind {
    fn parse(raw: Option<String>) -> Option<Kind> {
        match raw.as_deref() {
            Some("artist") => Some(Kind::Artist),
            Some("release") => Some(Kind::Release),
            Some("recording") => Some(Kind::Recording),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Artist => "artist",
            Kind::Release => "release",
            Kind::Recording => "recording",
        }
    }
}

/// Lookup statement per kind. `$1` is the validated MBID, cast in SQL so
/// the driver only ever binds a string. The parameter is typed `text`
/// (`$1::text::uuid`): a bare `$1::uuid` would make Postgres infer the
/// parameter as `uuid`, which tokio-postgres cannot bind from `&str`
/// ("error serializing parameter 0", every lookup 500s). Casting through
/// `text` keeps the `&str` binding working while the final `::uuid` cast
/// still hits the primary-key index. Selected ids are cast to `text`
/// to keep row decoding free of uuid/timestamp types.
fn lookup_sql(kind: Kind) -> &'static str {
    match kind {
        Kind::Artist => {
            "SELECT mbid::text AS mbid, name, sort_name, type, country::text AS country \
             FROM mb_artist WHERE mbid = $1::text::uuid"
        }
        Kind::Release => {
            "SELECT mbid::text AS mbid, artist_mbid::text AS artist_mbid, title, \
             release_type, year, caa_url \
             FROM mb_release WHERE mbid = $1::text::uuid"
        }
        Kind::Recording => {
            // A bare recording lookup has no release context, so it has
            // no medium: NULL keeps the shared row shape (the canonical
            // mb_recording row stores neither medium nor a complete
            // release list; mb_track does).
            "SELECT mbid::text AS mbid, release_mbid::text AS release_mbid, title, \
             track_no, duration_ms, NULL::smallint AS medium_no \
             FROM mb_recording WHERE mbid = $1::text::uuid"
        }
    }
}

/// Existence probe for a release MBID. The recordings listing must tell
/// "no such release" (404) apart from "release with no recordings yet"
/// (200, empty list), which the recordings query alone cannot.
const RELEASE_EXISTS_SQL: &str = "SELECT 1 FROM mb_release WHERE mbid = $1::text::uuid";

/// Recordings of one release, in album order with a deterministic
/// tiebreak and a hard limit. Listed from `mb_track` (migration 0016),
/// not from `mb_recording.release_mbid`: the mirror keeps only one
/// canonical release per recording there, so a reissue or compilation
/// would silently miss its shared tracks, and `mb_recording.track_no`
/// is the position within one medium, which interleaves the discs of
/// a multi-medium release. `mb_track` holds every track appearance
/// with its medium position, so ordering by (medium_no, track_no) is
/// true album order. Both positions are NOT NULL in `mb_track`.
const RELEASE_RECORDINGS_SQL: &str =
    "SELECT r.mbid::text AS mbid, t.release_mbid::text AS release_mbid, r.title, \
     t.track_no AS track_no, r.duration_ms, t.medium_no AS medium_no \
     FROM mb_track t JOIN mb_recording r ON r.mbid = t.recording_mbid \
     WHERE t.release_mbid = $1::text::uuid \
     ORDER BY t.medium_no ASC, t.track_no ASC, r.mbid ASC LIMIT 1000";

/// Trigram search statement per kind. `$1` is the normalized query; `%` is
/// the `pg_trgm` similarity predicate (GIN index in migration 0001),
/// ordered by score with a deterministic tiebreak. Release hits also carry
/// the release's artist (`artist_mbid` / `artist_name`, NULL when the
/// release has none) so callers can verify an "Artist - Album" query
/// against the artist instead of trusting a title-only score; other kinds
/// select NULLs for the same columns. `LIMIT`/`OFFSET` are interpolated
/// from `u32` values already resolved by [`handle_search_paged`] (an
/// effective limit in `1..=MAX_PAGE_SIZE` and any `u32` offset): plain
/// integers cannot inject SQL, while binding them as `$2`/`$3` would
/// reintroduce the parameter-typing pitfalls documented on [`lookup_sql`].
fn search_sql(kind: Kind, limit: u32, offset: u32) -> String {
    let base = match kind {
        Kind::Artist => {
            "SELECT mbid::text AS mbid, name AS label, similarity(name, $1) AS score, \
             NULL::text AS artist_mbid, NULL::text AS artist_name \
             FROM mb_artist WHERE name % $1 \
             ORDER BY score DESC, label ASC, mbid ASC"
        }
        Kind::Release => {
            "SELECT r.mbid::text AS mbid, r.title AS label, similarity(r.title, $1) AS score, \
             r.artist_mbid::text AS artist_mbid, a.name AS artist_name \
             FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid \
             WHERE r.title % $1 \
             ORDER BY score DESC, label ASC, r.mbid ASC"
        }
        Kind::Recording => {
            "SELECT mbid::text AS mbid, title AS label, similarity(title, $1) AS score, \
             NULL::text AS artist_mbid, NULL::text AS artist_name \
             FROM mb_recording WHERE title % $1 \
             ORDER BY score DESC, label ASC, mbid ASC"
        }
    };
    format!("{base} LIMIT {limit} OFFSET {offset}")
}

/// Validate an MBID string (UUID shape `8-4-4-4-12` hex). Keeps malformed
/// `:mbid` values out of Hyperdrive SQL and out of cache keys.
fn is_valid_mbid(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &byte) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Normalize a search query for SQL and cache keys: trimmed, lowercased,
/// interior whitespace collapsed. Case/whitespace variants of a query
/// therefore share one SQL shape and one cache entry.
fn normalize_query(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Stable KV key for a lookup success: `mb:lookup:<kind>:<mbid>`.
fn lookup_cache_key(kind: Kind, mbid: &str) -> String {
    format!("mb:lookup:{}:{}", kind.as_str(), mbid.to_lowercase())
}

/// Stable KV key for a release-recordings success:
/// `mb:release-recordings:<release mbid>`.
fn release_recordings_cache_key(release_mbid: &str) -> String {
    format!("mb:release-recordings:{}", release_mbid.to_lowercase())
}

/// Stable KV key for a search success: `mb:search:<kind>:<normalized q>`.
/// Unpaged-path helper (see [`SEARCH_LIMIT`]); paged reads use
/// [`search_paged_cache_key`].
#[allow(dead_code)]
fn search_cache_key(kind: Kind, raw_q: &str) -> String {
    format!("mb:search:{}:{}", kind.as_str(), normalize_query(raw_q))
}

/// Success-path `Cache-Control` value for the given TTL.
fn cache_control(ttl_s: u32) -> String {
    format!("public, max-age={ttl_s}")
}

// --- JSON entities --------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct Artist {
    mbid: String,
    name: String,
    sort_name: String,
    #[serde(rename = "type")]
    artist_type: Option<String>,
    country: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Release {
    mbid: String,
    artist_mbid: Option<String>,
    title: String,
    release_type: Option<String>,
    year: Option<i16>,
    caa_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Recording {
    mbid: String,
    release_mbid: Option<String>,
    title: String,
    track_no: Option<i16>,
    duration_ms: Option<i32>,
    /// Medium (disc) position within the release, from `mb_track`.
    /// Only the release-recordings listing sets it; a bare recording
    /// lookup answers `null`.
    medium_no: Option<i16>,
}

/// One lookup row, tagged by [`Kind`] again at serialization time so the
/// JSON carries `"kind"` alongside the flat entity fields.
#[derive(Debug, Clone)]
enum Entity {
    Artist(Artist),
    Release(Release),
    Recording(Recording),
}

impl Entity {
    fn to_json(&self, kind: Kind) -> serde_json::Value {
        let mut value = match self {
            Entity::Artist(a) => serde_json::to_value(a),
            Entity::Release(r) => serde_json::to_value(r),
            Entity::Recording(r) => serde_json::to_value(r),
        }
        .unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = &mut value {
            map.insert(
                "kind".to_string(),
                serde_json::Value::String(kind.as_str().to_string()),
            );
        }
        value
    }
}

#[derive(Debug, Clone)]
struct SearchHit {
    mbid: String,
    label: String,
    score: f32,
    /// Release hits only: the release's artist, when it has one.
    artist_mbid: Option<String>,
    artist_name: Option<String>,
}

impl SearchHit {
    fn to_json(&self, kind: Kind) -> serde_json::Value {
        // Artists answer `name`, releases/recordings answer `title`,
        // matching the lookup field names for the same row.
        let label_key = match kind {
            Kind::Artist => "name",
            Kind::Release | Kind::Recording => "title",
        };
        let mut value = serde_json::json!({
            "mbid": self.mbid,
            label_key: self.label,
            "score": self.score,
        });
        // Release hits always carry the artist keys (null when the
        // release has no artist) so consumers can require them.
        if kind == Kind::Release {
            if let serde_json::Value::Object(map) = &mut value {
                map.insert(
                    "artist_mbid".to_string(),
                    serde_json::json!(self.artist_mbid),
                );
                map.insert(
                    "artist_name".to_string(),
                    serde_json::json!(self.artist_name),
                );
            }
        }
        value
    }
}

// --- Core handler traits and logic ----------------------------------------

/// Opaque repository failure. Carries no detail on purpose: handler
/// responses must never leak DB/driver internals to clients.
#[derive(Debug)]
struct RepoError;

/// Read-only mirror repository over the `MB_DB` Hyperdrive binding.
/// `search` takes the resolved page so SQL applies `LIMIT`/`OFFSET`
/// directly; callers pass the clamped limit, never the raw request value.
trait MbRepo {
    async fn lookup(&self, kind: Kind, mbid: &str) -> Result<Option<Entity>, RepoError>;
    async fn search(
        &self,
        kind: Kind,
        normalized_q: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError>;
    /// Recordings of one release in album order (medium, then track).
    /// `None` means the release itself does not exist; `Some(empty)`
    /// is a release with no recordings in the mirror yet.
    async fn release_recordings(
        &self,
        release_mbid: &str,
    ) -> Result<Option<Vec<Recording>>, RepoError>;
}

/// Success-only response cache (KV in production). Deliberately
/// infallible: a failed `get` is a miss and a failed `put` is dropped,
/// so cache problems degrade to plain DB reads instead of errors.
trait SuccessCache {
    async fn get(&self, key: &str) -> Option<String>;
    async fn put(&self, key: &str, body: &str, ttl_s: u32);
}

/// Transport-neutral handler result. `cache_ttl` is `Some` exactly for
/// cacheable successes (status 200); the HTTP layer renders `Some(ttl)`
/// as `Cache-Control: public, max-age=<ttl>` and `None` as `no-store`.
#[derive(Debug, PartialEq, Eq)]
struct ApiResponse {
    status: u16,
    body: String,
    cache_ttl: Option<u32>,
}

fn error_response(err: ApiError) -> ApiResponse {
    ApiResponse {
        status: err.status,
        body: err.body_json(),
        cache_ttl: None,
    }
}

fn success_response(body: String, ttl_s: u32) -> ApiResponse {
    ApiResponse {
        status: 200,
        body,
        cache_ttl: Some(ttl_s),
    }
}

fn health_response() -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(r#"{"ok":true}"#.as_bytes().to_vec())
        .expect("static health response is valid")
}

struct HealthResponseParts {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn health_response_parts() -> HealthResponseParts {
    let response = health_response();
    HealthResponseParts {
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value
                        .to_str()
                        .expect("static health header is valid")
                        .to_owned(),
                )
            })
            .collect(),
        body: response.into_body(),
    }
}

/// Lookup core: validate → KV → DB. Only a found row is written to the
/// cache; missing rows (404) and read failures (500) never are.
async fn handle_lookup<R: MbRepo, C: SuccessCache>(
    repo: &R,
    cache: &C,
    kind: Kind,
    mbid: &str,
) -> ApiResponse {
    if !is_valid_mbid(mbid) {
        return error_response(ApiError::invalid_request("invalid mbid (must be UUID)"));
    }
    let key = lookup_cache_key(kind, mbid);
    if let Some(body) = cache.get(&key).await {
        return success_response(body, LOOKUP_CACHE_TTL_S);
    }
    match repo.lookup(kind, &mbid.to_lowercase()).await {
        Ok(Some(entity)) => {
            let body = entity.to_json(kind).to_string();
            cache.put(&key, &body, LOOKUP_CACHE_TTL_S).await;
            success_response(body, LOOKUP_CACHE_TTL_S)
        }
        Ok(None) => error_response(ApiError::not_found("not found")),
        Err(_) => error_response(ApiError::internal_error()),
    }
}

/// Release-recordings core: validate → KV → DB. A listing for an
/// existing release (including an empty one) is a cacheable success at
/// the lookup TTL; a missing release is a 404 and a read failure a 500,
/// neither cached.
async fn handle_release_recordings<R: MbRepo, C: SuccessCache>(
    repo: &R,
    cache: &C,
    release_mbid: &str,
) -> ApiResponse {
    if !is_valid_mbid(release_mbid) {
        return error_response(ApiError::invalid_request("invalid mbid (must be UUID)"));
    }
    let key = release_recordings_cache_key(release_mbid);
    if let Some(body) = cache.get(&key).await {
        return success_response(body, LOOKUP_CACHE_TTL_S);
    }
    let release_mbid = release_mbid.to_lowercase();
    match repo.release_recordings(&release_mbid).await {
        Ok(Some(mut recordings)) => {
            recordings.truncate(RELEASE_RECORDINGS_LIMIT);
            let body = serde_json::json!({
                "release_mbid": release_mbid,
                "recordings": recordings,
            })
            .to_string();
            cache.put(&key, &body, LOOKUP_CACHE_TTL_S).await;
            success_response(body, LOOKUP_CACHE_TTL_S)
        }
        Ok(None) => error_response(ApiError::not_found("not found")),
        Err(_) => error_response(ApiError::internal_error()),
    }
}

/// Search core: validate → KV → DB. A successful query (including an
/// empty result list) is cached for [`SEARCH_CACHE_TTL_S`]; a read
/// failure is a 500, never an empty success, and is never cached.
/// Unpaged entry point, kept alongside the paged path (see [`SEARCH_LIMIT`]).
#[allow(dead_code)]
async fn handle_search<R: MbRepo, C: SuccessCache>(
    repo: &R,
    cache: &C,
    raw_kind: Option<String>,
    raw_q: &str,
) -> ApiResponse {
    let q = raw_q.trim();
    let q_chars = q.chars().count();
    if q_chars < 2 {
        return error_response(ApiError::invalid_request("query too short (min 2 chars)"));
    }
    if q_chars > MAX_QUERY_CHARS {
        return error_response(ApiError::invalid_request("query too long (max 100 chars)"));
    }
    if q.chars().any(char::is_control) {
        return error_response(ApiError::invalid_request(
            "query contains control characters",
        ));
    }
    let kind = match Kind::parse(raw_kind) {
        Some(kind) => kind,
        None => {
            return error_response(ApiError::invalid_request(
                "type must be one of artist|release|recording",
            ))
        }
    };
    let key = search_cache_key(kind, q);
    if let Some(body) = cache.get(&key).await {
        return success_response(body, SEARCH_CACHE_TTL_S);
    }
    // The body echoes the normalized query, not the raw one: case and
    // whitespace variants share this cache entry, so the first writer's
    // raw casing must not leak into later variants' responses.
    let normalized_q = normalize_query(q);
    match repo
        .search(kind, &normalized_q, SEARCH_LIMIT as u32, 0)
        .await
    {
        Ok(mut hits) => {
            hits.truncate(SEARCH_LIMIT);
            let results: Vec<serde_json::Value> =
                hits.iter().map(|hit| hit.to_json(kind)).collect();
            let body = serde_json::json!({
                "q": normalized_q,
                "type": kind.as_str(),
                "results": results,
            })
            .to_string();
            cache.put(&key, &body, SEARCH_CACHE_TTL_S).await;
            success_response(body, SEARCH_CACHE_TTL_S)
        }
        Err(_) => error_response(ApiError::internal_error()),
    }
}

/// Stable 4xx for empty/whitespace search queries: 400 with the registered
/// `MB_SEARCH_EMPTY_QUERY` code (see `docs/api-errors.md`), `no-store`,
/// without touching the repo (never a scan) or the cache.
fn empty_query_response() -> ApiResponse {
    error_response(ApiError::by_code(
        "MB_SEARCH_EMPTY_QUERY",
        "search query must not be empty",
    ))
}

/// Stable KV key for a paged search success. Page coordinates are part of the
/// key so one page can never serve another page's cached body.
fn search_paged_cache_key(kind: Kind, normalized_q: &str, limit: u32, offset: u32) -> String {
    format!(
        "mb:search:{}:{normalized_q}:{limit}:{offset}",
        kind.as_str()
    )
}

/// Paged search core: additive sibling of [`handle_search`] serving
/// `GET /v1/mb/search` with `limit`/`offset`. `limit`: `None` defaults to
/// [`MAX_PAGE_SIZE`]; `Some(0)` is a 400; `1..=MAX_PAGE_SIZE` is honored;
/// anything above is clamped, never rejected. `offset`: `None` (and, at the
/// HTTP layer, unparsable) defaults to `0`; past the end of the result set is
/// a 200 with `"results": []`. Successes echo the effective `limit`/`offset`
/// alongside the normalized `q`, `type` and `results`.
async fn handle_search_paged<R: MbRepo, C: SuccessCache>(
    repo: &R,
    cache: &C,
    raw_kind: Option<String>,
    raw_q: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> ApiResponse {
    // Empty check first: before `type` validation so the code is stable
    // regardless of `type`, and before cache/repo so it is never a scan.
    if raw_q.trim().is_empty() {
        return empty_query_response();
    }
    let q = raw_q.trim();
    let q_chars = q.chars().count();
    if q_chars < 2 {
        return error_response(ApiError::invalid_request("query too short (min 2 chars)"));
    }
    if q_chars > MAX_QUERY_CHARS {
        return error_response(ApiError::invalid_request("query too long (max 100 chars)"));
    }
    if q.chars().any(char::is_control) {
        return error_response(ApiError::invalid_request(
            "query contains control characters",
        ));
    }
    let kind = match Kind::parse(raw_kind) {
        Some(kind) => kind,
        None => {
            return error_response(ApiError::invalid_request(
                "type must be one of artist|release|recording",
            ))
        }
    };
    let effective_limit = match limit {
        None => MAX_PAGE_SIZE as u32,
        Some(0) => return error_response(ApiError::invalid_request("limit must be 1..=25")),
        Some(n) => n.min(MAX_PAGE_SIZE as u32),
    };
    let effective_offset = offset.unwrap_or(0);
    let normalized_q = normalize_query(q);
    let key = search_paged_cache_key(kind, &normalized_q, effective_limit, effective_offset);
    if let Some(body) = cache.get(&key).await {
        return success_response(body, SEARCH_CACHE_TTL_S);
    }
    match repo
        .search(kind, &normalized_q, effective_limit, effective_offset)
        .await
    {
        Ok(mut hits) => {
            // Defensive: SQL already applied the clamped `LIMIT`, but a repo
            // that over-returns must not widen the page.
            hits.truncate(effective_limit as usize);
            let results: Vec<serde_json::Value> =
                hits.iter().map(|hit| hit.to_json(kind)).collect();
            let body = serde_json::json!({
                "q": normalized_q,
                "type": kind.as_str(),
                "limit": effective_limit,
                "offset": effective_offset,
                "results": results,
            })
            .to_string();
            cache.put(&key, &body, SEARCH_CACHE_TTL_S).await;
            success_response(body, SEARCH_CACHE_TTL_S)
        }
        Err(_) => error_response(ApiError::internal_error()),
    }
}

// --- HTTP layer -------------------------------------------------------------

mod http_layer {
    use super::*;
    use worker::kv::KvStore;
    use worker::{
        console_log, Cache, Env, Headers, Request, Response, ResponseBody, Result as WorkerResult,
        RouteContext,
    };

    /// Resolve the request trace from the inbound `x-trace-id` header: a
    /// valid 128-bit ID is kept, anything else is replaced with a fresh
    /// random ID. The invalid value is never logged.
    fn inbound_trace_id(req: &Request) -> String {
        let header = req
            .headers()
            .get(nntune_logging::TRACE_HEADER)
            .ok()
            .flatten();
        nntune_logging::resolve_trace_id(header.as_deref())
    }

    /// Emit one structured log line for this request
    /// (`docs/logging-standard.md`). Field validation runs at construction:
    /// a banned key or secret-like value drops the line instead of leaking
    /// it. All call-site fields are constants plus opaque kinds and status
    /// codes, so the drop branch only fires on a code bug.
    fn emit(level: Level, trace_id: &str, event: &str, fields: serde_json::Value) {
        let ts_millis = worker::Date::now().as_millis();
        let Ok(field_map) = nntune_logging::fields_from_json(fields) else {
            return;
        };
        let Ok(record) =
            LogRecord::new(LogWorker::Mb, level, trace_id, event, field_map, ts_millis)
        else {
            return;
        };
        console_log!("{}", record.to_json_line());
    }

    /// Outcome severity: successes are info, client errors warn, server
    /// errors are errors — so the trace stream stays signal, not noise.
    pub(crate) fn outcome_level(status: u16) -> Level {
        match status {
            200 => Level::Info,
            400 | 404 => Level::Warn,
            _ => Level::Error,
        }
    }

    /// Echo the request trace on a live response so callers can correlate.
    /// The edge-cache copy stays untraced: a cached trace would replay one
    /// request's ID to later clients. Best-effort: a header failure must not
    /// fail the request itself.
    fn with_trace(mut response: Response, trace_id: &str) -> Response {
        let _ = response
            .headers_mut()
            .append(nntune_logging::TRACE_HEADER, trace_id);
        response
    }

    /// KV-backed [`SuccessCache`]; degrades to no-op when the binding is
    /// absent so a misconfigured cache never blocks DB reads.
    enum EdgeCache {
        Kv(KvStore),
        Noop,
    }

    impl EdgeCache {
        fn from_env(env: &Env) -> Self {
            match env.kv(MB_CACHE_BINDING) {
                Ok(kv) => EdgeCache::Kv(kv),
                Err(_) => EdgeCache::Noop,
            }
        }
    }

    impl SuccessCache for EdgeCache {
        async fn get(&self, key: &str) -> Option<String> {
            match self {
                EdgeCache::Kv(kv) => kv.get(key).text().await.ok().flatten(),
                EdgeCache::Noop => None,
            }
        }

        async fn put(&self, key: &str, body: &str, ttl_s: u32) {
            if let EdgeCache::Kv(kv) = self {
                // KV expiration_ttl minimum is 60s; both TTLs exceed it.
                let _ = match kv.put(key, body) {
                    Ok(builder) => builder.expiration_ttl(u64::from(ttl_s)).execute().await,
                    Err(_) => return,
                };
            }
        }
    }

    /// [`MbRepo`] over Hyperdrive. Lazy: the Postgres connection is opened
    /// per query (Hyperdrive pools underneath), so a KV/edge cache hit
    /// never connects. Carries the request trace so the detached
    /// connection-error log still correlates.
    struct PgRepo {
        env: Env,
        trace_id: String,
    }

    impl PgRepo {
        async fn connect(&self) -> Result<tokio_postgres::Client, RepoError> {
            let hyperdrive = self.env.hyperdrive(MB_DB_BINDING).map_err(|_| RepoError)?;
            let socket = worker::Socket::builder()
                .secure_transport(worker::SecureTransport::StartTls)
                .connect(hyperdrive.host(), hyperdrive.port())
                .map_err(|_| RepoError)?;
            let config: tokio_postgres::Config = hyperdrive
                .connection_string()
                .parse()
                .map_err(|_| RepoError)?;
            let (client, connection) = config
                .connect_raw(socket, worker::postgres_tls::PassthroughTls)
                .await
                .map_err(|_| RepoError)?;
            let trace_id = self.trace_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                // Detail stays out of logs: driver errors can carry
                // host/role strings that are not needed to triage.
                if connection.await.is_err() {
                    emit(
                        Level::Error,
                        &trace_id,
                        "mb.db.connect_failed",
                        serde_json::json!({"stage": "connect"}),
                    );
                }
            });
            Ok(client)
        }
    }

    impl MbRepo for PgRepo {
        async fn lookup(&self, kind: Kind, mbid: &str) -> Result<Option<Entity>, RepoError> {
            let client = self.connect().await?;
            let row = client
                .query_opt(lookup_sql(kind), &[&mbid])
                .await
                .map_err(|_| RepoError)?;
            row.map(|row| entity_from_row(kind, &row)).transpose()
        }

        async fn search(
            &self,
            kind: Kind,
            normalized_q: &str,
            limit: u32,
            offset: u32,
        ) -> Result<Vec<SearchHit>, RepoError> {
            let client = self.connect().await?;
            let sql = search_sql(kind, limit, offset);
            let rows = client
                .query(sql.as_str(), &[&normalized_q])
                .await
                .map_err(|_| RepoError)?;
            rows.iter().map(hit_from_row).collect()
        }

        async fn release_recordings(
            &self,
            release_mbid: &str,
        ) -> Result<Option<Vec<Recording>>, RepoError> {
            let client = self.connect().await?;
            let exists = client
                .query_opt(RELEASE_EXISTS_SQL, &[&release_mbid])
                .await
                .map_err(|_| RepoError)?;
            if exists.is_none() {
                return Ok(None);
            }
            let rows = client
                .query(RELEASE_RECORDINGS_SQL, &[&release_mbid])
                .await
                .map_err(|_| RepoError)?;
            let recordings = rows
                .iter()
                .map(recording_from_row)
                .collect::<Result<_, _>>()?;
            Ok(Some(recordings))
        }
    }

    fn entity_from_row(kind: Kind, row: &tokio_postgres::Row) -> Result<Entity, RepoError> {
        // Column names/types are fixed by `lookup_sql`; a mismatch is a
        // server-side schema bug, reported as an opaque read failure.
        let entity = match kind {
            Kind::Artist => Entity::Artist(Artist {
                mbid: row.try_get("mbid").map_err(|_| RepoError)?,
                name: row.try_get("name").map_err(|_| RepoError)?,
                sort_name: row.try_get("sort_name").map_err(|_| RepoError)?,
                artist_type: row.try_get("type").map_err(|_| RepoError)?,
                country: row.try_get("country").map_err(|_| RepoError)?,
            }),
            Kind::Release => Entity::Release(Release {
                mbid: row.try_get("mbid").map_err(|_| RepoError)?,
                artist_mbid: row.try_get("artist_mbid").map_err(|_| RepoError)?,
                title: row.try_get("title").map_err(|_| RepoError)?,
                release_type: row.try_get("release_type").map_err(|_| RepoError)?,
                year: row.try_get("year").map_err(|_| RepoError)?,
                caa_url: row.try_get("caa_url").map_err(|_| RepoError)?,
            }),
            Kind::Recording => Entity::Recording(recording_from_row(row)?),
        };
        Ok(entity)
    }

    fn recording_from_row(row: &tokio_postgres::Row) -> Result<Recording, RepoError> {
        Ok(Recording {
            mbid: row.try_get("mbid").map_err(|_| RepoError)?,
            release_mbid: row.try_get("release_mbid").map_err(|_| RepoError)?,
            title: row.try_get("title").map_err(|_| RepoError)?,
            track_no: row.try_get("track_no").map_err(|_| RepoError)?,
            duration_ms: row.try_get("duration_ms").map_err(|_| RepoError)?,
            medium_no: row.try_get("medium_no").map_err(|_| RepoError)?,
        })
    }

    fn hit_from_row(row: &tokio_postgres::Row) -> Result<SearchHit, RepoError> {
        Ok(SearchHit {
            mbid: row.try_get("mbid").map_err(|_| RepoError)?,
            label: row.try_get("label").map_err(|_| RepoError)?,
            score: row.try_get("score").map_err(|_| RepoError)?,
            artist_mbid: row.try_get("artist_mbid").map_err(|_| RepoError)?,
            artist_name: row.try_get("artist_name").map_err(|_| RepoError)?,
        })
    }

    fn json_headers(cache_ttl: Option<u32>) -> WorkerResult<Headers> {
        let headers = Headers::new();
        headers.append("content-type", "application/json; charset=utf-8")?;
        match cache_ttl {
            Some(ttl) => headers.append("cache-control", &cache_control(ttl))?,
            None => headers.append("cache-control", "no-store")?,
        }
        Ok(headers)
    }

    fn render(api: &ApiResponse) -> WorkerResult<Response> {
        Ok(
            Response::from_body(ResponseBody::Body(api.body.clone().into_bytes()))?
                .with_status(api.status)
                .with_headers(json_headers(api.cache_ttl)?),
        )
    }

    /// Render a core result and, for successes only, store a copy in
    /// the edge Cache API keyed by the request URL. Only the served copy
    /// carries the `x-trace-id` echo (see [`with_trace`]).
    async fn serve(req_url: String, api: ApiResponse, trace_id: &str) -> WorkerResult<Response> {
        let response = with_trace(render(&api)?, trace_id);
        if api.cache_ttl.is_some() {
            let edge_copy = render(&api)?;
            // Best-effort: an edge-cache failure must not fail the read.
            let _ = Cache::default().put(req_url, edge_copy).await;
        }
        Ok(response)
    }

    async fn edge_hit(req: &Request) -> Option<Response> {
        let url = req.url().ok()?.to_string();
        Cache::default().get(url, false).await.ok().flatten()
    }

    pub async fn lookup_route(
        kind: Kind,
        req: &Request,
        ctx: &RouteContext<()>,
    ) -> WorkerResult<Response> {
        // Resolve before the edge lookup: cache hits are the hot path, and
        // they still echo the trace and log one line. The cached copy itself
        // stays untraced (wrapping only mutates the served clone).
        let trace_id = inbound_trace_id(req);
        if let Some(hit) = edge_hit(req).await {
            emit(
                Level::Info,
                &trace_id,
                "mb.lookup.completed",
                serde_json::json!({"kind": kind.as_str(), "status_code": 200, "cache": "edge"}),
            );
            return Ok(with_trace(hit, &trace_id));
        }
        let mbid = ctx.param("mbid").map(String::as_str).unwrap_or("");
        let repo = PgRepo {
            env: ctx.env.clone(),
            trace_id: trace_id.clone(),
        };
        let cache = EdgeCache::from_env(&ctx.env);
        let api = handle_lookup(&repo, &cache, kind, mbid).await;
        // Entity fields never log (names/titles are personal data); the line
        // carries the entity kind plus the outcome status only.
        emit(
            outcome_level(api.status),
            &trace_id,
            "mb.lookup.completed",
            serde_json::json!({"kind": kind.as_str(), "status_code": api.status}),
        );
        serve(req.url()?.to_string(), api, &trace_id).await
    }

    pub async fn release_recordings_route(
        req: &Request,
        ctx: &RouteContext<()>,
    ) -> WorkerResult<Response> {
        // Same trace contract as the other routes: resolve before the edge
        // lookup so cache hits still echo the trace and log one line.
        let trace_id = inbound_trace_id(req);
        if let Some(hit) = edge_hit(req).await {
            emit(
                Level::Info,
                &trace_id,
                "mb.release_recordings.completed",
                serde_json::json!({"status_code": 200, "cache": "edge"}),
            );
            return Ok(with_trace(hit, &trace_id));
        }
        let mbid = ctx.param("mbid").map(String::as_str).unwrap_or("");
        let repo = PgRepo {
            env: ctx.env.clone(),
            trace_id: trace_id.clone(),
        };
        let cache = EdgeCache::from_env(&ctx.env);
        let api = handle_release_recordings(&repo, &cache, mbid).await;
        // The mbid is an opaque UUID, but the line stays minimal (kind plus
        // outcome status) like the sibling routes.
        emit(
            outcome_level(api.status),
            &trace_id,
            "mb.release_recordings.completed",
            serde_json::json!({"status_code": api.status}),
        );
        serve(req.url()?.to_string(), api, &trace_id).await
    }

    pub fn healthz_route() -> WorkerResult<Response> {
        let health = health_response_parts();
        let headers = Headers::new();
        for (name, value) in health.headers {
            headers.append(&name, &value)?;
        }
        Ok(Response::from_body(ResponseBody::Body(health.body))?
            .with_status(health.status)
            .with_headers(headers))
    }

    pub async fn search_route(req: &Request, ctx: &RouteContext<()>) -> WorkerResult<Response> {
        let trace_id = inbound_trace_id(req);
        // Parsed-or-unknown: the raw `type` query value is untrusted input
        // and never reaches a log line verbatim.
        let kind_label = |query: &std::collections::HashMap<String, String>| {
            query
                .get("type")
                .and_then(|raw| Kind::parse(Some(raw.clone())))
                .map(|kind| kind.as_str())
                .unwrap_or("unknown")
        };
        if let Some(hit) = edge_hit(req).await {
            let url = req.url()?;
            let query: std::collections::HashMap<String, String> =
                url.query_pairs().into_owned().collect();
            emit(
                Level::Info,
                &trace_id,
                "mb.search.completed",
                serde_json::json!({"kind": kind_label(&query), "status_code": 200, "cache": "edge"}),
            );
            return Ok(with_trace(hit, &trace_id));
        }
        let url = req.url()?;
        let query: std::collections::HashMap<String, String> =
            url.query_pairs().into_owned().collect();
        let repo = PgRepo {
            env: ctx.env.clone(),
            trace_id: trace_id.clone(),
        };
        let cache = EdgeCache::from_env(&ctx.env);
        // Unparsable `limit`/`offset` fall back to the defaults (`None`):
        // a full first page. An explicit `limit=0` still reaches the handler
        // as `Some(0)` and is rejected there, never silently defaulted.
        let limit = query.get("limit").and_then(|v| v.parse::<u32>().ok());
        let offset = query.get("offset").and_then(|v| v.parse::<u32>().ok());
        let api = handle_search_paged(
            &repo,
            &cache,
            query.get("type").cloned(),
            query.get("q").map(String::as_str).unwrap_or(""),
            limit,
            offset,
        )
        .await;
        // The raw query text never logs (free-form user input); the line
        // carries the parsed kind plus the outcome status only.
        emit(
            outcome_level(api.status),
            &trace_id,
            "mb.search.completed",
            serde_json::json!({"kind": kind_label(&query), "status_code": api.status}),
        );
        serve(url.to_string(), api, &trace_id).await
    }

    pub async fn not_found(req: &Request) -> WorkerResult<Response> {
        let trace_id = inbound_trace_id(req);
        emit(
            Level::Warn,
            &trace_id,
            "mb.request.not_found",
            serde_json::json!({"status_code": 404}),
        );
        Ok(with_trace(
            render(&error_response(ApiError::not_found("not found")))?,
            &trace_id,
        ))
    }
}

#[worker::event(fetch)]
pub async fn main(
    req: worker::Request,
    env: worker::Env,
    _ctx: worker::Context,
) -> worker::Result<worker::Response> {
    worker::Router::new()
        .get_async("/healthz", |_, _| async { http_layer::healthz_route() })
        .get_async("/v1/mb/artist/:mbid", |req, ctx| async move {
            http_layer::lookup_route(Kind::Artist, &req, &ctx).await
        })
        .get_async("/v1/mb/release/:mbid", |req, ctx| async move {
            http_layer::lookup_route(Kind::Release, &req, &ctx).await
        })
        .get_async("/v1/mb/release/:mbid/recordings", |req, ctx| async move {
            http_layer::release_recordings_route(&req, &ctx).await
        })
        .get_async("/v1/mb/recording/:mbid", |req, ctx| async move {
            http_layer::lookup_route(Kind::Recording, &req, &ctx).await
        })
        .get_async("/v1/mb/search", |req, ctx| async move {
            http_layer::search_route(&req, &ctx).await
        })
        .or_else_any_method_async("/*catchall", |req, _| async move {
            http_layer::not_found(&req).await
        })
        .run(req, env)
        .await
}
#[cfg(test)]
mod search_pagination_acceptance;
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    /// Minimal executor: the fakes below complete on first poll.
    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        for _ in 0..100 {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
        panic!("test future did not complete");
    }

    const MBID: &str = "123e4567-e89b-12d3-a456-426614174000";

    #[derive(Default)]
    struct FakeRepo {
        lookup_calls: RefCell<u32>,
        search_calls: RefCell<u32>,
        recordings_calls: RefCell<u32>,
        last_recordings_mbid: RefCell<String>,
        last_search_q: RefCell<String>,
        entity: Option<Entity>,
        hits: Vec<SearchHit>,
        recordings: Option<Vec<Recording>>,
        fail: bool,
    }

    impl MbRepo for FakeRepo {
        async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
            *self.lookup_calls.borrow_mut() += 1;
            if self.fail {
                return Err(RepoError);
            }
            Ok(self.entity.clone())
        }

        async fn search(
            &self,
            _kind: Kind,
            q: &str,
            limit: u32,
            offset: u32,
        ) -> Result<Vec<SearchHit>, RepoError> {
            *self.search_calls.borrow_mut() += 1;
            *self.last_search_q.borrow_mut() = q.to_string();
            if self.fail {
                return Err(RepoError);
            }
            // Mirror SQL `LIMIT`/`OFFSET` so paging tests exercise the slice.
            let from = (offset as usize).min(self.hits.len());
            let to = from.saturating_add(limit as usize).min(self.hits.len());
            Ok(self.hits[from..to].to_vec())
        }

        async fn release_recordings(
            &self,
            release_mbid: &str,
        ) -> Result<Option<Vec<Recording>>, RepoError> {
            *self.recordings_calls.borrow_mut() += 1;
            *self.last_recordings_mbid.borrow_mut() = release_mbid.to_string();
            if self.fail {
                return Err(RepoError);
            }
            Ok(self.recordings.clone())
        }
    }

    #[derive(Default)]
    struct FakeCache {
        stored: RefCell<std::collections::HashMap<String, String>>,
        puts: RefCell<Vec<(String, u32)>>,
        gets: RefCell<u32>,
    }

    impl SuccessCache for FakeCache {
        async fn get(&self, key: &str) -> Option<String> {
            *self.gets.borrow_mut() += 1;
            self.stored.borrow().get(key).cloned()
        }

        async fn put(&self, key: &str, body: &str, ttl_s: u32) {
            self.puts.borrow_mut().push((key.to_string(), ttl_s));
            self.stored
                .borrow_mut()
                .insert(key.to_string(), body.to_string());
        }
    }

    fn artist() -> Entity {
        Entity::Artist(Artist {
            mbid: MBID.to_string(),
            name: "Test Artist".to_string(),
            sort_name: "Artist, Test".to_string(),
            artist_type: Some("Group".to_string()),
            country: Some("US".to_string()),
        })
    }

    fn hit(n: u32) -> SearchHit {
        SearchHit {
            mbid: format!("123e4567-e89b-12d3-a456-42661417{n:04}"),
            label: format!("Hit {n}"),
            score: 1.0 - (n as f32 / 100.0),
            artist_mbid: None,
            artist_name: None,
        }
    }

    #[test]
    fn health_response_is_uncached_json_without_dependencies() {
        let response = health_response_parts();
        assert_eq!(response.status, http::StatusCode::OK.as_u16());
        assert_eq!(response.headers.len(), 2);
        assert!(response
            .headers
            .contains(&("content-type".to_owned(), "application/json".to_owned())));
        assert!(response
            .headers
            .contains(&("cache-control".to_owned(), "no-store".to_owned())));
        assert_eq!(response.body, br#"{"ok":true}"#);
    }

    #[test]
    fn healthz_route_contract_precedes_the_catchall() {
        // Worker Response/Headers require wasm-bindgen, so pin the production
        // route and adapter source while testing the HTTP contract natively.
        let source = include_str!("lib.rs");
        let router = source
            .split_once("\n#[worker::event(fetch)]\npub async fn main")
            .expect("worker entrypoint must exist")
            .1
            .split_once("\n#[cfg(test)]\nmod tests")
            .expect("test module must follow the worker entrypoint")
            .0;
        let route = router
            .find(".get_async(\"/healthz\", |_, _| async { http_layer::healthz_route() })")
            .expect("GET /healthz must dispatch to its handler");
        let catchall = router
            .find(".or_else_any_method_async")
            .expect("router must retain its catch-all");
        assert!(
            route < catchall,
            "healthz must be registered before catch-all"
        );
        assert_eq!(router.matches("\"/healthz\"").count(), 1);

        let handler = source
            .split_once("pub fn healthz_route() -> WorkerResult<Response> {")
            .expect("healthz handler must exist")
            .1
            .split_once("\n    }\n")
            .expect("healthz handler must close")
            .0;
        let compact = |text: &str| {
            text.chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
        };
        assert_eq!(
            compact(handler),
            compact(
                r#"let health = health_response_parts();
        let headers = Headers::new();
        for (name, value) in health.headers {
            headers.append(&name, &value)?;
        }
        Ok(
            Response::from_body(ResponseBody::Body(health.body))?
                .with_status(health.status)
                .with_headers(headers)
        )"#
            )
        );
    }

    #[test]
    fn lookup_sql_is_parameterized_per_kind() {
        for (kind, table) in [
            (Kind::Artist, "mb_artist"),
            (Kind::Release, "mb_release"),
            (Kind::Recording, "mb_recording"),
        ] {
            let sql = lookup_sql(kind);
            assert!(sql.contains(table), "{sql}");
            // `$1` must stay `text`-typed so the `&str` bind works
            // (bare `$1::uuid` makes Postgres infer a uuid parameter,
            // which tokio-postgres refuses from `&str`: every lookup 500s).
            assert!(sql.contains("mbid = $1::text::uuid"), "{sql}");
            assert!(!sql.contains("= $1::uuid"), "{sql}");
            assert!(sql.contains("mbid::text AS mbid"), "{sql}");
        }
    }

    #[test]
    fn search_sql_is_trigram_bounded_and_ordered() {
        for kind in [Kind::Artist, Kind::Release, Kind::Recording] {
            let sql = search_sql(kind, SEARCH_LIMIT as u32, 0);
            assert!(sql.contains("% $1"), "{sql}");
            assert!(sql.contains("similarity("), "{sql}");
            assert!(sql.contains("ORDER BY score DESC"), "{sql}");
            assert!(sql.contains("LIMIT 25"), "{sql}");
            // Every kind selects the artist columns hit_from_row reads.
            assert!(sql.contains("AS artist_mbid"), "{sql}");
            assert!(sql.contains("AS artist_name"), "{sql}");
        }
        assert_eq!(SEARCH_LIMIT, 25);
        // Release search is artist-aware: the artist comes from the
        // joined mb_artist row, not from the title score.
        let release_sql = search_sql(Kind::Release, SEARCH_LIMIT as u32, 0);
        assert!(
            release_sql.contains("LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid"),
            "{release_sql}"
        );
        assert!(
            release_sql.contains("a.name AS artist_name"),
            "{release_sql}"
        );
    }

    #[test]
    fn search_release_hits_carry_the_artist_other_kinds_do_not() {
        let repo = FakeRepo {
            hits: vec![SearchHit {
                mbid: MBID.to_string(),
                label: "Load".to_string(),
                score: 0.333,
                artist_mbid: Some("65f4f0c5-ef9e-490c-aee3-909e7ae6b2ab".to_string()),
                artist_name: Some("Metallica".to_string()),
            }],
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("release".to_string()),
            "Metallica - Load",
        ));
        assert_eq!(resp.status, 200);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        let first = &body["results"][0];
        assert_eq!(first["title"], "Load");
        assert_eq!(first["artist_name"], "Metallica");
        assert_eq!(first["artist_mbid"], "65f4f0c5-ef9e-490c-aee3-909e7ae6b2ab");

        // A release without an artist still carries the keys, as null.
        let no_artist = SearchHit {
            artist_mbid: None,
            artist_name: None,
            ..hit(0)
        }
        .to_json(Kind::Release);
        assert!(no_artist["artist_mbid"].is_null());
        assert!(no_artist["artist_name"].is_null());
        // Non-release hits do not carry the keys at all.
        let artist_hit = hit(0).to_json(Kind::Artist);
        assert!(artist_hit.get("artist_name").is_none());
    }

    #[test]
    fn cache_keys_are_stable_and_normalized() {
        assert_eq!(
            lookup_cache_key(Kind::Artist, "123E4567-E89B-12D3-A456-426614174000"),
            format!("mb:lookup:artist:{MBID}")
        );
        assert_eq!(
            search_cache_key(Kind::Release, "  Blue   Monday "),
            "mb:search:release:blue monday"
        );
        assert_eq!(normalize_query("  Blue   Monday "), "blue monday");
    }

    #[test]
    fn lookup_cache_hit_skips_repo() {
        let cache = FakeCache::default();
        cache.stored.borrow_mut().insert(
            lookup_cache_key(Kind::Artist, MBID),
            r#"{"kind":"artist","cached":true}"#.to_string(),
        );
        let repo = FakeRepo::default();
        let resp = block_on(handle_lookup(&repo, &cache, Kind::Artist, MBID));
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(LOOKUP_CACHE_TTL_S));
        assert!(resp.body.contains("cached"));
        assert_eq!(*repo.lookup_calls.borrow(), 0);
    }

    #[test]
    fn lookup_found_returns_entity_and_caches_success() {
        let repo = FakeRepo {
            entity: Some(artist()),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_lookup(&repo, &cache, Kind::Artist, MBID));
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(86_400));
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["kind"], "artist");
        assert_eq!(body["mbid"], MBID);
        assert_eq!(body["name"], "Test Artist");
        assert_eq!(body["sort_name"], "Artist, Test");
        let puts = cache.puts.borrow();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].1, 86_400);
    }

    #[test]
    fn lookup_release_json_carries_caa_url() {
        let repo = FakeRepo {
            entity: Some(Entity::Release(Release {
                mbid: MBID.to_string(),
                artist_mbid: None,
                title: "Blue Monday".to_string(),
                release_type: Some("Single".to_string()),
                year: Some(1983),
                caa_url: None,
            })),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_lookup(&repo, &cache, Kind::Release, MBID));
        assert_eq!(resp.status, 200);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["title"], "Blue Monday");
        assert_eq!(body["year"], 1983);
        assert!(body["caa_url"].is_null());
        assert!(body["artist_mbid"].is_null());
    }

    #[test]
    fn error_routes_return_the_shared_envelope() {
        // Representative error routes: validation (400), missing (404), and
        // repository failure (500) all carry {code, message, retryable}.
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        let bad = block_on(handle_lookup(&repo, &cache, Kind::Artist, "nope"));
        let body: serde_json::Value = serde_json::from_str(&bad.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"code":"invalid_request","message":"invalid mbid (must be UUID)","retryable":false})
        );
        let missing = block_on(handle_lookup(&repo, &cache, Kind::Artist, MBID));
        let body: serde_json::Value = serde_json::from_str(&missing.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"code":"not_found","message":"not found","retryable":false})
        );
        let short_q = block_on(handle_search(
            &repo,
            &cache,
            Some("artist".to_string()),
            "x",
        ));
        let body: serde_json::Value = serde_json::from_str(&short_q.body).unwrap();
        assert_eq!(body["code"], "invalid_request");
        assert_eq!(body["retryable"], false);
    }

    #[test]
    fn art_bucket_env_matrix_matches_ops_inventory() {
        assert_eq!(ART_BUCKET_STAGING, "nntunev2-art-staging");
        assert_eq!(ART_BUCKET_PROD, "nntunev2-art-prod");
        assert_ne!(ART_BUCKET_STAGING, ART_BUCKET_PROD);
        assert_eq!(art_bucket_for_env("staging"), Some(ART_BUCKET_STAGING));
        assert_eq!(art_bucket_for_env("prod"), Some(ART_BUCKET_PROD));
        assert_eq!(art_bucket_for_env("production"), Some(ART_BUCKET_PROD));
        // Unknown or missing env resolves to nothing: never silently prod.
        assert_eq!(art_bucket_for_env(""), None);
        assert_eq!(art_bucket_for_env("dev"), None);
    }

    #[test]
    fn caa_url_builder_embeds_bucket_and_key() {
        let url = caa_url_for_art(ART_BUCKET_STAGING, "art/ab/cd/0123456789abcdef-500");
        assert!(url.contains(ART_BUCKET_STAGING), "{url}");
        assert!(url.contains("art/ab/cd/0123456789abcdef-500"), "{url}");
        assert!(!url.contains(ART_BUCKET_PROD), "{url}");
        // Leading slashes do not produce an empty path segment.
        let slashed = caa_url_for_art(ART_BUCKET_PROD, "/art/ab/cd/key-250");
        assert_eq!(slashed, "r2://nntunev2-art-prod/art/ab/cd/key-250");
    }

    #[test]
    fn lookup_release_preserves_stored_caa_url_until_public_host_is_configured() {
        let stored = "https://coverartarchive.org/release/abc/front-500";
        let repo = FakeRepo {
            entity: Some(Entity::Release(Release {
                mbid: MBID.to_string(),
                artist_mbid: None,
                title: "Blue Monday".to_string(),
                release_type: Some("Single".to_string()),
                year: Some(1983),
                caa_url: Some(stored.to_string()),
            })),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let response = block_on(handle_lookup(&repo, &cache, Kind::Release, MBID));
        let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(body["caa_url"], stored);
    }

    #[test]
    fn lookup_missing_is_404_and_not_cached() {
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_lookup(&repo, &cache, Kind::Recording, MBID));
        assert_eq!(resp.status, 404);
        assert_eq!(resp.cache_ttl, None);
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn lookup_repo_error_is_generic_500_and_not_cached() {
        let repo = FakeRepo {
            fail: true,
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_lookup(&repo, &cache, Kind::Artist, MBID));
        assert_eq!(resp.status, 500);
        assert_eq!(
            resp.body,
            r#"{"code":"internal_error","message":"unexpected error","retryable":true}"#
        );
        assert_eq!(resp.cache_ttl, None);
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn lookup_invalid_mbid_is_400_before_cache_or_repo() {
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_lookup(&repo, &cache, Kind::Artist, "nope"));
        assert_eq!(resp.status, 400);
        assert_eq!(*repo.lookup_calls.borrow(), 0);
        assert_eq!(*cache.gets.borrow(), 0);
    }

    fn recording(n: u32, track_no: Option<i16>) -> Recording {
        Recording {
            mbid: format!("123e4567-e89b-12d3-a456-42661417{n:04}"),
            release_mbid: Some(MBID.to_string()),
            title: format!("Track {n}"),
            track_no,
            duration_ms: Some(180_000),
            medium_no: Some(1),
        }
    }

    #[test]
    fn release_recordings_sql_is_track_ordered_bounded_and_parameterized() {
        // Listed from mb_track membership (every appearance, with the
        // medium position), never from the canonical
        // mb_recording.release_mbid, which omits shared recordings.
        assert!(
            RELEASE_RECORDINGS_SQL.contains("FROM mb_track t JOIN mb_recording r"),
            "{RELEASE_RECORDINGS_SQL}"
        );
        assert!(
            RELEASE_RECORDINGS_SQL.contains("t.release_mbid = $1::text::uuid"),
            "{RELEASE_RECORDINGS_SQL}"
        );
        assert!(
            !RELEASE_RECORDINGS_SQL.contains("= $1::uuid"),
            "{RELEASE_RECORDINGS_SQL}"
        );
        assert!(
            RELEASE_RECORDINGS_SQL.contains("ORDER BY t.medium_no ASC, t.track_no ASC, r.mbid ASC"),
            "{RELEASE_RECORDINGS_SQL}"
        );
        assert!(
            RELEASE_RECORDINGS_SQL.contains("LIMIT 1000"),
            "{RELEASE_RECORDINGS_SQL}"
        );
        assert_eq!(RELEASE_RECORDINGS_LIMIT, 1000);
        assert!(
            RELEASE_EXISTS_SQL.contains("FROM mb_release"),
            "{RELEASE_EXISTS_SQL}"
        );
        assert!(
            RELEASE_EXISTS_SQL.contains("mbid = $1::text::uuid"),
            "{RELEASE_EXISTS_SQL}"
        );
    }

    #[test]
    fn release_recordings_found_returns_rows_in_repo_order_and_caches_success() {
        let repo = FakeRepo {
            recordings: Some(vec![recording(1, Some(1)), recording(2, Some(2))]),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_release_recordings(
            &repo,
            &cache,
            "123E4567-E89B-12D3-A456-426614174000",
        ));
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(LOOKUP_CACHE_TTL_S));
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["release_mbid"], MBID);
        let recordings = body["recordings"].as_array().unwrap();
        assert_eq!(recordings.len(), 2);
        assert_eq!(recordings[0]["track_no"], 1);
        assert_eq!(recordings[0]["medium_no"], 1);
        assert_eq!(recordings[0]["title"], "Track 1");
        assert_eq!(recordings[1]["track_no"], 2);
        // The repo sees the normalized (lowercase) MBID, matching the key.
        assert_eq!(repo.last_recordings_mbid.borrow().as_str(), MBID);
        let puts = cache.puts.borrow();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].0, format!("mb:release-recordings:{MBID}"));
        assert_eq!(puts[0].1, LOOKUP_CACHE_TTL_S);
    }

    #[test]
    fn release_recordings_empty_success_is_200_and_cacheable() {
        let repo = FakeRepo {
            recordings: Some(vec![]),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(LOOKUP_CACHE_TTL_S));
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["recordings"].as_array().unwrap().len(), 0);
        assert_eq!(cache.puts.borrow().len(), 1);
    }

    #[test]
    fn release_recordings_are_capped_at_the_hard_limit() {
        let repo = FakeRepo {
            recordings: Some(
                (0..(RELEASE_RECORDINGS_LIMIT as u32 + 5))
                    .map(|n| recording(n, Some(1)))
                    .collect(),
            ),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
        assert_eq!(resp.status, 200);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body["recordings"].as_array().unwrap().len(),
            RELEASE_RECORDINGS_LIMIT
        );
    }

    #[test]
    fn release_recordings_missing_release_is_404_and_not_cached() {
        let repo = FakeRepo::default(); // recordings: None => no such release
        let cache = FakeCache::default();
        let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
        assert_eq!(resp.status, 404);
        assert_eq!(resp.cache_ttl, None);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"code":"not_found","message":"not found","retryable":false})
        );
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn release_recordings_invalid_mbid_is_400_before_cache_or_repo() {
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_release_recordings(&repo, &cache, "nope"));
        assert_eq!(resp.status, 400);
        assert_eq!(resp.cache_ttl, None);
        assert_eq!(*repo.recordings_calls.borrow(), 0);
        assert_eq!(*cache.gets.borrow(), 0);
    }

    #[test]
    fn release_recordings_repo_error_is_generic_500_and_not_cached() {
        let repo = FakeRepo {
            fail: true,
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
        assert_eq!(resp.status, 500);
        assert_eq!(
            resp.body,
            r#"{"code":"internal_error","message":"unexpected error","retryable":true}"#
        );
        assert_eq!(resp.cache_ttl, None);
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn release_recordings_cache_hit_skips_repo() {
        let cache = FakeCache::default();
        cache.stored.borrow_mut().insert(
            release_recordings_cache_key(MBID),
            r#"{"release_mbid":"x","cached":true}"#.to_string(),
        );
        let repo = FakeRepo::default();
        let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(LOOKUP_CACHE_TTL_S));
        assert!(resp.body.contains("cached"));
        assert_eq!(*repo.recordings_calls.borrow(), 0);
    }

    #[test]
    fn search_returns_ordered_hits_capped_at_limit_and_caches() {
        let repo = FakeRepo {
            hits: (0..30).map(hit).collect(),
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("artist".to_string()),
            "  Test  ",
        ));
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(300));
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["type"], "artist");
        assert_eq!(body["q"], "test");
        let results = body["results"].as_array().unwrap();
        assert_eq!(results.len(), 25);
        assert_eq!(results[0]["name"], "Hit 0");
        assert_eq!(cache.puts.borrow().len(), 1);
        assert_eq!(cache.puts.borrow()[0].1, 300);
        // SQL param is the normalized query, matching the cache key.
        assert_eq!(repo.last_search_q.borrow().as_str(), "test");
    }

    #[test]
    fn search_empty_success_is_200_and_cacheable() {
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("release".to_string()),
            "nothing matches",
        ));
        assert_eq!(resp.status, 200);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["results"].as_array().unwrap().len(), 0);
        assert_eq!(resp.cache_ttl, Some(300));
    }

    #[test]
    fn search_repo_error_is_not_an_empty_success() {
        let repo = FakeRepo {
            fail: true,
            ..Default::default()
        };
        let cache = FakeCache::default();
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("recording".to_string()),
            "anything",
        ));
        assert_eq!(resp.status, 500);
        assert!(!resp.body.contains("results"));
        assert_eq!(resp.cache_ttl, None);
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn search_cache_hit_skips_repo() {
        let cache = FakeCache::default();
        cache.stored.borrow_mut().insert(
            search_cache_key(Kind::Artist, "test"),
            r#"{"q":"test","cached":true}"#.to_string(),
        );
        let repo = FakeRepo::default();
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("artist".to_string()),
            " Test ",
        ));
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("cached"));
        assert_eq!(*repo.search_calls.borrow(), 0);
    }

    #[test]
    fn search_rejects_bad_input_before_repo() {
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        for (kind, q) in [
            (Some("artist".to_string()), "a"),
            (Some("track".to_string()), "valid query"),
            (None, "valid query"),
            (Some("artist".to_string()), "x"),
        ] {
            let resp = block_on(handle_search(&repo, &cache, kind, q));
            assert_eq!(resp.status, 400, "q={q}");
            assert_eq!(resp.cache_ttl, None);
        }
        let too_long = "a".repeat(MAX_QUERY_CHARS + 1);
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("artist".to_string()),
            &too_long,
        ));
        assert_eq!(resp.status, 400);
        assert_eq!(*repo.search_calls.borrow(), 0);
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn search_rejects_control_characters_before_cache_or_repo() {
        let repo = FakeRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search(
            &repo,
            &cache,
            Some("artist".to_string()),
            "\0ab",
        ));

        assert_eq!(resp.status, 400);
        assert_eq!(resp.cache_ttl, None);
        assert_eq!(*repo.search_calls.borrow(), 0);
        assert_eq!(*cache.gets.borrow(), 0);
        assert!(cache.puts.borrow().is_empty());
    }

    #[test]
    fn search_kind_accepts_known_types() {
        assert_eq!(Kind::parse(Some("artist".to_string())), Some(Kind::Artist));
        assert_eq!(
            Kind::parse(Some("release".to_string())),
            Some(Kind::Release)
        );
        assert_eq!(
            Kind::parse(Some("recording".to_string())),
            Some(Kind::Recording)
        );
    }

    #[test]
    fn search_kind_rejects_unknown_types() {
        assert_eq!(Kind::parse(None), None);
        assert_eq!(Kind::parse(Some("track".to_string())), None);
        assert_eq!(Kind::parse(Some(String::new())), None);
    }

    #[test]
    fn cache_control_formats_ttl() {
        assert_eq!(cache_control(300), "public, max-age=300");
        assert_eq!(cache_control(LOOKUP_CACHE_TTL_S), "public, max-age=86400");
    }

    #[test]
    fn mbid_validation_accepts_uuid_shape() {
        assert!(is_valid_mbid("123e4567-e89b-12d3-a456-426614174000"));
        assert!(is_valid_mbid("123E4567-E89B-12D3-A456-426614174000"));
    }

    #[test]
    fn mbid_validation_rejects_malformed_values() {
        assert!(!is_valid_mbid("not-a-uuid"));
        assert!(!is_valid_mbid(""));
        assert!(!is_valid_mbid("123e4567-e89b-12d3-a456-42661417400"));
        assert!(!is_valid_mbid("123e4567-e89b-12d3-a456-4266141740000"));
        assert!(!is_valid_mbid("123e4567_e89b_12d3_a456_426614174000"));
        assert!(!is_valid_mbid("gggggggg-hhhh-iiii-jjjj-kkkkkkkkkkkk"));
    }

    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    #[test]
    fn outcome_levels_favour_signal_over_noise() {
        assert_eq!(super::http_layer::outcome_level(200), Level::Info);
        assert_eq!(super::http_layer::outcome_level(400), Level::Warn);
        assert_eq!(super::http_layer::outcome_level(404), Level::Warn);
        assert_eq!(super::http_layer::outcome_level(500), Level::Error);
        assert_eq!(super::http_layer::outcome_level(503), Level::Error);
    }

    /// The route's `completed` line goes through the REAL emitter, not a
    /// hand-written fixture: same constructor and renderer production uses.
    #[test]
    fn structured_completed_lines_conform_to_the_standard() {
        for (event, kind) in [
            ("mb.lookup.completed", "artist"),
            ("mb.search.completed", "release"),
        ] {
            let fields = nntune_logging::fields_from_json(
                serde_json::json!({"kind": kind, "status_code": 200}),
            )
            .unwrap();
            let record = LogRecord::new(
                LogWorker::Mb,
                Level::Info,
                TRACE,
                event,
                fields,
                1_792_094_400_123,
            )
            .unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line()).unwrap();
            let object = parsed.as_object().unwrap();
            assert_eq!(object.len(), 6);
            assert_eq!(object["worker"], "mb");
            assert_eq!(object["trace_id"], TRACE);
            assert_eq!(object["event"], event);
            assert_eq!(object["ts"], "2026-10-15T20:00:00.123Z");
            assert_eq!(record.trace_id(), TRACE);
        }
    }

    /// Negative privacy coverage: the only shapes routes emit are kinds and
    /// status codes — entity content (names, titles) and raw query text never
    /// reach the emitter, and identity/credential/network shapes are rejected
    /// outright when they do.
    #[test]
    fn completed_lines_carry_no_entity_content() {
        for value in [
            serde_json::json!({"kind": "artist", "status_code": 200}),
            serde_json::json!({"kind": "unknown", "status_code": 400}),
            serde_json::json!({"status_code": 404}),
        ] {
            let fields = nntune_logging::fields_from_json(value).unwrap();
            assert!(
                fields.keys().all(|k| k == "kind" || k == "status_code"),
                "route shape carries more than kind + status"
            );
        }
        for value in [
            serde_json::json!({"name": "Test Artist"}),
            serde_json::json!({"email": "a@example.com"}),
            serde_json::json!({"client_ip": "192.0.2.1"}),
            serde_json::json!({"token": "tok_abc"}),
        ] {
            assert!(
                nntune_logging::fields_from_json(value).is_err(),
                "emitter accepted identity content"
            );
        }
    }
}
