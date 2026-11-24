use super::*;

pub(super) const CHECKOUT: &str = "/v1/billing/checkout";
pub(super) const PORTAL: &str = "/v1/billing/portal";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Portal {}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Checkout {
    plan_id: String,
}

// Keep checkout's large future off the shared dispatcher's stack frame.
#[inline(never)]
pub(super) fn browser_purchase<'a, S, M, D, N>(
    ctx: &'a Ctx<'_, S, M, D, N>,
    req: &'a Request,
    expected_session_binding: Option<&'a str>,
) -> Pin<Box<dyn Future<Output = Response> + 'a>>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    Box::pin(dispatch_purchase(ctx, req, expected_session_binding))
}

async fn dispatch_purchase<S, M, D, N>(
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
    if !ctx.cfg.billing_purchase_configured() {
        return unavailable();
    }
    let return_url = format!("{}/account/billing", ctx.cfg.origin);
    if req.path == PORTAL {
        if serde_json::from_slice::<Portal>(&req.body).is_err() {
            return invalid_request();
        }
        let Some(configuration) = ctx.cfg.stripe_portal_configuration.as_deref() else {
            return unavailable();
        };
        return portal_for(ctx, &claims.user_id, configuration, &return_url).await;
    }
    let Ok(Checkout { plan_id }) = serde_json::from_slice(&req.body) else {
        return invalid_request();
    };
    let input = match ctx.store.load_reconcile_input(&claims.user_id, None).await {
        Ok(input) => input,
        Err(_) => return unavailable(),
    };
    let Some(price) = selectable_test_price(&input.bindings, &plan_id) else {
        return invalid_request();
    };
    let result =
        ensure_checkout_session(ctx, &claims.user_id, price, &return_url, &return_url, None).await;
    match result {
        Ok(url) => redirect_response(url, "checkout.stripe.com"),
        Err(EnsureCheckoutError::Unavailable) => {
            json_response(409, serde_json::json!({"error": "billing unavailable"}))
        }
        _ => unavailable(),
    }
}

async fn portal_for<S: Store + SingleUseStore, M: Mailer, D: Dialer, N: AsyncDns>(
    ctx: &Ctx<'_, S, M, D, N>,
    user_id: &str,
    configuration: &str,
    return_url: &str,
) -> Response {
    let input = match ctx.store.load_reconcile_input(user_id, None).await {
        Ok(input) => input,
        Err(_) => return unavailable(),
    };
    let Some(customer) = input.stripe_customer_id else {
        return json_response(409, serde_json::json!({"error": "billing unavailable"}));
    };
    match ctx
        .stripe
        .create_portal_session(&customer, configuration, return_url)
        .await
    {
        Ok(session) if !session.livemode && session.customer == customer => {
            redirect_response(session.url, "billing.stripe.com")
        }
        _ => unavailable(),
    }
}

pub(super) fn selectable_test_price<'a>(
    bindings: &'a [crate::billing_reconcile::PriceBinding],
    plan: &str,
) -> Option<&'a str> {
    if crate::plan_limits::PlanId::parse_id(plan)? == crate::plan_limits::PlanId::Free {
        return None;
    }
    let mut eligible = bindings
        .iter()
        .filter(|row| row.plan_id == plan && row.selectable && !row.livemode);
    let price = eligible.next()?.price_id.as_str();
    eligible.next().is_none().then_some(price)
}

fn redirect_response(url: String, host: &str) -> Response {
    if !approved_redirect(&url, host) {
        return unavailable();
    }
    let mut response = json_response(200, serde_json::json!({"url": url}));
    response
        .extra_headers
        .push(("cache-control".into(), "private, no-store".into()));
    response
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
