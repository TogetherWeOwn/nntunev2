//! Independently owned L0 STOP guards, native and hermetic. These inspect source;
//! they do not instantiate a Worker/DO, establish authority, or exercise a live loader.

use nntune_stream::routes::ProbeDecision;
use quote::ToTokens;
use std::path::Path;

fn provider_stays_unavailable(source: &str) -> bool {
    let file = syn::parse_file(source).expect("parse runtime source");
    let mut bodies = Vec::new();
    for item in file.items {
        if let syn::Item::Impl(item) = item {
            if item.self_ty.to_token_stream().to_string() != "NntpPool" {
                continue;
            }
            for member in item.items {
                if let syn::ImplItem::Fn(method) = member {
                    if method.sig.ident == "provider" {
                        bodies.push(method.block.to_token_stream().to_string().replace(' ', ""));
                    }
                }
            }
        }
    }
    bodies == ["{Err(ProbeError::ProviderUnavailable)}"]
}

#[test]
fn actual_provider_body_is_unconditionally_unavailable() {
    assert!(
        provider_stays_unavailable(include_str!("../src/runtime.rs")),
        "L0 must not enable the provider loader"
    );
}

#[test]
fn stop_guard_rejects_comment_decoys_and_partial_enablement() {
    let cases = [
        "impl NntpPool { async fn provider(&self) { /* Err(ProbeError::ProviderUnavailable) */ Ok(inert) } }",
        "impl NntpPool { async fn provider(&self) { if enabled { Ok(inert) } else { Err(ProbeError::ProviderUnavailable) } } }",
        "impl Other { async fn provider(&self) { Err(ProbeError::ProviderUnavailable) } }",
        "impl NntpPool { async fn provider(&self) { load(); Err(ProbeError::ProviderUnavailable) } }",
    ];
    for case in cases {
        assert!(!provider_stays_unavailable(case));
    }
    assert!(provider_stays_unavailable(
        "impl NntpPool { async fn provider(&self) { Err(ProbeError::ProviderUnavailable) } }"
    ));
}

#[test]
fn portable_unavailability_response_remains_fixed_503() {
    assert_eq!(ProbeDecision::ProviderUnavailable.status(), 503);
    assert_eq!(
        ProbeDecision::ProviderUnavailable.body(),
        r#"{"error":"provider_unavailable"}"#
    );
}

fn has_seam_reference(stream: impl ToTokens) -> bool {
    // Conservative spelling guard, including aliases/imports; not a call-graph proof.
    stream
        .to_token_stream()
        .to_string()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| {
            matches!(
                word,
                "provider_response"
                    | "validate_provider_response"
                    | "Provider3Success"
                    | "ProviderOnly"
            )
        })
}

fn inspect_src(path: &Path, seam_definition: &Path, stream_lib: &Path, checked: &mut usize) {
    for entry in std::fs::read_dir(path).expect("read production source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            inspect_src(&path, seam_definition, stream_lib, checked);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") || path == seam_definition {
            continue;
        }
        *checked += 1;
        let source = std::fs::read_to_string(&path).expect("read production Rust");
        let file = syn::parse_file(&source).expect("parse production Rust");
        for item in file.items {
            if path == stream_lib {
                if let syn::Item::Mod(module) = &item {
                    if module.ident == "provider_response" {
                        assert!(
                            module.content.is_none(),
                            "only an inert out-of-line export is permitted"
                        );
                        continue;
                    }
                }
            }
            assert!(
                !has_seam_reference(item),
                "L0 seam must have no production caller/import: {}",
                path.display()
            );
        }
    }
}

#[test]
fn no_production_source_enables_or_consumes_the_inert_seam() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let stream_lib = root.join("workers/stream/src/lib.rs");
    let seam_definition = root.join("workers/stream/src/provider_response.rs");
    let mut checked = 0;
    for group in ["workers", "crates"] {
        for package in std::fs::read_dir(root.join(group)).expect("package directory") {
            let src = package.expect("package entry").path().join("src");
            if src.is_dir() {
                inspect_src(&src, &seam_definition, &stream_lib, &mut checked);
            }
        }
    }
    assert!(checked > 0, "source scan must not pass vacuously");
}
