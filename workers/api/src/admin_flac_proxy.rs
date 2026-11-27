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

fn retry_revision(body: &[u8]) -> Option<u32> {
    if body.len() > 128 || body.trim_ascii_start().first() != Some(&b'{') {
        return None;
    }
    let request: RetryRequest = serde_json::from_slice(body).ok()?;
    // The ingest retry must be able to advance the revision.
    request.revision.checked_add(1).map(|_| request.revision)
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
        .filter(|id| crate::ledger::is_valid_key_field(id, 64))
        .filter(|_| req.method == "POST")
        .ok_or_else(not_found)?;
    let claims = ctx.authenticate_admin(req, true).await?;
    let revision = retry_revision(&req.body).ok_or_else(invalid_request)?;
    let grant =
        ingest_grant::issue_scoped_at(&claims.user_id, "ingest:review:retry", ctx.now, 60, secret)
            .map_err(|_| unavailable())?;
    Ok(send(RetryPost {
        path: format!("/v1/ingest/review/{job_id}/retry"),
        authorization: ingest_grant::bearer(&grant),
        body: format!(r#"{{"revision":{revision}}}"#),
    })
    .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_payload_requires_a_bounded_object_and_advanceable_revision() {
        assert_eq!(
            retry_revision(br#" {"revision":4294967294}"#),
            Some(u32::MAX - 1)
        );
        for body in ["[4]", r#"{"revision":4294967295}"#] {
            assert_eq!(retry_revision(body.as_bytes()), None);
        }
    }
}
