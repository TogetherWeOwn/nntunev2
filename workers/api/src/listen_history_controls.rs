use super::*;
use crate::listen_history::{ExportCursor, EXPORT_PAGE_SIZE};

pub(super) const EXPORT_PATH: &str = "/v1/plays/export";
const EXPORTS_PER_MINUTE: u64 = 120;
const CLEARS_PER_MINUTE: u64 = 6;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    before_id: Option<String>,
}

pub(super) async fn export<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Keep cursor values out of URLs and require the cookie's exact Origin.
    if req.authorization.is_some() {
        return not_found();
    }
    let (claims, _) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let Some(body) = parse_body::<ExportRequest>(req) else {
        return validation_failed("invalid request");
    };
    let before = match ExportCursor::parse(body.before_id.as_deref()) {
        Ok(cursor) => cursor,
        Err(message) => return validation_failed(message),
    };
    match ctx
        .store
        .reserve_rate_limit(
            &claims.user_id,
            "plays.export",
            EXPORTS_PER_MINUTE,
            ctx.now,
            60,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return throttled(60),
        Err(_) => return internal(),
    }
    match ctx
        .store
        .export_listen_events(&claims.user_id, before.as_ref())
        .await
    {
        Ok(mut rows) => {
            let has_more = rows.len() > EXPORT_PAGE_SIZE;
            rows.truncate(EXPORT_PAGE_SIZE);
            let next_cursor = if has_more {
                rows.last().map(|row| row.cursor())
            } else {
                None
            };
            json_response(
                200,
                serde_json::json!({
                    "events": rows, "next_cursor": next_cursor,
                    "retention": "account_lifetime",
                }),
            )
        }
        Err(_) => internal(),
    }
}

pub(super) async fn clear<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.authorization.is_some() {
        return not_found();
    }
    let (claims, _) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match ctx
        .store
        .reserve_rate_limit(
            &claims.user_id,
            "plays.clear",
            CLEARS_PER_MINUTE,
            ctx.now,
            60,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return throttled(60),
        Err(_) => return internal(),
    }
    match ctx.store.clear_listen_events(&claims.user_id).await {
        Ok(()) => json_response(200, serde_json::json!({"ok": true})),
        Err(_) => internal(),
    }
}
