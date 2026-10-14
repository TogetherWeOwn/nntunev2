//! Content-addressed audio cache: the R2 shape behind a testable interface.
//!
//! - Blobs live under `audio/<sha256-hex-of-plaintext>`; identical audio
//!   uploaded for two tracks is stored once (dedup).
//! - `track/<track_id>` is a tiny pointer to the current blob hash.
//! - [`MultipartUpload`] models the R2 multipart write the Worker streams
//!   into while serving a miss: parts buffer, `complete` concatenates.
//!
//! [`ContentCache`] is the in-memory stand-in; production swaps it for R2
//! calls with identical semantics (see `wrangler.toml` bindings).

use std::collections::HashMap;
use std::sync::Arc;

use crate::grant::sha256;

pub struct ContentCache {
    blobs: HashMap<String, Arc<[u8]>>,
    index: HashMap<String, String>,
}

impl ContentCache {
    pub fn new() -> Self {
        Self {
            blobs: HashMap::new(),
            index: HashMap::new(),
        }
    }

    /// Store `plaintext` for `track_id`; returns the blob hash hex.
    pub fn put_track(&mut self, track_id: &str, plaintext: Vec<u8>) -> String {
        let hash = Self::hex_hash(&plaintext);
        self.blobs
            .entry(hash.clone())
            .or_insert_with(|| Arc::from(plaintext));
        self.index.insert(track_id.to_string(), hash.clone());
        hash
    }

    /// Fetch the cached plaintext for `track_id`, if any. Shared, not
    /// copied: a hit never duplicates the track in memory.
    pub fn get_track(&self, track_id: &str) -> Option<Arc<[u8]>> {
        self.index
            .get(track_id)
            .and_then(|h| self.blobs.get(h))
            .cloned()
    }

    /// Distinct blobs held (dedup probe).
    pub fn blob_count(&self) -> usize {
        self.blobs.len()
    }

    /// Lowercase hex SHA-256 of `data`.
    pub fn hex_hash(data: &[u8]) -> String {
        sha256(data).iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl Default for ContentCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A streaming R2 multipart write. Production uploads each part to R2 and
/// retains nothing; this in-memory stand-in appends parts to one buffer (the
/// bytes R2 would hold) so `complete` hands them over without a copy.
pub struct MultipartUpload {
    buf: Vec<u8>,
}

impl MultipartUpload {
    pub fn begin(track_id: &str) -> Self {
        let _ = track_id;
        Self { buf: Vec::new() }
    }

    pub fn part(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn complete(self) -> Vec<u8> {
        self.buf
    }

    pub fn bytes_staged(&self) -> u64 {
        self.buf.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_resolve_to_their_plaintext() {
        let mut cache = ContentCache::new();
        assert!(cache.get_track("t1").is_none());
        cache.put_track("t1", b"audio-bytes".to_vec());
        assert_eq!(cache.get_track("t1").as_deref(), Some(&b"audio-bytes"[..]));
    }

    #[test]
    fn identical_audio_is_stored_once() {
        let mut cache = ContentCache::new();
        let h1 = cache.put_track("t1", b"same-bytes".to_vec());
        let h2 = cache.put_track("t2", b"same-bytes".to_vec());
        assert_eq!(h1, h2);
        assert_eq!(cache.blob_count(), 1);
    }

    #[test]
    fn overwriting_a_track_repoints_it() {
        let mut cache = ContentCache::new();
        cache.put_track("t1", b"v1".to_vec());
        cache.put_track("t1", b"v2-longer".to_vec());
        assert_eq!(cache.get_track("t1").as_deref(), Some(&b"v2-longer"[..]));
    }

    #[test]
    fn multipart_buffers_parts_until_complete() {
        let mut up = MultipartUpload::begin("t1");
        up.part(b"hel");
        up.part(b"lo");
        assert_eq!(up.bytes_staged(), 5);
        assert_eq!(up.complete(), b"hello");
    }
}
