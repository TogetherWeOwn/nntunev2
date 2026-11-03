//! NZB writer acceptance: [`nntune_core::nzb_writer`] output parses with
//! the existing [`nntune_core::nzb`] parser, preserving per-segment byte
//! sizes exactly.
//!
//! Message-IDs below are synthetic (`*.test.post.invalid`); they are never
//! real article references.

use nntune_core::nzb::parse;
use nntune_core::nzb_writer::{write_nzb, FileInput, SegmentInput};

fn test_id(file: usize, segment: usize) -> String {
    format!("seg{segment}.9d2c4b@test-post-{file}.test.post.invalid")
}

fn posted_file(file: usize, segments: &[(i64,)]) -> FileInput {
    FileInput {
        subject: format!("obfuscated volume {file:04} [1/{}]", segments.len()),
        poster: "poster@test.post.invalid".into(),
        date: 1_800_000_000 + file as i64,
        groups: vec!["alt.binaries.test".into()],
        segments: segments
            .iter()
            .enumerate()
            .map(|(i, (bytes,))| SegmentInput {
                number: i + 1,
                message_id: test_id(file, i + 1),
                encoded_bytes: *bytes,
            })
            .collect(),
    }
}

#[test]
fn writer_output_parses_with_exact_byte_sizes() {
    let files = vec![
        posted_file(1, &[(725_000,), (725_000,), (41_208,)]),
        posted_file(2, &[(18_412,)]),
    ];
    let xml = write_nzb(&files).unwrap();
    let doc = parse(&xml).unwrap();
    assert_eq!(doc.files.len(), 2);
    assert_eq!(doc.encoded_bytes, 725_000 + 725_000 + 41_208 + 18_412);
    for (file, want) in doc.files.iter().zip(&files) {
        assert_eq!(file.subject, want.subject);
        assert_eq!(file.posted_at_unix, want.date);
        assert_eq!(file.segments.len(), want.segments.len());
        for (segment, want_segment) in file.segments.iter().zip(&want.segments) {
            assert_eq!(segment.number, want_segment.number);
            assert_eq!(segment.message_id, want_segment.message_id);
            assert_eq!(segment.encoded_bytes, want_segment.encoded_bytes);
        }
    }
    // The segment digest pins the exact segment set the poster sealed.
    assert_eq!(doc.segment_digest.len(), 32);
}

#[test]
fn writer_rejects_wrapped_message_ids() {
    let mut file = posted_file(1, &[(100,)]);
    file.segments[0].message_id = format!("<{}>", test_id(1, 1));
    assert!(write_nzb(&[file]).is_err());
}

#[test]
fn writer_rejects_gapped_segment_numbers() {
    let mut file = posted_file(1, &[(100,), (100,)]);
    file.segments[1].number = 7;
    assert!(write_nzb(&[file]).is_err());
}
