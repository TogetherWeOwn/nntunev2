use super::*;
use crate::digest_geometry::DigestGeometry;
use crate::flac_frame_header::{FrameHeader, Position};

// Synthetic 1 kHz mono silence, 16-bit PCM. The libFLAC-generated frames
// cover four 2,500-sample blocks and a 2,000-sample tail; each is 14 bytes.
const STREAMINFO: &[u8] = b"fLaC\x00\x00\x00\x22\x09\xc4\x09\xc4\x00\x00\x0e\x00\x00\x0e\
    \x00\x3e\x80\xf0\x00\x00\x2e\xe0\xc3\x0c\x3f\x99\xee\xbe\x77\xa5\x0c\xbd\x1e\xf6\xef\x15\xa3\x4b";
const AUDIO: &[u8] = b"\xff\xf8\x7c\x08\x00\x09\xc3\x01\x42\x00\x00\x00\xd0\xe9\
    \xff\xf8\x7c\x08\x01\x09\xc3\x01\x54\x00\x00\x00\xf8\xe9\
    \xff\xf8\x7c\x08\x02\x09\xc3\x01\x6e\x00\x00\x00\x80\xe9\
    \xff\xf8\x7c\x08\x03\x09\xc3\x01\x78\x00\x00\x00\xa8\xe9\
    \xff\xf8\x7c\x08\x04\x07\xcf\x01\xca\x00\x00\x00\x1e\x21";
const OLD_TITLE: &[u8] = b"\0\0\0\0\x01\0\0\0\x09\0\0\0TITLE=Old";
const OLD_LFE: &[u8] =
    b"\0\0\0\0\x02\0\0\0\x09\0\0\0TITLE=Old\x28\0\0\0waveformatextensible_channel_mask=0X0008";

fn fixture(comment: &[u8]) -> Vec<u8> {
    let mut input = STREAMINFO.to_vec();
    block_header(&mut input, 4, comment.len());
    input.extend_from_slice(comment);
    input.extend_from_slice(b"\x03\x00\x00\x04seek\x86\x00\x00\x03art");
    input.extend_from_slice(AUDIO);
    input
}

fn spans() -> [FrameSpan; 5] {
    std::array::from_fn(|i| FrameSpan {
        first_sample: 2500 * i as u64,
        samples: if i < 4 { 2500 } else { 2000 },
        bytes: 14 * i..14 * (i + 1),
    })
}

#[test]
fn rewrites_metadata_and_keeps_layout_and_exact_audio_frames() {
    let input = fixture(OLD_LFE);
    let prepared = prepare(&input, &[("TITLE", "Silent")], spans()).unwrap();
    let (preamble, audio_tail) = prepared.parts();
    let expected_tags = b"\x06\0\0\0NNTune\x02\0\0\0\x0c\0\0\0TITLE=Silent\
        \x25\0\0\0WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x8";
    let seek = 46 + expected_tags.len();
    assert_eq!(&preamble[..42], STREAMINFO);
    assert_eq!(&preamble[42..46], &[4, 0, 0, expected_tags.len() as u8]);
    assert_eq!(&preamble[46..seek], expected_tags);
    assert_eq!(&preamble[seek..seek + 4], &[0x83, 0, 0, 54]);
    assert_eq!(audio_tail, AUDIO);
    assert_eq!(
        audio_tail.as_ptr(),
        input[input.len() - AUDIO.len()..].as_ptr()
    );

    let output = [preamble, audio_tail].concat();
    let (info, audio) = FlacPreamble::default().push(&output).unwrap().unwrap();
    let (points, remainder) = preamble[seek + 4..].as_chunks::<18>();
    assert!(remainder.is_empty());
    assert_eq!(points.len(), 3);
    for (i, point) in points.iter().enumerate() {
        let sample = u64::from_be_bytes(point[..8].try_into().unwrap());
        let offset = u64::from_be_bytes(point[8..16].try_into().unwrap());
        let count = u16::from_be_bytes(point[16..].try_into().unwrap());
        assert_eq!((sample, offset), (5000 * i as u64, 28 * i as u64));
        assert_eq!(count, [2500, 2500, 2000][i]);
        let header = FrameHeader::parse(&audio[offset as usize..], &info)
            .unwrap()
            .unwrap();
        assert_eq!(header.position, Position::Frame(2 * i as u32));
        assert_eq!(header.block_size, count);
    }
}

#[test]
fn seeks_in_prepared_plaintext_and_maps_complete_sealed_chunks() {
    let input = fixture(OLD_TITLE);
    // Large replacement tags put the third frame across a sealed chunk cut.
    let title = "a".repeat(65_373);
    let prepared = prepare(&input, &[("TITLE", &title)], spans()).unwrap();
    assert_eq!(prepared.preamble.len(), 65_501);
    assert_eq!(prepared.plaintext_range(0..5000), Some(65_501..65_529));
    // A request inside the unindexed second frame starts at the first anchor
    // and covers both complete frames before the next anchor.
    assert_eq!(prepared.plaintext_range(2501..2502), Some(65_501..65_529));
    assert_eq!(prepared.plaintext_range(5000..10_000), Some(65_529..65_557));
    assert_eq!(prepared.plaintext_range(4999..5001), Some(65_501..65_557));
    assert_eq!(
        prepared.plaintext_range(11_999..u64::MAX),
        Some(65_557..65_571)
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
    assert_eq!(last.ciphertext, 65_600..65_651);
    assert_eq!(first.selection.start..first.selection.end, 65_529..65_536);
    assert_eq!(last.selection.start..last.selection.end, 0..21);
    assert!(!first.final_chunk);
    assert!(last.final_chunk);
}

fn assert_bad_geometry(input: &[u8], frames: impl IntoIterator<Item = FrameSpan>) {
    let error = prepare(input, &[], frames).err();
    assert_eq!(error, Some(PrepareError::FrameGeometry));
}

#[test]
fn refuses_truncated_metadata_missing_frames_and_inconsistent_geometry() {
    let input = fixture(OLD_TITLE);
    let bad_comment = fixture(&OLD_LFE[..OLD_LFE.len() - 1]);
    for malformed in [&input[..43], &bad_comment] {
        let error = prepare(malformed, &[], spans()).err();
        assert_eq!(error, Some(PrepareError::Preamble));
    }
    assert_bad_geometry(&input[..input.len() - 1], spans());
    assert_bad_geometry(&input, []);
    assert_bad_geometry(&input, spans().into_iter().take(2));
    let mut bad_sample = spans();
    bad_sample[2].first_sample += 1;
    assert_bad_geometry(&input, bad_sample);
    let mut gap = spans();
    gap[1].bytes.start += 1;
    assert_bad_geometry(&input, gap);
}

#[test]
fn only_the_final_frame_can_be_shorter_than_streaminfo_minimum() {
    let input = fixture(OLD_TITLE);
    let mut frames = spans();
    // Keep byte and sample continuity and the total; only the minimum is wrong.
    frames[0].samples -= 1;
    for frame in &mut frames[1..] {
        frame.first_sample -= 1;
    }
    frames[4].samples += 1;
    assert_bad_geometry(&input, frames);
    assert!(prepare(&input, &[], spans()).is_ok());
}

#[test]
fn bounds_replacement_comments_without_reintroducing_art() {
    let exact_fit = "x".repeat(MAX_COMMENT_BYTES - 20);
    let exact_fit = [("A", exact_fit.as_str())];
    let comments = vorbis_comments(&exact_fit, None).unwrap();
    assert_eq!(comments.len(), MAX_COMMENT_BYTES);
    assert_eq!(
        vorbis_comments(&exact_fit, Some(8)),
        Err(PrepareError::Tags)
    );
    for key in [
        "",
        "BAD=KEY",
        "coverart",
        "Metadata_Block_Picture",
        CHANNEL_MASK,
    ] {
        let result = vorbis_comments(&[(key, "0x4")], Some(8));
        assert_eq!(result, Err(PrepareError::Tags));
    }
}
