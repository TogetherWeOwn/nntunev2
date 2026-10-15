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
/// `LOOKUP_CACHE_TTL_S` applies to the wired DB path; the current
/// not-wired stub below serves `no-store` so no placeholder is cached.
#[allow(dead_code)]
const LOOKUP_CACHE_TTL_S: u32 = 86_400;
const SEARCH_CACHE_TTL_S: u32 = 300;

/// Validate an MBID string (UUID shape `8-4-4-4-12` hex). Keeps malformed
/// `:mbid` values out of Hyperdrive SQL once the DB wiring lands.
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

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

fn json_error(message: &str, status: u16) -> Result<Response> {
    let body = serde_json::to_string(&ApiError {
        error: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"internal"}"#.to_string());
    Response::from_body(worker::ResponseBody::Body(body.into_bytes())).map(|r| {
        r.with_status(status)
            .with_headers(headers_json().unwrap_or_default())
    })
}

fn headers_json() -> Result<worker::Headers> {
    let mut headers = worker::Headers::new();
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
/// Today it validates the route contract (MBID shape) so the API surface
/// is reviewable before the DB wiring lands. The stub answers `501` with
/// `no-store` so neither browsers nor the edge cache the placeholder; the
/// long `LOOKUP_CACHE_TTL_S` max-age applies only once real rows return.
async fn lookup(kind: &str, ctx: &RouteContext<()>) -> Result<Response> {
    let mbid = ctx.param("mbid").map(String::as_str).unwrap_or("");
    if !is_valid_mbid(mbid) {
        return json_error("invalid mbid (must be UUID)", 400);
    }
    console_log!("mb lookup kind={}", kind);
    let mut headers = headers_json()?;
    headers.append("cache-control", "no-store")?;
    Ok(Response::from_json(&serde_json::json!({
        "kind": kind,
        "status": "not_wired",
        "hint": "DB wiring lands in the next slice; contract is MBID lookup over Hyperdrive with KV cache.",
    }))?
    .with_status(501)
    .with_headers(headers))
}

async fn search(req: &Request, _ctx: &RouteContext<()>) -> Result<Response> {
    let url = req.url()?;
    let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
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
        .or_else_any_method_async("/*catchall", |_, _| async { json_error("not found", 404) })
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
}
