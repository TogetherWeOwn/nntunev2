//! NNTune v2 API worker: Subsonic-compatible surface on `/rest/*`.
//!
//! Transport-free dispatch lives in `nntune-subsonic` (unit + fixture tested
//! on native); this shell only adapts the worker runtime to it. The
//! production `Library` (PlanetScale via Hyperdrive, R2 audio/artwork via the
//! stream worker) plugs in behind the `Library` seam — until then the worker
//! serves an empty catalogue rather than fabricated rows, and per-client
//! `apiKey` credentials resolve against `subsonic_clients`
//! (`migrations/0001_subsonic_clients.sql`).

use nntune_subsonic::{MemStore, SubsonicApp};
use worker::*;

#[event(fetch)]
pub async fn main(req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_string();
    let query = url.query().unwrap_or("").to_string();
    let method = req.method().to_string();

    let app = SubsonicApp::new(MemStore::empty());
    let res = app.handle(&method, &path, &query);

    let headers = Headers::new();
    headers.set("Content-Type", &res.content_type)?;
    let mut response = Response::from_bytes(res.body)?;
    response = response.with_status(res.status);
    *response.headers_mut() = headers;
    Ok(response)
}
