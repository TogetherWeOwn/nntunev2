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
    cell::RefCell,
    io,
    task::{Context, Poll},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    None,
    Denied,
    LeaseLost,
    MediaVersion,
    PageVersion,
    Ciphertext,
}

pub(super) struct Source {
    pub(super) fixture: Fixture,
    pub(super) sealed: Vec<u8>,
    pub(super) fault: Fault,
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
        assert_eq!(grant.play_id.as_deref(), Some("private_fixture"));
        assert_eq!(grant.lease_epoch, Some(42));
        (self.fault != Fault::Denied).then(|| CatalogueMedia {
            reference: id(9),
            pins: self.fixture.pins(),
            media: self.fixture.media_input(),
        })
    }

    async fn lease_live(&self, _: &Grant) -> bool {
        self.fault != Fault::LeaseLost
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

pub(super) struct Writer<'a>(pub(super) &'a RefCell<Vec<u8>>);
impl OriginalWriter for Writer<'_> {
    fn poll_cancel(&mut self, _: &mut Context<'_>) -> Poll<SourceError> {
        Poll::Pending
    }
    fn poll_write(
        &mut self,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<Result<usize, SourceError>> {
        self.0.borrow_mut().push(bytes[0]);
        Poll::Ready(Ok(1))
    }
    fn poll_close(&mut self, _: &mut Context<'_>) -> Poll<Result<(), SourceError>> {
        Poll::Ready(Ok(()))
    }
    fn abort(&mut self) {}
}
