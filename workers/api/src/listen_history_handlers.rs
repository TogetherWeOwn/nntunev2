use super::*;
use crate::listen_history::{
    ExportCursor, ListenEvent, ListenEventBody, WriteOutcome, EVENTS_PER_MINUTE, EXPORT_PAGE_SIZE,
};

pub(super) const CONTEXT_PATH: &str = "/v1/plays/events/context";
pub(super) const EXPORT_PATH: &str = "/v1/plays/export";
const EXPORTS_PER_MINUTE: u64 = 120;
const CLEARS_PER_MINUTE: u64 = 6;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    before_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventRequest {
    playback_id: String,
    track_id: String,
    event: ListenEvent,
    context: Option<String>,
}

pub(super) async fn context<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.authorization.is_some() {
        return not_found();
    }
    let (claims, _) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let binding = match session::listen_context(&ctx.cfg.session_secret, &claims) {
        Ok(binding) => binding,
        Err(err) => return session_error_response(err),
    };
    let mut resp = json_response(200, serde_json::json!({"context": binding}));
    resp.extra_headers
        .push(("cache-control".into(), "private, no-store".into()));
    resp
}

pub(super) async fn write<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    bearer: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = if bearer.is_none() {
        match ctx.authenticate(req, true).await {
            Ok((claims, _)) => Some(claims),
            Err(resp) => return resp,
        }
    } else {
        None
    };
    let Some(body) = parse_body::<EventRequest>(req) else {
        return validation_failed("invalid request");
    };
    let owner = match (bearer, claims.as_ref()) {
        (None, Some(claims))
            if session::verify_listen_context(
                &ctx.cfg.session_keys(),
                claims,
                body.context.as_deref(),
            ) =>
        {
            claims.user_id.as_str()
        }
        (Some(owner), _) if body.context.is_none() => owner,
        (Some(_), _) => return validation_failed("unexpected history context"),
        _ => return generic_auth(),
    };
    let event = ListenEventBody {
        playback_id: body.playback_id,
        track_id: body.track_id,
        event: body.event,
    };
    if !event.valid() {
        return validation_failed("invalid history event");
    }
    match ctx
        .store
        .reserve_rate_limit(owner, "plays.history", EVENTS_PER_MINUTE, ctx.now, 60)
        .await
    {
        Ok(true) => {}
        Ok(false) => return throttled(60),
        Err(_) => return internal(),
    }
    match ctx.store.record_listen_event(owner, &event, ctx.now).await {
        Ok(WriteOutcome::Written) => json_response(200, serde_json::json!({"ok": true})),
        Ok(WriteOutcome::AccountDeleting) => account_deleting_response(),
        Ok(WriteOutcome::Conflict) => {
            let error = crate::api_error::ApiError::by_code(
                "conflict",
                "history event conflicts with playback",
            );
            Response {
                status: error.status,
                body: error.body_json(),
                set_cookie: None,
                retry_after: None,
                extra_headers: Vec::new(),
            }
        }
        Err(_) => internal(),
    }
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
