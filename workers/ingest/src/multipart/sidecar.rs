//! Pin one versioned sidecar PUT; stored bytes still need full readback.

use nntune_core::digest_page::object_key;
use nntune_core::{
    digest_enrollment::OriginManifest, digest_page::page_hash, media_envelope::EnvelopeContext,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarError {
    Origin,
    Target,
    Response,
    Version,
}

type Result<T> = std::result::Result<T, SidecarError>;

pub struct SidecarTarget<'a> {
    pub bucket: &'a str,
    pub key: String,
    pub context: &'a EnvelopeContext,
    pub map_id: [u8; 16],
    pub index: u32,
}

pub struct SidecarPut<'a> {
    target: SidecarTarget<'a>,
    body: &'a [u8],
    origin_hash: [u8; 32],
}

/// Authenticated transport must correlate this reply to the exact request,
/// using its actual configured bucket, logical key and configuration, not
/// provider-echoed or user fields. Supply raw version header occurrences.
pub struct SidecarPutReply<'a> {
    pub configuration: [u8; 16],
    pub bucket: &'a str,
    pub key: &'a str,
    pub status: u16,
    pub body: &'a [u8],
    pub version_headers: &'a [&'a str],
}

pub struct CompletedSidecar<'a> {
    target: SidecarTarget<'a>,
    version: String,
    encoded_len: usize,
    origin_hash: [u8; 32],
}

impl<'a> CompletedSidecar<'a> {
    pub fn pin(&self) -> (&SidecarTarget<'a>, &str) {
        (&self.target, &self.version)
    }

    pub(super) fn matches_origin(&self, encoded_len: u32, hash: &[u8; 32]) -> bool {
        self.encoded_len == encoded_len as usize && &self.origin_hash == hash
    }
}

impl<'a> SidecarPut<'a> {
    /// Use the authorized profile bucket and immutable intent; the key is
    /// profile-relative. Only the sealer manifest supplies the expected hash.
    pub fn new(
        bucket: &'a str,
        context: &'a EnvelopeContext,
        manifest: &OriginManifest,
        map_id: [u8; 16],
        index: u32,
        body: &'a [u8],
    ) -> Result<Self> {
        let [_, identities @ ..] = context.identities;
        let expected = manifest
            .candidate_page_hashes(&identities, context.plaintext_len, &map_id, &context.header)
            .and_then(|rows| rows.get(index as usize))
            .ok_or(SidecarError::Origin)?;
        // The origin hash binds both the exact encoded length and the bytes.
        if page_hash(body).as_ref() != Some(expected) {
            return Err(SidecarError::Origin);
        }
        let key = object_key(identities[2], identities[3], map_id, index);
        let target = SidecarTarget {
            bucket,
            key,
            context,
            map_id,
            index,
        };
        Ok(Self {
            target,
            body,
            origin_hash: *expected,
        })
    }

    pub fn request(&self) -> (&SidecarTarget<'a>, &'a [u8]) {
        (&self.target, self.body)
    }

    /// Consume the plan and record only the opaque version from its successful
    /// PUT. No HEAD/GET or ETag fallback, and no assumed checksum echo.
    pub fn verify_reply(self, reply: SidecarPutReply<'_>) -> Result<CompletedSidecar<'a>> {
        if reply.configuration != self.target.context.identities[5]
            || reply.bucket != self.target.bucket
            || reply.key != self.target.key
        {
            return Err(SidecarError::Target);
        }
        if reply.status != 200 || !reply.body.is_empty() {
            return Err(SidecarError::Response);
        }
        let [version] = reply.version_headers else {
            return Err(SidecarError::Version);
        };
        if !(1..=1024).contains(&version.len())
            || *version == "null"
            || !version.bytes().all(|b| b.is_ascii_graphic() && b != b',')
        {
            return Err(SidecarError::Version);
        }
        Ok(CompletedSidecar {
            target: self.target,
            version: (*version).into(),
            encoded_len: self.body.len(),
            origin_hash: self.origin_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        readback_inventory::{CompletedReadbackInventory, InventoryError},
        CompletedVersionedMedia,
    };
    use super::*;
    use nntune_core::{
        digest_enrollment::DigestEnrollment,
        media::PLAIN_CHUNK_SIZE,
        s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
    };

    fn id(byte: u8) -> [u8; 16] {
        let mut result = [byte; 16];
        result[6] = 0x40;
        result[8] = 0x80;
        result
    }

    #[test]
    fn sidecar_binds_the_second_page_to_its_index() {
        const CHUNKS_PER_PAGE: u64 = 256;
        let identities = [id(1), id(2), id(3), id(4), id(5), id(6)];
        let map_id = id(7);
        let plaintext_len = CHUNKS_PER_PAGE * PLAIN_CHUNK_SIZE + 1;
        let mut enrollment =
            DigestEnrollment::new(plaintext_len, identities[1..].try_into().unwrap(), map_id)
                .unwrap();
        let context = EnvelopeContext {
            identities,
            plaintext_len,
            header: enrollment.header().to_bytes(),
        };
        let chunk = vec![42; PLAIN_CHUNK_SIZE as usize];
        for _ in 1..CHUNKS_PER_PAGE {
            enrollment.seal_chunk(&chunk).unwrap();
        }
        let first_page = enrollment.seal_chunk(&chunk).unwrap().page.unwrap();
        let second_page = enrollment.seal_chunk(&[43]).unwrap().page.unwrap();
        let manifest = enrollment.finish().unwrap();

        let put = |page| SidecarPut::new("synthetic", &context, &manifest, map_id, 1, page);
        let plan = put(&second_page).unwrap();
        let (target, _) = plan.request();
        assert!(target.key.ends_with("/00000001"));
        assert_eq!(put(&first_page).err(), Some(SidecarError::Origin));
    }

    #[test]
    fn sidecar_pins_only_the_exact_origin_put() {
        use SidecarError::*;
        let identities = [id(1), id(2), id(3), id(4), id(5), id(6)];
        let mut enrollment =
            DigestEnrollment::new(1, identities[1..].try_into().unwrap(), id(7)).unwrap();
        let mut context = EnvelopeContext {
            identities,
            plaintext_len: 1,
            header: enrollment.header().to_bytes(),
        };
        let page = enrollment.seal_chunk(&[42]).unwrap().page.unwrap();
        let manifest = enrollment.finish().unwrap();
        let put = || SidecarPut::new("synthetic", &context, &manifest, id(7), 0, &page).unwrap();
        let plan = put();
        let (target, body) = plan.request();
        assert!(std::ptr::eq(body, page.as_slice()));
        let key = concat!(
            "media-digests/04040404040440048004040404040404/",
            "05050505050540058005050505050505/07070707070740078007070707070707/00000000"
        );
        assert_eq!(target.key, key);
        let mut changed = page.clone();
        changed[92] ^= 1;
        let cases = [
            (id(7), 0, changed.as_slice()),
            (id(7), 0, &page[..123]),
            (id(8), 0, &page),
            (id(7), 1, &page),
        ];
        for (map, index, bytes) in cases {
            let result = SidecarPut::new("synthetic", &context, &manifest, map, index, bytes);
            assert_eq!(result.err(), Some(Origin));
        }
        let reply = || SidecarPutReply {
            configuration: id(6),
            bucket: "synthetic",
            key,
            status: 200,
            body: b"",
            version_headers: &["A%2Fb+/="],
        };
        let mut completed = put().verify_reply(reply()).unwrap();
        assert_eq!(completed.pin().1, "A%2Fb+/=");
        assert_eq!(completed.pin().0.key, key);
        let hash = page_hash(&page).unwrap();
        assert!(completed.matches_origin(page.len() as u32, &hash));
        assert!(!completed.matches_origin(page.len() as u32 + 1, &hash));
        assert!(!completed.matches_origin(page.len() as u32, &[0; 32]));

        let locator = S3Locator::new(
            "synthetic".into(),
            format!("media/{}/{}", hex::encode(id(4)), hex::encode(id(5))),
            S3Binding::VersionId("media-version".into()),
        )
        .unwrap();
        let media = CompletedVersionedMedia(
            S3MediaMetadata::from_trusted_context(locator, &context).unwrap(),
        );
        let assemble_error = |receipt: &CompletedSidecar<'_>| {
            CompletedReadbackInventory::assemble(
                "synthetic",
                id(6),
                &context,
                id(7),
                &manifest,
                &media,
                std::slice::from_ref(receipt),
            )
            .err()
        };
        assert!(assemble_error(&completed).is_none());
        // Preserve every other receipt field to isolate the assembly check.
        completed.origin_hash[0] ^= 1;
        assert_eq!(assemble_error(&completed), Some(InventoryError::Sidecars));

        macro_rules! rejects {
            ($error:ident, $field:ident: $value:expr) => {
                let changed = SidecarPutReply {
                    $field: $value,
                    ..reply()
                };
                assert_eq!(put().verify_reply(changed).err(), Some($error));
            };
        }
        rejects!(Target, configuration: id(8));
        rejects!(Target, bucket: "another");
        rejects!(Target, key: "another");
        rejects!(Response, status: 204);
        rejects!(Response, body: b"<Error/>");
        rejects!(Version, version_headers: &[]);
        rejects!(Version, version_headers: &["v", "v"]);
        let long = "v".repeat(1025);
        for value in ["v, v", "null", "", " v", &long] {
            rejects!(Version, version_headers: &[value]);
        }
        context.identities[5] = id(8);
        assert!(SidecarPut::new("synthetic", &context, &manifest, id(7), 0, &page).is_err());
    }
}
