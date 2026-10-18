//! Fail-closed HTTP routing decisions for the stream Worker.
//!
//! Portable core (no `worker` dependency) so native `cargo test` pins the
//! exact statuses the wasm32-only [`crate::runtime`] adapter renders: the
//! Worker's public fetch path stays `404` until the play adapters are wired,
//! and the pool probe answers `404` off-route, `400` without an owner-named
//! instance, and retryable `503` until provider configuration is wired from
//! the account vault. A status change here (including the `503` → `200`
//! mutant) turns the native suite red long before deploy.

/// Durable Object class name. Must match `class_name` in
/// `workers/stream/wrangler.toml` and the `pub struct` in `src/runtime.rs`
/// (pinned by `do_class_name_matches_wrangler_and_runtime`).
pub const DO_CLASS_NAME: &str = "NntpPool";

/// Probe route: method and path the pool Durable Object serves.
pub const PROBE_METHOD: &str = "POST";
/// Probe route: method and path the pool Durable Object serves.
pub const PROBE_PATH: &str = "/probe";
/// Body route: path the pool Durable Object serves (same `POST` method).
pub const BODY_PATH: &str = "/body";

/// Public Worker fetch decision until the play adapters are wired.
pub const PUBLIC_FETCH_STATUS: u16 = 404;
/// Public Worker fetch decision until the play adapters are wired.
pub const PUBLIC_FETCH_BODY: &str = "not found";

/// Off-route (or wrong-method) pool request.
pub const NOT_FOUND_STATUS: u16 = 404;
/// Off-route (or wrong-method) pool request.
pub const NOT_FOUND_BODY: &str = "not found";
/// Probe on an instance without an owner name (`id_from_name(user_id)`).
pub const OWNER_MISSING_STATUS: u16 = 400;
/// Probe on an instance without an owner name (`id_from_name(user_id)`).
pub const OWNER_MISSING_BODY: &str = r#"{"error":"bad_request"}"#;
/// Probe on a named instance before provider wiring lands (retryable).
pub const PROVIDER_UNAVAILABLE_STATUS: u16 = 503;
/// Probe on a named instance before provider wiring lands (retryable).
pub const PROVIDER_UNAVAILABLE_BODY: &str = r#"{"error":"provider_unavailable"}"#;

/// Adapter action for one Durable Object request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAction {
    /// Return the supplied portable route decision without running a probe.
    Respond(ProbeDecision),
    /// Run request validation and probe execution.
    RunProbe,
}

/// Pool probe outcome for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeDecision {
    /// Wrong method or path: no such route.
    NotFound,
    /// Right route but the instance has no owner name.
    OwnerMissing,
    /// Right route, named instance, provider wiring not yet present.
    ProviderUnavailable,
}

impl ProbeDecision {
    /// HTTP status the adapter must render for this outcome.
    pub fn status(self) -> u16 {
        match self {
            ProbeDecision::NotFound => NOT_FOUND_STATUS,
            ProbeDecision::OwnerMissing => OWNER_MISSING_STATUS,
            ProbeDecision::ProviderUnavailable => PROVIDER_UNAVAILABLE_STATUS,
        }
    }

    /// Response body the adapter must render for this outcome.
    pub fn body(self) -> &'static str {
        match self {
            ProbeDecision::NotFound => NOT_FOUND_BODY,
            ProbeDecision::OwnerMissing => OWNER_MISSING_BODY,
            ProbeDecision::ProviderUnavailable => PROVIDER_UNAVAILABLE_BODY,
        }
    }
}

/// Adapter action for one Durable Object request, across both pool routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolAction {
    /// Return the supplied portable route decision without running anything.
    Respond(ProbeDecision),
    /// Run request validation and probe execution (`POST /probe`).
    RunProbe,
    /// Run request validation and body execution (`POST /body`).
    RunBody,
}

/// Decide what one Durable Object request runs: the probe route, the body
/// route, or a portable refusal. Exact method+path only, and a right-route
/// request on an owner-less instance is refused before any dial.
pub fn decide_pool_action(method: &str, path: &str, has_owner_name: bool) -> PoolAction {
    if method != PROBE_METHOD || (path != PROBE_PATH && path != BODY_PATH) {
        return PoolAction::Respond(ProbeDecision::NotFound);
    }
    if !has_owner_name {
        return PoolAction::Respond(ProbeDecision::OwnerMissing);
    }
    if path == BODY_PATH {
        PoolAction::RunBody
    } else {
        PoolAction::RunProbe
    }
}

/// Decide whether the adapter returns a route response or executes a probe.
pub fn decide_probe_action(method: &str, path: &str, has_owner_name: bool) -> ProbeAction {
    match decide_probe(method, path, has_owner_name) {
        ProbeDecision::NotFound => ProbeAction::Respond(ProbeDecision::NotFound),
        ProbeDecision::OwnerMissing => ProbeAction::Respond(ProbeDecision::OwnerMissing),
        ProbeDecision::ProviderUnavailable => ProbeAction::RunProbe,
    }
}

/// Dispatch the portable route action through the adapter's response or probe handler.
pub async fn dispatch_probe<T, F, Fut>(
    action: ProbeAction,
    respond: impl FnOnce(ProbeDecision) -> T,
    run_probe: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    match action {
        ProbeAction::Respond(decision) => respond(decision),
        ProbeAction::RunProbe => run_probe().await,
    }
}

/// Decide the pool probe outcome from the request method/path and whether
/// the Durable Object instance carries an owner name.
pub fn decide_probe(method: &str, path: &str, has_owner_name: bool) -> ProbeDecision {
    if method != PROBE_METHOD || path != PROBE_PATH {
        return ProbeDecision::NotFound;
    }
    if !has_owner_name {
        return ProbeDecision::OwnerMissing;
    }
    ProbeDecision::ProviderUnavailable
}

/// Decide the public Worker fetch response until the play adapters are wired.
pub fn decide_public_fetch() -> (u16, &'static str) {
    (PUBLIC_FETCH_STATUS, PUBLIC_FETCH_BODY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_line(text: &str, line: &str) -> bool {
        text.lines().any(|candidate| candidate.trim() == line)
    }

    fn non_doc_attributes(attrs: &[syn::Attribute]) -> Vec<String> {
        attrs
            .iter()
            .filter(|attribute| !attribute.path().is_ident("doc"))
            .map(tokens)
            .collect()
    }

    #[test]
    fn public_fetch_stays_not_found() {
        assert_eq!(decide_public_fetch(), (404, "not found"));
    }

    #[test]
    fn probe_needs_exact_post_and_path() {
        // Off-route and wrong-method requests never reach the owner or
        // provider checks: plain 404 either way.
        for (method, path) in [
            ("GET", "/probe"),
            ("PUT", "/probe"),
            ("POST", "/"),
            ("POST", "/other"),
            ("POST", "/probe/"),
            ("POST", "/PROBE"),
            ("post", "/probe"),
            ("", ""),
        ] {
            let decision = decide_probe(method, path, true);
            assert_eq!(decision, ProbeDecision::NotFound, "{method} {path}");
            assert_eq!(decision.status(), 404, "{method} {path}");
            assert_eq!(decision.body(), "not found", "{method} {path}");
        }
        let decision = decide_probe("POST", "/probe", true);
        assert_eq!(decision, ProbeDecision::ProviderUnavailable);
    }

    #[test]
    fn probe_without_owner_name_is_bad_request() {
        let decision = decide_probe("POST", "/probe", false);
        assert_eq!(decision, ProbeDecision::OwnerMissing);
        assert_eq!(decision.status(), 400);
        assert_eq!(decision.body(), r#"{"error":"bad_request"}"#);
    }

    #[test]
    fn probe_with_owner_fails_closed_with_retryable_provider_unavailable() {
        // The executed mutant `503 -> 200` stayed green with no test pinning
        // this: assert the exact fail-closed status and body.
        let decision = decide_probe("POST", "/probe", true);
        assert_eq!(decision, ProbeDecision::ProviderUnavailable);
        assert_eq!(decision.status(), 503);
        assert_ne!(
            decision.status(),
            200,
            "provider wiring is absent: never 200"
        );
        assert_eq!(decision.body(), r#"{"error":"provider_unavailable"}"#);
    }

    #[test]
    fn probe_dispatch_distinguishes_rejection_from_probe_execution() {
        assert_eq!(
            decide_probe_action("GET", "/probe", true),
            ProbeAction::Respond(ProbeDecision::NotFound)
        );
        assert_eq!(
            decide_probe_action("POST", "/probe", false),
            ProbeAction::Respond(ProbeDecision::OwnerMissing)
        );
        assert_eq!(
            decide_probe_action("POST", "/probe", true),
            ProbeAction::RunProbe
        );
    }

    #[test]
    fn pool_action_routes_probe_and_body_and_nothing_else() {
        assert_eq!(
            decide_pool_action("POST", "/probe", true),
            PoolAction::RunProbe
        );
        assert_eq!(
            decide_pool_action("POST", "/body", true),
            PoolAction::RunBody
        );
        for (method, path) in [
            ("GET", "/body"),
            ("POST", "/body/"),
            ("POST", "/BODY"),
            ("POST", "/other"),
        ] {
            assert_eq!(
                decide_pool_action(method, path, true),
                PoolAction::Respond(ProbeDecision::NotFound),
                "{method} {path}"
            );
        }
        assert_eq!(
            decide_pool_action("POST", "/body", false),
            PoolAction::Respond(ProbeDecision::OwnerMissing)
        );
        assert_eq!(BODY_PATH, "/body");
    }

    #[tokio::test]
    async fn dispatch_runs_the_selected_response_or_probe_handler() {
        let render = |decision: ProbeDecision| (decision.status(), decision.body());
        let run_probe = || async { (200, "probe ran") };

        assert_eq!(
            dispatch_probe(
                ProbeAction::Respond(ProbeDecision::NotFound),
                render,
                run_probe
            )
            .await,
            (404, "not found")
        );
        assert_eq!(
            dispatch_probe(
                ProbeAction::Respond(ProbeDecision::OwnerMissing),
                render,
                run_probe
            )
            .await,
            (400, r#"{"error":"bad_request"}"#)
        );
        assert_eq!(
            dispatch_probe(ProbeAction::RunProbe, render, run_probe).await,
            (200, "probe ran")
        );
    }

    fn tokens<T: quote::ToTokens>(value: &T) -> String {
        value.to_token_stream().to_string()
    }

    #[test]
    fn wasm_adapter_uses_dispatch_and_preserves_worker_status_rendering() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let runtime = std::fs::read_to_string(dir.join("src/runtime.rs")).expect("read runtime.rs");
        let parsed = syn::parse_file(&runtime).expect("parse runtime.rs");
        assert!(
            non_doc_attributes(&parsed.attrs).is_empty(),
            "runtime.rs must not have inner attributes that can disable its Worker exports"
        );

        let lib = std::fs::read_to_string(dir.join("src/lib.rs")).expect("read lib.rs");
        let parsed_lib = syn::parse_file(&lib).expect("parse lib.rs");
        let runtime_module = parsed_lib
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Mod(module) if module.ident == "runtime" => Some(module),
                _ => None,
            })
            .expect("runtime module declaration");
        let wasm_cfg: syn::Attribute = syn::parse_quote!(#[cfg(target_arch = "wasm32")]);
        assert_eq!(
            non_doc_attributes(&runtime_module.attrs),
            vec![tokens(&wasm_cfg)],
            "runtime module must be enabled only by the wasm32 target cfg"
        );

        let public_fetch = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == "fetch" => Some(function),
                _ => None,
            })
            .expect("public fetch handler");
        let expected_public_fetch = syn::parse_str::<syn::ItemFn>(
            "async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {\n                let (status, body) = routes::decide_public_fetch();\n                Response::error(body, status)\n            }",
        )
        .expect("parse expected public fetch handler");
        assert_eq!(
            tokens(&public_fetch.sig),
            tokens(&expected_public_fetch.sig)
        );
        assert_eq!(
            tokens(&public_fetch.block),
            tokens(&expected_public_fetch.block)
        );
        let event_attribute: syn::Attribute = syn::parse_quote!(#[event(fetch)]);
        assert_eq!(
            non_doc_attributes(&public_fetch.attrs),
            vec![tokens(&event_attribute)],
            "public fetch must have exactly its Worker event binding and no cfg/other attributes"
        );

        let durable_object_fetch = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Impl(implementation)
                    if implementation.trait_.as_ref().is_some_and(|(path, _)| {
                        path.segments
                            .last()
                            .is_some_and(|s| s.ident == "DurableObject")
                    }) =>
                {
                    implementation.items.iter().find_map(|item| match item {
                        syn::ImplItem::Fn(function) if function.sig.ident == "fetch" => {
                            Some(function)
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("NntpPool Durable Object fetch handler");
        assert!(
            non_doc_attributes(&durable_object_fetch.attrs).is_empty(),
            "Durable Object fetch handler must not carry cfg/other attributes"
        );
        let expected_durable_object_fetch = syn::parse_str::<syn::ImplItemFn>(
            "async fn fetch(&self, mut req: Request) -> Result<Response> {\n                let has_owner_name = self.state.id().name().is_some();\n                match routes::decide_pool_action(req.method().as_ref(), &req.path(), has_owner_name) {\n                    PoolAction::Respond(ProbeDecision::NotFound) => Response::error(ProbeDecision::NotFound.body(), ProbeDecision::NotFound.status(),),\n                    PoolAction::Respond(decision) => respond(route_reply(decision)),\n                    PoolAction::RunProbe => respond(self.probe(&mut req).await),\n                    PoolAction::RunBody => respond_body(self.body(&mut req).await),\n                }\n            }",
        )
        .expect("parse expected Durable Object handler");
        assert_eq!(
            tokens(&durable_object_fetch.sig),
            tokens(&expected_durable_object_fetch.sig)
        );
        assert_eq!(
            tokens(&durable_object_fetch.block),
            tokens(&expected_durable_object_fetch.block),
            "Durable Object fetch must keep each decision on its intended response path"
        );

        let durable_object = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Struct(item) if item.ident == "NntpPool" => Some(item),
                _ => None,
            })
            .expect("NntpPool Durable Object");
        let fetch_attribute: syn::Attribute = syn::parse_quote!(#[durable_object(fetch)]);
        assert_eq!(
            non_doc_attributes(&durable_object.attrs),
            vec![tokens(&fetch_attribute)],
            "NntpPool must have exactly its Durable Object fetch binding"
        );

        let route_reply = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == "route_reply" => Some(function),
                _ => None,
            })
            .expect("route_reply function");
        assert!(
            non_doc_attributes(&route_reply.attrs).is_empty(),
            "route_reply must not carry cfg/other attributes"
        );
        let expected_route_reply = syn::parse_str::<syn::ItemFn>(
            "fn route_reply(decision: ProbeDecision) -> ProbeReply {\n                ProbeReply {\n                    status: decision.status(),\n                    body: decision.body().to_string(),\n                    retry_after_secs: None,\n                }\n            }",
        )
        .expect("parse expected route_reply");
        assert_eq!(tokens(&route_reply.sig), tokens(&expected_route_reply.sig));
        assert_eq!(
            tokens(&route_reply.block),
            tokens(&expected_route_reply.block),
            "route_reply must preserve portable status and body"
        );

        let respond = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == "respond" => Some(function),
                _ => None,
            })
            .expect("respond function");
        assert!(
            non_doc_attributes(&respond.attrs).is_empty(),
            "respond must not carry cfg/other attributes"
        );
        let expected_respond = syn::parse_str::<syn::ItemFn>(
            "fn respond(reply: ProbeReply) -> Result<Response> {\n                let headers = Headers::new();\n                headers.set(\"content-type\", \"application/json\")?;\n                if let Some(secs) = reply.retry_after_secs {\n                    headers.set(\"retry-after\", &secs.to_string())?;\n                }\n                Ok(Response::ok(reply.body)?\n                    .with_status(reply.status)\n                    .with_headers(headers))\n            }",
        )
        .expect("parse expected respond");
        assert_eq!(tokens(&respond.sig), tokens(&expected_respond.sig));
        assert_eq!(
            tokens(&respond.block),
            tokens(&expected_respond.block),
            "respond must preserve its reply status and retry metadata"
        );
    }

    #[test]
    fn do_class_name_matches_wrangler_and_runtime() {
        // The fetch entry, the DO binding, and the SQLite migration must all
        // name the same class: the adapter struct, `wrangler.toml`, and this
        // constant drift together or this test fails.
        assert_eq!(DO_CLASS_NAME, "NntpPool");
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let wrangler =
            std::fs::read_to_string(dir.join("wrangler.toml")).expect("read wrangler.toml");
        assert!(
            has_line(&wrangler, r#"class_name = "NntpPool""#),
            "wrangler.toml must bind class_name \"NntpPool\":\n{wrangler}"
        );
        assert!(
            has_line(&wrangler, r#"new_sqlite_classes = ["NntpPool"]"#),
            "wrangler.toml must migrate \"NntpPool\" as a SQLite class:\n{wrangler}"
        );
        let runtime =
            std::fs::read_to_string(dir.join("src/runtime.rs")).expect("read src/runtime.rs");
        assert!(
            runtime.contains("pub struct NntpPool"),
            "src/runtime.rs must define `pub struct NntpPool`"
        );
    }
}
