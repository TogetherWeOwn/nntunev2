//! Acceptance tests (failing-first): pin source/configuration for the stream Worker's Wasm entrypoint.
//!
//! The Wrangler Durable Object binding and SQLite migration are a single
//! registration contract. Wrangler must point at the worker-build shim, and the
//! wasm-only runtime must declare its fetch entry and `NntpPool`. These native
//! source/config checks do not execute worker-build or inspect emitted Wasm.

use std::path::PathBuf;

fn source(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn without_comment(value: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some('"') if character == '\\' => escaped = true,
            Some(delimiter) if character == delimiter => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character == '#' => return &value[..index],
            None => {}
        }
    }
    value
}

fn tables(document: &str, expected: &str) -> Vec<String> {
    let expected = compact(expected);
    let mut result = Vec::new();
    let mut active = false;
    let mut table = String::new();
    for line in document.lines().chain(std::iter::once("")) {
        let line = without_comment(line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            if active {
                result.push(std::mem::take(&mut table));
            }
            active = compact(line) == expected;
        } else if active {
            table.push_str(line);
            table.push('\n');
        }
    }
    if active {
        result.push(table);
    }
    result
}

fn field(table: &str, key: &str) -> Option<String> {
    let lines = table.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let Some((name, raw_value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let mut value = without_comment(raw_value).trim().to_string();
        if value.starts_with('[') && !value.contains(']') {
            for continuation in lines.iter().skip(index + 1) {
                let continuation = without_comment(continuation).trim();
                value.push_str(continuation);
                if continuation.contains(']') {
                    break;
                }
            }
        }
        return Some(value);
    }
    None
}

fn toml_string(value: &str) -> Option<String> {
    let value = value.trim();
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    value
        .strip_prefix(quote)
        .and_then(|value| value.strip_suffix(quote))
        .map(str::to_string)
}

fn comma_separated_items(value: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some('"') if character == '\\' => escaped = true,
            Some(delimiter) if character == delimiter => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character == ',' => {
                items.push(&value[start..index]);
                start = index + character.len_utf8();
            }
            None => {}
        }
    }
    items.push(&value[start..]);
    items
}

fn string_array(value: &str) -> Option<Vec<String>> {
    let inner = value.trim().strip_prefix('[')?.strip_suffix(']')?;
    comma_separated_items(inner)
        .into_iter()
        .filter(|item| !item.trim().is_empty())
        .map(toml_string)
        .collect()
}

fn inline_table_field<'a>(table: &'a str, key: &str) -> Option<&'a str> {
    let inner = table.trim().strip_prefix('{')?.strip_suffix('}')?;
    comma_separated_items(inner).into_iter().find_map(|item| {
        let (name, value) = item.split_once('=')?;
        (name.trim() == key).then_some(without_comment(value).trim())
    })
}

fn dependency_version(value: &str) -> Option<String> {
    toml_string(value).or_else(|| inline_table_field(value, "version").and_then(toml_string))
}

fn version_is_in_series(value: &str, series: &str) -> bool {
    fn starts_in_series(value: &str, series: &str) -> bool {
        value == series
            || value
                .strip_prefix(series)
                .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with(".*"))
    }

    let constraints = value.split(',').map(str::trim).collect::<Vec<_>>();
    if constraints.len() == 1 {
        let requirement = constraints[0]
            .strip_prefix('^')
            .or_else(|| constraints[0].strip_prefix('~'))
            .or_else(|| constraints[0].strip_prefix('='))
            .unwrap_or(constraints[0])
            .trim();
        return starts_in_series(requirement, series);
    }

    let lower_bound = constraints.iter().find_map(|constraint| {
        constraint
            .strip_prefix(">=")
            .or_else(|| constraint.strip_prefix('>').map(|value| value.trim()))
            .map(str::trim)
    });
    let upper_bound = constraints
        .iter()
        .find_map(|constraint| constraint.strip_prefix('<').map(str::trim));
    let Some((major, minor)) = series.split_once('.') else {
        return false;
    };
    let Ok(minor) = minor.parse::<u64>() else {
        return false;
    };
    let next_series = format!("{major}.{}", minor + 1);

    lower_bound.map_or(false, |bound| starts_in_series(bound, series))
        && upper_bound.map_or(false, |bound| {
            bound == next_series || bound == format!("{next_series}.0")
        })
}

fn default_features_enabled(value: &str) -> bool {
    if toml_string(value).is_some() {
        return true;
    }
    match inline_table_field(value, "default-features") {
        None | Some("true") => true,
        Some("false") | Some(_) => false,
    }
}

fn raw_string_end(characters: &[char], start: usize) -> Option<usize> {
    let mut quote = match (characters.get(start), characters.get(start + 1)) {
        (Some('r'), _) => start + 1,
        (Some('b'), Some('r')) => start + 2,
        _ => return None,
    };
    let mut hashes = 0;
    while characters.get(quote) == Some(&'#') {
        hashes += 1;
        quote += 1;
    }
    if characters.get(quote) != Some(&'"') {
        return None;
    }
    let mut index = quote + 1;
    while index < characters.len() {
        if characters[index] == '"' {
            let mut closing = index + 1;
            let mut closing_hashes = 0;
            while characters.get(closing) == Some(&'#') {
                closing_hashes += 1;
                closing += 1;
            }
            if closing_hashes == hashes {
                return Some(closing);
            }
        }
        index += 1;
    }
    Some(characters.len())
}

fn quoted_string_end(characters: &[char], start: usize) -> usize {
    let mut index = start + 1;
    let mut escaped = false;
    while index < characters.len() {
        let character = characters[index];
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return index + 1;
        }
        index += 1;
    }
    characters.len()
}

fn char_literal_end(characters: &[char], start: usize) -> Option<usize> {
    let mut index = start + 1;
    if characters.get(index) == Some(&'\\') {
        index += 1;
        if characters.get(index) == Some(&'u') && characters.get(index + 1) == Some(&'{') {
            index += 2;
            while index < characters.len() && characters[index] != '}' {
                index += 1;
            }
            index += usize::from(characters.get(index) == Some(&'}'));
        } else if characters.get(index).is_some() {
            index += 1;
        } else {
            return None;
        }
    } else if characters
        .get(index)
        .is_some_and(|character| *character != '\n')
    {
        index += 1;
    } else {
        return None;
    }
    (characters.get(index) == Some(&'\'')).then_some(index + 1)
}

fn mask_range(output: &mut String, characters: &[char], start: usize, end: usize) {
    for character in &characters[start..end] {
        output.push(if *character == '\n' { '\n' } else { ' ' });
    }
}

fn rust_code(source: &str) -> String {
    let characters = source.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    let mut block_depth = 0;

    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();
        if block_depth > 0 {
            if character == '/' && next == Some('*') {
                block_depth += 1;
                index += 2;
            } else if character == '*' && next == Some('/') {
                block_depth -= 1;
                index += 2;
            } else {
                if character == '\n' {
                    output.push('\n');
                }
                index += 1;
            }
        } else if let Some(end) = raw_string_end(&characters, index) {
            mask_range(&mut output, &characters, index, end);
            index = end;
        } else if character == '"' {
            let end = quoted_string_end(&characters, index);
            mask_range(&mut output, &characters, index, end);
            index = end;
        } else if character == '\'' {
            if let Some(end) = char_literal_end(&characters, index) {
                mask_range(&mut output, &characters, index, end);
                index = end;
            } else {
                output.push(character);
                index += 1;
            }
        } else if character == '/' && next == Some('/') {
            while index < characters.len() && characters[index] != '\n' {
                index += 1;
            }
        } else if character == '/' && next == Some('*') {
            block_depth = 1;
            index += 2;
        } else {
            output.push(character);
            index += 1;
        }
    }
    output
}

fn worker_import_binds(source: &str, expected: &str, visible_name: &str) -> bool {
    let source = compact(&rust_code(source));
    ["useworker::", "use::worker::"].into_iter().any(|prefix| {
        source.split(prefix).skip(1).any(|remainder| {
            let Some((imports, _)) = remainder.split_once(';') else {
                return false;
            };
            let Some(imports) = imports
                .strip_prefix('{')
                .and_then(|imports| imports.strip_suffix('}'))
            else {
                return imports == "*" && expected == visible_name;
            };
            comma_separated_items(imports).into_iter().any(|item| {
                let (path, alias) = item.split_once("as").unwrap_or((item, item));
                path.rsplit("::").next() == Some(expected) && alias == visible_name
            })
        })
    })
}

fn worker_type_matches(actual: &str, expected: &str, source: &str) -> bool {
    let actual = compact(actual);
    let path = actual.strip_prefix("::").unwrap_or(&actual);
    if path == format!("worker::{expected}") {
        return true;
    }
    if let Some((module, name)) = path.split_once("::") {
        if name == expected && worker_import_binds(source, "self", module) {
            return true;
        }
        return false;
    }
    worker_import_binds(source, expected, &actual)
}

fn function_parameters(signature: &str) -> Option<Vec<&str>> {
    let open = signature.find('(')?;
    let close = open + signature[open..].find(')')?;
    let mut parameters = signature[open + 1..close]
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>();
    if parameters.last() == Some(&"") {
        parameters.pop();
    }
    (!parameters.iter().any(|parameter| parameter.is_empty())).then_some(parameters)
}

fn parameter_type(parameter: &str) -> Option<&str> {
    parameter.split_once(':').map(|(_, value)| value.trim())
}

fn result_response_type_matches(actual: &str, source: &str) -> bool {
    let Some(result_start) = actual.rfind("Result<") else {
        return false;
    };
    let result_type = format!("{}Result", &actual[..result_start]);
    let Some(response_type) = actual[result_start + "Result<".len()..].strip_suffix('>') else {
        return false;
    };
    worker_type_matches(&result_type, "Result", source)
        && worker_type_matches(response_type, "Response", source)
}

fn return_type(signature: &str) -> Option<&str> {
    let open = signature.find('(')?;
    let close = open + signature[open..].find(')')?;
    signature[close + 1..].strip_prefix("->")
}

fn attributed_signature(source: &str, attribute: &str, function_name: &str) -> Option<String> {
    let source = compact(&rust_code(source));
    let item = source.split_once(attribute)?.1;
    let function = item.find(&format!("asyncfn{function_name}("))?;
    let signature = &item[function..];
    Some(signature[..signature.find('{')?].to_string())
}

fn worker_fetch_signature_is_valid(signature: &str, source: &str) -> bool {
    let Some(parameters) = function_parameters(signature) else {
        return false;
    };
    let Some(result) = return_type(signature) else {
        return false;
    };
    parameters.len() == 3
        && ["Request", "Env", "Context"]
            .iter()
            .zip(parameters)
            .all(|(expected, parameter)| {
                parameter_type(parameter).map_or(false, |actual| {
                    worker_type_matches(actual, expected, source)
                })
            })
        && result_response_type_matches(result, source)
}

fn nntp_pool_impl_fetch_signature(source: &str) -> Option<String> {
    let code = compact(&rust_code(source));
    let imported = worker_import_binds(source, "DurableObject", "DurableObject");
    let markers = [
        "impl::worker::DurableObjectforNntpPool{",
        "implworker::DurableObjectforNntpPool{",
    ];
    let start = markers
        .iter()
        .find_map(|marker| code.find(marker).map(|start| start + marker.len()))
        .or_else(|| {
            imported
                .then(|| code.find("implDurableObjectforNntpPool{"))
                .flatten()
                .map(|start| start + "implDurableObjectforNntpPool{".len())
        })?;
    let implementation = &code[start..];
    let method = implementation.find("asyncfnfetch(")?;
    let signature = &implementation[method..];
    Some(signature[..signature.find('{')?].to_string())
}

fn worker_build_release_is_configured(command: &str) -> bool {
    command
        .split(|character| character == '&' || character == ';')
        .any(|stage| {
            let mut words = stage.split_whitespace();
            let executable = words
                .next()
                .unwrap_or_default()
                .rsplit('/')
                .next()
                .unwrap_or_default();
            executable == "worker-build" && words.next() == Some("--release")
        })
}

#[test]
fn wrangler_registers_the_exact_nntp_pool_class_and_sqlite_migration() {
    let wrangler = source("wrangler.toml");
    let bindings = tables(&wrangler, "[[durable_objects.bindings]]");
    assert_eq!(bindings.len(), 1, "expected one Durable Object binding");
    assert_eq!(
        field(&bindings[0], "name").and_then(|value| toml_string(&value)),
        Some("NNTP_POOL".to_string())
    );
    assert_eq!(
        field(&bindings[0], "class_name").and_then(|value| toml_string(&value)),
        Some("NntpPool".to_string())
    );

    let migrations = tables(&wrangler, "[[migrations]]");
    assert_eq!(migrations.len(), 1, "expected one SQLite migration");
    assert_eq!(
        field(&migrations[0], "tag").and_then(|value| toml_string(&value)),
        Some("v1".to_string())
    );
    assert!(
        field(&migrations[0], "new_classes").is_none(),
        "use a SQLite class, not a standard class"
    );
    assert_eq!(
        field(&migrations[0], "new_sqlite_classes").and_then(|value| string_array(&value)),
        Some(vec!["NntpPool".to_string()]),
        "the v1 migration must register only NntpPool as a SQLite class"
    );
}

#[test]
fn wrangler_configures_the_worker_build_shim() {
    let wrangler = source("wrangler.toml");
    assert_eq!(
        field(&wrangler, "main").and_then(|value| toml_string(&value)),
        Some("build/worker/shim.mjs".to_string())
    );
    let build = tables(&wrangler, "[build]");
    assert_eq!(build.len(), 1, "expected one Wrangler build table");
    let command = field(&build[0], "command").and_then(|value| toml_string(&value));
    assert!(
        command.map_or(false, |command| worker_build_release_is_configured(
            &command
        )),
        "Wrangler must invoke worker-build --release"
    );
}

#[test]
fn crate_configures_wasm_cdylib_and_target_scoped_worker_runtime() {
    let manifest = source("Cargo.toml");
    let lib = tables(&manifest, "[lib]");
    assert_eq!(lib.len(), 1, "expected one library target");
    let crate_types = field(&lib[0], "crate-type").and_then(|value| string_array(&value));
    assert_eq!(
        crate_types,
        Some(vec!["cdylib".to_string(), "rlib".to_string()])
    );

    let dependencies = tables(
        &manifest,
        "[target.'cfg(target_arch = \"wasm32\")'.dependencies]",
    );
    assert_eq!(
        dependencies.len(),
        1,
        "Worker dependencies must be wasm32-only"
    );
    let worker = field(&dependencies[0], "worker").expect("wasm32 Worker runtime dependency");
    assert!(
        dependency_version(&worker).map_or(false, |version| version_is_in_series(&version, "0.8"))
    );
    assert!(
        default_features_enabled(&worker),
        "Worker default features must remain enabled"
    );
    let wasm_bindgen =
        field(&dependencies[0], "wasm-bindgen").expect("wasm32 wasm-bindgen dependency");
    assert!(dependency_version(&wasm_bindgen)
        .map_or(false, |version| version_is_in_series(&version, "0.2")));

    let lib_source = source("src/lib.rs");
    assert!(
        lib_source
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| {
                compact(pair[0]) == "#[cfg(target_arch=\"wasm32\")]"
                    && compact(pair[1]) == "pubmodruntime;"
            }),
        "pub mod runtime must be gated by target_arch = wasm32"
    );
}

#[test]
fn wasm_runtime_declares_worker_fetch_and_nntp_pool_durable_object_handlers() {
    let runtime = source("src/runtime.rs");
    let code = compact(&rust_code(&runtime));
    assert!(
        code.contains("#[event(fetch)]"),
        "the Worker fetch handler must use event(fetch)"
    );
    let handler = attributed_signature(&runtime, "#[event(fetch)]", "fetch")
        .expect("async Worker fetch handler");
    assert!(
        worker_fetch_signature_is_valid(&handler, &runtime),
        "fetch must use Worker Request, Env, Context, and Result<Response> types"
    );
    assert!(
        code.contains("#[durable_object(fetch)]pubstructNntpPool"),
        "the public NntpPool struct must use durable_object(fetch)"
    );

    let implementation = nntp_pool_impl_fetch_signature(&runtime)
        .expect("NntpPool must implement worker::DurableObject with a fetch handler");
    let parameters = function_parameters(&implementation).expect("Durable Object fetch parameters");
    assert!(parameters.len() == 2 && compact(parameters[0]) == "&self");
    assert!(
        parameter_type(parameters[1]).map_or(false, |actual| worker_type_matches(
            actual, "Request", &runtime
        ))
    );
    assert!(
        return_type(&implementation).map_or(false, |result| result_response_type_matches(
            result, &runtime
        ))
    );
}

#[test]
fn parsers_handle_bounded_inline_versions_and_worker_glob_imports() {
    let worker = r#"{ version = ">=0.8, <0.9" }"#;
    assert_eq!(dependency_version(worker).as_deref(), Some(">=0.8, <0.9"));
    assert!(version_is_in_series(
        &dependency_version(worker).unwrap(),
        "0.8"
    ));
    assert!(default_features_enabled(worker));
    assert!(version_is_in_series(">0.8, <0.9", "0.8"));
    assert!(!version_is_in_series(">=0.8", "0.8"));

    let glob = "use worker::*;";
    for ty in [
        "Request",
        "Env",
        "Context",
        "Result",
        "Response",
        "DurableObject",
    ] {
        assert!(
            worker_type_matches(ty, ty, glob),
            "glob import should bind {ty}"
        );
    }
    assert!(!worker_type_matches("forged::Request", "Request", glob));
    assert!(!worker_type_matches("anyhow::Result", "Result", glob));
}

#[test]
fn rust_contract_checks_ignore_comments_and_string_literals() {
    let rust = r##"
use worker::*;
#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let braces = "{ fake #[event(fetch)] and impl DurableObject for NntpPool { } }";
    let raw = r#"{ fake #[durable_object(fetch)] }"#;
    Response::error("missing", 404)
}
#[durable_object(fetch)]
pub struct NntpPool;
impl
    worker :: DurableObject
    for NntpPool
{
    async fn fetch(&self, request: Request) -> Result<Response> {
        // A comment containing braces must not affect signature checks: { }
        Response::error("unavailable", 503)
    }
}
"##;
    let handler = attributed_signature(rust, "#[event(fetch)]", "fetch").unwrap();
    assert!(worker_fetch_signature_is_valid(&handler, rust));
    assert!(nntp_pool_impl_fetch_signature(rust).is_some());
    assert!(worker_type_matches("Request", "Request", rust));
    assert!(!worker_type_matches("my::Request", "Request", rust));
    assert!(!worker_type_matches("anyhow::Result", "Result", rust));
}
