use super::*;
use nntune_stream::enrolled_playback::{S3Get, S3GetReply, S3PlaybackTransport};
use std::cell::Cell;

struct Provider<'a> {
    media: &'a [u8],
    page: &'a [u8],
    offset: usize,
    calls: &'a Cell<u32>,
    rejected_version: bool,
}

impl<'a> S3PlaybackTransport for Provider<'a> {
    type Body = Body<'a>;

    async fn get(&mut self, req: S3Get<'_>) -> std::io::Result<S3GetReply<Self::Body>> {
        assert_eq!(req.configuration, id(5));
        assert_eq!(req.bucket, "fixture-media");
        let (status, range, bytes) = match self.calls.replace(self.calls.get() + 1) {
            0 => {
                assert_eq!(req.key, concat!("media-digests/00000000000040008000000000000003/", "00000000000040008000000000000004/00000000000040008000000000000006/00000000"));
                assert_eq!((req.version, req.range), ("page-v1", None));
                (200, None, self.page)
            }
            1 => {
                assert!(req.key.starts_with("media/"));
                assert_eq!(req.version, "media-v1");
                let start = self.offset;
                assert_eq!(req.range, Some(start as u64..65_617));
                (
                    206,
                    Some(format!("bytes {start}-65616/65617")),
                    &self.media[start..],
                )
            }
            _ => panic!("response repeated a provider GET"),
        };
        let version = if self.rejected_version {
            "other-version"
        } else {
            req.version
        };
        Ok(S3GetReply {
            status,
            version: Some(version.into()),
            content_range: range,
            content_length: Some(bytes.len() as u64),
            has_content_encoding: false,
            body: Body(bytes),
        })
    }
}

#[tokio::test]
async fn provider_reads_are_pinned_and_only_writer_accepted_plaintext_counts() {
    let f = Fixture::new();
    let media = [&f.context.header[..], &f.first, &f.last].concat();
    for (range, offset, rejected_version) in [
        (None, 0, false),
        (Some("bytes=-1"), 65_600, false),
        (Some("bytes=-0"), 0, false),
        (None, 0, true),
    ] {
        let calls = Cell::new(0);
        let provider = Provider {
            media: &media,
            page: &f.page,
            offset,
            calls: &calls,
            rejected_version,
        };
        let (outcome, play) =
            EnrolledPlayback::start(range, f.pins(), f.media_input(), now()).unwrap();
        let play = play.map(|play| play.with_s3(provider));
        assert_eq!(calls.get(), 0);
        if range == Some("bytes=-0") {
            assert_eq!(outcome, RangeOutcome::Unsatisfiable);
            assert!(play.is_none());
            continue;
        }
        let mut play = play.unwrap();
        if rejected_version {
            assert!(matches!(play.next(now).await, Err(PlaybackError::Binding)));
            assert!(matches!(play.next(now).await, Err(PlaybackError::Stopped)));
            assert_eq!((calls.get(), play.served_bytes()), (1, 0));
            continue;
        }
        if offset == 0 {
            let part = play.next(now).await.unwrap().unwrap();
            assert_eq!(part.as_slice()[65_535], b'Y');
            assert_eq!(play.served_bytes(), 0);
            play.written(65_536).unwrap();
        }
        assert_eq!(play.next(now).await.unwrap().unwrap().as_slice(), b"Z");
        play.written(1).unwrap();
        assert_eq!(play.served_bytes(), if offset == 0 { LEN } else { 1 });
        assert!(play.next(now).await.unwrap().is_none());
        play.finish().unwrap();
        assert_eq!(calls.get(), 2);
    }
}
