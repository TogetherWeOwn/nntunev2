//! These three edge pins supplement the behavioral lifecycle banks; an unused
//! native helper must not leave the deployed admission/consumer path unchanged.

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

fn calls(source: &str, caller: &str, callee: &str) -> bool {
    let code = code_only(source);
    let signature = format!("fn {caller}(");
    let start = code.find(&signature).expect("real edge function exists");
    let open = start + code[start..].find('{').expect("function body exists");
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
        })
        .expect("balanced function body");
    let compact: String = code[open..end]
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    compact.contains(&format!("{callee}(")) || compact.contains(&format!("{callee}::<"))
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
