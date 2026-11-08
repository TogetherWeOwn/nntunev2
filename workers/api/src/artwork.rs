//! Own-file artwork serving core (ADR 0002/0006/0013).
//!
//! Consumes the ingest producer's original-art descriptor, never extracts,
//! refills or writes art. The HTTP/R2 adapter is not wired yet: it must resolve
//! an authenticated identity, enforce current entitlement and catalogue
//! availability, and bound object collection before calling this core. Archive
//! art is deliberately unavailable here; an archive URL is not a storage or
//! redistribution licence. All successful responses remain private/no-store.

use std::future::Future;

use crate::admin_ingest::content_sha256_hex;

pub const MAX_ART_BYTES: usize = 5 << 20;
const MAX_ART_PIXELS: u64 = 16_000_000;

/// Metadata from the separately owned ingest output. Dimensions and MIME must
/// describe producer-validated image bytes; this reader does not decode images.
/// Fields are not caller input and are never serialized into an HTTP response.
#[derive(Debug, Clone)]
pub struct OriginalArt {
    pub source_kind: String,
    pub content_hash: String,
    pub byte_size: usize,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

impl OriginalArt {
    /// Derive the ADR 0013 key, never accept a stored URL or arbitrary R2 key.
    /// Embedded provenance permits this read path, not public redistribution or
    /// a cache write. No rights-to-cache flag is inferred from availability.
    fn object_key(&self) -> Option<String> {
        if self.source_kind != "embedded"
            || self.content_hash.len() != 64
            || !self
                .content_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.byte_size == 0
            || self.byte_size > MAX_ART_BYTES
            || self.width == 0
            || self.height == 0
            || u64::from(self.width) * u64::from(self.height) > MAX_ART_PIXELS
            || !matches!(
                self.mime.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            )
        {
            return None;
        }
        Some(format!(
            "art/{}/{}/{}",
            &self.content_hash[..2],
            &self.content_hash[2..4],
            self.content_hash
        ))
    }
}

/// No backend error text, object identifiers or cache-hit state reaches callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtError {
    NotFound,
    Unavailable,
}

pub struct ArtResponse {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

impl ArtResponse {
    pub fn headers(&self) -> [(&'static str, &'static str); 3] {
        [
            ("Cache-Control", "private, no-store"),
            ("X-Content-Type-Options", "nosniff"),
            ("Cross-Origin-Resource-Policy", "same-origin"),
        ]
    }
}

/// Storage adapter contract. No implementation is installed in the HTTP worker
/// until the ingest publication and release-to-art pointer are available.
pub trait ArtStore {
    /// Check current entitlement AND release catalogue availability. A cached
    /// object, descriptor or request MBID never grants access on its own.
    fn may_read_release(
        &self,
        user_id: &str,
        release_mbid: &str,
    ) -> impl Future<Output = Result<bool, ArtError>>;

    fn original_art(
        &self,
        release_mbid: &str,
    ) -> impl Future<Output = Result<Option<OriginalArt>, ArtError>>;

    /// Read only the derived key in the environment's art bucket. Enforce
    /// `max_bytes` while collecting, not after an unbounded allocation. Never
    /// fetch a remote URL or refill on a missing object.
    fn read_object(
        &self,
        key: &str,
        max_bytes: usize,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, ArtError>>;
}

/// The caller identity must already be authenticated by the HTTP boundary.
/// Authorization precedes descriptor lookup and byte reads on every request;
/// this function intentionally exposes no unauthenticated/hash-addressed API.
pub async fn serve_original(
    store: &impl ArtStore,
    authenticated_user_id: &str,
    release_mbid: &str,
) -> Result<ArtResponse, ArtError> {
    if authenticated_user_id.is_empty() || !crate::catalogue::is_catalogue_mbid(release_mbid) {
        return Err(ArtError::NotFound);
    }
    if !store
        .may_read_release(authenticated_user_id, release_mbid)
        .await?
    {
        return Err(ArtError::NotFound);
    }
    let art = store
        .original_art(release_mbid)
        .await?
        .ok_or(ArtError::NotFound)?;
    let key = art.object_key().ok_or(ArtError::NotFound)?;
    let bytes = store
        .read_object(&key, art.byte_size)
        .await?
        .ok_or(ArtError::NotFound)?;
    if bytes.len() != art.byte_size || content_sha256_hex(&bytes) != art.content_hash {
        return Err(ArtError::Unavailable);
    }
    Ok(ArtResponse {
        bytes,
        content_type: art.mime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::task::{Context, Poll, Waker};

    const RELEASE: &str = "11111111-2222-3333-4444-555555555555";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic producer output";

    struct Store {
        allowed: Result<bool, ArtError>,
        art: Option<OriginalArt>,
        object: Result<Option<Vec<u8>>, ArtError>,
        calls: RefCell<Vec<String>>,
    }

    impl Store {
        fn new() -> Self {
            Self {
                allowed: Ok(true),
                art: Some(OriginalArt {
                    source_kind: "embedded".into(),
                    content_hash:
                        "0d11ce2c95b7c8180b42a412fead376f365360f493067c5a9a1fce07e25ee5a1".into(),
                    byte_size: PNG.len(),
                    mime: "image/png".into(),
                    width: 1,
                    height: 1,
                }),
                object: Ok(Some(PNG.to_vec())),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ArtStore for Store {
        async fn may_read_release(&self, user: &str, release: &str) -> Result<bool, ArtError> {
            self.calls
                .borrow_mut()
                .push(format!("gate:{user}:{release}"));
            self.allowed
        }

        async fn original_art(&self, _: &str) -> Result<Option<OriginalArt>, ArtError> {
            self.calls.borrow_mut().push("descriptor".into());
            Ok(self.art.clone())
        }

        async fn read_object(
            &self,
            key: &str,
            max_bytes: usize,
        ) -> Result<Option<Vec<u8>>, ArtError> {
            let art = self.art.as_ref().unwrap();
            assert_eq!(key, art.object_key().unwrap());
            assert_eq!(max_bytes, art.byte_size);
            self.calls.borrow_mut().push("object".into());
            self.object.clone()
        }
    }

    fn run(store: &Store, user: &str, release: &str) -> Result<ArtResponse, ArtError> {
        let mut future = std::pin::pin!(serve_original(store, user, release));
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("in-memory art store must complete immediately"),
        }
    }

    #[test]
    fn original_bytes_are_hash_verified_and_never_publicly_cached() {
        let store = Store::new();
        let response = run(&store, "member", RELEASE).unwrap();
        assert_eq!(response.bytes, PNG);
        assert_eq!(response.content_type, "image/png");
        assert_eq!(
            response.headers()[0],
            ("Cache-Control", "private, no-store")
        );
        assert_eq!(response.headers()[1], ("X-Content-Type-Options", "nosniff"));
        assert_eq!(store.calls.borrow().len(), 3);
        let hash = &store.art.as_ref().unwrap().content_hash;
        assert_eq!(
            store.art.as_ref().unwrap().object_key().unwrap(),
            format!("art/{}/{}/{hash}", &hash[..2], &hash[2..4])
        );
    }

    #[test]
    fn unavailable_or_unauthorized_release_never_reads_art() {
        for allowed in [Ok(false), Err(ArtError::Unavailable)] {
            let mut store = Store::new();
            store.allowed = allowed;
            assert!(run(&store, "other-user", RELEASE).is_err());
            assert_eq!(
                store.calls.borrow().as_slice(),
                [format!("gate:other-user:{RELEASE}")]
            );
        }
        for (user, release) in [
            ("", RELEASE),
            ("member", "../art"),
            ("member", "not-an-mbid"),
        ] {
            let store = Store::new();
            assert!(matches!(
                run(&store, user, release),
                Err(ArtError::NotFound)
            ));
            assert!(store.calls.borrow().is_empty());
        }
    }

    #[test]
    fn poisoned_or_archive_descriptor_never_reads_an_object() {
        let mut invalid = Vec::new();
        for hash in [
            "",
            "../../object",
            &"a".repeat(63),
            &"é".repeat(32),
            &"A".repeat(64),
        ] {
            let mut art = Store::new().art.unwrap();
            art.content_hash = hash.into();
            invalid.push(art);
        }
        for mime in ["image/svg+xml", "text/html", "image/png\r\nX-Test: 1"] {
            let mut art = Store::new().art.unwrap();
            art.mime = mime.into();
            invalid.push(art);
        }
        for (width, height) in [(0, 1), (1, 0), (4001, 4000), (u32::MAX, u32::MAX)] {
            let mut art = Store::new().art.unwrap();
            art.width = width;
            art.height = height;
            invalid.push(art);
        }
        for size in [0, MAX_ART_BYTES + 1, usize::MAX] {
            let mut art = Store::new().art.unwrap();
            art.byte_size = size;
            invalid.push(art);
        }
        for source in ["caa", "unknown", "https://example.invalid/cover"] {
            let mut art = Store::new().art.unwrap();
            art.source_kind = source.into();
            invalid.push(art);
        }
        for art in invalid {
            let mut store = Store::new();
            store.art = Some(art);
            assert!(matches!(
                run(&store, "member", RELEASE),
                Err(ArtError::NotFound)
            ));
            assert_eq!(store.calls.borrow().len(), 2);
        }
    }

    #[test]
    fn missing_or_corrupt_objects_fail_closed_without_refill() {
        for object in [
            Ok(None),
            Ok(Some(Vec::new())),
            Ok(Some(vec![0; PNG.len()])),
            Ok(Some(vec![0; MAX_ART_BYTES + 1])),
            Err(ArtError::Unavailable),
        ] {
            let mut store = Store::new();
            store.object = object;
            assert!(run(&store, "member", RELEASE).is_err());
            assert_eq!(store.calls.borrow().len(), 3);
        }
        let mut store = Store::new();
        store.art = None;
        assert!(matches!(
            run(&store, "member", RELEASE),
            Err(ArtError::NotFound)
        ));
        assert_eq!(store.calls.borrow().len(), 2);
    }
}
