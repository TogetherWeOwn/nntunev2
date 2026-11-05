//! FLAC frame walk, seek index, tag rewrite and PICTURE strip (M2 ingest).
//!
//! Pipeline position: per published track, after the spool is reassembled
//! to a single stream (sibling card) and before encryption/post. Part A
//! matching is authoritative — this module never re-matches, it only
//! renders the match into tags.
//!
//! What this module does, all pure (no Worker bindings, no network):
//!
//! - [`walk_flac`]: validate `fLaC` magic, walk metadata blocks with caps,
//!   then walk every audio frame verifying the header CRC-8 and the frame
//!   CRC-16 (polynomial `x^16 + x^15 + x^2 + 1`, MSB-first, init zero —
//!   the same polynomial libFLAC uses). Corrupt files are rejected with a
//!   typed [`FlacReject`], never panicked over.
//! - Build the seek index (sample number → byte offset, one point per
//!   ~5 s, RFC 9639 SEEKTABLE 18-byte point shape) covering files with
//!   and without an embedded SEEKTABLE. An embedded SEEKTABLE is only
//!   recorded as present ([`FlacWalk::has_seektable`]); its points are
//!   never imported, so this module has no dependency on the offset-base
//!   convention of foreign writers.
//! - [`rewrite_tags`]: Picard-style Vorbis comments from the authoritative
//!   match (MBIDs/ISRC), strip every PICTURE block, rebuild metadata, then
//!   re-walk the output so the stored seek index matches the shifted
//!   offsets. The tagged bytes' sha256 is the dedupe + shared-cache key.
//! - Stage extracted pictures for the R2 art bucket: content hash plus the
//!   ADR 0003 key shape (`art/{hh}/{hh}/{sha}`). The actual R2 PUT and its
//!   serving path are Worker wire-up (a follow-up); only validated
//!   JPEG/PNG/WebP bytes are staged, at most 5 MiB each.
//!
//! Envelope boundary (spec §4, owner §10): MBIDs and ISRC live **inside
//! the encrypted payload only — never on the wire**. The tagged FLAC bytes
//! (which embed the tags) are encrypted before posting, so tags inside them
//! are fine. The *public* sidecar ([`FlacSidecar::public_json`]: seek
//! index, hashes, sizes) must never carry an MBID or ISRC — pinned by
//! test. The sealed sidecar ([`FlacSidecar::sealed_payload_json`]) carries
//! them and is what the envelope-minting sibling seals into
//! `track_sources.nzb_ref_*`.
//!
//! Cover-art rights (AGENTS.md): every cover-art use is rights-flagged.
//! This module only extracts and hashes embedded bytes for the art bucket;
//! any new art-bucket caching/PUT wiring is surfaced for review before it
//! ships. Art is referenced/proxied deliberately, never assumed licensed.
//!
//! MD5 note: STREAMINFO carries the MD5 of the decoded PCM. A frame walk
//! cannot recompute it without a full audio decoder (out of scope — the
//! gap list budgets this module at ~400 lines for walk + CRC + seek +
//! rewrite, not a decoder). [`walk_flac`] therefore records the declared
//! MD5 and total-sample count, checks the count against the walked frames,
//! and [`verify_pcm_md5`] compares a decoder-supplied PCM MD5 (for
//! example from the transcode container's ffmpeg decode) against the
//! declared value. An all-zero MD5 means "unknown" per spec and skips.
//!
//! Offset base: every byte offset in [`SeekPoint`] and
//! [`seektable_bytes`] is measured from the first byte of the FLAC stream
//! (byte 0 = `f`). Producer and consumer are this module, so the base is
//! consistent by construction.
//!
//! Caps (Worker isolate memory is 128 MiB; the caller feeds staged bytes,
//! so these bound CPU/misuse, never the job — an over-cap file is a typed
//! quarantine, and a spool larger than memory never reaches this fn):

use sha2::{Digest, Sha256};

use crate::queue::validate_mbid;

/// FLAC stream magic.
pub const FLAC_MAGIC: &[u8; 4] = b"fLaC";
/// Largest accepted input (1 GiB, above the intake guard's 64 GiB NZB
/// bound per file only in the pathological case; the caller stages far
/// less — this is a misuse backstop, not a job failure for real albums).
pub const MAX_FLAC_BYTES: usize = 1 << 30;
/// Most metadata blocks accepted (a healthy file has a handful).
pub const MAX_METADATA_BLOCKS: usize = 256;
/// Most metadata bytes accepted in total (16 MiB — covers large embedded
/// art without letting a hostile file force huge copies).
pub const MAX_FLAC_METADATA_BYTES: usize = 16 << 20;
/// Most audio frames accepted per walk (250k × 4 KiB ≈ 1B samples ≈
/// 88 min at 192 kHz — beyond that the file quarantines as pathological).
pub const MAX_FRAMES: usize = 250_000;
/// Largest single frame accepted (4 MiB — 65535 samples × 8 ch × 4 B
/// plus header is ~2 MiB; double is headroom, not a real-file bound).
pub const MAX_FRAME_BYTES: usize = 4 << 20;
/// Target seek spacing in seconds: emit a point once the gap since the
/// last point reaches this. Typical 4096-sample frames at 44.1 kHz land
/// the spacing at ~5.0–5.1 s, inside the required 5–10 s band.
pub const SEEK_INTERVAL_SECS: u64 = 5;
/// Largest Vorbis comment block this module writes/accepts (1 MiB).
pub const MAX_VORBIS_BYTES: usize = 1 << 20;
/// Largest single embedded picture staged (5 MiB, ADR 0003 art bounds).
pub const MAX_PICTURE_BYTES: usize = 5 << 20;
/// Most embedded pictures staged per file.
pub const MAX_PICTURES: usize = 16;
/// Total embedded picture bytes staged per file.
pub const MAX_TOTAL_PICTURE_BYTES: usize = 16 << 20;
/// MIME types staged to the art bucket (ADR 0003 accept list). Anything
/// else (including SVG) is skipped with a reason, never staged.
pub const ALLOWED_ART_MIME: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];
/// Vorbis vendor string written by [`build_vorbis_comment_block`].
pub const VORBIS_VENDOR: &str = "nntune-ingest flac/1";

/// R2 art buckets (ADR 0003 §R2 layout, `docs/ops.md` inventory).
/// Mirrors `workers/mb::{ART_BUCKET_STAGING, ART_BUCKET_PROD}` — the
/// cross-crate pin test below fails if either side renames its bucket,
/// so the bucket/key shape cannot drift silently.
pub const ART_BUCKET_STAGING: &str = "nntunev2-art-staging";
pub const ART_BUCKET_PROD: &str = "nntunev2-art-prod";

/// R2 key for validated art bytes, ADR 0003 shape:
/// `art/{sha[0:2]}/{sha[2:4]}/{sha}`.
pub fn art_r2_key(sha_hex: &str) -> String {
    format!("art/{}/{}/{}", &sha_hex[0..2], &sha_hex[2..4], sha_hex)
}

/// Why a FLAC file was rejected/quarantined. Stable [`code`] strings go
/// to the posted-file record and logs; [`detail`] carries the human
/// reason. No NZB/message-ID/subject/file-name material ever appears
/// here — only opaque offsets, counts and hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlacReject {
    /// First four bytes are not `fLaC`.
    NotFlac,
    /// The bytes end mid-structure.
    Truncated,
    /// A metadata block is malformed (bad type/length, STREAMINFO not
    /// first, wrong STREAMINFO size, over caps).
    BadMetadata(&'static str),
    /// A frame header does not parse, fails its CRC-8, or breaks sample
    /// continuity/rate consistency.
    BadFrameHeader,
    /// A frame's CRC-16 does not match its bytes.
    BadFrameCrc,
    /// A frame is larger than [`MAX_FRAME_BYTES`].
    FrameTooLarge,
    /// More frames than [`MAX_FRAMES`].
    TooManyFrames,
    /// Walked sample count disagrees with STREAMINFO's total (when known).
    SampleCountMismatch,
    /// Decoder-supplied PCM MD5 disagrees with STREAMINFO's MD5.
    Md5Mismatch,
    /// A tag value fails validation (empty required field, bad MBID,
    /// bad ISRC, NUL byte, over caps).
    BadTag(&'static str),
    /// A picture block is malformed or over caps.
    BadPicture(&'static str),
}

impl FlacReject {
    /// Stable machine code for records and logs.
    pub fn code(&self) -> &'static str {
        match self {
            FlacReject::NotFlac => "flac_not_flac",
            FlacReject::Truncated => "flac_truncated",
            FlacReject::BadMetadata(_) => "flac_bad_metadata",
            FlacReject::BadFrameHeader => "flac_bad_frame_header",
            FlacReject::BadFrameCrc => "flac_bad_frame_crc",
            FlacReject::FrameTooLarge => "flac_frame_too_large",
            FlacReject::TooManyFrames => "flac_too_many_frames",
            FlacReject::SampleCountMismatch => "flac_sample_count_mismatch",
            FlacReject::Md5Mismatch => "flac_md5_mismatch",
            FlacReject::BadTag(_) => "flac_bad_tag",
            FlacReject::BadPicture(_) => "flac_bad_picture",
        }
    }

    /// Human reason (no untrusted bytes, only static text).
    pub fn detail(&self) -> &'static str {
        match self {
            FlacReject::NotFlac => "missing fLaC magic",
            FlacReject::Truncated => "input ends mid-structure",
            FlacReject::BadMetadata(reason) => reason,
            FlacReject::BadFrameHeader => "frame header invalid or discontinuous",
            FlacReject::BadFrameCrc => "frame crc-16 mismatch",
            FlacReject::FrameTooLarge => "frame over size cap",
            FlacReject::TooManyFrames => "frame count over cap",
            FlacReject::SampleCountMismatch => "walked samples disagree with streaminfo total",
            FlacReject::Md5Mismatch => "pcm md5 disagrees with streaminfo md5",
            FlacReject::BadTag(reason) => reason,
            FlacReject::BadPicture(reason) => reason,
        }
    }
}

/// Parsed STREAMINFO (34-byte block 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub min_frame_size: u32,
    pub max_frame_size: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub md5: [u8; 16],
}

/// One seek point, RFC 9639 SEEKTABLE 18-byte shape: sample number of the
/// first sample in the target frame (`u64` BE), byte offset of the
/// frame header from the first byte of the stream (`u64` BE), frame
/// sample count (`u16` BE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekPoint {
    pub sample_number: u64,
    pub byte_offset: u64,
    pub frame_samples: u16,
}

/// One walked audio frame: byte span plus the sample span it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSpan {
    pub byte_offset: u64,
    pub byte_len: u32,
    pub first_sample: u64,
    pub samples: u32,
}

/// Result of [`walk_flac`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlacWalk {
    pub streaminfo: StreamInfo,
    pub frame_count: u32,
    pub total_samples: u64,
    /// Sample → byte-offset points, strictly increasing in both fields,
    /// first point always sample 0 at the first frame header.
    pub seek_index: Vec<SeekPoint>,
    /// Whether the file carried an embedded SEEKTABLE (recorded only —
    /// its points are never imported, see the module docs).
    pub has_seektable: bool,
    /// Byte offset of the first frame header (end of metadata).
    pub first_frame_offset: u64,
    /// Per-frame spans, in stream order.
    pub frame_spans: Vec<FrameSpan>,
}

impl FlacWalk {
    /// Byte offset of the frame covering `sample`: the greatest seek
    /// point at or before it (partition point — the index is strictly
    /// increasing, so this is exact). Sample 0 resolves to the first
    /// frame header.
    pub fn offset_for_sample(&self, sample: u64) -> u64 {
        let mut lo = 0usize;
        let mut hi = self.seek_index.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.seek_index[mid].sample_number <= sample {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return self.first_frame_offset;
        }
        self.seek_index[lo - 1].byte_offset
    }

    /// Byte range `[start, end)` covering `secs` seconds of audio from
    /// time `from_secs`: start at the seek point for the opening sample,
    /// end at the first seek point *past* the closing sample (or the
    /// staged length when the span runs to EOF). Seek granularity means
    /// the range over-covers by less than one seek interval; it never
    /// under-covers. Returns `None` for an empty walk or a caller-supplied
    /// `staged_len` at or below the resolved start.
    pub fn range_for_time_span(
        &self,
        from_secs: f64,
        secs: f64,
        staged_len: u64,
    ) -> Option<(u64, u64)> {
        if self.frame_spans.is_empty()
            || self.seek_index.is_empty()
            || secs <= 0.0
            || from_secs < 0.0
        {
            return None;
        }
        let rate = self.streaminfo.sample_rate as f64;
        if rate <= 0.0 {
            return None;
        }
        let first =
            ((from_secs * rate).floor().max(0.0) as u64).min(self.total_samples.saturating_sub(1));
        let last = ((from_secs + secs) * rate).ceil().max(0.0) as u64;
        let start = self.offset_for_sample(first);
        let end = self
            .seek_index
            .iter()
            .find(|point| point.sample_number > last)
            .map(|point| point.byte_offset)
            .unwrap_or(staged_len);
        if end <= start {
            return None;
        }
        Some((start, end))
    }
}

/// Serialize the seek index in the RFC 9639 SEEKTABLE point shape
/// (18 bytes per point: sample `u64` BE, offset `u64` BE, frame samples
/// `u16` BE), suitable for embedding or for the posted-file record.
pub fn seektable_bytes(index: &[SeekPoint]) -> Vec<u8> {
    let mut out = Vec::with_capacity(index.len() * 18);
    for point in index {
        out.extend_from_slice(&point.sample_number.to_be_bytes());
        out.extend_from_slice(&point.byte_offset.to_be_bytes());
        out.extend_from_slice(&point.frame_samples.to_be_bytes());
    }
    out
}

/// Match-derived tags rendered as Picard-style Vorbis comments.
/// Lifetimes borrow the caller's match output; validation happens in
/// [`build_vorbis_comment_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTags<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub album_artist: Option<&'a str>,
    /// Release date (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`).
    pub date: Option<&'a str>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    /// Recording MBID → `MUSICBRAINZ_TRACKID`.
    pub recording_mbid: &'a str,
    /// Release MBID → `MUSICBRAINZ_RELEASEID`.
    pub release_mbid: &'a str,
    /// Artist MBIDs → repeated `MUSICBRAINZ_ARTISTID`.
    pub artist_mbids: &'a [&'a str],
    /// ISRC → `ISRC`.
    pub isrc: Option<&'a str>,
}

/// One validated embedded picture staged for the art bucket (bytes held
/// by the caller for the R2 PUT wire-up — see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedArt {
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
    pub sha256_hex: String,
    /// ADR 0003 key: `art/{hh}/{hh}/{sha}`.
    pub r2_key: String,
    pub data: Vec<u8>,
}

/// One embedded picture skipped (not staged): invalid type or bytes,
/// with the static reason. Skipping never fails the file — audio
/// validity is independent of its pictures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedArt {
    pub reason: &'static str,
}

/// Output of [`rewrite_tags`]: the tagged bytes plus everything the
/// posted-file record needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedFlac {
    /// The tagged, PICTURE-stripped file bytes.
    pub bytes: Vec<u8>,
    /// Fresh walk of [`TaggedFlac::bytes`] (re-walked clean by
    /// construction — [`rewrite_tags`] returns `Err` otherwise).
    pub walk: FlacWalk,
    /// Plaintext sha256 of [`TaggedFlac::bytes`]: the dedupe +
    /// shared-cache key.
    pub sha256_hex: String,
    /// Pictures staged for the art bucket (R2 PUT is follow-up wire-up).
    pub art: Vec<ExtractedArt>,
    /// Pictures seen but not staged, with reasons.
    pub skipped_art: Vec<SkippedArt>,
}

/// Public sidecar: everything stored *outside* the encrypted payload
/// (seek index, hashes, sizes). Must never contain an MBID or ISRC —
/// pinned by test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlacSidecar {
    pub sha256_hex: String,
    pub byte_size: u64,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub frame_count: u32,
    pub seek_index: Vec<SeekPoint>,
    pub art: Vec<ArtRef>,
}

/// Content-addressed art reference (hash + key only — no bytes, no URLs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtRef {
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
    pub sha256_hex: String,
    pub r2_key: String,
}

impl From<&ExtractedArt> for ArtRef {
    fn from(art: &ExtractedArt) -> Self {
        ArtRef {
            mime: art.mime.clone(),
            width: art.width,
            height: art.height,
            byte_size: art.byte_size,
            sha256_hex: art.sha256_hex.clone(),
            r2_key: art.r2_key.clone(),
        }
    }
}

impl FlacSidecar {
    /// Build the public sidecar from a tagged output. Contains no MBIDs
    /// and no ISRC by construction (only hashes, counts, offsets).
    pub fn public(tagged: &TaggedFlac) -> Self {
        let walk = &tagged.walk;
        FlacSidecar {
            sha256_hex: tagged.sha256_hex.clone(),
            byte_size: tagged.bytes.len() as u64,
            sample_rate: walk.streaminfo.sample_rate,
            channels: walk.streaminfo.channels,
            bits_per_sample: walk.streaminfo.bits_per_sample,
            total_samples: walk.total_samples,
            frame_count: walk.frame_count,
            seek_index: walk.seek_index.clone(),
            art: tagged.art.iter().map(ArtRef::from).collect(),
        }
    }

    /// Minimal JSON for the posted-file record's public columns. Field
    /// order is fixed so re-runs hash identically.
    pub fn public_json(&self) -> String {
        let mut out = String::from("{");
        out.push_str(&format!("\"sha256\":\"{}\",", self.sha256_hex));
        out.push_str(&format!("\"bytes\":{},", self.byte_size));
        out.push_str(&format!("\"rate\":{},", self.sample_rate));
        out.push_str(&format!("\"channels\":{},", self.channels));
        out.push_str(&format!("\"bps\":{},", self.bits_per_sample));
        out.push_str(&format!("\"samples\":{},", self.total_samples));
        out.push_str(&format!("\"frames\":{},", self.frame_count));
        out.push_str("\"seek\":[");
        for (i, point) in self.seek_index.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "[{},{},{}]",
                point.sample_number, point.byte_offset, point.frame_samples
            ));
        }
        out.push_str("],\"art\":[");
        for (i, art) in self.art.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"mime\":\"{}\",\"w\":{},\"h\":{},\"bytes\":{},\"sha\":\"{}\",\"key\":\"{}\"}}",
                art.mime, art.width, art.height, art.byte_size, art.sha256_hex, art.r2_key
            ));
        }
        out.push_str("]}");
        out
    }

    /// Sealed payload JSON: the public sidecar PLUS the match identity
    /// (release/recording/artist MBIDs, ISRC, display tags). This is what
    /// the envelope-minting sibling seals into `track_sources.nzb_ref_*`
    /// — it never travels or stores in plaintext outside that envelope.
    pub fn sealed_payload_json(&self, tags: &TrackTags<'_>) -> String {
        let mut out = self.public_json();
        out.pop();
        out.push_str(",\"mb\":{");
        out.push_str(&format!(
            "\"recording\":\"{}\",\"release\":\"{}\",\"artists\":[",
            tags.recording_mbid, tags.release_mbid
        ));
        for (i, mbid) in tags.artist_mbids.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{mbid}\""));
        }
        out.push(']');
        if let Some(isrc) = tags.isrc {
            out.push_str(&format!(",\"isrc\":\"{isrc}\""));
        }
        out.push_str("},\"tags\":{");
        out.push_str(&format!(
            "\"title\":{},\"artist\":{},\"album\":{}",
            json_string(tags.title),
            json_string(tags.artist),
            json_string(tags.album)
        ));
        if let Some(album_artist) = tags.album_artist {
            out.push_str(&format!(",\"album_artist\":{}", json_string(album_artist)));
        }
        if let Some(date) = tags.date {
            out.push_str(&format!(",\"date\":\"{date}\""));
        }
        if let Some(n) = tags.track_number {
            out.push_str(&format!(",\"track_number\":{n}"));
        }
        if let Some(n) = tags.track_total {
            out.push_str(&format!(",\"track_total\":{n}"));
        }
        if let Some(n) = tags.disc_number {
            out.push_str(&format!(",\"disc_number\":{n}"));
        }
        if let Some(n) = tags.disc_total {
            out.push_str(&format!(",\"disc_total\":{n}"));
        }
        out.push_str("}}");
        out
    }
}

/// JSON string literal with the escapes the tag alphabet needs
/// (backslash, quote, controls). Tags are validated free of NUL before
/// this runs; anything else encodes safely.
fn json_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// --- CRC ----------------------------------------------------------------

fn crc8_table() -> &'static [u8; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u8; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u8; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut crc = i as u8;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 {
                    (crc << 1) ^ 0x07
                } else {
                    crc << 1
                };
            }
            *slot = crc;
        }
        table
    })
}

fn crc16_table() -> &'static [u16; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u16; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u16; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut crc = (i as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x8005
                } else {
                    crc << 1
                };
            }
            *slot = crc;
        }
        table
    })
}

fn crc8(data: &[u8]) -> u8 {
    let table = crc8_table();
    let mut crc = 0u8;
    for byte in data {
        crc = table[(crc ^ byte) as usize];
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let table = crc16_table();
    let mut crc = 0u16;
    for byte in data {
        crc = (crc << 8) ^ table[(((crc >> 8) ^ u16::from(*byte)) & 0xFF) as usize];
    }
    crc
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// --- Metadata walk -------------------------------------------------------

struct MetaBlock {
    block_type: u8,
    offset: usize,
    len: usize,
}

/// Split the metadata: `(blocks, first_frame_offset)`. Validates the
/// magic, the STREAMINFO-first rule and every cap.
fn split_metadata(bytes: &[u8]) -> Result<(Vec<MetaBlock>, usize), FlacReject> {
    if bytes.len() < 4 || &bytes[0..4] != FLAC_MAGIC {
        return Err(FlacReject::NotFlac);
    }
    if bytes.len() > MAX_FLAC_BYTES {
        return Err(FlacReject::FrameTooLarge);
    }
    let mut blocks = Vec::new();
    let mut pos = 4usize;
    let mut metadata_bytes = 0usize;
    loop {
        if blocks.len() >= MAX_METADATA_BLOCKS {
            return Err(FlacReject::BadMetadata("too many metadata blocks"));
        }
        if pos + 4 > bytes.len() {
            return Err(FlacReject::Truncated);
        }
        let header = bytes[pos];
        let last = header & 0x80 != 0;
        let block_type = header & 0x7F;
        let len = ((bytes[pos + 1] as usize) << 16)
            | ((bytes[pos + 2] as usize) << 8)
            | bytes[pos + 3] as usize;
        if block_type == 0x7F {
            return Err(FlacReject::BadMetadata("invalid block type 127"));
        }
        if block_type > 6 {
            return Err(FlacReject::BadMetadata("reserved block type"));
        }
        metadata_bytes = metadata_bytes.saturating_add(len);
        if metadata_bytes > MAX_FLAC_METADATA_BYTES {
            return Err(FlacReject::BadMetadata("metadata over size cap"));
        }
        let body_start = pos + 4;
        let body_end = body_start.saturating_add(len);
        if body_end > bytes.len() {
            return Err(FlacReject::Truncated);
        }
        if blocks.is_empty() && block_type != 0 {
            return Err(FlacReject::BadMetadata("streaminfo must be first"));
        }
        if block_type == 0 && len != 34 {
            return Err(FlacReject::BadMetadata("streaminfo must be 34 bytes"));
        }
        blocks.push(MetaBlock {
            block_type,
            offset: body_start,
            len,
        });
        pos = body_end;
        if last {
            break;
        }
    }
    Ok((blocks, pos))
}

fn parse_streaminfo(body: &[u8]) -> Result<StreamInfo, FlacReject> {
    if body.len() != 34 {
        return Err(FlacReject::BadMetadata("streaminfo must be 34 bytes"));
    }
    let min_block_size = u16::from_be_bytes([body[0], body[1]]);
    let max_block_size = u16::from_be_bytes([body[2], body[3]]);
    let min_frame_size =
        (u32::from(body[4]) << 16) | (u32::from(body[5]) << 8) | u32::from(body[6]);
    let max_frame_size =
        (u32::from(body[7]) << 16) | (u32::from(body[8]) << 8) | u32::from(body[9]);
    let packed = u64::from_be_bytes([
        body[10], body[11], body[12], body[13], body[14], body[15], body[16], body[17],
    ]);
    let sample_rate = (packed >> 44) as u32;
    let channels = (((packed >> 41) & 0x7) + 1) as u8;
    let bits_per_sample = (((packed >> 36) & 0x1F) + 1) as u8;
    let total_samples = packed & 0xF_FFFF_FFFF;
    if sample_rate == 0 || sample_rate > 1_048_575 {
        return Err(FlacReject::BadMetadata(
            "streaminfo sample rate out of range",
        ));
    }
    if !(1..=8).contains(&channels) {
        return Err(FlacReject::BadMetadata(
            "streaminfo channel count out of range",
        ));
    }
    if !(4..=32).contains(&bits_per_sample) {
        return Err(FlacReject::BadMetadata(
            "streaminfo bits per sample out of range",
        ));
    }
    if min_block_size != 0 && max_block_size != 0 && min_block_size > max_block_size {
        return Err(FlacReject::BadMetadata(
            "streaminfo block size range inverted",
        ));
    }
    if min_block_size < 16 && min_block_size != 0 {
        return Err(FlacReject::BadMetadata(
            "streaminfo block size below minimum",
        ));
    }
    let mut md5 = [0u8; 16];
    md5.copy_from_slice(&body[18..34]);
    Ok(StreamInfo {
        min_block_size,
        max_block_size,
        min_frame_size,
        max_frame_size,
        sample_rate,
        channels,
        bits_per_sample,
        total_samples,
        md5,
    })
}

// --- Frame walk ----------------------------------------------------------

struct FrameHeader {
    block_samples: u32,
    header_len: usize,
}

/// Decode an FLAC UTF-8 coded number at `pos`. Returns `(value, len)`.
fn decode_utf8_number(bytes: &[u8], pos: usize) -> Result<(u64, usize), FlacReject> {
    if pos >= bytes.len() {
        return Err(FlacReject::Truncated);
    }
    let first = bytes[pos];
    if first & 0x80 == 0 {
        return Ok((u64::from(first), 1));
    }
    let (mask, extra) = if first & 0xE0 == 0xC0 {
        (0x1F, 1)
    } else if first & 0xF0 == 0xE0 {
        (0x0F, 2)
    } else if first & 0xF8 == 0xF0 {
        (0x07, 3)
    } else if first & 0xFC == 0xF8 {
        (0x03, 4)
    } else if first & 0xFE == 0xFC {
        (0x01, 5)
    } else if first == 0xFE {
        (0x00, 6)
    } else {
        return Err(FlacReject::BadFrameHeader);
    };
    if pos + extra >= bytes.len() {
        return Err(FlacReject::Truncated);
    }
    let mut value = u64::from(first & mask);
    for byte in &bytes[pos + 1..pos + 1 + extra] {
        if byte & 0xC0 != 0x80 {
            return Err(FlacReject::BadFrameHeader);
        }
        value = (value << 6) | u64::from(byte & 0x3F);
    }
    Ok((value, 1 + extra))
}

fn block_samples_for(bits: u8, bytes: &[u8], pos: &mut usize) -> Result<u32, FlacReject> {
    match bits {
        0 => Err(FlacReject::BadFrameHeader),
        1 => Ok(192),
        2..=5 => Ok(576 << (bits - 2)),
        6 => {
            if *pos >= bytes.len() {
                return Err(FlacReject::Truncated);
            }
            let value = u32::from(bytes[*pos]) + 1;
            *pos += 1;
            Ok(value)
        }
        7 => {
            if *pos + 2 > bytes.len() {
                return Err(FlacReject::Truncated);
            }
            let value = u32::from(u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]])) + 1;
            *pos += 2;
            Ok(value)
        }
        _ => Ok(256 << (bits - 8)),
    }
}

fn rate_for(bits: u8, bytes: &[u8], pos: &mut usize) -> Result<Option<u32>, FlacReject> {
    let rate = match bits {
        0 => return Ok(None),
        1 => 88_200,
        2 => 176_400,
        3 => 192_000,
        4 => 8_000,
        5 => 16_000,
        6 => 22_050,
        7 => 24_000,
        8 => 32_000,
        9 => 44_100,
        10 => 48_000,
        11 => 96_000,
        12 => {
            if *pos >= bytes.len() {
                return Err(FlacReject::Truncated);
            }
            let value = u32::from(bytes[*pos]) * 1000;
            *pos += 1;
            value
        }
        13 => {
            if *pos + 2 > bytes.len() {
                return Err(FlacReject::Truncated);
            }
            let value = u32::from(u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]]));
            *pos += 2;
            value
        }
        14 => {
            if *pos + 2 > bytes.len() {
                return Err(FlacReject::Truncated);
            }
            let value = u32::from(u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]])) * 10;
            *pos += 2;
            value
        }
        _ => return Err(FlacReject::BadFrameHeader),
    };
    if rate == 0 {
        return Err(FlacReject::BadFrameHeader);
    }
    Ok(Some(rate))
}

/// Parse one frame header at `pos` (absolute byte offset). Returns the
/// header plus the absolute offset of the first byte after the header
/// CRC-8. The caller owns frame-extent discovery (scan for the next
/// validated sync) and the CRC-16 check.
fn parse_frame_header(
    bytes: &[u8],
    pos: usize,
    streaminfo: &StreamInfo,
    expected_sample: u64,
) -> Result<(FrameHeader, usize), FlacReject> {
    if pos + 4 > bytes.len() {
        return Err(FlacReject::Truncated);
    }
    if bytes[pos] != 0xFF || (bytes[pos + 1] & 0xFE) != 0xF8 {
        return Err(FlacReject::BadFrameHeader);
    }
    let variable = bytes[pos + 1] & 0x01 != 0;
    let block_bits = (bytes[pos + 2] >> 4) & 0x0F;
    let rate_bits = bytes[pos + 2] & 0x0F;
    let channels = (bytes[pos + 3] >> 4) & 0x0F;
    if channels > 0x0A {
        return Err(FlacReject::BadFrameHeader);
    }
    let sample_bits = (bytes[pos + 3] >> 1) & 0x07;
    if sample_bits == 0x03 {
        return Err(FlacReject::BadFrameHeader);
    }
    if bytes[pos + 3] & 0x01 != 0 {
        return Err(FlacReject::BadFrameHeader);
    }
    if block_bits == 0 || rate_bits == 0x0F {
        return Err(FlacReject::BadFrameHeader);
    }
    let mut cursor = pos + 4;
    let (coded, coded_len) = decode_utf8_number(bytes, cursor)?;
    cursor += coded_len;
    let block_samples = block_samples_for(block_bits, bytes, &mut cursor)?;
    let rate = rate_for(rate_bits, bytes, &mut cursor)?;
    if cursor + 1 > bytes.len() {
        return Err(FlacReject::Truncated);
    }
    if crc8(&bytes[pos..cursor]) != bytes[cursor] {
        return Err(FlacReject::BadFrameHeader);
    }
    let header_len = cursor + 1 - pos;
    let first_sample = if variable {
        coded
    } else {
        coded
            .checked_mul(u64::from(block_samples))
            .ok_or(FlacReject::BadFrameHeader)?
    };
    if first_sample != expected_sample {
        return Err(FlacReject::BadFrameHeader);
    }
    if let Some(rate) = rate {
        if rate != streaminfo.sample_rate {
            return Err(FlacReject::BadFrameHeader);
        }
    }
    Ok((
        FrameHeader {
            block_samples,
            header_len,
        },
        pos + header_len,
    ))
}

/// Walk every frame from `frames_start`, verifying CRC-16s and building
/// the seek index. Frame extents are found by scanning for the next
/// sync that parses *and* preserves sample continuity *and* carries a
/// matching CRC-16 — a false sync inside audio bytes fails one of the
/// three and is skipped.
fn walk_frames(
    bytes: &[u8],
    frames_start: usize,
    streaminfo: &StreamInfo,
) -> Result<(Vec<FrameSpan>, Vec<SeekPoint>, u64), FlacReject> {
    let mut spans: Vec<FrameSpan> = Vec::new();
    let mut seek_index: Vec<SeekPoint> = Vec::new();
    let mut pos = frames_start;
    let mut expected_sample = 0u64;
    let seek_gap = u64::from(streaminfo.sample_rate) * SEEK_INTERVAL_SECS;
    while pos < bytes.len() {
        if spans.len() >= MAX_FRAMES {
            return Err(FlacReject::TooManyFrames);
        }
        let (header, _) = parse_frame_header(bytes, pos, streaminfo, expected_sample)?;
        let min_end = pos + header.header_len + 1 + 2;
        if min_end > bytes.len() {
            return Err(FlacReject::Truncated);
        }
        // Find the next validated sync (or EOF for the last frame).
        let mut next_start: Option<usize> = None;
        let mut scan = pos + header.header_len + 1;
        while scan < bytes.len() {
            let Some(rel) = bytes[scan..].iter().position(|b| *b == 0xFF) else {
                break;
            };
            scan += rel;
            if scan + 1 >= bytes.len() {
                break;
            }
            if (bytes[scan + 1] & 0xFE) != 0xF8 {
                scan += 1;
                continue;
            }
            let candidate_len = scan - pos;
            if candidate_len < header.header_len + 3 || candidate_len > MAX_FRAME_BYTES {
                scan += 1;
                continue;
            }
            let after = expected_sample + u64::from(header.block_samples);
            if parse_frame_header(bytes, scan, streaminfo, after).is_ok()
                && crc16(&bytes[pos..scan - 2])
                    == u16::from_be_bytes([bytes[scan - 2], bytes[scan - 1]])
            {
                next_start = Some(scan);
                break;
            }
            scan += 1;
        }
        let end = match next_start {
            Some(next) => next,
            None => {
                if bytes.len() - pos > MAX_FRAME_BYTES {
                    return Err(FlacReject::FrameTooLarge);
                }
                bytes.len()
            }
        };
        let frame_len = end - pos;
        if frame_len < header.header_len + 3 {
            return Err(FlacReject::Truncated);
        }
        if frame_len > MAX_FRAME_BYTES {
            return Err(FlacReject::FrameTooLarge);
        }
        if crc16(&bytes[pos..end - 2]) != u16::from_be_bytes([bytes[end - 2], bytes[end - 1]]) {
            return Err(FlacReject::BadFrameCrc);
        }
        let frame_samples = header.block_samples;
        if seek_index.is_empty()
            || expected_sample - seek_index[seek_index.len() - 1].sample_number >= seek_gap
        {
            seek_index.push(SeekPoint {
                sample_number: expected_sample,
                byte_offset: pos as u64,
                frame_samples: frame_samples.min(65535) as u16,
            });
        }
        spans.push(FrameSpan {
            byte_offset: pos as u64,
            byte_len: frame_len as u32,
            first_sample: expected_sample,
            samples: frame_samples,
        });
        expected_sample += u64::from(frame_samples);
        pos = end;
    }
    if spans.is_empty() {
        return Err(FlacReject::Truncated);
    }
    Ok((spans, seek_index, expected_sample))
}

/// Validate a FLAC stream and build its seek index.
///
/// Streamed in the Worker sense: the caller feeds staged bytes (one
/// spool stream per file — never a whole NZB), and this walk touches
/// each byte a constant number of times with no copies of the audio.
pub fn walk_flac(bytes: &[u8]) -> Result<FlacWalk, FlacReject> {
    let (blocks, frames_start) = split_metadata(bytes)?;
    let streaminfo_block = blocks
        .iter()
        .find(|block| block.block_type == 0)
        .ok_or(FlacReject::BadMetadata("missing streaminfo"))?;
    let streaminfo =
        parse_streaminfo(&bytes[streaminfo_block.offset..streaminfo_block.offset + 34])?;
    let has_seektable = blocks.iter().any(|block| block.block_type == 3);
    let (frame_spans, seek_index, total_samples) = walk_frames(bytes, frames_start, &streaminfo)?;
    if streaminfo.total_samples != 0 && streaminfo.total_samples != total_samples {
        return Err(FlacReject::SampleCountMismatch);
    }
    let frame_count = frame_spans.len() as u32;
    Ok(FlacWalk {
        streaminfo,
        frame_count,
        total_samples,
        seek_index,
        has_seektable,
        first_frame_offset: frames_start as u64,
        frame_spans,
    })
}

/// Compare a decoder-supplied PCM MD5 against STREAMINFO. All-zero means
/// "unknown" per spec and always passes. Anything else must match
/// exactly, else [`FlacReject::Md5Mismatch`].
pub fn verify_pcm_md5(streaminfo: &StreamInfo, pcm_md5: &[u8; 16]) -> Result<(), FlacReject> {
    if streaminfo.md5 == [0u8; 16] {
        return Ok(());
    }
    if &streaminfo.md5 == pcm_md5 {
        Ok(())
    } else {
        Err(FlacReject::Md5Mismatch)
    }
}

// --- Tag rewrite + picture strip -----------------------------------------

fn validate_vorbis_key(key: &str) -> Result<(), FlacReject> {
    if key.is_empty() || key.len() > 128 {
        return Err(FlacReject::BadTag("vorbis key length out of range"));
    }
    if !key
        .bytes()
        .all(|byte| (0x20..=0x7D).contains(&byte) && byte != b'=')
    {
        return Err(FlacReject::BadTag(
            "vorbis key must be printable ascii without '='",
        ));
    }
    Ok(())
}

fn validate_tag_value(value: &str) -> Result<(), FlacReject> {
    if value.contains('\0') {
        return Err(FlacReject::BadTag("tag value must not contain NUL"));
    }
    if value.len() > 16 << 10 {
        return Err(FlacReject::BadTag("tag value over size cap"));
    }
    Ok(())
}

fn push_vorbis_entry(out: &mut Vec<u8>, key: &str, value: &str) -> Result<(), FlacReject> {
    validate_vorbis_key(key)?;
    validate_tag_value(value)?;
    let entry = format!("{key}={value}");
    let bytes = entry.as_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    if out.len() > MAX_VORBIS_BYTES {
        return Err(FlacReject::BadTag("vorbis comment over size cap"));
    }
    Ok(())
}

fn validate_date(date: &str) -> Result<(), FlacReject> {
    if date.is_empty() || date.len() > 10 {
        return Err(FlacReject::BadTag("date must be 1..=10 chars"));
    }
    if !date
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(FlacReject::BadTag("date must be alphanumeric plus '-'"));
    }
    Ok(())
}

fn validate_isrc(isrc: &str) -> Result<(), FlacReject> {
    if isrc.len() != 12 || !isrc.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(FlacReject::BadTag(
            "isrc must be 12 ascii-alphanumeric chars",
        ));
    }
    Ok(())
}

/// Build a Vorbis comment block body (vendor + entries) from the match.
/// Picard-style keys: `TITLE ARTIST ALBUM ALBUMARTIST DATE TRACKNUMBER
/// TRACKTOTAL DISCNUMBER DISCTOTAL ISRC MUSICBRAINZ_TRACKID
/// MUSICBRAINZ_RELEASEID MUSICBRAINZ_ARTISTID`. MBIDs are validated
/// canonical before they are written.
pub fn build_vorbis_comment_block(tags: &TrackTags<'_>) -> Result<Vec<u8>, FlacReject> {
    if tags.title.trim().is_empty() {
        return Err(FlacReject::BadTag("title must not be empty"));
    }
    if tags.artist.trim().is_empty() {
        return Err(FlacReject::BadTag("artist must not be empty"));
    }
    if tags.album.trim().is_empty() {
        return Err(FlacReject::BadTag("album must not be empty"));
    }
    for value in [tags.title, tags.artist, tags.album] {
        validate_tag_value(value)?;
    }
    if !validate_mbid(tags.recording_mbid) {
        return Err(FlacReject::BadTag("recording mbid must be canonical hex"));
    }
    if !validate_mbid(tags.release_mbid) {
        return Err(FlacReject::BadTag("release mbid must be canonical hex"));
    }
    for mbid in tags.artist_mbids {
        if !validate_mbid(mbid) {
            return Err(FlacReject::BadTag("artist mbid must be canonical hex"));
        }
    }
    if let Some(isrc) = tags.isrc {
        validate_isrc(isrc)?;
    }
    if let Some(date) = tags.date {
        validate_date(date)?;
    }
    let mut out = Vec::new();
    let vendor = VORBIS_VENDOR.as_bytes();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    // Count is patched after the entries are built (validation may fail
    // partway; the count must reflect exactly the written entries).
    let count_pos = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut count = 0u32;
    let mut push = |key: &str, value: &str| -> Result<(), FlacReject> {
        push_vorbis_entry(&mut out, key, value)?;
        count += 1;
        Ok(())
    };
    push("TITLE", tags.title)?;
    push("ARTIST", tags.artist)?;
    push("ALBUM", tags.album)?;
    if let Some(album_artist) = tags.album_artist {
        validate_tag_value(album_artist)?;
        push("ALBUMARTIST", album_artist)?;
    }
    if let Some(date) = tags.date {
        push("DATE", date)?;
    }
    if let Some(n) = tags.track_number {
        push("TRACKNUMBER", &n.to_string())?;
    }
    if let Some(n) = tags.track_total {
        push("TRACKTOTAL", &n.to_string())?;
    }
    if let Some(n) = tags.disc_number {
        push("DISCNUMBER", &n.to_string())?;
    }
    if let Some(n) = tags.disc_total {
        push("DISCTOTAL", &n.to_string())?;
    }
    if let Some(isrc) = tags.isrc {
        push("ISRC", isrc)?;
    }
    push("MUSICBRAINZ_TRACKID", tags.recording_mbid)?;
    push("MUSICBRAINZ_RELEASEID", tags.release_mbid)?;
    for mbid in tags.artist_mbids {
        push("MUSICBRAINZ_ARTISTID", mbid)?;
    }
    out[count_pos..count_pos + 4].copy_from_slice(&count.to_le_bytes());
    if out.len() > MAX_VORBIS_BYTES {
        return Err(FlacReject::BadTag("vorbis comment over size cap"));
    }
    Ok(out)
}

/// Parse one PICTURE block body. Returns the staged art, or `None` with a
/// skip reason when the picture is invalid or over caps (never fails the
/// file — audio validity is independent of its pictures).
fn parse_picture(body: &[u8]) -> Result<ExtractedArt, SkippedArt> {
    let mut cursor = 0usize;
    let take_u32 = |cursor: &mut usize| -> Option<u32> {
        if *cursor + 4 > body.len() {
            return None;
        }
        let value = u32::from_be_bytes([
            body[*cursor],
            body[*cursor + 1],
            body[*cursor + 2],
            body[*cursor + 3],
        ]);
        *cursor += 4;
        Some(value)
    };
    let _picture_type = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture header truncated",
    })?;
    let mime_len = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture mime truncated",
    })?;
    if mime_len as usize > body.len().saturating_sub(cursor) {
        return Err(SkippedArt {
            reason: "picture mime overruns block",
        });
    }
    let mime =
        std::str::from_utf8(&body[cursor..cursor + mime_len as usize]).map_err(|_| SkippedArt {
            reason: "picture mime not utf-8",
        })?;
    let mime = mime.to_ascii_lowercase();
    cursor += mime_len as usize;
    let desc_len = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture description truncated",
    })?;
    if desc_len as usize > body.len().saturating_sub(cursor) {
        return Err(SkippedArt {
            reason: "picture description overruns block",
        });
    }
    cursor += desc_len as usize;
    let width = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture dimensions truncated",
    })?;
    let height = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture dimensions truncated",
    })?;
    let _depth = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture dimensions truncated",
    })?;
    let _colors = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture dimensions truncated",
    })?;
    let data_len = take_u32(&mut cursor).ok_or(SkippedArt {
        reason: "picture data truncated",
    })?;
    if data_len as usize > body.len().saturating_sub(cursor) {
        return Err(SkippedArt {
            reason: "picture data overruns block",
        });
    }
    if !ALLOWED_ART_MIME.contains(&mime.as_str()) {
        return Err(SkippedArt {
            reason: "picture mime not in the art accept list",
        });
    }
    if data_len as usize > MAX_PICTURE_BYTES {
        return Err(SkippedArt {
            reason: "picture over size cap",
        });
    }
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(SkippedArt {
            reason: "picture dimensions out of range",
        });
    }
    let data = body[cursor..cursor + data_len as usize].to_vec();
    let sha256_hex = sha256_hex(&data);
    Ok(ExtractedArt {
        mime,
        width,
        height,
        byte_size: data.len(),
        r2_key: art_r2_key(&sha256_hex),
        sha256_hex,
        data,
    })
}

/// Rewrite tags and strip pictures: replace every Vorbis comment with one
/// Picard-style block built from `tags`, drop every PICTURE block
/// (staging validated art for the R2 art bucket), drop PADDING, keep all
/// other metadata in order, then re-walk the output so the seek index
/// matches the shifted offsets and the sha256 covers the final bytes.
pub fn rewrite_tags(input: &[u8], tags: &TrackTags<'_>) -> Result<TaggedFlac, FlacReject> {
    let (blocks, frames_start) = split_metadata(input)?;
    let vorbis = build_vorbis_comment_block(tags)?;
    let mut art = Vec::new();
    let mut skipped_art = Vec::new();
    let mut total_art_bytes = 0usize;
    let mut kept: Vec<(u8, &[u8])> = Vec::new();
    for block in &blocks {
        let body = &input[block.offset..block.offset + block.len];
        match block.block_type {
            0 => kept.push((0, body)),
            4 => {}
            6 => {
                if art.len() >= MAX_PICTURES {
                    skipped_art.push(SkippedArt {
                        reason: "too many pictures",
                    });
                    continue;
                }
                match parse_picture(body) {
                    Ok(picture) => {
                        total_art_bytes += picture.byte_size;
                        if total_art_bytes > MAX_TOTAL_PICTURE_BYTES {
                            total_art_bytes -= picture.byte_size;
                            skipped_art.push(SkippedArt {
                                reason: "pictures over total cap",
                            });
                        } else {
                            art.push(picture);
                        }
                    }
                    Err(skipped) => skipped_art.push(skipped),
                }
            }
            1 => {}
            typ => kept.push((typ, body)),
        }
    }
    let mut bytes = Vec::with_capacity(frames_start + vorbis.len() + 4);
    bytes.extend_from_slice(FLAC_MAGIC);
    // STREAMINFO stays first; the fresh Vorbis comment follows it; the
    // surviving blocks keep their relative order after that.
    let mut ordered: Vec<(u8, &[u8])> = Vec::with_capacity(kept.len() + 1);
    let mut streaminfo: Option<(u8, &[u8])> = None;
    for (typ, body) in kept {
        if typ == 0 && streaminfo.is_none() {
            streaminfo = Some((typ, body));
        } else {
            ordered.push((typ, body));
        }
    }
    let streaminfo = streaminfo.ok_or(FlacReject::BadMetadata("missing streaminfo"))?;
    let mut all: Vec<(u8, &[u8])> = Vec::with_capacity(ordered.len() + 2);
    all.push(streaminfo);
    all.push((4, &vorbis));
    all.extend(ordered);
    for (index, (typ, body)) in all.iter().enumerate() {
        let last = index + 1 == all.len();
        bytes.push((if last { 0x80 } else { 0x00 }) | typ);
        let len = body.len();
        bytes.push((len >> 16) as u8);
        bytes.push((len >> 8) as u8);
        bytes.push(len as u8);
        bytes.extend_from_slice(body);
    }
    bytes.extend_from_slice(&input[frames_start..]);
    // Re-walk clean: identical audio frames at shifted offsets, with a
    // fresh seek index. Any failure here is a bug in the splice above.
    let walk = walk_flac(&bytes)?;
    let sha256_hex = sha256_hex(&bytes);
    Ok(TaggedFlac {
        bytes,
        walk,
        sha256_hex,
        art,
        skipped_art,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Synthetic FLAC builder (test-only encoder) ----------------------
    //
    // Emits small but structurally valid FLAC: magic, STREAMINFO,
    // optional Vorbis comment / PICTURE / SEEKTABLE, then frames with a
    // fixed 4-byte-style header, 8-bit verbatim-style subframe bodies
    // (content is opaque to the walker — only offsets and CRCs matter),
    // and correct CRC-8/CRC-16 values. Sample bytes stay below 0x80 so no
    // false sync can appear inside a frame body.

    const TEST_RATE: u32 = 44_100;
    const TEST_BLOCK: u32 = 192;

    fn test_streaminfo(total_samples: u64, md5: [u8; 16]) -> Vec<u8> {
        let mut body = Vec::with_capacity(34);
        body.extend_from_slice(&192u16.to_be_bytes());
        body.extend_from_slice(&192u16.to_be_bytes());
        body.extend_from_slice(&[0, 1, 0]);
        body.extend_from_slice(&[0, 1, 0]);
        let packed: u64 = (u64::from(TEST_RATE) << 44)
            | (1u64 << 41)
            | (7u64 << 36)
            | (total_samples & 0xF_FFFF_FFFF);
        body.extend_from_slice(&packed.to_be_bytes());
        body.extend_from_slice(&md5);
        body
    }

    fn utf8_number(mut value: u64) -> Vec<u8> {
        if value < 0x80 {
            return vec![value as u8];
        }
        let mut parts = vec![(0x80 | (value & 0x3F)) as u8];
        value >>= 6;
        while value >= 0x20 {
            parts.push((0x80 | (value & 0x3F)) as u8);
            value >>= 6;
        }
        let first_len = parts.len();
        let first = match first_len {
            1 => 0xC0 | (value as u8),
            2 => 0xE0 | (value as u8),
            3 => 0xF0 | (value as u8),
            4 => 0xF8 | (value as u8),
            5 => 0xFC | (value as u8),
            _ => 0xFE,
        };
        parts.push(first);
        parts.reverse();
        parts
    }

    fn test_frame(frame_no: u64, samples: u32, seed: u8) -> Vec<u8> {
        // Fixed blocking (strategy 0), blocksize 0001 (192), rate 1001
        // (44100), stereo independent (0001), 8-bit samples (001).
        assert_eq!(samples, TEST_BLOCK);
        let mut header = vec![0xFF, 0xF8, 0x19, 0x12];
        header.extend_from_slice(&utf8_number(frame_no));
        let header_crc = crc8(&header);
        let mut frame = header;
        frame.push(header_crc);
        // Two channels of verbatim-style bodies: subframe header 0x02
        // (verbatim, no wasted bits) + raw sample bytes < 0x80.
        for channel in 0..2u8 {
            frame.push(0x02);
            for i in 0..samples {
                frame.push((seed.wrapping_add(channel).wrapping_add(i as u8)) & 0x7F);
            }
        }
        let frame_crc = crc16(&frame);
        frame.extend_from_slice(&frame_crc.to_be_bytes());
        frame
    }

    struct TestFlacOpts {
        frames: u32,
        vorbis: bool,
        picture: bool,
        bad_picture: bool,
        seektable: bool,
        md5: [u8; 16],
    }

    fn test_picture_block() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&3u32.to_be_bytes());
        body.extend_from_slice(&9u32.to_be_bytes());
        body.extend_from_slice(b"image/png");
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&8u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        // Minimal 1x1 PNG (opaque to the walker — only length matters).
        let png = [
            0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE,
        ];
        body.extend_from_slice(&(png.len() as u32).to_be_bytes());
        body.extend_from_slice(&png);
        body
    }

    fn build_test_flac(opts: &TestFlacOpts) -> Vec<u8> {
        let total = u64::from(opts.frames) * u64::from(TEST_BLOCK);
        let mut out = Vec::new();
        out.extend_from_slice(FLAC_MAGIC);
        let mut blocks: Vec<(u8, Vec<u8>)> = Vec::new();
        blocks.push((0, test_streaminfo(total, opts.md5)));
        if opts.vorbis {
            let mut comment = Vec::new();
            let vendor = b"old-encoder";
            comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
            comment.extend_from_slice(vendor);
            comment.extend_from_slice(&1u32.to_le_bytes());
            let entry = b"TITLE=Old Title";
            comment.extend_from_slice(&(entry.len() as u32).to_le_bytes());
            comment.extend_from_slice(entry);
            blocks.push((4, comment));
        }
        if opts.picture {
            blocks.push((6, test_picture_block()));
        }
        if opts.bad_picture {
            blocks.push((6, vec![0xFF, 0x00]));
        }
        if opts.seektable {
            // One placeholder point: presence is recorded, points never
            // imported, so the content is irrelevant to the walker.
            blocks.push((3, vec![0xFF; 18]));
        }
        for (index, (typ, body)) in blocks.iter().enumerate() {
            let last = index + 1 == blocks.len();
            out.push((if last { 0x80 } else { 0x00 }) | typ);
            out.push((body.len() >> 16) as u8);
            out.push((body.len() >> 8) as u8);
            out.push(body.len() as u8);
            out.extend_from_slice(body);
        }
        for frame_no in 0..opts.frames {
            out.extend_from_slice(&test_frame(u64::from(frame_no), TEST_BLOCK, frame_no as u8));
        }
        out
    }

    fn default_opts() -> TestFlacOpts {
        TestFlacOpts {
            frames: 30,
            vorbis: true,
            picture: true,
            bad_picture: false,
            seektable: false,
            md5: [0u8; 16],
        }
    }

    fn test_tags<'a>() -> TrackTags<'a> {
        TrackTags {
            title: "Test Track",
            artist: "Test Artist",
            album: "Test Album",
            album_artist: Some("Test Album Artist"),
            date: Some("2026-11-03"),
            track_number: Some(3),
            track_total: Some(12),
            disc_number: Some(1),
            disc_total: Some(2),
            recording_mbid: "f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f",
            release_mbid: "a1b2c3d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d",
            artist_mbids: &["11111111-2222-4333-8444-555555555555"],
            isrc: Some("USRC17607839"),
        }
    }

    #[test]
    fn walk_accepts_good_fixture() {
        let bytes = build_test_flac(&default_opts());
        let walk = walk_flac(&bytes).expect("good fixture must walk");
        assert_eq!(walk.frame_count, 30);
        assert_eq!(walk.total_samples, 30 * u64::from(TEST_BLOCK));
        assert_eq!(walk.streaminfo.sample_rate, TEST_RATE);
        assert_eq!(walk.streaminfo.channels, 2);
        assert_eq!(walk.streaminfo.bits_per_sample, 8);
        assert!(!walk.has_seektable);
        // First seek point is sample 0 at the first frame header.
        assert_eq!(walk.seek_index[0].sample_number, 0);
        assert_eq!(walk.seek_index[0].byte_offset, walk.first_frame_offset);
        // Spans tile the audio without gaps or overlaps.
        for window in walk.frame_spans.windows(2) {
            assert_eq!(
                window[0].byte_offset + u64::from(window[0].byte_len),
                window[1].byte_offset
            );
            assert_eq!(
                window[0].first_sample + u64::from(window[0].samples),
                window[1].first_sample
            );
        }
        let last = walk.frame_spans.last().expect("frames");
        assert_eq!(
            last.byte_offset + u64::from(last.byte_len),
            bytes.len() as u64
        );
    }

    #[test]
    fn walk_records_seektable_presence_without_importing() {
        let mut opts = default_opts();
        opts.seektable = true;
        let walk = walk_flac(&build_test_flac(&opts)).expect("seektable file must walk");
        assert!(walk.has_seektable);
        // The built index covers the file regardless of the embedded one.
        assert_eq!(walk.seek_index[0].sample_number, 0);
    }

    #[test]
    fn walk_rejects_non_flac() {
        assert_eq!(walk_flac(b"ID3\x04junk").unwrap_err(), FlacReject::NotFlac);
        assert_eq!(walk_flac(b"").unwrap_err(), FlacReject::NotFlac);
        assert_eq!(walk_flac(b"fLa").unwrap_err(), FlacReject::NotFlac);
    }

    #[test]
    fn walk_rejects_truncation() {
        let bytes = build_test_flac(&default_opts());
        // A torn tail fails closed (CRC mismatch over the torn frame).
        assert!(walk_flac(&bytes[..bytes.len() - 10]).is_err());
        // A cut inside the metadata cannot be mistaken for audio.
        assert_eq!(walk_flac(&bytes[..20]).unwrap_err(), FlacReject::Truncated);
        assert_eq!(walk_flac(&bytes[..6]).unwrap_err(), FlacReject::Truncated);
    }

    #[test]
    fn walk_rejects_frame_crc_corruption() {
        let mut bytes = build_test_flac(&default_opts());
        // Flip a byte deep inside the second frame's audio body: the
        // header CRC-8 still passes, the frame CRC-16 must fail.
        let second = walk_flac(&bytes).expect("baseline").frame_spans[1];
        let corrupt_at = second.byte_offset as usize + 12;
        bytes[corrupt_at] ^= 0x01;
        assert_eq!(walk_flac(&bytes).unwrap_err(), FlacReject::BadFrameCrc);
    }

    #[test]
    fn walk_rejects_header_corruption() {
        let mut bytes = build_test_flac(&default_opts());
        // Corrupt the sync of the third frame without fixing CRCs: the
        // second frame's extent scan finds no valid continuation.
        let third = walk_flac(&bytes).expect("baseline").frame_spans[2];
        bytes[third.byte_offset as usize] = 0x00;
        assert!(walk_flac(&bytes).is_err());
    }

    #[test]
    fn walk_rejects_sample_count_mismatch() {
        let mut opts = default_opts();
        opts.frames = 10;
        let mut bytes = build_test_flac(&opts);
        // STREAMINFO claims 11 frames' worth of samples: the packed
        // rate/channels/bps/total u64 sits at body bytes 10..18.
        let total_pos = 4 + 4 + 10;
        let claimed = 11u64 * u64::from(TEST_BLOCK);
        let packed =
            (u64::from(TEST_RATE) << 44) | (1u64 << 41) | (7u64 << 36) | (claimed & 0xF_FFFF_FFFF);
        bytes[total_pos..total_pos + 8].copy_from_slice(&packed.to_be_bytes());
        assert_eq!(
            walk_flac(&bytes).unwrap_err(),
            FlacReject::SampleCountMismatch
        );
    }

    #[test]
    fn pcm_md5_unknown_passes_and_mismatch_fails() {
        let bytes = build_test_flac(&default_opts());
        let walk = walk_flac(&bytes).expect("baseline");
        assert!(verify_pcm_md5(&walk.streaminfo, &[0xABu8; 16]).is_ok());
        let mut opts = default_opts();
        opts.md5 = [0x42u8; 16];
        let walk = walk_flac(&build_test_flac(&opts)).expect("md5 fixture");
        assert!(verify_pcm_md5(&walk.streaminfo, &[0x42u8; 16]).is_ok());
        assert_eq!(
            verify_pcm_md5(&walk.streaminfo, &[0x43u8; 16]).unwrap_err(),
            FlacReject::Md5Mismatch
        );
    }

    #[test]
    fn seek_index_spacing_and_resolution() {
        // ~10.4 s of audio: points must land every 5–10 s.
        let mut opts = default_opts();
        opts.frames = 2400;
        opts.vorbis = false;
        opts.picture = false;
        let bytes = build_test_flac(&opts);
        let walk = walk_flac(&bytes).expect("long fixture");
        assert!(walk.seek_index.len() >= 2, "{}", walk.seek_index.len());
        let rate = u64::from(TEST_RATE);
        for window in walk.seek_index.windows(2) {
            let gap = window[1].sample_number - window[0].sample_number;
            assert!(
                gap >= 5 * rate && gap < 10 * rate,
                "seek gap {gap} samples outside 5-10 s"
            );
            assert!(window[1].byte_offset > window[0].byte_offset);
        }
        // Sample → offset lands inside the frame covering the sample.
        for sample in [0, 1, 44099, 44100, 220500, walk.total_samples - 1] {
            let offset = walk.offset_for_sample(sample);
            let span = walk
                .frame_spans
                .iter()
                .find(|span| {
                    sample >= span.first_sample
                        && sample < span.first_sample + u64::from(span.samples)
                })
                .expect("sample must be covered");
            assert_eq!(offset, walk.offset_for_sample(span.first_sample));
            assert!(offset <= span.byte_offset);
        }
        // Time → range covers the requested span.
        let staged_len = bytes.len() as u64;
        let (start, end) = walk
            .range_for_time_span(2.0, 3.0, staged_len)
            .expect("range");
        assert!(start < end);
        assert!(end <= staged_len);
        assert_eq!(start, walk.offset_for_sample(2 * u64::from(TEST_RATE)));
        // The 5 s mark (sample 220500) lands just before the second seek
        // point on the 192-sample frame grid, so the range must extend to
        // that point rather than degenerating to an empty range.
        assert_eq!(end, walk.seek_index[1].byte_offset);
    }

    #[test]
    fn tag_rewrite_replaces_vorbis_and_strips_pictures() {
        let input = build_test_flac(&default_opts());
        let input_walk = walk_flac(&input).expect("baseline");
        let tagged = rewrite_tags(&input, &test_tags()).expect("rewrite");
        // One Vorbis block, zero pictures, streaminfo untouched.
        let (blocks, _) = split_metadata(&tagged.bytes).expect("output metadata");
        assert_eq!(blocks.iter().filter(|b| b.block_type == 4).count(), 1);
        assert_eq!(blocks.iter().filter(|b| b.block_type == 6).count(), 0);
        assert_eq!(blocks[0].block_type, 0);
        // The stripped output re-walks clean with identical audio frames.
        assert_eq!(tagged.walk.frame_count, input_walk.frame_count);
        assert_eq!(tagged.walk.total_samples, input_walk.total_samples);
        assert_eq!(tagged.walk.frame_spans.len(), input_walk.frame_spans.len());
        for (before, after) in input_walk
            .frame_spans
            .iter()
            .zip(tagged.walk.frame_spans.iter())
        {
            assert_eq!(before.samples, after.samples);
            assert_eq!(before.first_sample, after.first_sample);
            let before_body = {
                let start = before.byte_offset as usize;
                &input[start..start + before.byte_len as usize]
            };
            let after_body = {
                let start = after.byte_offset as usize;
                &tagged.bytes[start..start + after.byte_len as usize]
            };
            // Frame headers differ only where the blocking number moved
            // (it did not — same frame order), so raw bytes are identical.
            assert_eq!(before_body, after_body);
        }
        // Art was staged for the R2 art bucket in the ADR 0003 key shape.
        assert_eq!(tagged.art.len(), 1);
        let picture = &tagged.art[0];
        assert_eq!(picture.mime, "image/png");
        assert_eq!((picture.width, picture.height), (1, 1));
        assert!(picture.r2_key.starts_with("art/"));
        assert_eq!(
            picture.r2_key,
            format!(
                "art/{}/{}/{}",
                &picture.sha256_hex[0..2],
                &picture.sha256_hex[2..4],
                picture.sha256_hex
            )
        );
        assert!(tagged.skipped_art.is_empty());
        // sha256 is stable across re-runs of the same input+tags.
        let again = rewrite_tags(&input, &test_tags()).expect("rewrite again");
        assert_eq!(tagged.sha256_hex, again.sha256_hex);
        assert_eq!(tagged.bytes, again.bytes);
    }

    #[test]
    fn tag_rewrite_inserts_vorbis_when_absent_and_skips_bad_pictures() {
        let mut opts = default_opts();
        opts.vorbis = false;
        opts.bad_picture = true;
        opts.picture = false;
        let input = build_test_flac(&opts);
        let tagged = rewrite_tags(&input, &test_tags()).expect("rewrite");
        let (blocks, _) = split_metadata(&tagged.bytes).expect("output metadata");
        assert_eq!(blocks.iter().filter(|b| b.block_type == 4).count(), 1);
        assert!(tagged.art.is_empty());
        assert_eq!(tagged.skipped_art.len(), 1);
        walk_flac(&tagged.bytes).expect("output re-walks");
    }

    #[test]
    fn vorbis_block_carries_picard_keys_and_validates_mbids() {
        let body = build_vorbis_comment_block(&test_tags()).expect("comment");
        let text = String::from_utf8_lossy(&body);
        for key in [
            "TITLE=Test Track",
            "ARTIST=Test Artist",
            "ALBUM=Test Album",
            "ALBUMARTIST=Test Album Artist",
            "DATE=2026-11-03",
            "TRACKNUMBER=3",
            "TRACKTOTAL=12",
            "DISCNUMBER=1",
            "DISCTOTAL=2",
            "ISRC=USRC17607839",
            "MUSICBRAINZ_TRACKID=f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f",
            "MUSICBRAINZ_RELEASEID=a1b2c3d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d",
            "MUSICBRAINZ_ARTISTID=11111111-2222-4333-8444-555555555555",
        ] {
            assert!(text.contains(key), "missing {key}");
        }
        let mut bad = test_tags();
        bad.recording_mbid = "not-a-mbid";
        assert_eq!(
            build_vorbis_comment_block(&bad).unwrap_err(),
            FlacReject::BadTag("recording mbid must be canonical hex")
        );
        let mut bad_isrc = test_tags();
        bad_isrc.isrc = Some("short");
        assert!(build_vorbis_comment_block(&bad_isrc).is_err());
        let mut empty = test_tags();
        empty.title = "  ";
        assert!(build_vorbis_comment_block(&empty).is_err());
    }

    #[test]
    fn public_sidecar_carries_no_mbids_or_isrc() {
        let input = build_test_flac(&default_opts());
        let tagged = rewrite_tags(&input, &test_tags()).expect("rewrite");
        let sidecar = FlacSidecar::public(&tagged);
        let public = sidecar.public_json();
        let tags = test_tags();
        for secret in [
            tags.recording_mbid,
            tags.release_mbid,
            tags.artist_mbids[0],
            tags.isrc.expect("isrc"),
        ] {
            assert!(!public.contains(secret), "public sidecar leaks {secret}");
        }
        // The sealed payload is where the match identity lives.
        let sealed = sidecar.sealed_payload_json(&tags);
        for secret in [
            tags.recording_mbid,
            tags.release_mbid,
            tags.artist_mbids[0],
            tags.isrc.expect("isrc"),
        ] {
            assert!(sealed.contains(secret), "sealed payload misses {secret}");
        }
        // Round-trip: the JSON the envelope seals parses back.
        let parsed: serde_json::Value =
            serde_json::from_str(&sealed).expect("sealed payload is json");
        assert_eq!(parsed["sha256"], tagged.sha256_hex);
        assert_eq!(parsed["mb"]["isrc"], "USRC17607839");
        let public_parsed: serde_json::Value =
            serde_json::from_str(&public).expect("public sidecar is json");
        assert!(public_parsed.get("mb").is_none());
        assert!(public_parsed.get("tags").is_none());
    }

    #[test]
    fn art_bucket_names_match_the_mb_worker() {
        // The ingest art path and workers/mb must resolve the same
        // buckets: pin both spellings in source so a rename on either
        // side fails here instead of splitting art across buckets.
        let mb = include_str!("../../mb/src/lib.rs");
        for bucket in [ART_BUCKET_STAGING, ART_BUCKET_PROD] {
            assert!(
                mb.contains(&format!("\"{bucket}\"")),
                "workers/mb must define the {bucket} bucket"
            );
        }
        assert_eq!(ART_BUCKET_STAGING, "nntunev2-art-staging");
        assert_eq!(ART_BUCKET_PROD, "nntunev2-art-prod");
        assert_eq!(
            art_r2_key("ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12"),
            "art/ab/12/ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12"
        );
    }

    #[test]
    fn reject_codes_are_stable() {
        let cases = [
            (FlacReject::NotFlac, "flac_not_flac"),
            (FlacReject::Truncated, "flac_truncated"),
            (FlacReject::BadMetadata("x"), "flac_bad_metadata"),
            (FlacReject::BadFrameHeader, "flac_bad_frame_header"),
            (FlacReject::BadFrameCrc, "flac_bad_frame_crc"),
            (FlacReject::FrameTooLarge, "flac_frame_too_large"),
            (FlacReject::TooManyFrames, "flac_too_many_frames"),
            (
                FlacReject::SampleCountMismatch,
                "flac_sample_count_mismatch",
            ),
            (FlacReject::Md5Mismatch, "flac_md5_mismatch"),
            (FlacReject::BadTag("x"), "flac_bad_tag"),
            (FlacReject::BadPicture("x"), "flac_bad_picture"),
        ];
        for (reject, code) in cases {
            assert_eq!(reject.code(), code);
            assert!(!reject.detail().is_empty());
        }
    }
}
