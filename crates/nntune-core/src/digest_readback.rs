//! Bounded content verification against an existing sealer-origin manifest.
//!
//! The frame verifier accepts externally framed bytes. The body adapter also
//! checks exact lengths and EOF on the supplied bodies. Neither establishes
//! locator/version, complete sidecar inventory, or media validity. Success
//! grants no custody, publication, or reuse authority.

use crate::digest_enrollment::OriginManifest;
use crate::digest_range::{DigestRange, RangeError};
use crate::media::FileKey;
use sha2::{Digest, Sha256};
use std::{future::Future, ops::AsyncFnMut};

#[derive(Debug, PartialEq, Eq)]
pub enum ReadbackError {
    Frame(RangeError),
    Incomplete,
    Failed,
    PlaintextDigest,
}

/// Authenticates each frame before hashing plaintext and releases no plaintext.
/// Retains no chunk buffers between calls; the incremental hasher may retain
/// a partial plaintext block and does not guarantee erasure. Do not log state.
pub struct DigestReadback<'a> {
    range: DigestRange<'a>,
    expected: &'a [u8; 32],
    hash: Option<Sha256>,
}

impl<'a> DigestReadback<'a> {
    pub fn new(manifest: &'a OriginManifest, key: &'a FileKey) -> Result<Self, ReadbackError> {
        let range = DigestRange::new(manifest, key, 0, manifest.geometry().plaintext_len())
            .map_err(ReadbackError::Frame)?;
        Ok(Self {
            range,
            expected: manifest.plaintext_sha256(),
            hash: Some(Sha256::new()),
        })
    }

    /// Verify one complete sealed chunk against its enrolled page. Supply the
    /// exact wire header only for index zero. Any refusal poisons the readback.
    pub fn push(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
    ) -> Result<(), ReadbackError> {
        let mut hash = self.hash.take().ok_or(ReadbackError::Failed)?;
        if (index == 0) != wire_header.is_some() {
            return Err(ReadbackError::Frame(RangeError::Header));
        }
        let part = self
            .range
            .open_next(index, page, sealed, wire_header)
            .map_err(ReadbackError::Frame)?;
        hash.update(part.as_slice());
        self.hash = Some(hash);
        Ok(())
    }

    /// Require every expected frame and the original whole-plaintext digest.
    /// This does not prove EOF; the transport must also reject surplus data.
    pub fn finish(self) -> Result<(), ReadbackError> {
        let hash = self.hash.ok_or(ReadbackError::Failed)?;
        if self.range.next_indices().is_some() {
            return Err(ReadbackError::Incomplete);
        }
        if <[u8; 32]>::from(hash.finalize()) != *self.expected {
            return Err(ReadbackError::PlaintextDigest);
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BodyReadbackError {
    Transport,
    Truncated,
    Trailing,
    Geometry,
    Content(ReadbackError),
}

pub trait ReadbackBody {
    /// Fill up to `dst.len()` bytes. With nonempty `dst`, `Ok(0)` must mean
    /// clean EOF of this entire object body, not a temporary lack of bytes,
    /// a declared-length cap, or failed HTTP framing. Return errors as errors.
    fn read(&mut self, dst: &mut [u8]) -> impl Future<Output = std::io::Result<usize>>;
}

async fn read_exact(
    body: &mut impl ReadbackBody,
    mut dst: &mut [u8],
) -> Result<(), BodyReadbackError> {
    while !dst.is_empty() {
        let n = body
            .read(dst)
            .await
            .map_err(|_| BodyReadbackError::Transport)?;
        if n == 0 {
            return Err(BodyReadbackError::Truncated);
        }
        dst = dst.get_mut(n..).ok_or(BodyReadbackError::Transport)?;
    }
    Ok(())
}

async fn require_eof(body: &mut impl ReadbackBody) -> Result<(), BodyReadbackError> {
    match body.read(&mut [0]).await {
        Ok(0) => Ok(()),
        Ok(1) => Err(BodyReadbackError::Trailing),
        _ => Err(BodyReadbackError::Transport),
    }
}

/// Check the exact media body and each expected page body, one at a time.
/// The caller supplies already-authorized, version-pinned full-body reads
/// with deadlines. Fetching the expected indices does not prove that the
/// trusted inventory contains no extra locators. This returns no receipt.
pub async fn verify_readback_bodies<B: ReadbackBody>(
    manifest: &OriginManifest,
    key: &FileKey,
    mut media: impl ReadbackBody,
    mut open_page: impl AsyncFnMut(u32) -> std::io::Result<B>,
) -> Result<(), BodyReadbackError> {
    let geometry = manifest.geometry();
    let mut readback = DigestReadback::new(manifest, key).map_err(BodyReadbackError::Content)?;
    let mut header = [0; 48];
    read_exact(&mut media, &mut header).await?;
    for index in 0..geometry.pages() {
        let page = geometry.page(index).ok_or(BodyReadbackError::Geometry)?;
        let mut bytes = vec![0; page.encoded_len as usize];
        let mut body = open_page(index)
            .await
            .map_err(|_| BodyReadbackError::Transport)?;
        read_exact(&mut body, &mut bytes).await?;
        require_eof(&mut body).await?;
        drop(body);
        for chunk in page.first..page.first + page.count {
            let (len, _) = geometry.chunk(chunk).ok_or(BodyReadbackError::Geometry)?;
            let mut sealed = vec![0; len as usize];
            read_exact(&mut media, &mut sealed).await?;
            readback
                .push(
                    chunk,
                    &bytes,
                    sealed,
                    (chunk == 0).then_some(header.as_slice()),
                )
                .map_err(BodyReadbackError::Content)?;
        }
    }
    require_eof(&mut media).await?;
    readback.finish().map_err(BodyReadbackError::Content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_test_fixture::{fixture, Fixture};

    struct Body<'a>(&'a [u8], bool);

    impl ReadbackBody for Body<'_> {
        async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() && self.1 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            // Deliberately split header, ciphertext, and page across reads.
            let n = dst.len().min(self.0.len()).min(11);
            dst[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    async fn bodies(f: &Fixture, media: Body<'_>, page: Body<'_>) -> Result<(), BodyReadbackError> {
        let Body(bytes, fail_at_eof) = page;
        verify_readback_bodies(&f.manifest, &f.key, media, async move |index| {
            assert_eq!(index, 0);
            Ok(Body(bytes, fail_at_eof))
        })
        .await
    }

    #[tokio::test]
    async fn readback_bodies_require_clean_exact_eof() {
        let f = fixture(&vec![5; 65_537]);
        let mut media = f.header.to_vec();
        media.extend(f.chunks.iter().flatten().copied());
        let page = &f.pages[0];
        assert_eq!(
            bodies(&f, Body(&media, false), Body(page, false)).await,
            Ok(())
        );
        for (m, p, expected) in [
            (
                Body(&media[..media.len() - 1], false),
                Body(page, false),
                BodyReadbackError::Truncated,
            ),
            (
                Body(&media, false),
                Body(&page[..page.len() - 1], false),
                BodyReadbackError::Truncated,
            ),
            (
                Body(&media, true),
                Body(page, false),
                BodyReadbackError::Transport,
            ),
            (
                Body(&media, false),
                Body(page, true),
                BodyReadbackError::Transport,
            ),
        ] {
            assert_eq!(bodies(&f, m, p).await, Err(expected));
        }
        let mut extra_page = page.clone();
        extra_page.push(0);
        assert_eq!(
            bodies(&f, Body(&media, false), Body(&extra_page, false)).await,
            Err(BodyReadbackError::Trailing)
        );
        media.push(0);
        assert_eq!(
            bodies(&f, Body(&media, false), Body(page, false)).await,
            Err(BodyReadbackError::Trailing)
        );
    }

    fn first(readback: &mut DigestReadback<'_>, f: &Fixture) {
        readback
            .push(0, &f.pages[0], f.chunks[0].clone(), Some(&f.header))
            .unwrap();
    }

    #[test]
    fn complete_readback_requires_every_expected_frame_and_original_digest() {
        let mut plaintext = vec![5; 65_537];
        plaintext[65_536] = 8;
        let f = fixture(&plaintext);
        let mut partial = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut partial, &f);
        assert_eq!(partial.finish(), Err(ReadbackError::Incomplete));

        let verify = |expected: Option<&[u8; 32]>| {
            let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
            if let Some(expected) = expected {
                readback.expected = expected;
            }
            first(&mut readback, &f);
            readback
                .push(1, &f.pages[0], f.chunks[1].clone(), None)
                .unwrap();
            readback.finish()
        };
        assert_eq!(verify(None), Ok(()));
        let mut wrong_digest = *f.manifest.plaintext_sha256();
        wrong_digest[0] ^= 1;
        assert_eq!(
            verify(Some(&wrong_digest)),
            Err(ReadbackError::PlaintextDigest)
        );
    }

    #[test]
    fn header_on_later_frame_poisons_readback() {
        let f = fixture(&vec![5; 65_537]);
        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), Some(&f.header)),
            Err(ReadbackError::Frame(RangeError::Header))
        );
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Failed)
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));
    }

    #[test]
    fn refused_or_surplus_frame_poisons_readback() {
        let f = fixture(&vec![5; 65_537]);
        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        let mut changed = f.chunks[1].clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(
            readback.push(1, &f.pages[0], changed, None),
            Err(ReadbackError::Frame(RangeError::DigestMismatch))
        );
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Failed)
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));

        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        readback
            .push(1, &f.pages[0], f.chunks[1].clone(), None)
            .unwrap();
        assert_eq!(
            readback.push(2, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Frame(RangeError::InvalidChunk))
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));
    }
}
