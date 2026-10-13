//! NNTune v2 MusicBrainz mirror read API.
//!
//! Routes (all JSON, all read-only):
//! - `GET /v1/mb/artist/:mbid` — artist lookup by MBID.
//! - `GET /v1/mb/release/:mbid` — release lookup by MBID.
//! - `GET /v1/mb/recording/:mbid` — recording lookup by MBID.
//! - `GET /v1/mb/search?q=&type=artist|release|recording` — trigram search.
//!
//! Data comes from PlanetScale Postgres (`nntunev2-mb`) via Hyperdrive;
//! hot paths are cached in KV / Cache API. Cover art bytes live in the
//! `nntunev2-art` R2 bucket; this worker serves cacheable URLs, never
//! secrets. See `README.md` and `migrations/0001_mb_mirror.sql`.

use serde::Serialize;
use worker::{console_log, Env, Request, Response, Result, RouteContext, Router};

/// Cache TTLs: lookups are immutable-ish (long TTL), search is short TTL.
const LOOKUP_CACHE_TTL_S: u32 = 86_400;
const SEARCH_CACHE_TTL_S: u32 = 300;

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

fn json_error(message: &str, status: u16) -> Result<Response> {
    let body = serde_json::to_string(&ApiError {
        error: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"internal"}"#.to_string());
    Response::from_body(worker::ResponseBody::Body(body.into_bytes()))
        .map(|r| {
            r.with_status(status)
                .with_headers(headers_json().unwrap_or_default())
        })
}

fn headers_json() -> Result<worker::Headers> {
    let headers = worker::Headers::new();
    headers.append("content-type", "application/json; charset=utf-8")?;
    Ok(headers)
}

fn cache_control(ttl_s: u32) -> String {
    format!("public, max-age={ttl_s}")
}

/// Validate a `type=` search selector. Keeps unknown types out of SQL.
fn parse_search_kind(raw: Option<String>) -> Option<&'static str> {
    match raw.as_deref() {
        Some("artist") => Some("artist"),
        Some("release") => Some("release"),
        Some("recording") => Some("recording"),
        _ => None,
    }
}

/// Placeholder lookup handler: binds Hyperdrive + KV in a later slice.
/// Today it validates the route contract (MBID shape, cache headers) so
/// the API surface is reviewable before the DB wiring lands.
async fn lookup(kind: &str, _ctx: &RouteContext<Env>) -> Result<Response> {
    console_log!("mb lookup kind={}", kind);
    let mut headers = headers_json()?;
    headers.append("cache-control", &cache_control(LOOKUP_CACHE_TTL_S))?;
    Ok(Response::from_json(&serde_json::json!({
        "kind": kind,
        "status": "not_wired",
        "hint": "DB wiring lands in the next slice; contract is MBID lookup over Hyperdrive with KV cache.",
    }))?
    .with_headers(headers))
}

async fn search(req: &Request, _ctx: &RouteContext<Env>) -> Result<Response> {
    let url = req.url()?;
    let query: std::collections::HashMap<String, String> =
        url.query_pairs().into_owned().collect();
    let q = query.get("q").map(String::as_str).unwrap_or("").trim();
    if q.len() < 2 {
        return json_error("query too short (min 2 chars)", 400);
    }
    let kind = parse_search_kind(query.get("type").cloned());
    if kind.is_none() {
        return json_error("type must be one of artist|release|recording", 400);
    }
    let mut headers = headers_json()?;
    headers.append("cache-control", &cache_control(SEARCH_CACHE_TTL_S))?;
    Ok(Response::from_json(&serde_json::json!({
        "q": q,
        "type": kind,
        "results": [],
        "status": "not_wired",
    }))?
    .with_headers(headers))
}

#[worker::event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    Router::new()
        .get_async("/v1/mb/artist/:mbid", |_req, ctx| async move {
            lookup("artist", &ctx).await
        })
        .get_async("/v1/mb/release/:mbid", |_req, ctx| async move {
            lookup("release", &ctx).await
        })
        .get_async("/v1/mb/recording/:mbid", |_req, ctx| async move {
            lookup("recording", &ctx).await
        })
        .get_async("/v1/mb/search", |req, ctx| async move {
            search(&req, &ctx).await
        })
        .or_else_any_method_async("/*catchall", |_, _| async {
            json_error("not found", 404)
        })
        .run(req, env)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_kind_accepts_known_types() {
        assert_eq!(
            parse_search_kind(Some("artist".to_string())),
            Some("artist")
        );
        assert_eq!(
            parse_search_kind(Some("release".to_string())),
            Some("release")
        );
        assert_eq!(
            parse_search_kind(Some("recording".to_string())),
            Some("recording")
        );
    }

    #[test]
    fn search_kind_rejects_unknown_types() {
        assert_eq!(parse_search_kind(None), None);
        assert_eq!(parse_search_kind(Some("track".to_string())), None);
        assert_eq!(parse_search_kind(Some(String::new())), None);
    }

    #[test]
    fn cache_control_formats_ttl() {
        assert_eq!(cache_control(300), "public, max-age=300");
        assert_eq!(cache_control(86_400), "public, max-age=86400");
    }
}
