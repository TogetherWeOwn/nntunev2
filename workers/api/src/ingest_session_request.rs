//! Inactive request validation and idempotency comparison; no upload or storage.

use sha2::{Digest, Sha256};

const MAX_REQUEST_METADATA_BYTES: usize = 16 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_KEY_BYTES: usize = 128;

/// A claimed plaintext digest binds the request, not proof of uploaded bytes.
pub struct Request<'a> {
    pub file_bytes: u64,
    pub content_sha256: [u8; 32],
    pub title: &'a str,
    pub artist: &'a str,
    pub album: Option<&'a str>,
}

/// Fingerprints are sensitive and deliberately do not implement Debug.
#[derive(PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

#[derive(Debug, PartialEq, Eq)]
pub enum RequestError {
    InvalidKey,
    InvalidFileSize,
    InvalidMetadata,
    Conflict,
    InvalidStoredIdentity,
}

impl Request<'_> {
    pub fn fingerprint(&self) -> Result<Fingerprint, RequestError> {
        if self.file_bytes == 0 || self.file_bytes > MAX_FILE_BYTES {
            return Err(RequestError::InvalidFileSize);
        }
        let fields = [Some(self.title), Some(self.artist), self.album];
        if self.title.is_empty()
            || self.artist.is_empty()
            || fields
                .iter()
                .flatten()
                .any(|s| s.len() > MAX_REQUEST_METADATA_BYTES)
            || fields.iter().flatten().map(|s| s.len()).sum::<usize>() > MAX_REQUEST_METADATA_BYTES
        {
            return Err(RequestError::InvalidMetadata);
        }
        let mut hash = Sha256::new();
        hash.update(b"ingest-session-v1\0");
        hash.update(self.file_bytes.to_be_bytes());
        hash.update(self.content_sha256);
        for field in fields {
            match field {
                None => hash.update([0]),
                Some(value) => {
                    hash.update([1]);
                    hash.update((value.len() as u32).to_be_bytes());
                    hash.update(value.as_bytes());
                }
            }
        }
        Ok(Fingerprint(hash.finalize().into()))
    }
}

/// Invoke after authorization and scoped lookup by environment, library,
/// principal and key. A future store must atomically reserve that tuple.
/// A matching create retry returns an identity, not permission to reuse bytes.
pub fn replay<'a>(
    key: &str,
    request: &Fingerprint,
    existing: Option<(&Fingerprint, &'a str)>,
) -> Result<Option<&'a str>, RequestError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(RequestError::InvalidKey);
    }
    match existing {
        None => Ok(None),
        Some((previous, _)) if previous != request => Err(RequestError::Conflict),
        Some((_, "")) => Err(RequestError::InvalidStoredIdentity),
        Some((_, identity)) => Ok(Some(identity)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request<'static> {
        Request {
            file_bytes: 42,
            content_sha256: [7; 32],
            title: "synthetic title",
            artist: "synthetic artist",
            album: None,
        }
    }

    #[test]
    fn retries_bind_content_and_return_identity() {
        let first = request().fingerprint().unwrap();
        let same = request().fingerprint().unwrap();
        let changed = Request {
            content_sha256: [8; 32],
            ..request()
        }
        .fingerprint()
        .unwrap();
        let album = Request {
            album: Some(""),
            ..request()
        }
        .fingerprint()
        .unwrap();
        assert_eq!(replay("key", &same, None), Ok(None));
        assert_eq!(
            replay("key", &same, Some((&first, "session"))),
            Ok(Some("session"))
        );
        assert_eq!(
            replay("key", &changed, Some((&first, "session"))),
            Err(RequestError::Conflict)
        );
        assert_eq!(
            replay("key", &album, Some((&first, "session"))),
            Err(RequestError::Conflict)
        );
        assert_eq!(replay("", &same, None), Err(RequestError::InvalidKey));
        assert_eq!(
            replay("key", &same, Some((&first, ""))),
            Err(RequestError::InvalidStoredIdentity)
        );
    }

    #[test]
    fn limits_are_checked_before_hashing() {
        assert!(matches!(
            Request {
                file_bytes: 0,
                ..request()
            }
            .fingerprint(),
            Err(RequestError::InvalidFileSize)
        ));
        assert!(matches!(
            Request {
                file_bytes: MAX_FILE_BYTES + 1,
                ..request()
            }
            .fingerprint(),
            Err(RequestError::InvalidFileSize)
        ));
        assert!(Request {
            file_bytes: MAX_FILE_BYTES,
            ..request()
        }
        .fingerprint()
        .is_ok());
        let long = "x".repeat(MAX_REQUEST_METADATA_BYTES + 1);
        assert!(matches!(
            Request {
                title: &long,
                ..request()
            }
            .fingerprint(),
            Err(RequestError::InvalidMetadata)
        ));
        assert!(matches!(
            Request {
                title: "",
                ..request()
            }
            .fingerprint(),
            Err(RequestError::InvalidMetadata)
        ));
        assert!(matches!(
            Request {
                artist: "",
                ..request()
            }
            .fingerprint(),
            Err(RequestError::InvalidMetadata)
        ));
        let half = "x".repeat(MAX_REQUEST_METADATA_BYTES / 2);
        let over_half = "x".repeat(MAX_REQUEST_METADATA_BYTES / 2 + 1);
        assert!(Request {
            title: &half,
            artist: &half,
            ..request()
        }
        .fingerprint()
        .is_ok());
        assert!(matches!(
            Request {
                title: &half,
                artist: &over_half,
                ..request()
            }
            .fingerprint(),
            Err(RequestError::InvalidMetadata)
        ));
        assert_eq!(format!("{:?}", RequestError::Conflict), "Conflict");
    }

    #[test]
    fn keys_are_bounded_ascii_graphic() {
        let fingerprint = request().fingerprint().unwrap();
        let limit = "k".repeat(MAX_KEY_BYTES);
        let over_limit = "k".repeat(MAX_KEY_BYTES + 1);
        assert_eq!(replay(&limit, &fingerprint, None), Ok(None));
        for key in [over_limit.as_str(), "has space", "has\ncontrol"] {
            assert_eq!(
                replay(key, &fingerprint, None),
                Err(RequestError::InvalidKey)
            );
        }
    }
}
