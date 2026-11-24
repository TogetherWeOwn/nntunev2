use super::*;

pub(super) const CHECKOUT: &str = "/v1/billing/checkout";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Checkout {
    plan_id: String,
}

pub(super) async fn browser_purchase<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    expected_session_binding: Option<&str>,
) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let (claims, _) = match preference_authenticate(ctx, req, true).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if let Some(expected) = expected_session_binding {
        if let Err(response) = require_session_binding(&claims, Some(expected)) {
            return response;
        }
    }
    if ctx.cfg.stripe_secret_key.is_none() || ctx.cfg.stripe_webhook_secret.is_none() {
        return unavailable();
    }
    let Ok(Checkout { plan_id }) = serde_json::from_slice(&req.body) else {
        return invalid_request();
    };
    let input = match ctx.store.load_reconcile_input(&claims.user_id, None).await {
        Ok(input) => input,
        Err(_) => return unavailable(),
    };
    let mut matches = input
        .bindings
        .iter()
        .filter(|binding| binding.plan_id == plan_id && binding.selectable && !binding.livemode);
    let Some(price) = matches.next() else {
        return invalid_request();
    };
    if matches.next().is_some() {
        return unavailable();
    }
    let return_url = format!("{}/account/billing", ctx.cfg.origin);
    let result = ensure_checkout_session(
        ctx,
        &claims.user_id,
        &price.price_id,
        &return_url,
        &return_url,
        None,
    )
    .await;
    match result {
        Ok(url) if approved_redirect(&url, "checkout.stripe.com") => {
            let mut response = json_response(200, serde_json::json!({"url": url}));
            response
                .extra_headers
                .push(("cache-control".into(), "private, no-store".into()));
            response
        }
        Err(EnsureCheckoutError::Unavailable) => {
            json_response(409, serde_json::json!({"error": "billing unavailable"}))
        }
        _ => unavailable(),
    }
}

fn approved_redirect(value: &str, host: &str) -> bool {
    value.starts_with("https://")
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == b'\\')
        && url::Url::parse(value).is_ok_and(|url| {
            url.host_str() == Some(host)
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
        })
}
