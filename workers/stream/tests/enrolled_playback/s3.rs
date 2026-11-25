use super::*;
use nntune_stream::{enrolled_playback::S3GetReply, s3_range_response::S3RangeResponseError};
use std::{cell::Cell, future::pending};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Seek,
    InvalidRange,
    Version,
    Encoding,
    Truncated,
    Trailing,
    Corrupt,
    Expired,
    Pending,
}

struct ControlledBody<'a> {
    bytes: Body<'a>,
    fault: Fault,
}

impl ReadbackBody for ControlledBody<'_> {
    async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        if self.fault == Fault::Pending {
            return pending().await;
        }
        self.bytes.read(dst).await
    }
}

#[tokio::test]
async fn bounded_range_delivers_in_order_across_two_full_chunks() {
    let mut plaintext = vec![b'A'; 131_072];
    plaintext[65_534..65_539].copy_from_slice(b"vwxyz");
    let f = Fixture::from_plaintext(&plaintext);
    let sealed = [&f.context.header[..], &f.first, &f.last].concat();

    let (outcome, play) =
        EnrolledPlayback::start(Some("bytes=65534-65538"), f.pins(), f.media_input(), now())
            .unwrap();
    let RangeOutcome::Partial(range) = outcome else {
        panic!("expected 206");
    };
    assert_eq!(
        content_range_value(range, 131_072),
        "bytes 65534-65538/131072"
    );
    assert_eq!(range.len(), 5);

    let mut play = play
        .unwrap()
        .open_s3(now, async |get| {
            assert_eq!(get.range, 0..131_152);
            assert_eq!(get.sealed_len, 131_152);
            Ok(S3GetReply {
                status: 206,
                version: Some("media-v1".into()),
                content_range: Some("bytes 0-131151/131152".into()),
                content_length: Some(131_152),
                has_content_encoding: false,
                body: Body(&sealed),
            })
        })
        .await
        .unwrap();

    let first = play.next(f.page(), now).await.unwrap().unwrap();
    assert_eq!(first.as_slice(), b"vw");
    assert_eq!(play.served_bytes(), 0);
    play.written(first.as_slice().len()).unwrap();
    assert_eq!(play.served_bytes(), 2);

    let second = play.next(f.page(), now).await.unwrap().unwrap();
    assert_eq!(second.as_slice(), b"xyz");
    assert_eq!(play.served_bytes(), 2);
    play.written(second.as_slice().len()).unwrap();
    assert_eq!(play.served_bytes(), range.len());
    assert!(play.next(f.page(), now).await.unwrap().is_none());
    play.finish().unwrap();
}

#[tokio::test]
async fn pinned_media_play_seek_and_refusals_preserve_written_prefix() {
    use PlaybackError::{Binding, Expired, Response, Stopped};
    use S3RangeResponseError::{Length, ObjectIdentity};

    let f = Fixture::new();
    for fault in [
        Fault::None,
        Fault::Seek,
        Fault::InvalidRange,
        Fault::Version,
        Fault::Encoding,
        Fault::Expired,
        Fault::Pending,
        Fault::Truncated,
        Fault::Trailing,
        Fault::Corrupt,
    ] {
        let mut media = [&f.context.header[..], &f.first, &f.last].concat();
        let (range, offset) = match fault {
            Fault::Seek => (Some("bytes=-1"), 65_600),
            Fault::InvalidRange => (Some("bytes=-0"), 0),
            _ => (None, 0),
        };
        match fault {
            Fault::Truncated => media.truncate(65_616),
            Fault::Trailing => media.push(0),
            Fault::Corrupt => media[65_616] ^= 1,
            _ => {}
        }
        let clock = Cell::new(now());
        let (outcome, play) =
            EnrolledPlayback::start(range, f.pins(), f.media_input(), now()).unwrap();
        if fault == Fault::InvalidRange {
            assert_eq!(outcome, RangeOutcome::Unsatisfiable);
            assert!(play.is_none());
            continue;
        }
        let opened = play
            .unwrap()
            .open_s3(
                || clock.get(),
                async |req| {
                    assert_eq!(req.configuration, id(5));
                    assert!(req.locator == f.media.locator());
                    assert_eq!(req.usable_until, now() + Duration::from_secs(1));
                    let start = offset;
                    assert_eq!(req.range, start as u64..65_617);
                    assert_eq!(req.sealed_len, 65_617);
                    if fault == Fault::Expired {
                        clock.set(req.usable_until);
                    }
                    let version = if fault == Fault::Version {
                        "wrong-version"
                    } else {
                        "media-v1"
                    };
                    Ok(S3GetReply {
                        status: 206,
                        version: Some(version.into()),
                        content_range: Some(format!("bytes {start}-65616/65617")),
                        content_length: Some(65_617 - start as u64),
                        has_content_encoding: fault == Fault::Encoding,
                        body: ControlledBody {
                            bytes: Body(&media[start..]),
                            fault,
                        },
                    })
                },
            )
            .await;
        let expected = match fault {
            Fault::Version => Some(Response(ObjectIdentity)),
            Fault::Encoding => Some(Binding),
            Fault::Expired => Some(Expired),
            _ => None,
        };
        if let Some(error) = expected {
            assert_eq!(opened.err(), Some(error));
            continue;
        }
        let mut play = opened.unwrap();
        assert_eq!(play.next_page(now()).unwrap(), Some(0));
        let page = f.page();
        if fault == Fault::Pending {
            {
                let next = std::pin::pin!(play.next(page, || clock.get()));
                assert!(futures_util::poll!(next).is_pending());
            }
            assert!(matches!(play.next(page, now).await, Err(Stopped)));
            assert_eq!(play.served_bytes(), 0);
            continue;
        }
        if offset == 0
            && matches!(
                fault,
                Fault::None | Fault::Truncated | Fault::Trailing | Fault::Corrupt
            )
        {
            let part = play.next(page, now).await.unwrap().unwrap();
            assert_eq!(part.as_slice()[65_535], b'Y');
            assert_eq!(play.served_bytes(), 0);
            play.written(65_536).unwrap();
        }
        let prefix = play.served_bytes();
        let last = play.next(page, || clock.get()).await;
        if !matches!(fault, Fault::None | Fault::Seek) {
            assert!(
                match fault {
                    Fault::Truncated | Fault::Trailing => last.err() == Some(Response(Length)),
                    Fault::Corrupt => matches!(last, Err(PlaybackError::Frame(_))),
                    _ => false,
                },
                "fault {fault:?} was not refused"
            );
            assert!(matches!(play.next(page, now).await, Err(Stopped)));
            assert_eq!(play.served_bytes(), prefix);
            continue;
        }
        assert_eq!(last.unwrap().unwrap().as_slice(), b"Z");
        play.written(1).unwrap();
        assert_eq!(play.served_bytes(), if offset == 0 { LEN } else { 1 });
        assert!(play.next(page, now).await.unwrap().is_none());
        play.finish().unwrap();
    }
}

#[tokio::test]
async fn sequential_playback_waits_for_one_eof_at_the_short_final_frame() {
    use std::{future::poll_fn, task::Poll};

    struct EofBody<'a> {
        bytes: Body<'a>,
        eof_ready: &'a Cell<bool>,
        eof_reads: &'a Cell<usize>,
    }

    impl ReadbackBody for EofBody<'_> {
        async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            let count = self.bytes.read(dst).await?;
            if count == 0 {
                self.eof_reads.set(self.eof_reads.get() + 1);
                poll_fn(|_| {
                    if self.eof_ready.get() {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
            }
            Ok(count)
        }
    }

    let mut plaintext = vec![b'A'; LEN as usize];
    plaintext[65_535..].copy_from_slice(b"YZ");
    let f = Fixture::from_plaintext(&plaintext);
    let sealed = [&f.context.header[..], &f.first, &f.last].concat();
    let eof_ready = Cell::new(false);
    let eof_reads = Cell::new(0);

    let (outcome, play) = EnrolledPlayback::start(None, f.pins(), f.media_input(), now()).unwrap();
    assert_eq!(outcome, RangeOutcome::Full);
    let mut play = play
        .unwrap()
        .open_s3(now, async |get| {
            assert!(get.locator == f.media.locator());
            assert_eq!(get.range, 0..65_617);
            Ok(S3GetReply {
                status: 206,
                version: Some("media-v1".into()),
                content_range: Some("bytes 0-65616/65617".into()),
                content_length: Some(65_617),
                has_content_encoding: false,
                body: EofBody {
                    bytes: Body(&sealed),
                    eof_ready: &eof_ready,
                    eof_reads: &eof_reads,
                },
            })
        })
        .await
        .unwrap();

    let first = play.next(f.page(), now).await.unwrap().unwrap();
    assert_eq!(first.as_slice(), &plaintext[..65_536]);
    assert_eq!(eof_reads.get(), 0);
    play.written(first.as_slice().len()).unwrap();

    let last = {
        // Drive the pending EOF read explicitly, without a timer or socket.
        let mut next = std::pin::pin!(play.next(f.page(), now));
        assert!(futures_util::poll!(&mut next).is_pending());
        assert_eq!(eof_reads.get(), 1);
        eof_ready.set(true);
        let Poll::Ready(part) = futures_util::poll!(&mut next) else {
            panic!("the final frame must be ready once the body confirms EOF");
        };
        part.unwrap().unwrap()
    };
    assert_eq!(last.as_slice(), &plaintext[65_536..]);
    assert_eq!(play.served_bytes(), 65_536);
    play.written(last.as_slice().len()).unwrap();
    assert_eq!(play.served_bytes(), LEN);
    assert!(play.next_page(now()).unwrap().is_none());
    assert!(play.next(f.page(), now).await.unwrap().is_none());
    play.finish().unwrap();
    assert_eq!(eof_reads.get(), 1);
}
