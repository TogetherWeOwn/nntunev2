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
    io,
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
    play_id: &'static str,
    fault: Fault,
    checked_lease: Cell<bool>,
    pub(super) revoked: Cell<bool>,
    clock: Cell<SystemTime>,
}

impl Source {
    pub(super) fn new(fault: Fault) -> Self {
        Self::with_media(
            Fixture::from_plaintext(FLAC),
            "private_1234567890abcdefghijkl",
            fault,
        )
    }

    pub(super) fn browser() -> Self {
        Self::with_media(
            Fixture::new(),
            "private_browser_AAAAAAAAAAAAAAAAAAAAAA",
            Fault::None,
        )
    }

    fn with_media(fixture: Fixture, play_id: &'static str, fault: Fault) -> Self {
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
            play_id,
            fault,
            checked_lease: Cell::new(false),
            revoked: Cell::new(false),
            clock: Cell::new(start),
        }
    }

    pub(super) fn time(&self) -> SystemTime {
        self.clock.get()
    }
}

pub(super) struct BodyBytes(std::io::Cursor<Vec<u8>>);
impl ReadbackBody for BodyBytes {
    async fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        std::io::Read::read(&mut self.0, dst)
    }
}

impl PrivateSource for Source {
    type Body = BodyBytes;

    async fn resolve(&self, grant: &Grant) -> Option<CatalogueMedia<'_>> {
        assert_eq!(grant.user_id, "member");
        assert_eq!(grant.play_id.as_deref(), Some(self.play_id));
        assert_eq!(grant.lease_epoch, Some(42));
        assert_eq!(grant.track_id, "track");
        (self.fault != Fault::Denied).then(|| {
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
        !self.revoked.get()
            && self.fault != Fault::LeaseLost
            && !(checked && self.fault == Fault::Displaced)
    }

    async fn media(&self, get: S3Get<'_>) -> io::Result<S3GetReply<Self::Body>> {
        assert_eq!(get.configuration, id(5));
        assert!(get.locator == self.fixture.media.locator());
        assert_eq!(get.range, 0..self.sealed.len() as u64);
        let mut reply = reply("media-v1", self.sealed.clone());
        reply.version = Some(
            if self.fault == Fault::MediaVersion {
                "wrong"
            } else {
                "media-v1"
            }
            .into(),
        );
        reply.status = 206;
        reply.content_range = Some(format!(
            "bytes 0-{}/{}",
            self.sealed.len() - 1,
            self.sealed.len()
        ));
        Ok(reply)
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
        let version = if self.fault == Fault::PageVersion {
            "wrong"
        } else {
            "page-v1"
        };
        Ok((version.into(), self.fixture.page.clone()))
    }
}

fn reply(version: &str, bytes: Vec<u8>) -> S3GetReply<BodyBytes> {
    S3GetReply {
        status: 200,
        version: Some(version.into()),
        content_range: None,
        content_length: Some(bytes.len() as u64),
        has_content_encoding: false,
        body: BodyBytes(io::Cursor::new(bytes)),
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
