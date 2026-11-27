//! Prepared request handlers only. The production router does not call them.

use super::*;
use crate::listen_history::{ListenEvent, ListenEventBody, WriteOutcome};

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

pub(super) async fn write<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.body.len() > MAX_BODY_BYTES {
        return invalid_request();
    }
    // Select one auth mode before parsing. A presented bearer is never
    // rescued by a cookie; native lookup budgets stay separate from history.
    let (owner, claims) = if req.authorization.is_some() {
        match authorize_bearer(
            ctx,
            req.authorization.as_deref(),
            crate::native_rest::Scope::PlaysWrite,
        )
        .await
        {
            Ok(row) => (row.record.user_id, None),
            Err(resp) => return resp,
        }
    } else {
        match ctx.authenticate(req, true).await {
            Ok((claims, _)) => (claims.user_id.clone(), Some(claims)),
            Err(resp) => return resp,
        }
    };
    if req.body.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'{') {
        return validation_failed("invalid request");
    }
    let Some(body) = parse_body::<EventRequest>(req) else {
        return validation_failed("invalid request");
    };
    if let Some(claims) = claims {
        if !session::verify_listen_context(
            &ctx.cfg.session_keys(),
            &claims,
            body.context.as_deref(),
        ) {
            return generic_auth();
        }
    } else if body.context.is_some() {
        return validation_failed("unexpected history context");
    }
    let event = ListenEventBody {
        playback_id: body.playback_id,
        track_id: body.track_id,
        event: body.event,
    };
    if !event.valid_identity() {
        return validation_failed("invalid history event");
    }
    match ctx
        .store
        .reserve_rate_limit(&owner, "plays.history", 120, ctx.now, 60)
        .await
    {
        Ok(true) => {}
        Ok(false) => return throttled(60),
        Err(_) => return internal(),
    }
    match ctx.store.record_listen_event(&owner, &event, ctx.now).await {
        Ok(WriteOutcome::Written) => json_response(200, serde_json::json!({"ok": true})),
        Ok(WriteOutcome::AccountDeleting) => account_deleting_response(),
        Ok(WriteOutcome::Conflict) => {
            let error = crate::api_error::ApiError::by_code(
                "conflict",
                "history event conflicts with playback",
            );
            json_response(error.status, serde_json::json!(error))
        }
        Err(_) => internal(),
    }
}
