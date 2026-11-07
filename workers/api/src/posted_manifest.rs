//! API-owned posted_manifest_v1 mint/open boundary. Aggregate NZB, base64 and
//! JSON buffers are bounded and wipe on drop before leaving this boundary. The
//! existing XML writer's temporary escaped attributes are bounded by a preflight,
//! but are not wiping buffers. No files, URLs, Queue payloads or diagnostics carry
//! the document; caller-owned inputs retain their existing lifetime.
//!
//! This authenticates custody, not posting, provider independence or selection.
//! Geometry is derived from media v1; encoded NZB sizes remain actual wire sizes,
//! not ciphertext lengths. Callers supply their already-validated FLAC index and
//! recovery references. Open returns authenticated JSON bytes, not a trusted
//! serving plan: readers must validate its schema and bind it to selected custody.

use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use nntune_core::{
    media_layout,
    nzb::normalize_message_id,
    nzb_writer::{self, FileInput},
};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::{
    field_envelope::{self, Context, FieldEnvelope},
    posted_media::MediaKeyError,
    rotation::KekSource,
};

/// Two MiB of XML plus base64 expansion, article geometry and index metadata.
/// Larger documents refuse rather than storing an unbounded secret envelope.
pub const MAX_POSTED_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_POSTED_NZB_BYTES: usize = 2 * 1024 * 1024;

/// Borrowed secrets; deliberately no Debug or serialization of this input.
pub struct ManifestInput<'a> {
    pub file: &'a FileInput,
    pub plaintext_bytes: u64,
    pub chunks_per_article: u64,
    pub flac_seek_index: Option<&'a serde_json::Value>,
    pub par2_message_ids: &'a [String],
    pub posted_via: &'a [String],
}

/// Maps directly to manifest_* columns. No plaintext document or Debug.
#[derive(Clone)]
pub struct SealedManifest {
    pub envelope_version: u16,
    pub payload_format: u16,
    pub kek_version: u32,
    pub envelope: FieldEnvelope,
}

#[derive(Serialize)]
struct Document<'a> {
    format: u16,
    nzb_xml_base64: &'a str,
    plaintext_ranges: Vec<[u64; 2]>,
    ciphertext_offsets: Vec<[u64; 2]>,
    flac_seek_index: Option<&'a serde_json::Value>,
    par2_message_ids: &'a [String],
    posted_via: &'a [String],
}

struct SecretWriter {
    bytes: Zeroizing<Vec<u8>>,
    limit: usize,
}

impl SecretWriter {
    fn new(limit: usize) -> Self {
        Self {
            // Reserve once so growth never frees earlier plaintext allocations.
            bytes: Zeroizing::new(Vec::with_capacity(limit)),
            limit,
        }
    }
}

impl Write for SecretWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("posted manifest size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn context(hash: &str) -> Result<Context<'_>, MediaKeyError> {
    let mut context = crate::posted_media::context(hash)?;
    context.field = "manifest";
    Ok(context)
}

fn bounded_xml_input(file: &FileInput) -> bool {
    // quick-xml builds start elements before the sink sees them. Bound that
    // staging too: at most six output bytes per input byte (attribute quotes),
    // plus generous fixed markup/date/number overhead per element.
    let Some(overhead) = file
        .groups
        .len()
        .checked_add(file.segments.len())
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(256))
    else {
        return false;
    };
    let mut fields = [&file.subject, &file.poster]
        .into_iter()
        .chain(file.groups.iter())
        .chain(file.segments.iter().map(|s| &s.message_id));
    fields
        .try_fold(overhead, |total, value| {
            value
                .len()
                .checked_mul(6)
                .and_then(|len| total.checked_add(len))
                .filter(|size| *size <= MAX_POSTED_NZB_BYTES)
        })
        .is_some_and(|size| size <= MAX_POSTED_NZB_BYTES)
}

/// Seal the actual NZB writer output and canonical media-v1 article coordinates.
/// Replay must reuse the surviving envelope; minting grants no posting authority.
pub fn seal(
    keks: &impl KekSource,
    kek_version: u32,
    plaintext_sha256: &str,
    input: &ManifestInput<'_>,
) -> Result<SealedManifest, MediaKeyError> {
    let context = context(plaintext_sha256)?;
    if kek_version == 0 || kek_version > i32::MAX as u32 || input.plaintext_bytes == 0 {
        return Err(MediaKeyError::Invalid);
    }
    let kek = keks.kek(kek_version).ok_or(MediaKeyError::UnknownKey)?;
    let spans = media_layout::plan_articles(input.plaintext_bytes, input.chunks_per_article)
        .ok_or(MediaKeyError::Invalid)?;
    if spans.len() != input.file.segments.len()
        || !bounded_xml_input(input.file)
        || input.par2_message_ids.len() > media_layout::MAX_ARTICLE_SPANS as usize
        || input
            .par2_message_ids
            .iter()
            .any(|id| normalize_message_id(id).is_err())
        || input.posted_via.is_empty()
        || input.posted_via.len() > 2
        || input.posted_via.iter().any(|label| {
            label.is_empty()
                || label.len() > 64
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        || input
            .flac_seek_index
            .is_some_and(|index| !index.is_object())
    {
        return Err(MediaKeyError::Invalid);
    }
    let mut nzb = SecretWriter::new(MAX_POSTED_NZB_BYTES);
    nzb_writer::write_nzb_to(&mut nzb, std::slice::from_ref(input.file))
        .map_err(|_| MediaKeyError::Invalid)?;
    let encoded = Zeroizing::new(STANDARD.encode(&nzb.bytes));
    let document = Document {
        format: 1,
        nzb_xml_base64: &encoded,
        plaintext_ranges: spans
            .iter()
            .map(|s| [s.plaintext_start, s.plaintext_end])
            .collect(),
        ciphertext_offsets: spans
            .iter()
            .map(|s| [s.ciphertext_offset, s.ciphertext_len])
            .collect(),
        flac_seek_index: input.flac_seek_index,
        par2_message_ids: input.par2_message_ids,
        posted_via: input.posted_via,
    };
    let mut json = SecretWriter::new(MAX_POSTED_MANIFEST_BYTES);
    serde_json::to_writer(&mut json, &document).map_err(|_| MediaKeyError::Invalid)?;
    let envelope = field_envelope::seal(kek, kek_version, &context, &json.bytes)
        .map_err(|_| MediaKeyError::Crypto)?;
    Ok(SealedManifest {
        envelope_version: 2,
        payload_format: 1,
        kek_version,
        envelope,
    })
}

/// Authenticate only the exact selected row's manifest field. No schema,
/// header/chunk authentication, content digest or serving authority is inferred.
pub fn open(
    keks: &impl KekSource,
    plaintext_sha256: &str,
    stored: &SealedManifest,
) -> Result<Zeroizing<Vec<u8>>, MediaKeyError> {
    let context = context(plaintext_sha256)?;
    if stored.envelope_version != 2
        || stored.payload_format != 1
        || stored.kek_version == 0
        || stored.kek_version > i32::MAX as u32
        || !(17..=MAX_POSTED_MANIFEST_BYTES + 16).contains(&stored.envelope.ciphertext.len())
        || stored.envelope.dek_wrapped.len() != 48
    {
        return Err(MediaKeyError::Invalid);
    }
    let kek = keks
        .kek(stored.kek_version)
        .ok_or(MediaKeyError::UnknownKey)?;
    field_envelope::open(kek, stored.kek_version, &context, &stored.envelope)
        .map(Zeroizing::new)
        .map_err(|_| MediaKeyError::Crypto)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntune_core::nzb_writer::SegmentInput;

    struct Keys(Zeroizing<[u8; 32]>);
    impl Keys {
        fn new() -> Self {
            Self(Zeroizing::new(
                crate::random::secure_random_bytes().unwrap(),
            ))
        }
    }
    impl KekSource for Keys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            (version == 1).then_some(&self.0)
        }
    }

    fn file(count: usize) -> FileInput {
        FileInput {
            subject: "synthetic".into(),
            poster: "synthetic".into(),
            date: 1,
            groups: vec!["synthetic".into()],
            segments: (1..=count)
                .map(|number| SegmentInput {
                    number,
                    message_id: format!("{:032x}@postie", number),
                    encoded_bytes: 123 + number as i64,
                })
                .collect(),
        }
    }

    fn input(file: &FileInput) -> ManifestInput<'_> {
        ManifestInput {
            file,
            plaintext_bytes: 65_537,
            chunks_per_article: 1,
            flac_seek_index: None,
            par2_message_ids: &[],
            posted_via: &[],
        }
    }

    #[test]
    fn actual_nzb_and_successor_coordinates_roundtrip_without_size_conflation() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        let file = file(2);
        let labels = vec!["backbone-a".into(), "backbone-b".into()];
        let input = ManifestInput {
            posted_via: &labels,
            ..input(&file)
        };
        let sealed = seal(&keys, 1, &row, &input).unwrap();
        let opened = open(&keys, &row, &sealed).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&opened).unwrap();
        let nzb = Zeroizing::new(
            STANDARD
                .decode(document["nzb_xml_base64"].as_str().unwrap())
                .unwrap(),
        );
        assert!(nntune_core::nzb::parse(&nzb).is_ok());
        let mut expected = SecretWriter::new(MAX_POSTED_NZB_BYTES);
        nzb_writer::write_nzb_to(&mut expected, std::slice::from_ref(&file)).unwrap();
        assert!(*nzb == *expected.bytes);
        assert_eq!(
            document["plaintext_ranges"],
            serde_json::json!([[0, 65536], [65536, 65537]])
        );
        assert_eq!(
            document["ciphertext_offsets"],
            serde_json::json!([[48, 65552], [65600, 17]])
        );
        assert!(!sealed
            .envelope
            .ciphertext
            .windows(nzb.len())
            .any(|w| w == nzb.as_slice()));
    }

    #[test]
    fn substitution_tamper_and_wrong_key_refuse() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        let file = file(2);
        let labels = vec!["backbone-a".into()];
        let input = ManifestInput {
            posted_via: &labels,
            ..input(&file)
        };
        let sealed = seal(&keys, 1, &row, &input).unwrap();
        assert!(matches!(
            open(&keys, &"b".repeat(64), &sealed),
            Err(MediaKeyError::Crypto)
        ));
        assert!(matches!(
            open(&Keys::new(), &row, &sealed),
            Err(MediaKeyError::Crypto)
        ));
        let mut tampered = sealed.clone();
        tampered.envelope.ciphertext[0] ^= 1;
        assert!(matches!(
            open(&keys, &row, &tampered),
            Err(MediaKeyError::Crypto)
        ));
        let key = crate::posted_media::mint(&keys, 1, &row)
            .unwrap()
            .sealed_key;
        let swapped = SealedManifest {
            envelope: key.envelope,
            ..sealed
        };
        assert!(matches!(
            open(&keys, &row, &swapped),
            Err(MediaKeyError::Crypto)
        ));
    }

    #[test]
    fn invalid_geometry_labels_and_envelope_shapes_refuse() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        let file = file(2);
        let labels = vec!["backbone-a".into()];
        let valid = ManifestInput {
            posted_via: &labels,
            ..input(&file)
        };
        for (bytes, chunks) in [(0, 1), (1, 1), (65_537, 0)] {
            let input = ManifestInput {
                plaintext_bytes: bytes,
                chunks_per_article: chunks,
                ..valid
            };
            assert!(matches!(
                seal(&keys, 1, &row, &input),
                Err(MediaKeyError::Invalid)
            ));
        }
        assert!(matches!(
            seal(&keys, 1, &row, &input(&file)),
            Err(MediaKeyError::Invalid)
        ));
        let sealed = seal(&keys, 1, &row, &valid).unwrap();
        for version in [0, 1, 3] {
            let stored = SealedManifest {
                envelope_version: version,
                ..sealed.clone()
            };
            assert!(matches!(
                open(&keys, &row, &stored),
                Err(MediaKeyError::Invalid)
            ));
        }
        let mut stored = sealed;
        stored
            .envelope
            .ciphertext
            .resize(MAX_POSTED_MANIFEST_BYTES + 17, 0);
        assert!(matches!(
            open(&keys, &row, &stored),
            Err(MediaKeyError::Invalid)
        ));
    }

    #[test]
    fn escaped_attribute_bomb_refuses_before_xml_staging() {
        let keys = Keys::new();
        let mut file = file(2);
        let labels = vec!["backbone-a".into()];
        assert!(bounded_xml_input(&file));
        file.subject = "&".repeat(MAX_POSTED_NZB_BYTES / 6 + 1);
        assert!(!bounded_xml_input(&file));
        let input = ManifestInput {
            posted_via: &labels,
            ..input(&file)
        };
        assert!(matches!(
            seal(&keys, 1, &"a".repeat(64), &input),
            Err(MediaKeyError::Invalid)
        ));
    }

    #[test]
    fn bounded_writers_refuse_before_copying_over_limit() {
        let mut writer = SecretWriter::new(3);
        writer.write_all(&[1, 2, 3]).unwrap();
        assert!(writer.write_all(&[4]).is_err());
        assert_eq!(writer.bytes.as_slice(), &[1, 2, 3]);
    }
}
