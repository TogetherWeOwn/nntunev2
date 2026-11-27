//! Internal API-to-Stream projection of a committed receipt's protected read.

use std::time::{Duration, UNIX_EPOCH};

use hkdf::hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::digest_geometry::DigestGeometry;
use crate::digest_read_snapshot::DigestReadSnapshot;
use crate::media_envelope::{matches_scheme2, EnvelopeContext};
use crate::s3_locator::S3MediaMetadata;

pub const RESOLVE_PATH: &str = "/internal/v1/private-playback/resolve";
pub const LEASE_PATH: &str = "/internal/v1/private-playback/lease";
pub const SECRET_BINDING: &str = "PRIVATE_PLAYBACK_CUSTODY_SECRET";
pub const AUTH_SCHEME: &str = "PlaybackCustody";
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Both calls POST the exact v4 bearer as their body. Use an independent key
/// on both Workers and verify the MAC and the live receipt on every request.
pub fn signature(secret: &[u8], path: &str, bearer: &str) -> Option<[u8; 32]> {
    if secret.len() < 32
        || !(1..=1800).contains(&bearer.len())
        || !bearer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || !matches!(path, RESOLVE_PATH | LEASE_PATH)
    {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).ok()?;
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(bearer.as_bytes());
    Some(mac.finalize().into_bytes().into())
}

/// Successful resolve response (200 JSON). Lease rechecks return empty 204.
/// Contains no root key or browser credentials.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustodySnapshot {
    pub bearer_sha256: [u8; 32],
    pub catalogue_reference: [u8; 16],
    pub reference_id: [u8; 16],
    /// Root version, environment, library, object, generation, provider version.
    pub identities: [[u8; 16]; 6],
    pub map_id: [u8; 16],
    pub plaintext_len: u64,
    pub plaintext_sha256: [u8; 32],
    pub bucket: String,
    pub key: String,
    pub media_version: String,
    pub page_versions: Vec<String>,
    pub usable_until_unix: u64,
    pub header: Vec<u8>,
    pub envelope_record: Vec<u8>,
    pub page_hashes: Vec<[u8; 32]>,
}

impl CustodySnapshot {
    /// Check the snapshot's internal bindings. The caller still authenticates
    /// the envelope and checks the bearer, live lease and configured provider.
    /// Manifest construction validates the page hashes after taking ownership.
    pub fn pins(&self) -> Option<DigestReadSnapshot<'_>> {
        let pages = usize::try_from(DigestGeometry::new(self.plaintext_len)?.pages()).ok()?;
        let valid_version = |version: &str| {
            (1..=1024).contains(&version.len())
                && version != "null"
                && version.bytes().all(|b| b.is_ascii_graphic() && b != b',')
        };
        let context = EnvelopeContext {
            identities: self.identities,
            plaintext_len: self.plaintext_len,
            header: self.header.as_slice().try_into().ok()?,
        };
        if self.reference_id != self.catalogue_reference
            || self.plaintext_len == 0
            || self.map_id[6] >> 4 != 4
            || self.map_id[8] >> 6 != 2
            || self.page_versions.len() != pages
            || !valid_version(&self.media_version)
            || !self.page_versions.iter().all(|v| valid_version(v))
            || !matches_scheme2(&self.envelope_record, &context)
            || !S3MediaMetadata::matches_target(&self.bucket, &self.key, &context)
        {
            return None;
        }
        let [root_version_id, environment_id, library_id, object_id, generation_id, provider_configuration] =
            self.identities;
        Some(DigestReadSnapshot {
            environment_id,
            library_id,
            reference_id: self.reference_id,
            object_id,
            generation_id,
            provider_configuration,
            root_version_id,
            plaintext_len: self.plaintext_len,
            plaintext_sha256: self.plaintext_sha256,
            map_id: self.map_id,
            media_version: &self.media_version,
            page_versions: &self.page_versions,
            usable_until: UNIX_EPOCH.checked_add(Duration::from_secs(self.usable_until_unix))?,
        })
    }
}
