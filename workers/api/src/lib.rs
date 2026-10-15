//! NNTune v2 API: accounts/auth/session/vault core and Subsonic Worker shell.
//!
//! The auth, session and vault modules are native + wasm32 compatible. Their
//! HTTP + Hyperdrive wiring remains a follow-up. The wasm32 entry adapts
//! `/rest/*` to the transport-free `nntune-subsonic` dispatcher.
//!
//! The production `Library` (PlanetScale via Hyperdrive, R2 audio/artwork via
//! the stream worker) plugs in behind the `Library` seam. Until then the
//! Worker uses `MemStore::empty()`, which holds no credentials and rejects
//! every auth attempt. Per-client `apiKey` credentials will resolve against
//! `subsonic_clients` (`migrations/0001_subsonic_clients.sql`).

pub mod auth;
pub mod session;
pub mod vault;

#[cfg(target_arch = "wasm32")]
use futures_util::StreamExt;
#[cfg(target_arch = "wasm32")]
use nntune_subsonic::{oversized_body_response, MemStore, SubsonicApp, MAX_BODY_BYTES};
#[cfg(target_arch = "wasm32")]
use worker::*;

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
pub async fn main(mut req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_string();
    let query = url.query().unwrap_or("").to_string();
    let method = req.method().to_string();

    // v1 reads params from query plus POST form body (`$request->input()`).
    let content_type = req
        .headers()
        .get("content-type")
        .unwrap_or_default()
        .unwrap_or_default();
    // v1 `MergeFormPostParams` refuses bodies over 10 MB on `Content-Length`
    // before buffering: answer error 10 without reading the body at all.
    if method.eq_ignore_ascii_case("POST")
        && content_type.contains("application/x-www-form-urlencoded")
        && req
            .headers()
            .get("content-length")
            .unwrap_or_default()
            .unwrap_or_default()
            .parse::<usize>()
            .is_ok_and(|declared| declared > MAX_BODY_BYTES)
    {
        let res = oversized_body_response(&query);
        return respond(&res);
    }
    let body = if method.eq_ignore_ascii_case("POST")
        && content_type.contains("application/x-www-form-urlencoded")
    {
        let mut stream = match req.stream() {
            Ok(stream) => stream,
            Err(_) => return Response::error("Unable to read request body", 400),
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => return Response::error("Unable to read request body", 400),
            };
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                let res = oversized_body_response(&query);
                return respond(&res);
            }
            bytes.extend_from_slice(&chunk);
        }
        match String::from_utf8(bytes) {
            Ok(body) => body,
            Err(_) => return Response::error("Invalid UTF-8 request body", 400),
        }
    } else {
        String::new()
    };

    let app = SubsonicApp::new(MemStore::empty());
    let res = app.handle(&method, &path, &query, &body);
    respond(&res)
}

#[cfg(target_arch = "wasm32")]
fn respond(res: &nntune_subsonic::HttpResponse) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", &res.content_type)?;
    let mut response = Response::from_bytes(res.body.clone())?;
    response = response.with_status(res.status);
    *response.headers_mut() = headers;
    Ok(response)
}
