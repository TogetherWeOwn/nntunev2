//! Generated-data original range composition; no service binding or real dial.
use super::*;
use nntune_core::original_source::{OriginalSource, SourceError, PAYLOAD_FORMAT};
use nntune_stream::account_operation::Operation;
use nntune_stream::account_playback::{
    read_original_range, OriginalCatalogue, OriginalRangeReceipt, OriginalRangeRequest,
};
use std::ops::Range;

struct Sources {
    value: Option<OriginalSource>,
    calls: Cell<usize>,
    clock: Rc<Cell<u64>>,
    advance_ms: u64,
    hangs: bool,
    dropped: Rc<Cell<bool>>,
}
impl OriginalCatalogue for Sources {
    async fn source(&self, track_id: &str) -> Result<Option<OriginalSource>, CatalogueUnavailable> {
        assert_eq!(track_id, uid(50));
        self.calls.set(self.calls.get() + 1);
        self.clock.set(self.clock.get() + self.advance_ms);
        if self.hangs {
            let _flag = DropFlag(self.dropped.clone());
            std::future::pending::<()>().await;
        }
        Ok(self.value.clone())
    }
}
fn token() -> String {
    grant::issue_v2(USER, &uid(50), PLAY, EPOCH, NOW_S + 600, SECRET)
}
fn reference(i: usize) -> String {
    format!("generated-{i}@fixture.invalid")
}
fn article(data: &[u8], index: usize, count: usize, offset: usize, size: usize) -> Vec<u8> {
    let mut out = if count == 1 {
        format!("=ybegin line=128 size={size} name=generated\r\n").into_bytes()
    } else {
        format!("=ybegin part={} total={count} line=128 size={size} name=generated\r\n=ypart begin={} end={}\r\n", index+1, offset+1, offset+data.len()).into_bytes()
    };
    nntune_core::yenc::encode_body(&mut out, data).unwrap();
    let crc = nntune_stream::yenc::crc32(data);
    out.extend(
        format!(
            "\r\n=yend size={} {}={:08x}\r\n",
            data.len(),
            if count == 1 { "crc32" } else { "pcrc32" },
            crc
        )
        .bytes(),
    );
    out
}
fn setup(sizes: &[usize]) -> (Sources, Fake, Rc<Cell<u64>>) {
    let clk = clock();
    let size: usize = sizes.iter().sum();
    let value = json!({
        "track_id": uid(50), "source_job_id": "generated-job", "file_ordinal": 2,
        "file_size": size, "article_count": sizes.len(),
        "articles": sizes.iter().enumerate().map(|(i, _)| json!({
            "number": i+1, "reference": reference(i), "encoded_bytes": 256,
        })).collect::<Vec<_>>(),
        // Intentionally stale: actual contiguous geometry is the authority.
        "range_hint": {"part_size": 3, "final_begin": 0, "final_end": 1},
    });
    let source = OriginalSource::parse(
        PAYLOAD_FORMAT,
        &serde_json::to_vec(&value).unwrap(),
        &uid(50),
        "generated-job",
    )
    .unwrap();
    let sources = Sources {
        value: Some(source),
        calls: Cell::new(0),
        clock: clk.clone(),
        advance_ms: 0,
        hangs: false,
        dropped: Rc::new(Cell::new(false)),
    };
    let fake = Fake::one(OperationReply::NotFound);
    let mut offset = 0;
    for (i, &n) in sizes.iter().enumerate() {
        let data: Vec<u8> = (offset..offset + n).map(|v| v as u8).collect();
        fake.0.bodies.borrow_mut().insert(
            reference(i),
            served(
                1,
                Output::Body(article(&data, i, sizes.len(), offset, size)),
            ),
        );
        offset += n;
    }
    (sources, fake, clk)
}
async fn read_with(
    sources: &Sources,
    fake: &Fake,
    clk: &Rc<Cell<u64>>,
    token: &str,
    range: Range<u64>,
    deadline: impl Future<Output = ()>,
    sink: impl FnMut(&[u8]) -> Result<(), SourceError>,
) -> Result<OriginalRangeReceipt, ApiError> {
    tokio::time::timeout(
        GUARD,
        read_original_range(
            sources,
            fake,
            fake,
            keys(),
            SECRET,
            OriginalRangeRequest {
                token,
                track_id: &uid(50),
                range,
            },
            || clk.get(),
            deadline,
            sink,
        ),
    )
    .await
    .expect("range composition must terminate under guard")
}
fn operations(fake: &Fake) -> Vec<OperationRequest> {
    fake.0
        .execute_calls
        .borrow()
        .iter()
        .map(|(_, _, body)| serde_json::from_slice(body).unwrap())
        .collect()
}

#[tokio::test]
async fn nonuniform_range_uses_final_proof_prefix_walk_and_one_account() {
    let (sources, fake, clk) = setup(&[3, 2, 4, 2]);
    let mut out = Vec::new();
    let receipt = read_with(
        &sources,
        &fake,
        &clk,
        &token(),
        4..10,
        std::future::pending(),
        |bytes| {
            out.extend_from_slice(bytes);
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(out, (4..10).collect::<Vec<_>>());
    assert_eq!(receipt.verified_bytes, 6);
    assert_eq!(receipt.accounts, vec![uid(1); 4]);
    assert_eq!(receipt.grant.play_id.as_deref(), Some(PLAY));
    assert_eq!(receipt.grant.track_id, uid(50));
    assert_eq!(sources.calls.get(), 1);
    let ops = operations(&fake);
    assert_eq!(
        ops.iter()
            .map(|r| match &r.operation {
                Operation::Body(id) => id.clone(),
                _ => panic!("BODY only"),
            })
            .collect::<Vec<_>>(),
        [3, 0, 1, 2].map(reference)
    );
    assert!(ops.iter().all(|r| r.class == WorkClass::Stream));
}

#[tokio::test]
async fn early_range_still_proves_final_part_without_reading_whole_file() {
    let (sources, fake, clk) = setup(&[3, 3, 2]);
    let mut out = Vec::new();
    let receipt = read_with(
        &sources,
        &fake,
        &clk,
        &token(),
        1..2,
        std::future::pending(),
        |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(out, [1]);
    assert_eq!(receipt.accounts.len(), 2);
    assert_eq!(fake.executes(), 2);
    assert!(fake.0.bodies.borrow().contains_key(&reference(1)));
}

#[tokio::test]
async fn single_part_is_fetched_once_not_refetched_for_proof() {
    let (sources, fake, clk) = setup(&[4]);
    let mut out = Vec::new();
    assert_eq!(
        read_with(
            &sources,
            &fake,
            &clk,
            &token(),
            1..4,
            std::future::pending(),
            |b| {
                out.extend_from_slice(b);
                Ok(())
            }
        )
        .await
        .unwrap()
        .verified_bytes,
        3
    );
    assert_eq!(out, [1, 2, 3]);
    assert_eq!(fake.executes(), 1);
}

#[tokio::test]
async fn invalid_grants_including_ready_deadline_touch_no_source_or_pool() {
    let bad = [
        String::new(),
        "bad".into(),
        grant::issue(USER, &uid(50), NOW_S + 60, SECRET),
        grant::issue_v2(USER, &uid(50), PLAY, EPOCH, NOW_S - 1, SECRET),
    ];
    for t in bad {
        let (sources, fake, clk) = setup(&[3]);
        let err = read_with(
            &sources,
            &fake,
            &clk,
            &t,
            0..3,
            std::future::ready(()),
            |_| panic!("no output"),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.status, 401);
        assert_eq!(sources.calls.get(), 0);
        assert_eq!(fake.reads(), 0);
    }
    let (sources, fake, clk) = setup(&[3]);
    let t = grant::issue_v2(USER, &uid(51), PLAY, EPOCH, NOW_S + 60, SECRET);
    assert_eq!(
        read_with(
            &sources,
            &fake,
            &clk,
            &t,
            0..3,
            std::future::pending(),
            |_| panic!("no output")
        )
        .await
        .err()
        .unwrap()
        .status,
        403
    );
    assert_eq!(sources.calls.get(), 0);
}

#[tokio::test]
async fn source_missing_or_bound_to_another_track_never_dispatches() {
    for missing in [true, false] {
        let (mut sources, fake, clk) = setup(&[3]);
        sources.value = if missing {
            None
        } else {
            let mut v: Value =
                serde_json::from_slice(&sources.value.take().unwrap().encode().unwrap()).unwrap();
            v["track_id"] = uid(51).into();
            Some(
                OriginalSource::parse(
                    PAYLOAD_FORMAT,
                    &serde_json::to_vec(&v).unwrap(),
                    &uid(51),
                    "generated-job",
                )
                .unwrap(),
            )
        };
        let err = read_with(
            &sources,
            &fake,
            &clk,
            &token(),
            0..3,
            std::future::pending(),
            |_| panic!("no output"),
        )
        .await
        .err()
        .unwrap();
        generic_upstream(&err);
        assert_eq!(fake.reads(), 0);
    }
}

#[tokio::test]
async fn invalid_ranges_dispatch_nothing() {
    for range in [0..0, 3..4, Range { start: 2, end: 1 }, 0..u64::MAX] {
        let (sources, fake, clk) = setup(&[3]);
        assert_eq!(
            read_with(
                &sources,
                &fake,
                &clk,
                &token(),
                range,
                std::future::pending(),
                |_| panic!("no output")
            )
            .await
            .err()
            .unwrap()
            .status,
            416
        );
        assert_eq!(fake.reads(), 0);
    }
}

#[tokio::test]
async fn grant_expiry_during_lookup_or_body_prevents_output() {
    for during_lookup in [true, false] {
        let (mut sources, fake, clk) = setup(&[3]);
        if during_lookup {
            sources.advance_ms = 700_000;
        } else {
            let clk = clk.clone();
            *fake.0.after_execute.borrow_mut() = Some(Box::new(move || clk.set(NOW_MS + 700_000)));
        }
        assert_eq!(
            read_with(
                &sources,
                &fake,
                &clk,
                &token(),
                0..3,
                std::future::pending(),
                |_| panic!("no expired output")
            )
            .await
            .err()
            .unwrap()
            .status,
            401
        );
        assert_eq!(fake.executes(), usize::from(!during_lookup));
    }
}

#[tokio::test]
async fn corrupt_final_proof_releases_no_prefix() {
    let (sources, fake, clk) = setup(&[3, 3, 2]);
    // Valid CRC, wrong final geometry/whole-file size.
    fake.0.bodies.borrow_mut().insert(
        reference(2),
        served(1, Output::Body(article(&[6, 7], 2, 3, 6, 9))),
    );
    let err = read_with(
        &sources,
        &fake,
        &clk,
        &token(),
        0..1,
        std::future::pending(),
        |_| panic!("no unproven prefix"),
    )
    .await
    .err()
    .unwrap();
    generic_upstream(&err);
    assert_eq!(fake.executes(), 1);
}

#[tokio::test]
async fn later_failure_aborts_partial_output_without_retry_or_success_receipt() {
    let (sources, fake, clk) = setup(&[3, 3, 2]);
    fake.0.bodies.borrow_mut().insert(
        reference(1),
        OperationReply::Failed(OperationError::Unavailable),
    );
    let mut out = Vec::new();
    let err = read_with(
        &sources,
        &fake,
        &clk,
        &token(),
        0..8,
        std::future::pending(),
        |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .await
    .err()
    .unwrap();
    generic_upstream(&err);
    assert_eq!(out, [0, 1, 2]);
    assert_eq!(fake.executes(), 3);
}

#[tokio::test]
async fn checksum_corruption_or_gap_cannot_be_repaired_by_range_hint() {
    for gap in [true, false] {
        let (sources, fake, clk) = setup(&[3, 3, 2]);
        let mut body = article(&[0, 1, 2], 0, 3, usize::from(gap), 8);
        if !gap {
            let i = body.windows(7).position(|w| w == b"pcrc32=").unwrap() + 7;
            body[i] = if body[i] == b'0' { b'1' } else { b'0' };
        }
        fake.0
            .bodies
            .borrow_mut()
            .insert(reference(0), served(1, Output::Body(body)));
        generic_upstream(
            &read_with(
                &sources,
                &fake,
                &clk,
                &token(),
                0..8,
                std::future::pending(),
                |_| panic!("no corrupt output"),
            )
            .await
            .err()
            .unwrap(),
        );
        assert_eq!(fake.executes(), 2);
    }
}

#[tokio::test]
async fn sink_failure_stops_more_provider_work() {
    let (sources, fake, clk) = setup(&[3, 3, 2]);
    generic_upstream(
        &read_with(
            &sources,
            &fake,
            &clk,
            &token(),
            0..8,
            std::future::pending(),
            |_| Err(SourceError::Transport),
        )
        .await
        .err()
        .unwrap(),
    );
    assert_eq!(fake.executes(), 2);
}

#[tokio::test]
async fn expiry_in_sink_stops_the_walk_before_another_dispatch() {
    let (sources, fake, clk) = setup(&[3, 3, 2]);
    generic_upstream(
        &read_with(
            &sources,
            &fake,
            &clk,
            &token(),
            0..8,
            std::future::pending(),
            |_| {
                clk.set(NOW_MS + 700_000);
                Ok(())
            },
        )
        .await
        .err()
        .unwrap(),
    );
    assert_eq!(fake.executes(), 2);
}

#[tokio::test]
async fn whole_call_deadline_cancels_catalogue_or_dispatched_operation() {
    for lookup in [true, false] {
        let (mut sources, fake, clk) = setup(&[3]);
        sources.hangs = lookup;
        fake.0.execute_hangs.set(!lookup);
        generic_upstream(
            &read_with(
                &sources,
                &fake,
                &clk,
                &token(),
                0..3,
                tokio::time::sleep(SHORT),
                |_| panic!("no output"),
            )
            .await
            .err()
            .unwrap(),
        );
        assert_eq!(fake.executes(), usize::from(!lookup));
        assert!(if lookup {
            sources.dropped.get()
        } else {
            fake.0.execute_dropped.get()
        });
    }
}
