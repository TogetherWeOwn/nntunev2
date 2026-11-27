//! Feature-gated real-workerd observations for the Worker span regression.

#[cfg(target_arch = "wasm32")]
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

#[cfg(target_arch = "wasm32")]
use {
    futures_util::future::join_all,
    serde_json::{json, Value},
    worker::observability::{active_span, enter_span_async, in_current_span, start_span},
    worker::{event, Context, Delay, Env, Request, Response},
};

#[cfg(target_arch = "wasm32")]
const TASKS: usize = 8;
#[cfg(target_arch = "wasm32")]
static ACTIVE_SCENARIOS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "wasm32")]
struct ActiveScenario;
#[cfg(target_arch = "wasm32")]
impl Drop for ActiveScenario {
    fn drop(&mut self) {
        ACTIVE_SCENARIOS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(target_arch = "wasm32")]
fn ident() -> Option<String> {
    active_span().map(|span| span.span_context().span_id)
}

#[cfg(target_arch = "wasm32")]
async fn sample(idx: usize) -> Value {
    let before = ident();
    Delay::from(Duration::from_millis(3 + (idx % 3 * 4) as u64)).await;
    let mid = ident();
    Delay::from(Duration::from_millis(3 + ((idx + 1) % 3 * 4) as u64)).await;
    json!({"before": before, "mid": mid, "end": ident()})
}

#[cfg(target_arch = "wasm32")]
async fn task(idx: usize, scoped: bool) -> Value {
    let mut observation = if scoped {
        enter_span_async(&format!("span-probe-{idx}"), |_| sample(idx)).await
    } else {
        sample(idx).await
    };
    observation["after"] = json!(ident());
    observation
}

#[cfg(target_arch = "wasm32")]
async fn scenario(scoped: bool) -> Value {
    ACTIVE_SCENARIOS.fetch_add(1, Ordering::SeqCst);
    let _active = ActiveScenario;
    let root = ident();
    let tasks = join_all((0..TASKS).map(|idx| task(idx, scoped))).await;
    let overlapping = ACTIVE_SCENARIOS.load(Ordering::SeqCst) > 1;
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
        "root_after": root_after, "child_expected": child_expected, "overlapping": overlapping,
        "observations": {"root": root, "tasks": tasks, "child": child}
    })
}

fn probe_mode(method: &str, path: &str, query: Option<&str>) -> Result<bool, (u16, &'static str)> {
    if path != "/probe" {
        return Err((404, "not found"));
    }
    if method != "GET" {
        return Err((405, "method not allowed"));
    }
    match query {
        Some("mode=scoped") => Ok(true),
        Some("mode=unscoped") => Ok(false),
        _ => Err((400, "bad mode")),
    }
}

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
pub async fn span_probe(req: Request, _env: Env, _ctx: Context) -> worker::Result<Response> {
    let url = req.url()?;
    // The SDK's Method conversion treats unknown verbs as GET.
    let scoped = match probe_mode(&req.inner().method(), url.path(), url.query()) {
        Ok(scoped) => scoped,
        Err((status, message)) => {
            let mut response = Response::error(message, status)?;
            if status == 405 {
                response.headers_mut().set("Allow", "GET")?;
            }
            return Ok(response);
        }
    };
    let result = enter_span_async("span-probe-root", |_| scenario(scoped)).await;
    Response::from_json(&result)
}

#[cfg(test)]
mod tests {
    use super::probe_mode;

    #[test]
    fn probe_requires_exact_get_path_and_single_mode() {
        assert_eq!(probe_mode("GET", "/probe", Some("mode=scoped")), Ok(true));
        assert_eq!(
            probe_mode("GET", "/probe", Some("mode=unscoped")),
            Ok(false)
        );
        for method in ["POST", "HEAD", "OPTIONS", "get", "CUSTOM"] {
            assert!(matches!(
                probe_mode(method, "/probe", Some("mode=scoped")),
                Err((405, _))
            ));
        }
        assert!(matches!(
            probe_mode("GET", "/probe/", Some("mode=scoped")),
            Err((404, _))
        ));
        for query in [
            None,
            Some(""),
            Some("mode=scoped&mode=unscoped"),
            Some("mode=%73coped"),
        ] {
            assert!(matches!(probe_mode("GET", "/probe", query), Err((400, _))));
        }
    }
}
