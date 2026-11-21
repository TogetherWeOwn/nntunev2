use super::*;
use nntune_core::{digest_range::RangeError, digest_verifier::PageError};
use nntune_stream::s3_range_response::S3RangeResponseError;

#[test]
fn mismatched_lookup_pins_do_not_open_a_cursor() {
    let f = Fixture::new();
    for pins in [
        DigestReadSnapshot {
            environment_id: id(8),
            ..f.pins()
        },
        DigestReadSnapshot {
            library_id: id(8),
            ..f.pins()
        },
        DigestReadSnapshot {
            generation_id: id(8),
            ..f.pins()
        },
        DigestReadSnapshot {
            plaintext_len: 1,
            ..f.pins()
        },
        DigestReadSnapshot {
            plaintext_sha256: [0; 32],
            ..f.pins()
        },
        DigestReadSnapshot {
            map_id: id(8),
            ..f.pins()
        },
        DigestReadSnapshot {
            media_version: "wrong-media",
            ..f.pins()
        },
        DigestReadSnapshot {
            page_versions: &[],
            ..f.pins()
        },
    ] {
        assert!(matches!(
            EnrolledPlayback::start(Some("bytes=0-"), pins, f.media_input(), now()),
            Err(PlaybackError::Binding)
        ));
    }
    let mut play = EnrolledPlayback::start(Some("bytes=65536-"), f.pins(), f.media_input(), now())
        .unwrap()
        .1
        .unwrap();
    assert!(matches!(
        play.next_chunk(f.pins().usable_until),
        Err(PlaybackError::Expired)
    ));
    assert!(matches!(
        play.next_chunk(now()),
        Err(PlaybackError::Stopped)
    ));
    assert!(matches!(
        EnrolledPlayback::start(Some("bytes=65537-"), f.pins(), f.media_input(), now()),
        Ok((RangeOutcome::Unsatisfiable, None))
    ));
}

#[tokio::test]
async fn partial_write_cannot_acknowledge_more_than_offered() {
    let f = Fixture::new();
    let mut play =
        EnrolledPlayback::start(Some("bytes=65534-65535"), f.pins(), f.media_input(), now())
            .unwrap()
            .1
            .unwrap();
    let part = play
        .open_next(
            f.media.locator(),
            f.response(true),
            Body(&f.first),
            f.page(),
            Some(&f.context.header),
            now,
        )
        .await
        .unwrap();
    assert_eq!(part.as_slice(), b"AA");
    play.written(1).unwrap();
    assert!(matches!(play.written(2), Err(PlaybackError::Delivery)));
    assert_eq!(play.served_bytes(), 1);
    assert!(matches!(play.finish(), Err(PlaybackError::Delivery)));
}

#[tokio::test]
async fn bad_metadata_page_and_sealed_bytes_release_no_plaintext() {
    let f = Fixture::new();
    let mut corrupt = f.last.clone();
    corrupt[16] ^= 1;
    let mut trailing = f.last.clone();
    trailing.push(0);
    let mut wrong_page = f.page.clone();
    wrong_page[0] ^= 1;
    let response = f.response(false);
    for (response, bytes, page, error) in [
        (
            S3RangeResponse {
                status: 200,
                ..response
            },
            f.last.as_slice(),
            f.page(),
            PlaybackError::Response(S3RangeResponseError::Status),
        ),
        (
            S3RangeResponse {
                binding: None,
                ..response
            },
            f.last.as_slice(),
            f.page(),
            PlaybackError::Response(S3RangeResponseError::ObjectIdentity),
        ),
        (
            response,
            &f.last[..16],
            f.page(),
            PlaybackError::Response(S3RangeResponseError::Length),
        ),
        (
            response,
            trailing.as_slice(),
            f.page(),
            PlaybackError::Response(S3RangeResponseError::Length),
        ),
        (
            response,
            corrupt.as_slice(),
            f.page(),
            PlaybackError::Frame(RangeError::DigestMismatch),
        ),
        (
            response,
            f.last.as_slice(),
            ("wrong-page", f.page.as_slice()),
            PlaybackError::Binding,
        ),
        (
            response,
            f.last.as_slice(),
            (f.page().0, wrong_page.as_slice()),
            PlaybackError::Frame(RangeError::Page(PageError::HashMismatch)),
        ),
    ] {
        let mut play =
            EnrolledPlayback::start(Some("bytes=65536-"), f.pins(), f.media_input(), now())
                .unwrap()
                .1
                .unwrap();
        let result = play
            .open_next(f.media.locator(), response, Body(bytes), page, None, now)
            .await;
        assert!(matches!(result, Err(actual) if actual == error));
        assert_eq!(play.served_bytes(), 0);
        assert!(matches!(
            play.next_chunk(now()),
            Err(PlaybackError::Stopped)
        ));
    }
}
