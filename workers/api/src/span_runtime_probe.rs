//! Test-only Worker entry for the real-workerd span regression
//! (`.github/scripts/worker_span_runtime.py`). Compiled only for wasm32 with
//! the off-by-default `span-runtime-test` feature, which also removes the
//! production `main` entry from that build; the ordinary bundle never
//! contains this module or a `/probe` route.
//!
//! It drives the Worker 0.9 observability API directly:
//! `enter_span_async` (per-task span), `active_span` (identity sampling) and
//! `in_current_span` (a future created inside a separate live span but awaited
//! after leaving its scope). `mode=scoped` is the positive path; `mode=unscoped` is
//! the negative control that omits `enter_span_async`, i.e. no active-span
//! propagation, and must be rejected by the evaluator.
//!
//! The only place that depends on the exact shape of the Span value is
//! `ident()`; everything else is the public API surface under test.

use std::time::Duration;

use futures_util::future::join_all;
use serde_json::json;
use worker::observability::{active_span, enter_span_async, in_current_span, start_span};
use worker::{event, Context, Delay, Env, Request, Response, Result};

const TASKS: usize = 8;

/// Stable string identity of the currently active span (None at root).
fn ident() -> Option<String> {
    active_span().map(|span| span.span_context().span_id)
}

async fn pause(idx: usize, step: usize) {
    // Real workerd timer so the tasks genuinely interleave.
    Delay::from(Duration::from_millis(3 + ((idx + step) % 3 * 4) as u64)).await;
}

async fn body(idx: usize) -> serde_json::Value {
    let before = ident();
    pause(idx, 0).await;
    let mid = ident();
    pause(idx, 2).await;
    let end = ident();
    json!({ "before": before, "mid": mid, "end": end })
}

async fn scoped_task(idx: usize) -> serde_json::Value {
    let mut out = enter_span_async(&format!("span-probe-{idx}"), |_| body(idx)).await;
    // Back outside the task span: must be the root identity again.
    out["after_scope"] = json!(ident());
    let span = start_span(&format!("child-probe-{idx}"));
    let expected = span.span_context().span_id;
    let child = span.in_scope(|| in_current_span(async move {
        pause(idx, 1).await;
        ident()
    }));
    out["child_expected"] = json!(expected);
    out["child"] = json!(child.await);
    out["after_child"] = json!(ident());
    span.end();
    out
}

async fn unscoped_task(idx: usize) -> serde_json::Value {
    let mut out = body(idx).await;
    out["after_scope"] = json!(ident());
    let child = in_current_span(async move {
        pause(idx, 1).await;
        ident()
    });
    out["child"] = json!(child.await);
    out["after_child"] = json!(ident());
    out
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
    let root_before = ident();
    let tasks = if scoped {
        join_all((0..TASKS).map(scoped_task)).await
    } else {
        join_all((0..TASKS).map(unscoped_task)).await
    };
    Response::from_json(&json!({
        "mode": if scoped { "scoped" } else { "unscoped" },
        "root_before": root_before,
        "root_after": ident(),
        "tasks": tasks,
    }))
}
