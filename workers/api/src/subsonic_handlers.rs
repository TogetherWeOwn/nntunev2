//! Native-session-owned compatibility credential lifecycle; never a /rest authorizer.
use super::*;
use crate::subsonic_auth::AuthMode;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MintBody {
    label: String,
    #[serde(default = "default_mode")]
    auth_mode: String,
}

fn default_mode() -> String {
    "apikey".to_string()
}

fn private(mut response: Response) -> Response {
    response
        .extra_headers
        .push(("cache-control".to_string(), "private, no-store".to_string()));
    response
}

pub(super) async fn mint<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(response) => return private(response),
    };
    let Some(body) = parse_body::<MintBody>(req) else {
        return private(validation_failed("invalid request"));
    };
    let mode = match body.auth_mode.as_str() {
        "apikey" => AuthMode::ApiKey,
        "token" => AuthMode::Token,
        "both" => AuthMode::Both,
        _ => return private(validation_failed("invalid auth mode")),
    };
    let Some(index) = ctx.cfg.subsonic_index.as_ref() else {
        return private(unavailable());
    };
    let issued = match crate::subsonic_lifecycle::issue(
        &claims.user_id,
        &body.label,
        mode,
        ctx.now,
        &index.current_pepper(),
        ctx.cfg.kek_version,
        &ctx.cfg.kek,
    ) {
        Ok(issued) => issued,
        Err(crate::subsonic_lifecycle::LifecycleError::Invalid) => {
            return private(validation_failed("invalid request"))
        }
        Err(_) => return private(unavailable()),
    };
    match ctx
        .store
        .insert_subsonic_credential(&issued.prepared, &claims)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return private(match ctx.store.account_deleting(&claims.user_id).await {
                Ok(true) => account_deleting_response(),
                Ok(false) => validation_failed("credential limit reached"),
                Err(_) => internal(),
            });
        }
        Err(_) => return private(internal()),
    }
    let record = &issued.prepared.record;
    ctx.audit(
        &claims.user_id,
        &format!("subsonic_credential_created:{}", record.id),
        true,
    )
    .await;
    // The one-time strings never cross the persistence boundary. Listing omits
    // even the generated username. A failed commit can never reach this body.
    private(json_response(
        201,
        serde_json::json!({
            "id": record.id,
            "username": record.username,
            "auth_mode": body.auth_mode,
            "api_key": issued.api_key.as_ref().map(|s| s.as_str()),
            "token_secret": issued.token_secret.as_ref().map(|s| s.as_str()),
        }),
    ))
}

pub(super) async fn list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _) = match ctx.authenticate(req, false).await {
        Ok(pair) => pair,
        Err(response) => return private(response),
    };
    private(
        match ctx
            .store
            .subsonic_credentials_for_user(&claims.user_id)
            .await
        {
            Ok(rows) => json_response(200, serde_json::json!({"credentials": rows})),
            Err(_) => internal(),
        },
    )
}

pub(super) async fn revoke<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(response) => return private(response),
    };
    let id = req
        .path
        .strip_prefix("/v1/subsonic-credentials/")
        .unwrap_or("");
    if !crate::meter_sink::is_uuid_shaped(id) {
        return private(not_found());
    }
    let result = ctx
        .store
        .revoke_subsonic_credential(id, &claims.user_id, ctx.now)
        .await;
    private(match result {
        Ok(true) => {
            ctx.audit(
                &claims.user_id,
                &format!("subsonic_credential_revoked:{id}"),
                true,
            )
            .await;
            json_response(200, serde_json::json!({"ok": true}))
        }
        Ok(false) => not_found(),
        Err(_) => internal(),
    })
}
