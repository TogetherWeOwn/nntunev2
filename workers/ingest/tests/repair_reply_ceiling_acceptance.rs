//! Repair-reply byte-ceiling pin (tests-only).
//!
//! Contract under test (`workers/ingest/src/repair.rs:29,83`):
//!
//! - `MAX_REPAIR_REPLY_BYTES == 64 << 10` (65,536 bytes), enforced as
//!   `body.len() > MAX_REPAIR_REPLY_BYTES` fail-closed to `Retry` before any
//!   JSON parse in `classify_repair_reply`;
//! - a 200 reply with a well-formed `{"status":"repaired",...}` body of
//!   exactly 65,536 bytes classifies `RepairCall::Repaired`;
//! - the same shape at 65,537 bytes classifies `RepairCall::Retry`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising the ceiling lets the 65,537-byte well-formed repaired reply
//!   through, failing `ceiling_plus_one_repaired_classifies_retry`;
//! - lowering the ceiling rejects the exact 65,536-byte well-formed repaired
//!   reply, failing `exact_ceiling_repaired_classifies_repaired`;
//! - moving the length check after the JSON parse (or dropping it) lets an
//!   oversized well-formed reply classify `Repaired`, failing
//!   `ceiling_plus_one_repaired_classifies_retry` — the existing unit pin at
//!   `repair.rs` only covers an oversized non-JSON body, not this boundary;
//! - changing the `200`/`"repaired"` mapping breaks the exact-ceiling test
//!   even though its body stays well-formed.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test repair_reply_ceiling_acceptance
//! ```

use nntune_ingest::repair::{classify_repair_reply, RepairCall};

/// Byte ceiling under test: `MAX_REPAIR_REPLY_BYTES == 64 << 10`.
const CEILING_BYTES: usize = 64 << 10;

/// Build a well-formed `{"status":"repaired","pad":"<pad>"}` body of exactly
/// `total_len` bytes. Padding lives inside the `pad` JSON string value (plain
/// `x` bytes, no escaping), so the body stays well-formed at any size and the
/// only variable the classifier may react to is the byte length.
fn repaired_body(total_len: usize) -> Vec<u8> {
    const PREFIX: &[u8] = br#"{"status":"repaired","pad":""#;
    const SUFFIX: &[u8] = br#""}"#;
    assert!(
        total_len >= PREFIX.len() + SUFFIX.len(),
        "body must fit the repaired envelope"
    );
    let pad_len = total_len - PREFIX.len() - SUFFIX.len();
    let mut body = Vec::with_capacity(total_len);
    body.extend_from_slice(PREFIX);
    body.extend_from_slice(&vec![b'x'; pad_len]);
    body.extend_from_slice(SUFFIX);
    body
}

#[test]
fn exact_ceiling_repaired_classifies_repaired() {
    let body = repaired_body(CEILING_BYTES);
    assert_eq!(body.len(), 65_536, "pin body must be exactly the ceiling");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("pin body must stay well-formed JSON");
    assert_eq!(
        value.get("status").and_then(|s| s.as_str()),
        Some("repaired"),
        "pin body must keep status=repaired"
    );
    assert_eq!(
        classify_repair_reply(200, &body),
        RepairCall::Repaired,
        "a well-formed repaired reply at exactly 65,536 bytes must classify Repaired"
    );
}

#[test]
fn ceiling_plus_one_repaired_classifies_retry() {
    let body = repaired_body(CEILING_BYTES + 1);
    assert_eq!(body.len(), 65_537, "pin body must be exactly ceiling + 1");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("pin body must stay well-formed JSON");
    assert_eq!(
        value.get("status").and_then(|s| s.as_str()),
        Some("repaired"),
        "pin body must keep status=repaired so only the byte ceiling flips the verdict"
    );
    assert_eq!(
        classify_repair_reply(200, &body),
        RepairCall::Retry,
        "a well-formed repaired reply at 65,537 bytes must fail closed to Retry"
    );
}
