use super::*;
use crate::digest_geometry::DigestGeometry;
use crate::flac_frame_header::{FrameHeader, Position};

// Synthetic 1 kHz mono silence, 16-bit PCM. The libFLAC-generated frames
// cover 5,000 + 5,000 + 2,000 samples; each frame occupies 14 bytes.
const STREAMINFO: &[u8] = b"fLaC\x00\x00\x00\x22\x13\x88\x13\x88\x00\x00\x0e\x00\x00\x0e\
    \x00\x3e\x80\xf0\x00\x00\x2e\xe0\xc3\x0c\x3f\x99\xee\xbe\x77\xa5\x0c\xbd\x1e\xf6\xef\x15\xa3\x4b";
const AUDIO: &[u8] = b"\xff\xf8\x7c\x08\x00\x13\x87\x01\x68\x00\x00\x00\x23\x07\
    \xff\xf8\x7c\x08\x01\x13\x87\x01\x7e\x00\x00\x00\x0b\x07\
    \xff\xf8\x7c\x08\x02\x07\xcf\x01\xbe\x00\x00\x00\xee\x21";

fn fixture() -> Vec<u8> {
    let mut input = STREAMINFO.to_vec();
    // Old comment, seek and picture payloads are opaque to the preamble
    // parser. Give them distinct markers to catch accidentally copying them.
    input.extend_from_slice(b"\x04\x00\x00\x03old\x03\x00\x00\x04seek\x86\x00\x00\x03art");
    input.extend_from_slice(AUDIO);
    input
}

fn spans() -> [FrameSpan; 3] {
    std::array::from_fn(|i| FrameSpan {
        first_sample: 5000 * i as u64,
        samples: [5000, 5000, 2000][i],
        bytes: 14 * i..14 * (i + 1),
    })
}

#[test]
fn rewrites_metadata_and_keeps_exact_audio_frames() {
    let input = fixture();
    let prepared = prepare(&input, &[("TITLE", "Silent")], spans()).unwrap();
    let (preamble, audio_tail) = prepared.parts();
    let expected_tags = b"\x06\x00\x00\x00NNTune\x01\x00\x00\x00\x0c\x00\x00\x00TITLE=Silent";
    assert_eq!(&preamble[..42], STREAMINFO);
    assert_eq!(&preamble[42..46], &[4, 0, 0, expected_tags.len() as u8]);
    assert_eq!(&preamble[46..76], expected_tags);
    assert_eq!(&preamble[76..80], &[0x83, 0, 0, 54]);
    assert_eq!(preamble.len(), 134);
    assert_eq!(audio_tail, AUDIO);
    assert_eq!(
        audio_tail.as_ptr(),
        input[input.len() - AUDIO.len()..].as_ptr()
    );

    let output = [preamble, audio_tail].concat();
    let (info, audio) = FlacPreamble::default().push(&output).unwrap().unwrap();
    let (points, remainder) = preamble[80..].as_chunks::<18>();
    assert!(remainder.is_empty());
    for (i, point) in points.iter().enumerate() {
        let sample = u64::from_be_bytes(point[..8].try_into().unwrap());
        let offset = u64::from_be_bytes(point[8..16].try_into().unwrap());
        let count = u16::from_be_bytes(point[16..].try_into().unwrap());
        assert_eq!((sample, offset), (5000 * i as u64, 14 * i as u64));
        assert_eq!(count, [5000, 5000, 2000][i]);
        let header = FrameHeader::parse(&audio[offset as usize..], &info)
            .unwrap()
            .unwrap();
        assert_eq!(header.position, Position::Frame(i as u32));
        assert_eq!(header.block_size, count);
    }
}

#[test]
fn seeks_in_prepared_plaintext_and_maps_complete_sealed_chunks() {
    let input = fixture();
    // Large replacement tags put the second frame across a sealed chunk cut.
    let title = "a".repeat(65_387);
    let prepared = prepare(&input, &[("TITLE", &title)], spans()).unwrap();
    assert_eq!(prepared.preamble.len(), 65_515);
    assert_eq!(prepared.plaintext_range(0..5000), Some(65_515..65_529));
    assert_eq!(prepared.plaintext_range(5000..10_000), Some(65_529..65_543));
    assert_eq!(prepared.plaintext_range(4999..5001), Some(65_515..65_543));
    assert_eq!(
        prepared.plaintext_range(11_999..u64::MAX),
        Some(65_543..65_557)
    );
    assert_eq!(prepared.plaintext_range(12_000..12_001), None);
    assert_eq!(prepared.plaintext_range(5000..5000), None);

    let stored_len = (prepared.preamble.len() + prepared.audio.len()) as u64;
    let range = prepared.plaintext_range(5001..5002).unwrap();
    let plan = DigestGeometry::new(stored_len)
        .unwrap()
        .range(range.start, range.end)
        .unwrap();
    let first = plan.sealed_chunk(0).unwrap();
    let last = plan.sealed_chunk(1).unwrap();
    assert_eq!(first.ciphertext, 48..65_600);
    assert_eq!(last.ciphertext, 65_600..65_637);
    assert_eq!(first.selection.start..first.selection.end, 65_529..65_536);
    assert_eq!(last.selection.start..last.selection.end, 0..7);
    assert!(!first.final_chunk);
    assert!(last.final_chunk);
}

#[test]
fn refuses_truncated_metadata_missing_frames_and_inconsistent_geometry() {
    let input = fixture();
    assert_eq!(
        prepare(&input[..43], &[], spans()).err(),
        Some(PrepareError::Preamble)
    );
    assert_eq!(
        prepare(&input[..input.len() - 1], &[], spans()).err(),
        Some(PrepareError::FrameGeometry)
    );
    assert_eq!(
        prepare(&input, &[], []).err(),
        Some(PrepareError::FrameGeometry)
    );
    assert_eq!(
        prepare(&input, &[], spans().into_iter().take(2)).err(),
        Some(PrepareError::FrameGeometry)
    );
    let mut bad_sample = spans();
    bad_sample[2].samples -= 1;
    assert_eq!(
        prepare(&input, &[], bad_sample).err(),
        Some(PrepareError::FrameGeometry)
    );
    let mut gap = spans();
    gap[1].bytes.start += 1;
    assert_eq!(
        prepare(&input, &[], gap).err(),
        Some(PrepareError::FrameGeometry)
    );
}

#[test]
fn bounds_replacement_comments_without_reintroducing_art() {
    let exact_fit = "x".repeat(MAX_COMMENT_BYTES - 20);
    assert_eq!(
        vorbis_comments(&[("A", &exact_fit)]).unwrap().len(),
        MAX_COMMENT_BYTES
    );
    assert_eq!(
        vorbis_comments(&[("A", &exact_fit), ("B", "b")]),
        Err(PrepareError::Tags)
    );
    for key in ["", "BAD=KEY", "coverart", "Metadata_Block_Picture"] {
        assert_eq!(vorbis_comments(&[(key, "x")]), Err(PrepareError::Tags));
    }
}
