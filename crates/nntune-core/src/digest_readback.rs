//! Bounded content verification against an existing sealer-origin manifest.
//!
//! The frame verifier accepts externally framed bytes. The pinned adapter
//! additionally checks full responses and EOF against an existing inventory.
//! Success does not establish inventory trust, media validity, custody, or eligibility.

use crate::digest_enrollment::OriginManifest;
use crate::digest_range::{DigestRange, RangeError};
use crate::media::FileKey;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, future::Future, ops::AsyncFnMut};

#[derive(Debug, PartialEq, Eq)]
pub enum ReadbackError {
    Frame(RangeError),
    Incomplete,
    Failed,
    PlaintextDigest,
    MediaValidation,
}

// Private to this module: a decoder may inspect authenticated frames, but
// must not expose plaintext or commit any result while verification is pending.
trait MediaValidation {
    fn push(&mut self, plaintext: &[u8]) -> Result<(), ()>;
    fn finish(self) -> Result<(), ()>;
}

impl MediaValidation for () {
    fn push(&mut self, _: &[u8]) -> Result<(), ()> {
        Ok(())
    }

    fn finish(self) -> Result<(), ()> {
        Ok(())
    }
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
        self.push_validating(index, page, sealed, wire_header, &mut ())
    }

    fn push_validating(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
        validation: &mut impl MediaValidation,
    ) -> Result<(), ReadbackError> {
        let mut hash = self.hash.take().ok_or(ReadbackError::Failed)?;
        if (index == 0) != wire_header.is_some() {
            return Err(ReadbackError::Frame(RangeError::Header));
        }
        let part = self
            .range
            .open_next(index, page, sealed, wire_header)
            .map_err(ReadbackError::Frame)?;
        let plaintext = part.as_slice();
        hash.update(plaintext);
        validation
            .push(plaintext)
            .map_err(|()| ReadbackError::MediaValidation)?;
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
    Inventory,
    Response,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ReadbackPin<'a> {
    pub configuration: [u8; 16],
    pub object_key: &'a str,
    /// `None` is only for a qualified immutable, unversioned profile.
    pub version: Option<&'a str>,
}

pub struct ReadbackResponse<B> {
    /// Configuration and key actually used for the request.
    pub configuration: [u8; 16],
    pub object_key: String,
    /// Version actually observed in the response, not the requested version.
    pub version: Option<String>,
    pub status: u16,
    pub content_length: Option<u64>,
    pub has_content_range: bool,
    pub has_content_encoding: bool,
    pub body: B,
}

async fn open_pinned<'a, B>(
    open: &mut impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
    pin: ReadbackPin<'a>,
    length: u64,
) -> Result<B, BodyReadbackError> {
    let response = open(pin).await.map_err(|_| BodyReadbackError::Transport)?;
    if response.configuration != pin.configuration
        || response.object_key != pin.object_key
        || response.version.as_deref() != pin.version
        || response.status != 200
        || response.content_length != Some(length)
        || response.has_content_range
        || response.has_content_encoding
    {
        return Err(BodyReadbackError::Response);
    }
    Ok(response.body)
}

/// Check a complete ordered locator inventory against the origin manifest,
/// then verify full responses without releasing plaintext. The caller must
/// supply the full inventory from one authorized immutable snapshot, with
/// canonical keys and a qualified provider profile. The opener must use the
/// requested pin with deadlines, no redirects or decoding, and reject
/// ambiguous metadata. This does not list storage or issue a receipt.
pub async fn verify_pinned_readback<'a, B: ReadbackBody>(
    manifest: &OriginManifest,
    key: &FileKey,
    identities: &[[u8; 16]; 5],
    media: ReadbackPin<'a>,
    pages: &[(u32, ReadbackPin<'a>)],
    open: impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
) -> Result<(), BodyReadbackError> {
    verify_readback(manifest, key, identities, media, pages, open, ()).await
}

async fn verify_readback<'a, B: ReadbackBody>(
    manifest: &OriginManifest,
    key: &FileKey,
    identities: &[[u8; 16]; 5],
    media: ReadbackPin<'a>,
    pages: &[(u32, ReadbackPin<'a>)],
    mut open: impl AsyncFnMut(ReadbackPin<'a>) -> std::io::Result<ReadbackResponse<B>>,
    mut validation: impl MediaValidation,
) -> Result<(), BodyReadbackError> {
    let geometry = manifest.geometry();
    let indices = pages.iter().map(|&(index, _)| index);
    let origin = manifest.candidate_page_hashes(
        identities,
        geometry.plaintext_len(),
        manifest.map_id(),
        manifest.header_bytes(),
    );
    if indices.ne(0..geometry.pages()) || origin.is_none() {
        return Err(BodyReadbackError::Inventory);
    }
    let mut keys = HashSet::with_capacity(pages.len() + 1);
    let mut pins = std::iter::once(media).chain(pages.iter().map(|&(_, pin)| pin));
    if !pins.all(|pin| {
        pin.configuration == identities[4]
            && !pin.object_key.is_empty()
            && keys.insert(pin.object_key)
            && pin.version.is_some() == media.version.is_some()
            && !matches!(pin.version, Some("" | "null"))
    }) {
        return Err(BodyReadbackError::Inventory);
    }
    drop(keys);
    let mut readback = DigestReadback::new(manifest, key).map_err(BodyReadbackError::Content)?;
    let mut media = open_pinned(&mut open, media, geometry.sealed_len()).await?;
    let mut header = [0; 48];
    read_exact(&mut media, &mut header).await?;
    for &(index, pin) in pages {
        let page = geometry.page(index).ok_or(BodyReadbackError::Geometry)?;
        let mut bytes = vec![0; page.encoded_len as usize];
        let mut body = open_pinned(&mut open, pin, u64::from(page.encoded_len)).await?;
        read_exact(&mut body, &mut bytes).await?;
        require_eof(&mut body).await?;
        drop(body);
        for chunk in page.first..page.first + page.count {
            let (len, _) = geometry.chunk(chunk).ok_or(BodyReadbackError::Geometry)?;
            let mut sealed = vec![0; len as usize];
            read_exact(&mut media, &mut sealed).await?;
            readback
                .push_validating(
                    chunk,
                    &bytes,
                    sealed,
                    (chunk == 0).then_some(header.as_slice()),
                    &mut validation,
                )
                .map_err(BodyReadbackError::Content)?;
        }
    }
    require_eof(&mut media).await?;
    readback.finish().map_err(BodyReadbackError::Content)?;
    validation
        .finish()
        .map_err(|()| BodyReadbackError::Content(ReadbackError::MediaValidation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_test_fixture::{fixture, id, Fixture};

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

    fn pin(object_key: &str) -> ReadbackPin<'_> {
        ReadbackPin {
            configuration: id(1),
            object_key,
            version: Some("original"),
        }
    }

    async fn bodies(f: &Fixture, media: Body<'_>, page: Body<'_>) -> Result<(), BodyReadbackError> {
        validate_bodies(f, media, page, ()).await
    }

    async fn validate_bodies(
        f: &Fixture,
        media: Body<'_>,
        page: Body<'_>,
        validation: impl MediaValidation,
    ) -> Result<(), BodyReadbackError> {
        verify_readback(
            &f.manifest,
            &f.key,
            &[id(1); 5],
            pin("media"),
            &[(0, pin("page"))],
            async |request: ReadbackPin<'_>| {
                let (key, body, length) = match request.object_key {
                    "media" => ("media", &media, f.manifest.geometry().sealed_len()),
                    "page" => ("page", &page, f.pages[0].len() as u64),
                    _ => panic!("unexpected synthetic request"),
                };
                assert!(request == pin(key));
                Ok(ReadbackResponse {
                    configuration: id(1),
                    object_key: key.into(),
                    version: Some("original".into()),
                    status: 200,
                    content_length: Some(length),
                    has_content_range: false,
                    has_content_encoding: false,
                    body: Body(body.0, body.1),
                })
            },
            validation,
        )
        .await
    }

    #[tokio::test]
    async fn validation_observes_only_authenticated_frames_and_finishes_after_eof() {
        struct Probe<'a> {
            remaining: &'a [u8],
            chunks: usize,
            finished: bool,
            refuse_chunk: bool,
            refuse_finish: bool,
        }

        impl MediaValidation for &mut Probe<'_> {
            fn push(&mut self, plaintext: &[u8]) -> Result<(), ()> {
                let (expected, rest) = self.remaining.split_at(plaintext.len());
                assert_eq!(plaintext, expected);
                self.remaining = rest;
                self.chunks += 1;
                if self.refuse_chunk {
                    Err(())
                } else {
                    Ok(())
                }
            }

            fn finish(self) -> Result<(), ()> {
                assert!(self.remaining.is_empty());
                self.finished = true;
                if self.refuse_finish {
                    Err(())
                } else {
                    Ok(())
                }
            }
        }

        let mut plaintext = vec![5; 65_537];
        plaintext[65_536] = 8;
        let f = fixture(&plaintext);
        let mut media = f.header.to_vec();
        media.extend(f.chunks.iter().flatten().copied());
        let mut corrupt = media.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        let mut trailing = media.clone();
        trailing.push(0);
        let invalid = BodyReadbackError::Content(ReadbackError::MediaValidation);
        for (wire, refuse_chunk, refuse_finish, chunks, finished, outcome) in [
            (&media, false, false, 2, true, Ok(())),
            (
                &media,
                true,
                false,
                1,
                false,
                Err(BodyReadbackError::Content(ReadbackError::MediaValidation)),
            ),
            (&media, false, true, 2, true, Err(invalid)),
            (
                &trailing,
                false,
                false,
                2,
                false,
                Err(BodyReadbackError::Trailing),
            ),
            (
                &corrupt,
                false,
                false,
                1,
                false,
                Err(BodyReadbackError::Content(ReadbackError::Frame(
                    RangeError::DigestMismatch,
                ))),
            ),
        ] {
            let mut probe = Probe {
                remaining: &plaintext,
                chunks: 0,
                finished: false,
                refuse_chunk,
                refuse_finish,
            };
            assert_eq!(
                validate_bodies(&f, Body(wire, false), Body(&f.pages[0], false), &mut probe).await,
                outcome
            );
            assert_eq!((probe.chunks, probe.finished), (chunks, finished));
        }
    }

    #[tokio::test]
    async fn invalid_inventory_is_refused_before_opening_any_body() {
        let f = fixture(b"abc");
        let media = pin("media");
        let mut wrong_config = pin("page");
        wrong_config.configuration = id(2);
        let mut unversioned = pin("page");
        unversioned.version = None;
        let mut null_version = pin("page");
        null_version.version = Some("null");
        let mut wrong_identity = [id(1); 5];
        wrong_identity[0] = id(2);
        for (identities, pages) in [
            ([id(1); 5], vec![]),
            ([id(1); 5], vec![(0, pin("page")), (1, pin("extra"))]),
            ([id(1); 5], vec![(1, pin("page"))]),
            ([id(1); 5], vec![(0, media)]),
            ([id(1); 5], vec![(0, wrong_config)]),
            ([id(1); 5], vec![(0, unversioned)]),
            ([id(1); 5], vec![(0, null_version)]),
            (wrong_identity, vec![(0, pin("page"))]),
        ] {
            let open = async |_| -> std::io::Result<ReadbackResponse<Body<'static>>> {
                panic!("invalid inventory opened a body")
            };
            assert_eq!(
                verify_pinned_readback(&f.manifest, &f.key, &identities, media, &pages, open).await,
                Err(BodyReadbackError::Inventory)
            );
        }
    }

    #[tokio::test]
    async fn pinned_open_rejects_mismatched_response_metadata() {
        for (status, length, version, key, configuration, range, encoding) in [
            (206, Some(4), Some("original"), "page", id(1), false, false),
            (200, Some(5), Some("original"), "page", id(1), false, false),
            (200, None, Some("original"), "page", id(1), false, false),
            (200, Some(4), None, "page", id(1), false, false),
            (200, Some(4), Some("other"), "page", id(1), false, false),
            (200, Some(4), Some("original"), "other", id(1), false, false),
            (200, Some(4), Some("original"), "page", id(2), false, false),
            (200, Some(4), Some("original"), "page", id(1), true, false),
            (200, Some(4), Some("original"), "page", id(1), false, true),
        ] {
            let mut open = async |_| {
                Ok(ReadbackResponse {
                    configuration,
                    object_key: key.into(),
                    version: version.map(str::to_owned),
                    status,
                    content_length: length,
                    has_content_range: range,
                    has_content_encoding: encoding,
                    body: (),
                })
            };
            let result = open_pinned(&mut open, pin("page"), 4).await;
            assert_eq!(result, Err(BodyReadbackError::Response));
        }
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
