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
    assert_eq!(part.as_slice(), b"AY");
    play.written(1).unwrap();
    assert!(matches!(play.written(2), Err(PlaybackError::Delivery)));
    assert!(matches!(
        play.next_chunk(now()),
        Err(PlaybackError::Stopped)
    ));
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

#[test]
fn missing_partial_or_mismatched_encryption_metadata_refuses_start() {
    fn env_err<'a>(f: &'a Fixture, edit: impl FnOnce(&mut EnrolledMedia<'a>)) {
        let mut m = f.media_input();
        edit(&mut m);
        assert!(matches!(
            EnrolledPlayback::start(Some("bytes=0-0"), f.pins(), m, now()),
            Err(PlaybackError::Frame(RangeError::Envelope(_)))
        ));
    }
    fn bind_err<'a>(
        f: &'a Fixture,
        edit: impl FnOnce(&mut DigestReadSnapshot<'a>, &mut EnrolledMedia<'a>),
    ) {
        let (mut p, mut m) = (f.pins(), f.media_input());
        edit(&mut p, &mut m);
        assert!(matches!(
            EnrolledPlayback::start(Some("bytes=0-0"), p, m, now()),
            Err(PlaybackError::Binding)
        ));
    }

    let f = Fixture::new();
    let (wrong_root, mut corrupt) = ([8; 32], f.envelope);
    corrupt[229] ^= 1;
    env_err(&f, |m| m.root = &wrong_root);
    env_err(&f, |m| {
        m.expected_record = &corrupt;
        m.retained_record = &corrupt;
    });
    env_err(&f, |m| m.retained_record = &[]);
    env_err(&f, |m| m.retained_record = &f.envelope[..229]);
    env_err(&f, |m| m.retained_record = &corrupt);

    let mut header = f.context.header;
    header[0] ^= 1;
    let altered = EnvelopeContext {
        identities: f.context.identities,
        plaintext_len: f.context.plaintext_len,
        header,
    };
    let loc = f.media.locator();
    let etag_loc = S3Locator::new(
        loc.bucket().into(),
        loc.key().into(),
        S3Binding::Etag("\"etag\"".into()),
    )
    .unwrap();
    let etag_media = S3MediaMetadata::from_trusted_context(etag_loc, &f.context).unwrap();
    bind_err(&f, |_, m| m.context = &altered);
    bind_err(&f, |_, m| m.media = &etag_media);
    bind_err(&f, |p, _| p.root_version_id = id(8));
    bind_err(&f, |p, _| p.object_id = id(8));
    bind_err(&f, |p, _| p.provider_configuration = id(8));
    assert!(matches!(
        EnrolledPlayback::start(Some("bytes=0-0"), f.pins(), f.media_input(), now()),
        Ok((RangeOutcome::Partial(_), Some(_)))
    ));
}
