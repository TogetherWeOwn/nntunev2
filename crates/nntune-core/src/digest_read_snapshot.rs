//! Read pins passed between trusted storage lookup and playback code.

use std::time::SystemTime;

/// A borrowed view of the exact target and pins from the protected read. The
/// production caller must obtain it from the independently authorized lookup
/// after confirmed commit. Constructing a view does not confer authority.
pub struct DigestReadSnapshot<'a> {
    pub environment_id: [u8; 16],
    pub library_id: [u8; 16],
    pub reference_id: [u8; 16],
    pub object_id: [u8; 16],
    pub generation_id: [u8; 16],
    pub provider_configuration: [u8; 16],
    pub root_version_id: [u8; 16],
    pub plaintext_len: u64,
    pub plaintext_sha256: [u8; 32],
    pub map_id: [u8; 16],
    /// An opaque provider version pinned to this media object.
    pub media_version: &'a str,
    /// Opaque provider versions in canonical digest page order.
    pub page_versions: &'a [String],
    /// Exclusive deadline. The reader checks the clock before provider reads.
    pub usable_until: SystemTime,
}
