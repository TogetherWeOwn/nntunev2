//! Pin one versioned sidecar PUT; stored bytes still need full readback.

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
}

impl<'a> CompletedSidecar<'a> {
    pub fn pin(&self) -> (&SidecarTarget<'a>, &str) {
        (&self.target, &self.version)
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
        let object = hex::encode(identities[2]);
        let generation = hex::encode(identities[3]);
        let map = hex::encode(map_id);
        let key = format!("media-digests/{object}/{generation}/{map}/{index:08x}");
        let target = SidecarTarget {
            bucket,
            key,
            context,
            map_id,
            index,
        };
        Ok(Self { target, body })
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntune_core::digest_enrollment::DigestEnrollment;

    fn id(byte: u8) -> [u8; 16] {
        let mut result = [byte; 16];
        result[6] = 0x40;
        result[8] = 0x80;
        result
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
        let completed = put().verify_reply(reply()).unwrap();
        assert_eq!(completed.pin().1, "A%2Fb+/=");
        assert_eq!(completed.pin().0.key, key);
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
