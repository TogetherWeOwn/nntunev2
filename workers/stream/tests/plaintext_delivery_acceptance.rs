use nntune_core::digest_enrollment::DigestEnrollment;
use nntune_core::digest_geometry::DigestGeometry;
use nntune_core::digest_range::{DigestRange, OpenPart, RangeError};
use nntune_core::media::FileKey;
use nntune_stream::delivered::{DeliveredInterval, DeliveredSink, VecDeliveredSink};
use nntune_stream::meter_sink::MeterBook;
use nntune_stream::range::{plan_encrypted_range, RangeOutcome};

const FILE_BYTES: u64 = 65_539;

// Fake response adapter: true acknowledges the entire offered slice. Fetching,
// authenticating or dropping a part is not an acknowledgment.
fn deliver(
    sink: &mut impl DeliveredSink,
    play_id: &str,
    start: u64,
    part: Result<Option<OpenPart>, RangeError>,
    write: impl FnOnce(&[u8]) -> bool,
) -> Result<(), RangeError> {
    let Some(part) = part? else { return Ok(()) };
    let bytes = part.as_slice();
    if !bytes.is_empty() && write(bytes) {
        sink.delivered(DeliveredInterval {
            user_id: "user-1".into(),
            play_id: play_id.into(),
            track_id: "track-1".into(),
            start,
            end: start + bytes.len() as u64,
            file_bytes: Some(FILE_BYTES),
        });
    }
    Ok(())
}

#[test]
fn meters_only_accepted_trimmed_plaintext_per_play() {
    let mut id = [1; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    let mut map_id = id;
    map_id[0] = 2;
    let mut enrollment = DigestEnrollment::new(FILE_BYTES, [id; 5], map_id).unwrap();
    let key = FileKey::from_bytes(*enrollment.key().expose_secret());
    let header = enrollment.header().to_bytes();
    let mut plaintext = vec![0; 65_536];
    plaintext[65_535] = 11;
    let first = enrollment.seal_chunk(&plaintext).unwrap();
    let last = enrollment.seal_chunk(&[22, 33, 44]).unwrap();
    let page = last.page.unwrap();
    let manifest = enrollment.finish().unwrap();
    let wire: Vec<_> = header
        .into_iter()
        .chain(first.bytes)
        .chain(last.bytes)
        .collect();

    let geometry = DigestGeometry::new(FILE_BYTES).unwrap();
    let (outcome, plan) = plan_encrypted_range(Some("bytes=65535-"), geometry);
    let RangeOutcome::Partial(range) = outcome else {
        panic!("expected a partial range")
    };
    let plan = plan.unwrap();
    let fetch = |index| {
        let span = plan.sealed_chunk(index).unwrap().ciphertext;
        wire[span.start as usize..span.end as usize].to_vec()
    };
    let cursor = || DigestRange::new(&manifest, &key, range.start, range.end + 1).unwrap();
    let open = |reader: &mut DigestRange<'_>, index, bytes| {
        reader
            .open_next(
                index,
                &page,
                bytes,
                (index == 0).then_some(header.as_slice()),
            )
            .map(Some)
    };
    let reject_offer =
        |_: &[u8]| -> bool { panic!("empty or unverified offer reached the writer") };
    let accept = |expected: &'static [u8]| {
        move |bytes: &[u8]| {
            assert_eq!(bytes, expected);
            true
        }
    };
    let mut sink = VecDeliveredSink::default();

    deliver(&mut sink, "empty", range.start, Ok(None), reject_offer).unwrap();
    let fetched = fetch(0);
    assert_eq!(fetched, fetch(0)); // Repeated ciphertext fetch, with no delivery.
    let aborted = open(&mut cursor(), 0, fetched);
    deliver(&mut sink, "aborted", range.start, aborted, |_| false).unwrap();
    assert!(sink.0.is_empty());

    let mut reader = cursor();
    let first = open(&mut reader, 0, fetch(0));
    deliver(&mut sink, "play-1", range.start, first, accept(&[11])).unwrap();
    let mut damaged = fetch(1);
    *damaged.last_mut().unwrap() ^= 1;
    let failed = open(&mut reader, 1, damaged);
    assert_eq!(
        deliver(&mut sink, "play-1", 65_536, failed, reject_offer),
        Err(RangeError::DigestMismatch),
    );
    assert_eq!(reader.next_indices(), None);
    assert_eq!(sink.0.len(), 1);
    assert_eq!(sink.0[0].len(), 1);

    // A fresh response for the same play can redeliver an accepted span;
    // deliberately playing the track again uses a different play id.
    for play_id in ["play-1", "play-2"] {
        let mut reader = cursor();
        for (index, start, expected) in
            [(0, range.start, &[11][..]), (1, 65_536, &[22, 33, 44][..])]
        {
            let part = open(&mut reader, index, fetch(index));
            deliver(&mut sink, play_id, start, part, accept(expected)).unwrap();
        }
        assert_eq!(reader.next_indices(), None);
    }

    let mut meter = MeterBook::new("user-1");
    let mut intervals = sink.0.into_iter();
    for (play_id, start, end) in [
        ("play-1", 65_535, 65_536),
        ("play-1", 65_535, 65_536),
        ("play-1", 65_536, FILE_BYTES),
        ("play-2", 65_535, 65_536),
        ("play-2", 65_536, FILE_BYTES),
    ] {
        let interval = intervals.next().unwrap();
        assert_eq!(
            (interval.play_id.as_str(), interval.start, interval.end),
            (play_id, start, end)
        );
        assert!(meter.record(&interval, 1));
    }
    assert!(intervals.next().is_none());
    let windows = meter.pending_windows();
    assert_eq!(windows.len(), 2);
    for window in windows {
        assert_eq!(window.union_bytes, 4);
        assert_eq!(window.file_bytes, Some(FILE_BYTES));
    }
}
