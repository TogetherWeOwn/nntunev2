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

use crate::grant::sha256;

pub struct ContentCache {
    blobs: HashMap<String, Vec<u8>>,
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
        self.blobs.entry(hash.clone()).or_insert(plaintext);
        self.index.insert(track_id.to_string(), hash.clone());
        hash
    }

    /// Fetch the cached plaintext for `track_id`, if any.
    pub fn get_track(&self, track_id: &str) -> Option<Vec<u8>> {
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

/// A streaming R2 multipart write: parts buffer until `complete`.
pub struct MultipartUpload {
    parts: Vec<Vec<u8>>,
    staged: u64,
}

impl MultipartUpload {
    pub fn begin(track_id: &str) -> Self {
        let _ = track_id;
        Self {
            parts: Vec::new(),
            staged: 0,
        }
    }

    pub fn part(&mut self, data: &[u8]) {
        self.staged += data.len() as u64;
        self.parts.push(data.to_vec());
    }

    pub fn complete(self) -> Vec<u8> {
        self.parts.concat()
    }

    pub fn bytes_staged(&self) -> u64 {
        self.staged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_resolve_to_their_plaintext() {
        let mut cache = ContentCache::new();
        assert_eq!(cache.get_track("t1"), None);
        cache.put_track("t1", b"audio-bytes".to_vec());
        assert_eq!(cache.get_track("t1"), Some(b"audio-bytes".to_vec()));
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
        assert_eq!(cache.get_track("t1"), Some(b"v2-longer".to_vec()));
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
