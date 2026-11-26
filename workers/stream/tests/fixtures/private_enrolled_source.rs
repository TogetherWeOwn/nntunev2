use super::*;
use nntune_core::{digest_readback::ReadbackPin, original_source::SourceError};
use nntune_stream::{
    enrolled_playback::{
        route::{CatalogueMedia, PrivateSource},
        S3Get, S3GetReply,
    },
    grant::Grant,
    original_play::OriginalWriter,
};
use std::{
    cell::{Cell, RefCell},
    future::poll_fn,
    io,
    rc::Rc,
    task::{Context, Poll},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    None,
    Off,
    WrongTrack,
    WrongReference,
    WrongDigest,
    Expired,
    Displaced,
    ExpiredAtWrite,
    Writer,
    Denied,
    LeaseLost,
    MediaVersion,
    PageVersion,
    Ciphertext,
}

pub(super) const FLAC: &[u8] =
    include_bytes!("../../../../fixtures/synthetic-flac/admin-upload/fixture-1.flac");

pub(super) struct Case {
    pub fault: Fault,
    pub range: Option<&'static str>,
    pub status: u16,
    pub delivered: std::ops::Range<usize>,
    pub content_range: Option<&'static str>,
}

impl Case {
    pub(super) fn response_status(&self) -> u16 {
        match self.fault {
            Fault::Writer | Fault::ExpiredAtWrite => 200,
            Fault::None => self.status,
            _ => 0,
        }
    }
}

pub(super) fn cases() -> impl Iterator<Item = Case> {
    use Fault::*;
    let len = FLAC.len();
    [
        (None, Option::None, 200, 0..len, Option::None),
        (
            None,
            Some("bytes=-2"),
            206,
            len - 2..len,
            Some("bytes 51-52/53"),
        ),
        (None, Some("bytes=-0"), 416, 0..0, Some("bytes */53")),
        (Off, Option::None, 404, 0..0, Option::None),
        (WrongTrack, Option::None, 403, 0..0, Option::None),
        (WrongReference, Option::None, 404, 0..0, Option::None),
        (WrongDigest, Option::None, 404, 0..0, Option::None),
        (Denied, Option::None, 404, 0..0, Option::None),
        (LeaseLost, Option::None, 404, 0..0, Option::None),
        (Displaced, Option::None, 404, 0..0, Option::None),
        (Expired, Option::None, 401, 0..0, Option::None),
        (ExpiredAtWrite, Option::None, 502, 0..0, Option::None),
        (MediaVersion, Option::None, 502, 0..0, Option::None),
        (PageVersion, Option::None, 502, 0..0, Option::None),
        (Ciphertext, Option::None, 502, 0..0, Option::None),
        (Writer, Option::None, 502, 0..1, Option::None),
    ]
    .into_iter()
    .map(|(fault, range, status, delivered, content_range)| Case {
        fault,
        range,
        status,
        delivered,
        content_range,
    })
}

pub(super) struct Source {
    fixture: Fixture,
    sealed: Vec<u8>,
    track_id: &'static str,
    play_id: &'static str,
    lease_epoch: u64,
    current_epoch: Rc<Cell<u64>>,
    fault: Fault,
    checked_lease: Cell<bool>,
    pub(super) empty_media_once: Cell<bool>,
    pub(super) page_reads: Cell<usize>,
    pub(super) revoked: Cell<bool>,
    pub(super) short_media_once: Cell<bool>,
    pub(super) expected_media_start: Cell<u64>,
    pub(super) pause_final_frame: Cell<Option<Rc<FrameGate>>>,
    clock: Cell<SystemTime>,
}

impl Source {
    pub(super) fn new(fault: Fault) -> Self {
        Self::with_media(
            Fixture::from_plaintext(FLAC),
            "track",
            "private_1234567890abcdefghijkl",
            fault,
        )
    }

    pub(super) fn browser() -> Self {
        Self::with_media(
            Fixture::new(),
            "track",
            "private_browser_AAAAAAAAAAAAAAAAAAAAAA",
            Fault::None,
        )
    }

    pub(super) fn other_track() -> Self {
        let mut plaintext = vec![b'B'; LEN as usize];
        plaintext[65_535..].copy_from_slice(b"QR");
        Self::with_media(
            Fixture::from_object(&plaintext, id(10)),
            "other-track",
            "private_browser_BBBBBBBBBBBBBBBBBBBBBB",
            Fault::None,
        )
    }

    fn with_media(
        fixture: Fixture,
        track_id: &'static str,
        play_id: &'static str,
        fault: Fault,
    ) -> Self {
        let mut sealed = [&fixture.context.header[..], &fixture.first, &fixture.last].concat();
        if fault == Fault::Ciphertext {
            sealed[48] ^= 1;
        }
        let start = if fault == Fault::Expired {
            now() + Duration::from_secs(2)
        } else {
            now()
        };
        Self {
            fixture,
            sealed,
            track_id,
            play_id,
            lease_epoch: 42,
            current_epoch: Rc::new(Cell::new(42)),
            fault,
            checked_lease: Cell::new(false),
            empty_media_once: Cell::new(false),
            page_reads: Cell::new(0),
            revoked: Cell::new(false),
            short_media_once: Cell::new(false),
            expected_media_start: Cell::new(0),
            pause_final_frame: Cell::new(None),
            clock: Cell::new(start),
        }
    }

    pub(super) fn time(&self) -> SystemTime {
        self.clock.get()
    }

    pub(super) fn with_current_epoch(mut self, epoch: u64, current_epoch: Rc<Cell<u64>>) -> Self {
        self.lease_epoch = epoch;
        self.current_epoch = current_epoch;
        self
    }

    fn is_current(&self) -> bool {
        self.current_epoch.get() == self.lease_epoch
    }
}

#[derive(Default)]
pub(super) struct FrameGate {
    pub(super) waiting: Cell<bool>,
    pub(super) released: Cell<bool>,
    pub(super) dropped: Cell<bool>,
}

pub(super) struct BodyBytes {
    cursor: io::Cursor<Vec<u8>>,
    gate: Option<(u64, Rc<FrameGate>)>,
}

impl ReadbackBody for BodyBytes {
    async fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        if let Some((_, gate)) = self
            .gate
            .as_ref()
            .filter(|(offset, _)| self.cursor.position() == *offset)
        {
            gate.waiting.set(true);
            // The test invokes each poll and controls when the frame is readable.
            poll_fn(|_| {
                if gate.released.get() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        }
        io::Read::read(&mut self.cursor, dst)
    }
}

impl Drop for BodyBytes {
    fn drop(&mut self) {
        if let Some((_, gate)) = &self.gate {
            gate.dropped.set(true);
        }
    }
}

impl PrivateSource for Source {
    type Body = BodyBytes;

    async fn resolve(&self, grant: &Grant) -> Option<CatalogueMedia<'_>> {
        assert_eq!(grant.user_id, "member");
        assert_eq!(grant.play_id.as_deref(), Some(self.play_id));
        assert_eq!(grant.lease_epoch, Some(self.lease_epoch));
        assert_eq!(grant.track_id, self.track_id);
        (self.is_current() && self.fault != Fault::Denied).then(|| {
            let reference = if self.fault == Fault::WrongReference {
                id(8)
            } else {
                id(9)
            };
            let mut pins = self.fixture.pins();
            if self.fault == Fault::WrongDigest {
                pins.plaintext_sha256[0] ^= 1;
            }
            CatalogueMedia {
                reference,
                pins,
                media: self.fixture.media_input(),
            }
        })
    }

    async fn lease_live(&self, _: &Grant) -> bool {
        let checked = self.checked_lease.replace(true);
        if checked && self.fault == Fault::ExpiredAtWrite {
            self.clock.set(self.fixture.pins().usable_until);
        }
        self.is_current()
            && !self.revoked.get()
            && self.fault != Fault::LeaseLost
            && !(checked && self.fault == Fault::Displaced)
    }

    async fn media(&self, get: S3Get<'_>) -> io::Result<S3GetReply<Self::Body>> {
        assert_eq!(get.configuration, id(5));
        assert!(get.locator == self.fixture.media.locator());
        let start = self.expected_media_start.get();
        let end = self.sealed.len() as u64;
        assert_eq!(get.range, start..end);
        assert_eq!(get.sealed_len, end);
        let mut bytes = self.sealed[start as usize..].to_vec();
        if self.short_media_once.replace(false) {
            // Keep the pinned response metadata intact while dropping a body byte.
            bytes.truncate(bytes.len() - 1);
        }
        if self.empty_media_once.replace(false) {
            // Return EOF on the first read with the same pinned response metadata.
            bytes.clear();
        }
        let version = if self.fault == Fault::MediaVersion {
            "wrong"
        } else {
            "media-v1"
        };
        Ok(S3GetReply {
            status: 206,
            version: Some(version.into()),
            content_range: Some(format!("bytes {start}-{}/{end}", end - 1)),
            content_length: Some(end - start),
            has_content_encoding: false,
            body: BodyBytes {
                cursor: io::Cursor::new(bytes),
                gate: self.pause_final_frame.take().map(|gate| {
                    assert!(!self.fixture.last.is_empty());
                    let offset = end - start - self.fixture.last.len() as u64;
                    (offset, gate)
                }),
            },
        })
    }

    async fn page(
        &self,
        bucket: &str,
        pin: ReadbackPin<'_>,
        length: u32,
        until: SystemTime,
    ) -> io::Result<(String, Vec<u8>)> {
        assert_eq!(pin.configuration, id(5));
        assert_eq!(pin.version, Some("page-v1"));
        assert_eq!(bucket, "fixture-media");
        assert_eq!(until, self.fixture.pins().usable_until);
        let key = self
            .fixture
            .media
            .locator()
            .key()
            .strip_prefix("media/")
            .unwrap();
        assert_eq!(
            pin.object_key,
            format!(
                "media-digests/{key}/{:032x}/00000000",
                u128::from_be_bytes(id(6))
            )
        );
        assert_eq!(length as usize, self.fixture.page.len());
        self.page_reads.set(self.page_reads.get() + 1);
        let version = if self.fault == Fault::PageVersion {
            "wrong"
        } else {
            "page-v1"
        };
        Ok((version.into(), self.fixture.page.clone()))
    }
}

pub(super) struct Writer<'a> {
    pub(super) output: &'a RefCell<Vec<u8>>,
    pub(super) fault: Fault,
    pub(super) aborted: &'a Cell<bool>,
    pub(super) revoke_on_write: Option<&'a Cell<bool>>,
}
impl OriginalWriter for Writer<'_> {
    fn poll_cancel(&mut self, _: &mut Context<'_>) -> Poll<SourceError> {
        Poll::Pending
    }
    fn poll_write(
        &mut self,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        if self.fault == Fault::Writer && !self.output.borrow().is_empty() {
            return Poll::Ready(Err(SourceError::Transport));
        }
        self.output.borrow_mut().push(bytes[0]);
        if let Some(revoked) = self.revoke_on_write {
            revoked.set(true);
        }
        Poll::Ready(Ok(1))
    }
    fn poll_close(&mut self, _: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        Poll::Ready(Ok(()))
    }
    fn abort(&mut self) {
        self.aborted.set(true);
    }
}
