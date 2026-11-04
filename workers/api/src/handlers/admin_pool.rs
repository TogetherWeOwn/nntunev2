//! Session-only service account management. No provider credential is served
//! back to the operator; writes use a fresh session and the store's role fence.

use super::*;
use crate::pool_providers::{
    self, PoolProviderSettings, PoolProviderWrite, PoolProviderWriteOutcome, PoolSecretWrite,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
struct Password(String);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteBody {
    settings: PoolProviderSettings,
    expected_revision: u64,
    password: Option<Password>,
}

pub(super) async fn list<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if let Err(resp) = ctx.authenticate_admin(req, false).await {
        return resp;
    }
    match ctx.store.pool_providers().await {
        Ok(rows) => json_response(
            200,
            serde_json::json!({"providers": rows.iter().map(|row| row.view()).collect::<Vec<_>>()}),
        ),
        Err(_) => internal(),
    }
}

pub(super) async fn write<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let claims = match ctx.authenticate_admin(req, true).await {
        Ok(claims) => claims,
        Err(resp) => return resp,
    };
    let create = req.method == "POST";
    let path_id = if create {
        None
    } else {
        match req.path.strip_prefix("/v1/admin/pool-providers/") {
            Some(id) if pool_providers::lower_uuid_ok(id) => Some(id),
            _ => return not_found(),
        }
    };
    // Deserialize the credential into a zeroizing holder, not an ordinary
    // request-body String clone. Request transport owns the original bytes.
    let Some(body) = parse_body::<WriteBody>(req) else {
        return validation_failed("invalid request");
    };
    if body.settings.validate().is_err()
        || (create && body.expected_revision != 0)
        || (!create && body.expected_revision == 0)
        || path_id.is_some_and(|id| id != body.settings.id)
        || body.expected_revision >= i64::MAX as u64
        || (create && body.password.is_none())
    {
        return validation_failed("invalid pool provider");
    }
    let secret = if let Some(password) = body.password {
        let secret = pool_providers::PoolProviderSecret {
            password: password.0.clone(),
        };
        if secret.validate().is_err() {
            return validation_failed("invalid pool provider");
        }
        let secret_ref = match providers::mint_provider_id() {
            Ok(id) => id,
            Err(_) => return unavailable(),
        };
        let Some(kek) = ctx.cfg.kek_by_version(ctx.cfg.kek_version) else {
            return internal();
        };
        let envelope = match pool_providers::encrypt_pool_secret(
            kek,
            ctx.cfg.kek_version,
            &secret_ref,
            &secret,
        ) {
            Ok(envelope) => envelope,
            Err(vault::VaultError::RandomnessUnavailable) => return unavailable(),
            Err(_) => return internal(),
        };
        Some(PoolSecretWrite {
            secret_ref,
            kek_version: ctx.cfg.kek_version,
            envelope,
        })
    } else {
        None
    };
    let write = PoolProviderWrite {
        settings: body.settings,
        expected_revision: body.expected_revision,
        secret,
    };
    match ctx
        .store
        .admin_write_pool_provider(&claims.user_id, &write, &ctx.source_ip, ctx.now)
        .await
    {
        Ok(PoolProviderWriteOutcome::Applied(row)) => json_response(
            if create { 201 } else { 200 },
            serde_json::json!({"provider": row.view()}),
        ),
        Ok(PoolProviderWriteOutcome::ActorNotAdmin) => not_found(),
        Ok(PoolProviderWriteOutcome::Conflict) => {
            let error =
                crate::api_error::ApiError::by_code("conflict", "pool provider revision conflict");
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
