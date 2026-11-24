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
