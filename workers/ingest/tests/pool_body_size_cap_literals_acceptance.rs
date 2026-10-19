//! Pool-body 16 MiB oversize guard literal pins (`workers/ingest/src/pool_body.rs:25`, enforced at `:89`).
//!
//! Contract under test:
//!
//! - `MAX_BODY_REPLY_BYTES == 16_777_216` (16 MiB: `16 << 20`, the pool's
//!   own article cap mirrored from the stream crate's
//!   `MAX_BODY_ARTICLE_BYTES`; a longer `200` is a contract bug, not bytes
//!   to buffer).
//! - An exactly-at-cap `200` body passes through the public pool-body entry
//!   (`classify_body_reply`) as `BodyFetch::Bytes`.
//! - A cap+1 `200` body fails closed through the same entry as
//!   `Err(PoolBodyError::Rejected)`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising the cap (e.g. 17 MiB) lets the cap+1 body through as `Bytes`,
//!   failing `cap_plus_one_fails_closed_through_public_entry`;
//! - lowering the cap rejects the exactly-at-cap body, failing
//!   `exactly_at_cap_passes_through_public_entry`;
//! - changing the comparison from `>` to `>=` rejects the exactly-at-cap
//!   body, failing `exactly_at_cap_passes_through_public_entry`;
//! - dropping the length check (or mapping oversize to `Bytes`/`TooLarge`
//!   instead of `Rejected`) lets the cap+1 body through, failing the cap+1
//!   pin — the in-module symbolic oversize test at `pool_body_contract.rs`
//!   uses the constant, so only this file pins the 16 MiB literal on both
//!   sides of the boundary.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test pool_body_size_cap_literals_acceptance
//! ```

use nntune_ingest::pool_body::{
    classify_body_reply, BodyFetch, PoolBodyError, MAX_BODY_REPLY_BYTES,
};

/// Byte cap under test: `MAX_BODY_REPLY_BYTES = 16 << 20`.
const CAP_BYTES: usize = 16_777_216;

#[test]
fn max_body_reply_bytes_is_sixteen_mebibytes() {
    assert_eq!(
        MAX_BODY_REPLY_BYTES, 16_777_216,
        "pool-body oversize guard must stay exactly 16 MiB (16 << 20)"
    );
    assert_eq!(
        MAX_BODY_REPLY_BYTES,
        16 << 20,
        "pin constant must keep its declared 16 << 20 form"
    );
    assert_eq!(
        MAX_BODY_REPLY_BYTES, CAP_BYTES,
        "pin constant must track the implementation cap"
    );
}

#[test]
fn exactly_at_cap_passes_through_public_entry() {
    let at_cap = vec![0x41u8; CAP_BYTES];
    assert_eq!(at_cap.len(), 16_777_216, "pin body must be exactly the cap");
    match classify_body_reply(200, None, &at_cap) {
        Ok(BodyFetch::Bytes(bytes)) => {
            assert_eq!(
                bytes.len(),
                CAP_BYTES,
                "exactly-at-cap reply must round-trip every byte"
            );
            assert_eq!(bytes, at_cap, "exactly-at-cap reply bytes must be intact");
        }
        other => panic!("exactly-at-cap 200 must pass as Bytes, got {other:?}"),
    }
}

#[test]
fn cap_plus_one_fails_closed_through_public_entry() {
    let oversize = vec![0x41u8; CAP_BYTES + 1];
    assert_eq!(
        oversize.len(),
        16_777_216 + 1,
        "pin body must be exactly cap + 1"
    );
    assert_eq!(
        classify_body_reply(200, None, &oversize),
        Err(PoolBodyError::Rejected),
        "cap+1 200 reply must fail closed as Rejected, never buffered"
    );
}
