//! NNTune v2 API worker: Subsonic-compatible surface on `/rest/*`.
//!
//! Transport-free dispatch lives in `nntune-subsonic` (unit + fixture tested
//! on native); this shell only adapts the worker runtime to it. The
//! production `Library` (PlanetScale via Hyperdrive, R2 audio/artwork via the
//! stream worker) plugs in behind the `Library` seam — until then the worker
//! uses `MemStore::empty()`, which holds no credentials and rejects every
//! auth attempt, rather than serving fabricated rows. Per-client `apiKey`
//! credentials will resolve against `subsonic_clients`
//! (`migrations/0001_subsonic_clients.sql`).

use nntune_subsonic::{oversized_body_response, MemStore, SubsonicApp, MAX_BODY_BYTES};
use worker::*;

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
        req.text().await.unwrap_or_default()
    } else {
        String::new()
    };

    let app = SubsonicApp::new(MemStore::empty());
    let res = app.handle(&method, &path, &query, &body);
    respond(&res)
}

fn respond(res: &nntune_subsonic::HttpResponse) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", &res.content_type)?;
    let mut response = Response::from_bytes(res.body.clone())?;
    response = response.with_status(res.status);
    *response.headers_mut() = headers;
    Ok(response)
}
