//! Dedicated service-binding credential channel; never session or bearer auth.

use super::*;
use crate::pool_resolver::{
    self as wire, AccountSnapshot, AuthError, DialMaterial, ResolveRequest,
};

pub(super) async fn handle<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.origin.is_some() || req.cookie.is_some() {
        return not_found();
    }
    match wire::verify(
        ctx.cfg.pool_resolver_secret.as_deref(),
        &req.path,
        req.authorization.as_deref(),
        &req.body,
        ctx.now,
    ) {
        Ok(()) => {}
        Err(AuthError::NotConfigured) => return internal(),
        Err(AuthError::Unauthorized) => return not_found(),
    }
    let object = req.body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{');
    let mut response = if !object {
        validation_failed("invalid request")
    } else if req.path == wire::REGISTRY_PATH {
        registry(ctx, req).await
    } else {
        resolve(ctx, req).await
    };
    // Neither HTTP caches nor browser tooling may retain dial material.
    response
        .extra_headers
        .push(("cache-control".into(), "no-store".into()));
    response
}

async fn registry<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if serde_json::from_slice::<wire::RegistryRequest>(&req.body).is_err() {
        return validation_failed("invalid request");
    }
    let rows = match ctx.store.pool_providers().await {
        Ok(rows) if rows.len() <= 1000 => rows,
        _ => return internal(),
    };
    if rows.iter().any(|row| {
        row.settings.validate().is_err()
            || !crate::pool_providers::lower_uuid_ok(&row.secret_ref)
            || row.revision == 0
            || row.revision > i64::MAX as u64
    }) {
        return internal();
    }
    let accounts: Vec<_> = rows.iter().map(AccountSnapshot::from).collect();
    json_response(200, serde_json::json!({"accounts": accounts}))
}

async fn resolve<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let request = match serde_json::from_slice::<ResolveRequest>(&req.body) {
        Ok(request) if request.valid() => request,
        _ => return validation_failed("invalid request"),
    };
    let stored = match ctx.store.pool_account(&request.provider_id).await {
        Ok(Some(stored)) if request.matches(&stored.row) => stored,
        Ok(_) => return unavailable(),
        Err(_) => return internal(),
    };
    let Some(kek) = ctx.cfg.kek_by_version(stored.kek_version) else {
        return unavailable();
    };
    let secret = match crate::pool_providers::decrypt_pool_secret(
        kek,
        stored.kek_version,
        &stored.row.secret_ref,
        &stored.envelope,
    ) {
        Ok(secret) => secret,
        Err(_) => return unavailable(),
    };
    let settings = &stored.row.settings;
    // Resolve all families once, bounded and cancel-safe. The returned peer is
    // exact: the runtime must not turn this back into a hostname socket dial.
    let addresses = if let Ok(ip) = settings.host.parse::<IpAddr>() {
        vec![ip]
    } else {
        resolve_bounded(ctx.dns, &settings.host).await
    };
    let target =
        match vault::authorize_pool_dial(&settings.host, settings.port, &StaticDns { addresses }) {
            Ok(target) => target,
            Err(_) => return unavailable(),
        };
    let Some(peer) = target.addresses().first() else {
        return unavailable();
    };
    let material = DialMaterial {
        provider_id: settings.id.clone(),
        revision: stored.row.revision,
        secret_ref: stored.row.secret_ref.clone(),
        host: settings.host.clone(),
        port: target.port(),
        tls: true,
        peer: peer.to_string(),
        expires_at: ctx.now.saturating_add(wire::DIAL_MATERIAL_TTL_SECS),
        username: settings.username.clone(),
        password: secret.password.clone(),
    };
    // Serialize directly: no credential-bearing serde_json::Value temporary.
    match serde_json::to_string(&material) {
        Ok(body) => Response {
            status: 200,
            body,
            set_cookie: None,
            retry_after: None,
            extra_headers: Vec::new(),
        },
        Err(_) => internal(),
    }
}
