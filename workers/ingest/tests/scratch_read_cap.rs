//! R2 scratch object read cap (tests-only acceptance contract).
//!
//! Contract under test (`R2Scratch::get` in
//! `workers/ingest/src/queue_consumer.rs`, the `ScratchStore` impl over the
//! `INGEST_SCRATCH` bucket): before the object's body is materialised, its
//! R2 metadata size is checked against a fixed cap — the same shape as
//! repair's `read_object_capped` (`workers/repair/src/runtime.rs`), which
//! checks `object.size()` before `object.body()`:
//!
//! - `MAX_SCRATCH_OBJECT_BYTES == 32 << 20` (32 MiB), declared in
//!   `queue_consumer.rs` as `const MAX_SCRATCH_OBJECT_BYTES: u64 = 32 << 20;`
//!   (`u64`, because `object.size()` is `u64` and the Worker target's
//!   `usize` is 32-bit — a size must never be truncated through `usize`).
//! - In `get`, after the absent-object `Ok(None)` early return and before
//!   any `object.body()` call:
//!   `if object.size() > MAX_SCRATCH_OBJECT_BYTES { return Err(VerifyError::Transient("scratch object oversize".into())); }`
//!   Strict `>`: an object of exactly the cap is admitted.
//! - The well-formed path is unchanged: absent object → `Ok(None)`,
//!   bodyless object → `Ok(None)`, read failures →
//!   `VerifyError::Transient("scratch read failed")`, bytes returned as
//!   `Some(bytes)`.
//!
//! Why 32 MiB and not the 16 MiB article cap (`MAX_BODY_REPLY_BYTES`):
//! scratch holds three object classes, all read back through this one
//! getter. Spool segment objects are one decoded article each
//! (≤ `max_article_bytes`, 16 MiB) and spool index objects are JSON
//! segment lists (≤ 60k segments per NZB, well under 32 MiB), but an
//! *assembled* staged file is one whole scratch object of up to
//! `max_file_bytes` (32 MiB, `VerifyCaps::default()`, the caps the queue
//! consumer passes in production). A 16 MiB cap would reject lawful
//! assembled objects on redelivery and fail the step transiently forever.
//! 32 MiB is the tightest cap that admits every object the pipeline
//! itself can stage, while a planted or swapped object orders of
//! magnitude larger is refused from metadata alone.
//!
//! `R2Scratch` is private to `queue_consumer.rs` and a Worker `Bucket`
//! has no native constructor, so — like
//! `mb_fetch_cap_literals_acceptance` — the literal value and call-site
//! wiring (check-before-body ordering, error shape) are pinned by source
//! text, the boundary arithmetic mirrors the pinned predicate exactly
//! (`size > cap` → reject, on `u64` sizes), and the cap's relationship
//! to the pipeline's own size bounds is asserted through the public
//! surface (`VerifyCaps`, `VerifyError`, `MAX_BODY_REPLY_BYTES`).
//!
//! Sensitivity (what each pin kills):
//!
//! - removing the check, or moving it after `object.body()` /
//!   `body.bytes()`, flips the ordering pin (the body would buffer first);
//! - raising/lowering the cap flips the literal pin; lowering it to the
//!   16 MiB article cap also flips the `VerifyCaps` relationship pin;
//! - weakening `>` to `>=` flips the predicate and strictness pins (an
//!   exactly-at-cap assembled object must stay admissible);
//! - routing the size through `as usize` flips the truncation pin (on
//!   wasm32, `2^32 + 1` bytes would read as a 1-byte object);
//! - reclassifying oversize as `Permanent`, or rewording its detail,
//!   flips the error-shape pins;
//! - changing the absent/bodyless/read-failed returns flips the
//!   well-formed-path pins.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test scratch_read_cap
//! ```

use nntune_ingest::pool_body::MAX_BODY_REPLY_BYTES;
use nntune_ingest::verify::{VerifyCaps, VerifyError};

/// The crate source under test. Sliced to the pre-`#[cfg(test)]` production
/// half per assertion so this file's own literals cannot satisfy the pins.
const CONSUMER_RS: &str = include_str!("../src/queue_consumer.rs");

/// Scratch object read cap under test: `MAX_SCRATCH_OBJECT_BYTES = 32 << 20`.
const CAP_BYTES: u64 = 32 << 20;

/// Exact declaration the production source must carry.
const CAP_DECL: &str = "const MAX_SCRATCH_OBJECT_BYTES: u64 = 32 << 20;";

/// Oversize rejection detail (`get` must build exactly this error).
const OVERSIZE_DETAIL: &str = "scratch object oversize";

/// Pre-existing read-failure detail the well-formed path keeps.
const READ_FAILED_DETAIL: &str = "scratch read failed";

/// Production source before the unit-test module: pins below must match the
/// shipped code, never this file's own literals.
fn production_source() -> &'static str {
    CONSUMER_RS
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Slice of one function body out of the production source, bounded by the
/// next function, so wiring pins cannot match a neighboring function.
fn fn_body(source: &str, name: &str, next: &str) -> String {
    let after = source
        .split_once(name)
        .unwrap_or_else(|| panic!("{name} must exist"))
        .1;
    after
        .split_once(next)
        .unwrap_or_else(|| panic!("{next} must bound {name}"))
        .0
        .to_string()
}

/// The `R2Scratch::get` body (the only `get` in the consumer, bounded by
/// the sibling `put` in the same `ScratchStore` impl).
fn get_body() -> String {
    fn_body(
        production_source(),
        "async fn get(&self, key: &str)",
        "async fn put(&self, key: &str",
    )
}

/// Mirror of the required `get` gate: metadata size strictly over the cap
/// is rejected before the body is touched; anything at or under it is
/// admitted to the existing read path. Length-only on `u64` sizes, so the
/// boundary is exercised without allocating cap-sized buffers; the
/// predicate is the exact complement of the pinned source text
/// (`if object.size() > MAX_SCRATCH_OBJECT_BYTES { return Err(..) }`).
fn size_admitted(size: u64) -> bool {
    size <= CAP_BYTES
}

#[test]
fn cap_literal_is_32_mib_declared_once() {
    assert_eq!(CAP_BYTES, 33_554_432, "scratch read cap must stay 32 MiB");
    assert_eq!(CAP_BYTES, 32 * 1024 * 1024);
    let source = production_source();
    assert!(
        source.contains(CAP_DECL),
        "queue_consumer.rs must declare `{CAP_DECL}`"
    );
    assert_eq!(
        source.matches(CAP_DECL).count(),
        1,
        "MAX_SCRATCH_OBJECT_BYTES must be declared exactly once"
    );
}

#[test]
fn cap_covers_every_legitimate_scratch_object() {
    let caps = VerifyCaps::default();
    // The largest object the pipeline stages is an assembled file of
    // `max_file_bytes`; the read cap must admit it exactly, or lawful
    // staged objects would be refused on redelivery.
    assert_eq!(
        CAP_BYTES, caps.max_file_bytes,
        "read cap must equal max_file_bytes (the largest staged object)"
    );
    // Spool segment objects (one decoded article each) sit far below it.
    assert_eq!(caps.max_article_bytes as u64, CAP_BYTES / 2);
    assert_eq!(MAX_BODY_REPLY_BYTES as u64, caps.max_article_bytes as u64);
    assert!(
        caps.max_par2_bytes < CAP_BYTES,
        "the buffered par2 packet bound must stay under the read cap"
    );
    assert!(size_admitted(caps.max_file_bytes));
    assert!(size_admitted(caps.max_article_bytes as u64));
    assert!(size_admitted(caps.max_par2_bytes));
}

#[test]
fn get_checks_object_size_before_touching_body() {
    let get = get_body();
    let check = "if object.size() > MAX_SCRATCH_OBJECT_BYTES {";
    assert!(
        get.contains(check),
        "get must reject on `object.size()` against MAX_SCRATCH_OBJECT_BYTES"
    );
    assert!(
        get.contains(&format!(
            "return Err(VerifyError::Transient(\"{OVERSIZE_DETAIL}\".into()));"
        )),
        "an oversize object must fail as Transient(\"{OVERSIZE_DETAIL}\")"
    );
    // Ordering is the whole point: the size gate runs after the
    // absent-object return but before the body is requested or buffered,
    // so an oversize object is refused from R2 metadata alone.
    let at = |needle: &str| {
        get.find(needle)
            .unwrap_or_else(|| panic!("{needle} must exist in get"))
    };
    let absent_return = at("let Some(object) = object else {");
    let size_check = at(check);
    let body_request = at("object.body()");
    let body_buffered = at(".bytes()");
    assert!(
        absent_return < size_check,
        "absent object must still return Ok(None) before the size gate"
    );
    assert!(
        size_check < body_request,
        "size gate must run before object.body() — no buffering first"
    );
    assert!(
        body_request < body_buffered,
        "body bytes are read only after the gate has passed"
    );
}

#[test]
fn get_gate_is_strict_and_never_truncates_size() {
    let get = get_body();
    assert!(
        !get.contains(">= MAX_SCRATCH_OBJECT_BYTES"),
        "the gate must be strict `>`: exactly-at-cap objects are admitted"
    );
    assert!(
        !get.contains("as usize"),
        "object.size() (u64) must never be truncated through usize (32-bit on wasm32)"
    );
}

#[test]
fn exact_cap_and_smaller_sizes_admitted() {
    assert!(size_admitted(0), "an empty object reaches the body path");
    assert!(size_admitted(1));
    assert!(size_admitted(CAP_BYTES - 1));
    assert!(
        size_admitted(CAP_BYTES),
        "exactly-at-cap is admitted (strict `>` gate)"
    );
}

#[test]
fn cap_plus_one_and_huge_sizes_rejected() {
    assert!(!size_admitted(CAP_BYTES + 1));
    assert!(!size_admitted(1 << 40), "a 1 TiB planted object is refused");
    // The wasm32 truncation trap: 2^32 + 1 must not read as 1 byte.
    assert!(!size_admitted((1 << 32) + 1));
    assert!(!size_admitted(u64::MAX));
}

#[test]
fn oversize_error_shape_is_transient_not_permanent() {
    // The exact public error `get` must build for an oversize object
    // (pinned to the call site by source text above): retryable on
    // redelivery like every other scratch failure, never the Permanent
    // variant that quarantines the job.
    let err = VerifyError::Transient(OVERSIZE_DETAIL.into());
    match &err {
        VerifyError::Transient(detail) => assert_eq!(detail, OVERSIZE_DETAIL),
        VerifyError::Permanent(_) => panic!("oversize must stay Transient"),
    }
    assert_ne!(
        err,
        VerifyError::Permanent(OVERSIZE_DETAIL.into()),
        "the oversize classification must be the retryable variant"
    );
}

#[test]
fn wellformed_path_returns_unchanged() {
    let get = get_body();
    // Absent object and bodyless object still read as "not staged".
    assert_eq!(
        get.matches("return Ok(None);").count(),
        2,
        "absent and bodyless objects must both still return Ok(None)"
    );
    // Read failures (execute and body read) keep the existing detail.
    assert_eq!(
        get.matches(&format!(
            "VerifyError::Transient(\"{READ_FAILED_DETAIL}\".into())"
        ))
        .count(),
        2,
        "both read-failure sites must keep Transient(\"{READ_FAILED_DETAIL}\")"
    );
    // Admitted bytes are still returned whole as Some(bytes) — the gate
    // refuses oversize objects outright, it never truncates one.
    assert!(
        get.contains(".map(Some)"),
        "admitted body bytes must still be returned as Some(bytes)"
    );
}
