//! Unmounted app-to-ingest review retry.

use super::{
    invalid_request, not_found, unavailable, AsyncDns, Ctx, Dialer, Mailer, Request, Response,
    SingleUseStore, Store,
};
use crate::ingest_grant;
use serde::Deserialize;

pub(super) struct RetryPost {
    pub path: String,
    pub authorization: String,
    pub body: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryRequest {
    revision: u32,
}

/// Post JSON only to the configured ingest service, with redirects disabled.
/// The transport returns a bounded, redacted review result or a fixed error.
pub(super) async fn retry<S, M, D, N, T>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    secret: Option<&str>,
    send: impl AsyncFnOnce(RetryPost) -> T,
) -> Result<T, Response>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let job_id = req
        .path
        .strip_prefix("/v1/admin/ingest/review/")
        .and_then(|path| path.strip_suffix("/retry"))
        .filter(|id| {
            (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .filter(|_| req.method == "POST")
        .ok_or_else(not_found)?;
    let claims = ctx.authenticate_admin(req, true).await?;
    if req.body.len() > 128 || req.body.trim_ascii_start().first() != Some(&b'{') {
        return Err(invalid_request());
    }
    let request: RetryRequest = serde_json::from_slice(&req.body).map_err(|_| invalid_request())?;
    let grant =
        ingest_grant::issue_scoped_at(&claims.user_id, "ingest:review:retry", ctx.now, 60, secret)
            .map_err(|_| unavailable())?;
    Ok(send(RetryPost {
        path: format!("/v1/ingest/review/{job_id}/retry"),
        authorization: ingest_grant::bearer(&grant),
        body: format!(r#"{{"revision":{}}}"#, request.revision),
    })
    .await)
}
