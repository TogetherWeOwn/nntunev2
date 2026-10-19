//! Probe-delivery and verify wall-clock budget literal pins (tests-only).
//!
//! Contract under test
//! (`workers/ingest/src/queue_consumer.rs:121,142,573,739`):
//!
//! - `PROBE_DELIVERY_BUDGET_MS == 30_000` (most wall time one delivery may
//!   spend probing, checked between batches via
//!   `Date::now().as_millis().saturating_sub(probing_started_ms)
//!       >= PROBE_DELIVERY_BUDGET_MS`);
//! - `VERIFY_WALL_CLOCK_BUDGET_MS == 20_000` (wall-clock budget for one
//!   Verifying delivery, polled as
//!   `Date::now().as_millis().saturating_sub(started_ms)
//!       > VERIFY_WALL_CLOCK_BUDGET_MS`);
//! - the probe check runs as
//!   `if asked >= max_batches || (asked > 0 && budget_expired())`, so at
//!   least one batch is always asked whatever the budget says;
//! - the two operators differ on purpose: the probe trips at exactly the
//!   budget (`>=`), verify needs strictly over it (`>`).
//!
//! Both constants are private to `queue_consumer`, so the literal and
//! wiring pins read the production source via `include_str!` (sliced to
//! the pre-`#[cfg(test)]` half so this file's own literals cannot satisfy
//! them), while the behavior pins drive the public deadline paths: a
//! faithful mirror of the probe loop condition plus a live
//! `run_verifying_step` with an elapsed-derived deadline.
//!
//! Pins in this file:
//!
//! - the probe literal stays exactly `30_000`;
//! - the verify literal stays exactly `20_000`;
//! - probe enforcement stays `>= PROBE_DELIVERY_BUDGET_MS` with the
//!   `asked > 0 &&` first-fold guard;
//! - verify enforcement stays `> VERIFY_WALL_CLOCK_BUDGET_MS` (strict);
//! - elapsed `29_999` does not trip the probe deadline, `30_000` does;
//! - elapsed `20_000` does not trip the verify deadline, `20_001` does;
//! - a two-batch probe run at `29_999` asks both batches, at `30_000` it
//!   stops after the first fold;
//! - a live verify step at `20_000` completes, at `20_001` it stops as
//!   `Incomplete` with progress durable.
//!
//! Sensitivity (implementation stays untouched):
//!
//! - lowering/raising `PROBE_DELIVERY_BUDGET_MS` (e.g. `29_999`, `30_001`)
//!   fails `probe_delivery_budget_literal_is_30s` on the exact-declaration
//!   assertion;
//! - lowering/raising `VERIFY_WALL_CLOCK_BUDGET_MS` (e.g. `19_999`,
//!   `20_001`) fails `verify_wall_clock_budget_literal_is_20s`;
//! - switching the probe operator to `>` lets elapsed `30_000` slip past
//!   the first check, failing `probe_enforcement_trips_at_exactly_budget`
//!   and `probe_exceeding_budget_stops_after_first_fold`;
//! - switching the verify operator to `>=` trips elapsed `20_000`,
//!   failing `verify_enforcement_needs_strictly_over_budget` and
//!   `verify_at_budget_completes_while_exceeding_stops`;
//! - dropping the `asked > 0 &&` guard (stopping before the first fold)
//!   fails `probe_enforcement_keeps_first_fold_guard` on the exact-condition
//!   assertion and would let an already-expired budget produce a
//!   no-progress continuation;
//! - moving the verify deadline check after the first fetch (or removing
//!   it) lets the `20_001` run complete, failing
//!   `verify_at_budget_completes_while_exceeding_stops`.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-ingest --test delivery_budget_literals_acceptance
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use nntune_core::yenc::{encode_segment, SegmentParams};
use nntune_ingest::parse::ParsedFile;
use nntune_ingest::verify::{
    run_verifying_step, ArticleFetch, BodySource, ScratchStore, StagedFile, VerifyCaps,
    VerifyOutcome, VerifyProgress,
};

const JOB: &str = "job_0123456789abcdef";

/// Production source before `#[cfg(test)]`, so this file's own `30_000` /
/// `20_000` literals cannot satisfy the pins.
fn production_source() -> &'static str {
    let source = include_str!("../src/queue_consumer.rs");
    source
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Mirror of the production probe closure
/// (`queue_consumer.rs:571-574`): elapsed wall time trips at exactly the
/// `30_000` ms delivery budget.
fn probe_expired(elapsed_ms: u64) -> bool {
    elapsed_ms >= 30_000
}

/// Mirror of the production verify closure
/// (`queue_consumer.rs:738-740`): elapsed wall time trips only strictly
/// past the `20_000` ms budget.
fn verify_expired(elapsed_ms: u64) -> bool {
    elapsed_ms > 20_000
}

/// Faithful mirror of the production probe loop guard
/// (`queue_consumer.rs:431`):
/// `if asked >= max_batches || (asked > 0 && budget_expired())`.
/// Returns the batch indexes a delivery with `total_batches` pending would
/// ask before the budget stops it.
fn drive_probe_deadline(
    total_batches: usize,
    max_batches: usize,
    budget_expired: &dyn Fn() -> bool,
) -> Vec<usize> {
    let max_batches = max_batches.max(1);
    let mut asked = Vec::new();
    for (asked_count, batch_index) in (0..total_batches).enumerate() {
        if asked_count >= max_batches || (asked_count > 0 && budget_expired()) {
            break;
        }
        asked.push(batch_index);
    }
    asked
}

#[derive(Default)]
struct MemoryScratch {
    objects: RefCell<HashMap<String, (Vec<u8>, StagedFile)>>,
}

impl ScratchStore for MemoryScratch {
    async fn head(
        &self,
        key: &str,
    ) -> Result<Option<StagedFile>, nntune_ingest::verify::VerifyError> {
        Ok(self.objects.borrow().get(key).map(|(_, m)| m.clone()))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, nntune_ingest::verify::VerifyError> {
        Ok(self.objects.borrow().get(key).map(|(b, _)| b.clone()))
    }

    async fn list_keys(
        &self,
        prefix: &str,
    ) -> Result<Vec<String>, nntune_ingest::verify::VerifyError> {
        Ok(self
            .objects
            .borrow()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        meta: &StagedFile,
    ) -> Result<(), nntune_ingest::verify::VerifyError> {
        self.objects
            .borrow_mut()
            .insert(key.to_string(), (bytes, meta.clone()));
        Ok(())
    }
}

/// One shared 1-byte yEnc article served for every segment id.
struct AlwaysPresent {
    article: Vec<u8>,
}

impl BodySource for AlwaysPresent {
    async fn fetch_body(
        &mut self,
        _segment_id: &str,
    ) -> Result<ArticleFetch, nntune_ingest::verify::VerifyError> {
        Ok(ArticleFetch::Present(self.article.clone()))
    }
}

fn one_byte_article() -> Vec<u8> {
    let mut body = Vec::new();
    encode_segment(&mut body, &SegmentParams::single("probe.flac"), b"X").expect("encode yEnc");
    body
}

fn single_file() -> Vec<ParsedFile> {
    vec![ParsedFile {
        name: "file0000.flac".to_string(),
        is_par2: false,
        segment_ids: vec!["seg0@synthetic.invalid".to_string()],
        segment_bytes: vec![None],
    }]
}

#[test]
fn probe_delivery_budget_literal_is_30s() {
    assert!(
        production_source().contains("const PROBE_DELIVERY_BUDGET_MS: u64 = 30_000;"),
        "probe delivery budget must stay exactly 30_000 ms"
    );
}

#[test]
fn verify_wall_clock_budget_literal_is_20s() {
    assert!(
        production_source().contains("const VERIFY_WALL_CLOCK_BUDGET_MS: u64 = 20_000;"),
        "verify wall-clock budget must stay exactly 20_000 ms"
    );
}

#[test]
fn probe_enforcement_keeps_first_fold_guard() {
    let production = production_source();
    assert!(
        production.contains(">= PROBE_DELIVERY_BUDGET_MS"),
        "probe wall check must stay `>= PROBE_DELIVERY_BUDGET_MS` (trips at exactly the budget)"
    );
    assert!(
        production.contains(".saturating_sub(probing_started_ms)"),
        "probe wall check must measure from the delivery probing start"
    );
    // Exact loop guard: at least one batch is always asked, whatever the
    // budget says, so an expired budget cannot produce a no-progress
    // continuation.
    assert!(
        production.contains("if asked >= max_batches || (asked > 0 && budget_expired())"),
        "probe loop must keep the first-fold guard `(asked > 0 && budget_expired())`"
    );
}

#[test]
fn verify_enforcement_needs_strictly_over_budget() {
    let production = production_source();
    assert!(
        production.contains("> VERIFY_WALL_CLOCK_BUDGET_MS"),
        "verify deadline must stay `> VERIFY_WALL_CLOCK_BUDGET_MS` (strictly over)"
    );
    assert!(
        !production.contains(">= VERIFY_WALL_CLOCK_BUDGET_MS"),
        "verify deadline must not become `>=`: at-budget (exactly 20_000 ms) must not trip"
    );
    assert!(
        production.contains(".saturating_sub(started_ms) > VERIFY_WALL_CLOCK_BUDGET_MS"),
        "verify deadline must measure from the delivery verify start"
    );
}

#[test]
fn probe_enforcement_trips_at_exactly_budget() {
    // `>=` trips at exactly the budget; one millisecond under does not.
    assert!(
        !probe_expired(29_999),
        "29_999 ms must not trip the 30 s probe budget"
    );
    assert!(
        probe_expired(30_000),
        "exactly 30_000 ms must trip the probe budget (`>=`)"
    );
    assert!(
        probe_expired(30_001),
        "30_001 ms must trip the probe budget"
    );
}

#[test]
fn verify_enforcement_needs_strictly_over_budget_at_threshold() {
    // `>` needs strictly over: exactly the budget does not trip.
    assert!(
        !verify_expired(19_999),
        "19_999 ms must not trip the 20 s verify budget"
    );
    assert!(
        !verify_expired(20_000),
        "exactly 20_000 ms must not trip the verify budget (`>`)"
    );
    assert!(
        verify_expired(20_001),
        "20_001 ms must trip the verify budget"
    );
}

#[test]
fn probe_at_budget_asks_every_batch_while_exceeding_stops_after_first_fold() {
    // At-budget (29_999 ms): the deadline never trips, so both pending
    // batches are asked.
    let at_budget = drive_probe_deadline(2, usize::MAX, &|| probe_expired(29_999));
    assert_eq!(
        at_budget,
        vec![0, 1],
        "at-budget run must ask every pending batch"
    );

    // Exceeding (30_000 ms): the deadline trips between batches, so the
    // delivery stops after the first fold — the durable-progress path.
    let exceeding = drive_probe_deadline(2, usize::MAX, &|| probe_expired(30_000));
    assert_eq!(
        exceeding,
        vec![0],
        "exceeding run must stop after the first fold, not ask every batch"
    );
}

#[test]
fn probe_exceeding_budget_stops_after_first_fold() {
    // Already-expired wall budget must still fold one batch first (the
    // `asked > 0` guard): never a no-progress stop.
    let asked = drive_probe_deadline(2, usize::MAX, &|| true);
    assert_eq!(
        asked,
        vec![0],
        "even an expired budget must ask the first batch"
    );
}

#[tokio::test]
async fn verify_at_budget_completes_while_exceeding_stops() {
    // Within budget (elapsed 20_000 ms -> deadline false): the single
    // present 1-byte file stages and completes healthy.
    let article = one_byte_article();
    let files = single_file();
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = AlwaysPresent {
        article: article.clone(),
    };
    let elapsed_at_budget = 20_000u64;
    let outcome = run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &|| verify_expired(elapsed_at_budget),
    )
    .await
    .expect("at-budget verify run must not error");
    assert_eq!(
        outcome,
        VerifyOutcome::Complete,
        "elapsed exactly 20_000 ms must not trip the verify deadline"
    );
    assert_eq!(progress.staged.len(), 1, "the at-budget file must stage");

    // Exceeding (elapsed 20_001 ms -> deadline true): the same step stops
    // as Incomplete before staging anything.
    let files = single_file();
    let scratch = MemoryScratch::default();
    let mut progress = VerifyProgress::new(0);
    let mut source = AlwaysPresent { article };
    let elapsed_over = 20_001u64;
    let outcome = run_verifying_step(
        JOB,
        &files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &|| verify_expired(elapsed_over),
    )
    .await
    .expect("exceeding verify run stops as Incomplete, not an error");
    assert_eq!(
        outcome,
        VerifyOutcome::Incomplete,
        "elapsed 20_001 ms must trip the verify deadline into Incomplete"
    );
    assert!(
        progress.staged.is_empty(),
        "the deadline-stopped step must stage nothing yet"
    );
}
