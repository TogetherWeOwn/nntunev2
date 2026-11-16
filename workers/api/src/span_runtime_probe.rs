//! Feature-gated real-workerd observations for the Worker span regression.
use std::time::Duration;

use futures_util::future::join_all;
use serde_json::{json, Value};
use worker::observability::{active_span, enter_span_async, in_current_span, start_span};
use worker::{event, Context, Delay, Env, Request, Response, Result};

const TASKS: usize = 8;

fn ident() -> Option<String> {
    active_span().map(|span| span.span_context().span_id)
}

async fn sample(idx: usize) -> Value {
    let before = ident();
    Delay::from(Duration::from_millis(3 + (idx % 3 * 4) as u64)).await;
    let mid = ident();
    Delay::from(Duration::from_millis(3 + ((idx + 1) % 3 * 4) as u64)).await;
    json!({"before": before, "mid": mid, "end": ident()})
}

async fn task(idx: usize, scoped: bool) -> Value {
    let mut observation = if scoped {
        enter_span_async(&format!("span-probe-{idx}"), |_| sample(idx)).await
    } else {
        sample(idx).await
    };
    observation["after"] = json!(ident());
    observation
}

async fn scenario(scoped: bool) -> Value {
    let root = ident();
    let tasks = join_all((0..TASKS).map(|idx| task(idx, scoped))).await;
    let root_after = ident();
    let (mut child, child_expected) = if scoped {
        let span = start_span("child-probe");
        let expected = span.span_context().span_id;
        let future = span.in_scope(|| in_current_span(sample(TASKS)));
        let result = future.await;
        span.end();
        (result, Some(expected))
    } else {
        (in_current_span(sample(TASKS)).await, None)
    };
    child["after"] = json!(ident());
    json!({
        "mode": if scoped { "scoped" } else { "unscoped" },
        "root_after": root_after, "child_expected": child_expected,
        "observations": {"root": root, "tasks": tasks, "child": child}
    })
}

#[event(fetch)]
pub async fn span_probe(req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    if url.path() != "/probe" {
        return Response::error("not found", 404);
    }
    let scoped = match url.query() {
        Some("mode=scoped") => true,
        Some("mode=unscoped") => false,
        _ => return Response::error("bad mode", 400),
    };
    let result = enter_span_async("span-probe-root", |_| scenario(scoped)).await;
    Response::from_json(&result)
}
