//! Edge pins supplement the behavioral lifecycle banks; an unused native helper
//! must not leave the deployed admission/consumer path unchanged.

fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut code = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            i += source[i..].find('\n').unwrap_or(bytes.len() - i);
        } else if bytes[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'"' {
            let hashes = bytes[..i]
                .iter()
                .rev()
                .take_while(|byte| **byte == b'#')
                .count();
            let raw = i > hashes && bytes[i - hashes - 1] == b'r';
            i += 1;
            if raw {
                let suffix = format!("\"{}", "#".repeat(hashes));
                i += source[i..].find(&suffix).expect("closed raw string") + suffix.len();
            } else {
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
        } else {
            i += 1;
            continue;
        }
        code[start..i].fill(b' ');
    }
    String::from_utf8(code).expect("source remains UTF-8")
}

fn function_body<'a>(code: &'a str, caller: &str) -> Option<&'a str> {
    let signature = format!("fn {caller}(");
    let start = code.find(&signature)?;
    let open = start + code[start..].find('{')?;
    let mut depth = 0;
    let end = code
        .as_bytes()
        .iter()
        .enumerate()
        .skip(open)
        .find_map(|(index, byte)| {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
            None
        })?;
    Some(&code[open..end])
}

fn direct_call(body: &str, callee: &str) -> bool {
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    [format!("{callee}("), format!("{callee}::<")].iter().any(|needle| {
        compact.match_indices(needle).any(|(at, _)| {
            at == 0 || !matches!(compact.as_bytes()[at - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
        })
    })
}

fn calls(source: &str, caller: &str, callee: &str) -> bool {
    function_body(&code_only(source), caller).is_some_and(|body| direct_call(body, callee))
}

fn reachable_call(entry_source: &str, caller: &str, helper_source: &str, target: &str) -> bool {
    let entry = code_only(entry_source);
    let helpers = code_only(helper_source);
    let names: Vec<_> = helpers
        .match_indices("fn ")
        .filter_map(|(at, _)| {
            let tail = &helpers[at + 3..];
            let end = tail.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))?;
            Some(&tail[..end])
        })
        .collect();
    let Some(entry_body) = function_body(&entry, caller) else {
        return false;
    };
    let mut pending = vec![entry_body];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(body) = pending.pop() {
        if direct_call(body, target) {
            return true;
        }
        for name in &names {
            if direct_call(body, name) && visited.insert(*name) {
                if let Some(next) = function_body(&helpers, name) {
                    pending.push(next);
                }
            }
        }
    }
    false
}

#[test]
fn real_admission_route_uses_writer_aware_sequencer() {
    assert!(
        calls(
            include_str!("../src/lib.rs"),
            "admit_metered_job",
            "drive_metered_admission_with_writer",
        ),
        "admit_metered_job must await the behaviorally tested durable admission path"
    );
}

#[test]
fn real_message_processor_uses_durable_checkpoint_settlement() {
    assert!(
        calls(
            include_str!("../src/queue_consumer.rs"),
            "process_message",
            "settle_checkpoint_with",
        ),
        "process_message must settle terminal status through the tested runtime boundary"
    );
}

#[test]
fn real_raw_delivery_uses_the_tested_ack_retry_and_dlq_sequencer() {
    assert!(
        calls(
            include_str!("../src/queue_consumer.rs"),
            "process_raw_message",
            "apply_settlement_with",
        ),
        "process_raw_message must apply the behaviorally tested delivery settlement"
    );
}

fn edge_impl(name: &str) -> &'static str {
    let production = include_str!("../src/queue_consumer.rs")
        .split_once("#[cfg(test)]")
        .unwrap()
        .0;
    let start = production.find(name).expect("existing edge implementation");
    let rest = &production[start..];
    let end = rest.find("\n}\n").expect("edge implementation closes") + 2;
    &rest[..end]
}

#[test]
fn matching_edge_calls_the_behaviorally_tested_sequencer() {
    assert!(calls(
        include_str!("../src/queue_consumer.rs"),
        "run_match_write",
        "run_match_write_with",
    ));
}

#[test]
fn transcode_edge_calls_the_behaviorally_tested_sequencer() {
    assert!(calls(
        include_str!("../src/queue_consumer.rs"),
        "run_transcode_step",
        "run_transcode_step_with",
    ));
}

#[test]
fn materialization_edge_delegates_to_the_real_fenced_native_writer() {
    assert!(
        calls(
            edge_impl("impl MatchWriteRuntime for EdgeMatchWrite"),
            "materialize_tracks",
            "writer_execution::materialize_tracks",
        ),
        "selected DB IDs must come from the tested native track writer"
    );
}

#[test]
fn context_edge_delegates_to_the_real_native_identity_validation() {
    assert!(
        calls(
            edge_impl("impl TranscodeRuntime for EdgeTranscode"),
            "validate_context",
            "writer_execution::publication_context",
        ),
        "a successful fake context is not evidence of edge DB validation"
    );
}

#[test]
fn media_edge_delegates_to_the_real_fenced_media_then_grant_writer() {
    assert!(
        calls(
            edge_impl("impl TranscodeRuntime for EdgeTranscode"),
            "publish_media",
            "writer_execution::publish_media",
        ),
        "the real adapter must execute native media/grant semantics"
    );
}

#[test]
fn detailed_matching_edge_does_not_retain_the_id_only_parser_as_its_only_path() {
    let source = include_str!("../src/queue_consumer.rs")
        .split_once("#[cfg(test)]")
        .unwrap()
        .0;
    let helpers = format!(
        "{source}\n{}",
        include_str!("../src/matching.rs")
            .split_once("#[cfg(test)]")
            .unwrap()
            .0
    );
    assert!(
        reachable_call(
            edge_impl("impl MatchWriteRuntime for EdgeMatchWrite"),
            "resolve_tracks",
            &helpers,
            "parse_mb_recordings_detailed",
        ),
        "the detailed edge path must reach the metadata parser, not just leave it unused"
    );
}

#[test]
fn detailed_parser_reachability_rejects_unused_or_comment_only_helpers() {
    let helpers = r#"
        async fn wrapper() { parse_mb_recordings_detailed(body); }
        async fn dead() { parse_mb_recordings_detailed(body); }
    "#;
    assert!(reachable_call(
        "async fn edge() { wrapper().await; }",
        "edge",
        helpers,
        "parse_mb_recordings_detailed"
    ));
    assert!(!reachable_call(
        "async fn edge() { id_only_parser(body); }",
        "edge",
        helpers,
        "parse_mb_recordings_detailed"
    ));
    assert!(!reachable_call(
        "async fn edge() { /* wrapper(); */ let text = \"wrapper()\"; }",
        "edge",
        helpers,
        "parse_mb_recordings_detailed"
    ));
    assert!(!calls(
        "async fn edge() { unused_wrapper(); }",
        "edge",
        "wrapper"
    ));
}

#[test]
fn coupling_pin_ignores_comments_strings_and_calls_outside_the_named_function() {
    let fixture = r##"
        async fn edge() {
            // helper(); }
            /* helper(); /* nested */ } */
            let text = "helper(); }";
            let raw = r#"helper(); }"#;
        }
        async fn other() { helper(); }
    "##;
    assert!(!calls(fixture, "edge", "helper"));
    assert!(calls(fixture, "other", "helper"));
}
