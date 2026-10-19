//! Probe/ingest message-id validation agreement pin (tests-only).
//!
//! Contract under test:
//!
//! - `workers/stream/src/probe.rs:166` `valid_probe_id` claims to be
//!   identical to `workers/ingest/src/probe.rs:64` `validate_message_id`
//!   (RFC 5536 `msg-id`, bracketless): 1..=512 bytes of printable ASCII, no
//!   `<`/`>`, exactly one `@` with non-empty sides.
//!
//! A stream-accepted id must never be rejected at the ingest envelope, and
//! vice versa, so this file pins the agreement two ways:
//!
//! - the shared vector table below drives `valid_probe_id` directly: every
//!   vector pins its expected verdict (empty, 513-byte, space, `<`/`>`,
//!   double-`@`, NUL, non-printable, plus valid controls including the exact
//!   512-byte boundary);
//! - `ingest_validator_body_matches_stream` extracts both validator bodies
//!   as source text and requires them identical modulo the function name.
//!   Identical bodies agree on every input, so the behavioral vectors pin
//!   ingest transitively: any unilateral rule change on either side breaks
//!   the body equality, and any stream-side semantic change also breaks the
//!   vectors.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - relaxing/tightening either validator (cap, charset, `<`/`>` ban, `@`
//!   splitting, empty-side rule) changes its body text, so the equality pin
//!   fails naming the drifted side;
//! - relaxing/tightening the stream validator additionally fails the vector
//!   asserts on the drifted vectors;
//! - tightening both validators identically keeps the equality pin green but
//!   fails the vectors whose verdicts changed (semantics moved; the vectors
//!   are the contract);
//! - deleting/renaming either function fails the extraction with a message
//!   pointing at both files.
//!
//! The ingest side is read as source text via `include_str!`, not imported
//! as a crate: `workers/ingest` is a standalone worker excluded from the
//! root workspace AND from the container build contexts (`.dockerignore`
//! `workers/*` minus `!workers/stream/`), so a `nntune-ingest` path
//! dev-dependency breaks `containers/mb-import` and `containers/repair`
//! clean-checkout builds. This follows the
//! `tests/id_cap_agreement_acceptance.rs` precedent. `cargo build` never
//! compiles `tests/`, so the missing file in those contexts is never
//! touched; full-checkout `cargo test` always has it.
//!
//! Out of scope: changing either validator's rules, the crate-level guard
//! (core `nntune-core` NNTP argument validation, sibling card), the stream
//! `nntp.rs` BODY adapter's own 250-byte shape rule (a different ceiling,
//! pinned by `tests/message_id_shape_regression.rs`).
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test probe_id_agreement_acceptance
//! ```

use nntune_stream::probe::valid_probe_id;

/// Ingest validator source, read as text (NOT a crate import — see module
/// docs for why a `nntune-ingest` dev-dependency is forbidden here).
const INGEST_PROBE_RS: &str = include_str!("../../ingest/src/probe.rs");
/// Stream validator source, read as text so the agreement pin compares the
/// two published bodies instead of re-stating one of them.
const STREAM_PROBE_RS: &str = include_str!("../src/probe.rs");

const STREAM_SIG: &str = "pub fn valid_probe_id(id: &str) -> bool {";
const INGEST_SIG: &str = "pub fn validate_message_id(id: &str) -> bool {";

/// The text of the validator starting at `signature` through its closing
/// `}` at column 0 (both bodies are flat: no nested column-0 braces).
fn validator_body<'a>(source: &'a str, file: &str, signature: &str) -> &'a str {
    let start = source.find(signature).unwrap_or_else(|| {
        panic!(
            "{file} must still declare `{signature}`: \
             the probe/ingest message-id agreement moved — \
             update both validators together"
        )
    });
    let rest = &source[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{file} validator body must still close its `}}` at column 0"));
    &rest[..end + "\n}".len()]
}

/// Shared agreement vectors: (input, expected verdict). Both validators must
/// agree with this table; the body-equality pin extends the agreement to
/// every input.
fn agreement_vectors() -> Vec<(String, bool)> {
    let at_cap = format!("{}@{}", "a".repeat(250), "b".repeat(261));
    let over_cap = format!("{}@{}", "a".repeat(250), "b".repeat(262));
    assert_eq!(at_cap.len(), 512, "exactly-at-cap control must be 512 B");
    assert_eq!(over_cap.len(), 513, "over-cap vector must be 513 B");
    vec![
        // Valid controls.
        ("seg-one@synthetic.invalid".to_string(), true),
        (at_cap, true),
        // Empty and overlong.
        (String::new(), false),
        (over_cap, false),
        // Space (0x20, below the printable range).
        ("has space@synthetic.invalid".to_string(), false),
        // Angle brackets: bracketed, trailing, and injection-shaped.
        ("<seg@synthetic.invalid>".to_string(), false),
        ("seg@synthetic.invalid>".to_string(), false),
        (
            "x@y>\r\nAUTHINFO USER attacker\r\nQUIT\r\n<z".to_string(),
            false,
        ),
        // More than one `@`.
        ("x@y@z".to_string(), false),
        ("a@@synthetic.invalid".to_string(), false),
        // NUL.
        ("x\0@synthetic.invalid".to_string(), false),
        // Other non-printables (below 0x21, plus DEL).
        ("x@sy\nthetic.invalid".to_string(), false),
        ("x@sy\rthetic.invalid".to_string(), false),
        ("a\x01b@synthetic.invalid".to_string(), false),
        ("a\x7fb@synthetic.invalid".to_string(), false),
        // Empty sides of a single `@`.
        ("@synthetic.invalid".to_string(), false),
        ("local@".to_string(), false),
        // No `@` at all.
        ("not-an-id".to_string(), false),
    ]
}

#[test]
fn shared_vectors_cover_every_required_shape() {
    let vectors = agreement_vectors();
    assert_eq!(
        vectors.len(),
        18,
        "agreement table must keep all 18 vectors \
         (empty, 513 B, space, brackets, double-@, NUL, non-printable, controls)"
    );
    assert!(
        vectors.iter().any(|(_, ok)| *ok),
        "table needs a valid control"
    );
    assert!(vectors.iter().any(|(_, ok)| !*ok), "table needs rejections");
}

#[test]
fn stream_validator_matches_every_shared_vector() {
    for (id, expected) in agreement_vectors() {
        assert_eq!(
            valid_probe_id(&id),
            expected,
            "valid_probe_id({id:?}) must be {expected}"
        );
    }
}

#[test]
fn ingest_validator_body_matches_stream() {
    let stream = validator_body(STREAM_PROBE_RS, "workers/stream/src/probe.rs", STREAM_SIG)
        .replace("valid_probe_id", "VALIDATOR");
    let ingest = validator_body(INGEST_PROBE_RS, "workers/ingest/src/probe.rs", INGEST_SIG)
        .replace("validate_message_id", "VALIDATOR");
    assert_eq!(
        stream, ingest,
        "probe/ingest message-id validators drifted: change both \
         `valid_probe_id` (workers/stream/src/probe.rs) and \
         `validate_message_id` (workers/ingest/src/probe.rs) together, \
         then re-run the shared vectors"
    );
}
