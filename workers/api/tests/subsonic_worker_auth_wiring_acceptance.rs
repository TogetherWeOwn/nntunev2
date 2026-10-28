//! Narrow SOURCE-SHAPE wiring tripwires, not a Worker/runtime integration test.
//! Native policy and SQL-plan tests cannot prove a wasm route consumes them.
//! These fail on the current unconfigured route. No config values, credentials,
//! database, Cloudflare, staging, or production services are accessed here.

const ENTRY: &str = include_str!("../src/lib.rs");
const STORE: &str = include_str!("../src/worker_entry.rs");

fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}("))
        .expect("production function must exist");
    let tail = &source[start..];
    let brace = tail.find('{').expect("function body");
    let mut depth = 0;
    for (offset, ch) in tail[brace..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return &tail[..=brace + offset];
        }
    }
    panic!("unterminated production function");
}

fn without_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn worker_compat_route_receives_env_and_prepares_the_same_native_tested_auth() {
    let route = without_line_comments(function_body(ENTRY, "subsonic_response"));
    let (signature, body) = route.split_once('{').unwrap();
    assert!(
        signature.contains("Env"),
        "compat route cannot resolve required store/keyring/limiter bindings without Env"
    );
    assert!(
        body.contains("prepare_subsonic_auth("),
        "native-tested versioned preparation must feed the Worker route"
    );
    assert!(
        !body.contains("MemStore::fixtures"),
        "fixture authentication must never enter runtime"
    );
    assert!(body.contains("SubsonicApp::new("));
}

#[test]
fn production_pg_adapter_uses_the_tested_query_plans_instead_of_legacy_sql() {
    let store = without_line_comments(STORE);
    // These application-domain methods are required by the independently
    // specified native store port. Require each driver path to execute the
    // corresponding production SQL plan; token/comment presence elsewhere is
    // insufficient. This is intentionally a shape pin, not SQL execution.
    for (method, plan) in [
        ("clients_by_candidates", "subsonic_lookup_query("),
        ("cas_pepper", "subsonic_rehmac_query("),
        ("mark_last_used", "subsonic_last_used_query("),
        ("unrevoked_for_pepper", "subsonic_unrevoked_pepper_query("),
    ] {
        let body = function_body(&store, method);
        assert!(
            body.contains(plan),
            "Worker storage bypasses the native-tested SQL plan"
        );
        assert!(
            !body.contains("subsonic_clients"),
            "inert legacy table must not become authority"
        );
    }
}
