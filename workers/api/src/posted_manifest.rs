//! API-owned posted_manifest_v1 mint/open boundary. Aggregate NZB, base64 and
//! JSON buffers are bounded and wipe on drop before leaving this boundary. The
//! existing XML writer's temporary escaped attributes are bounded by a preflight,
//! but are not wiping buffers. No files, URLs, Queue payloads or diagnostics carry
//! the document; caller-owned inputs retain their existing lifetime.
//!
//! This authenticates custody, not posting, provider independence or selection.
//! Geometry is derived from media v1; encoded NZB sizes remain actual wire sizes,
//! not ciphertext lengths. Callers supply their already-validated FLAC index and
//! recovery references. Raw open authenticates JSON only; validated open also
//! checks schema, NZB structure and exact caller-selected media-v1 geometry.
//! Neither authenticates media content, FLAC seek entries or provider topology.
//! Returned JSON/NZB buffers wipe; serde/XML parser intermediates do not all wipe.

use std::{
    collections::HashSet,
    fmt,
    io::{self, Write},
    marker::PhantomData,
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use nntune_core::{
    media_layout, nzb,
    nzb_writer::{self, FileInput},
    segment_identity::valid_segment_id,
};
use serde::{de, Deserialize, Deserializer, Serialize};
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

fn valid_labels(labels: &[String]) -> bool {
    !labels.is_empty()
        && labels.len() <= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 64
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        && (labels.len() != 2 || labels[0] != labels[1])
}

fn valid_identities<'a>(mut ids: impl Iterator<Item = &'a String>) -> bool {
    let mut identities = HashSet::new();
    ids.all(|id| valid_segment_id(id) && identities.insert(id.as_str()))
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
        || !valid_identities(
            input
                .file
                .segments
                .iter()
                .map(|segment| &segment.message_id)
                .chain(input.par2_message_ids.iter()),
        )
        || !valid_labels(input.posted_via)
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

/// Secret document validated against caller-selected geometry. Deliberately no
/// Debug/serde. Accessors borrow wiping buffers; do not log or publish their data.
/// This carries no selection, network, content-integrity or cleanup authority.
pub struct ValidatedManifest {
    json: Zeroizing<Vec<u8>>,
    nzb: Zeroizing<Vec<u8>>,
    spans: Vec<media_layout::ArticleSpan>,
}

impl ValidatedManifest {
    pub fn json(&self) -> &[u8] {
        &self.json
    }

    pub fn nzb_xml(&self) -> &[u8] {
        &self.nzb
    }

    pub fn article_spans(&self) -> &[media_layout::ArticleSpan] {
        &self.spans
    }
}

// Bound repeated coordinate/reference arrays independently of the byte cap,
// refusing before allocating a full hostile array.
struct BoundedList<T, const MAX: usize>(Vec<T>);

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for BoundedList<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T, const MAX: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> de::Visitor<'de> for Visitor<T, MAX> {
            type Value = BoundedList<T, MAX>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bounded manifest array")
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element()? {
                    if values.len() == MAX {
                        return Err(de::Error::custom("manifest array limit"));
                    }
                    values.push(value);
                }
                Ok(BoundedList(values))
            }
        }
        deserializer.deserialize_seq(Visitor::<T, MAX>(PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDocument<'a> {
    format: u16,
    // The writer's base64 alphabet never needs JSON escaping. Borrow it without
    // allocating another document-sized String; escaped alternatives refuse.
    nzb_xml_base64: &'a str,
    plaintext_ranges: BoundedList<[u64; 2], { nzb::MAX_SEGMENTS_PER_FILE }>,
    ciphertext_offsets: BoundedList<[u64; 2], { nzb::MAX_SEGMENTS_PER_FILE }>,
    flac_seek_index: Option<serde_json::Value>,
    par2_message_ids: BoundedList<String, { media_layout::MAX_ARTICLE_SPANS as usize }>,
    posted_via: BoundedList<String, 2>,
}

fn decode_document(bytes: &[u8]) -> Result<StoredDocument<'_>, MediaKeyError> {
    struct DocumentVisitor;
    impl<'de> de::Visitor<'de> for DocumentVisitor {
        type Value = StoredDocument<'de>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("manifest object")
        }

        fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            StoredDocument::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }
    // Derived struct decoders also accept positional arrays. Force a map and
    // consume EOF, preserving duplicate/unknown-field rejection in the derive.
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let document = Deserializer::deserialize_map(&mut decoder, DocumentVisitor)
        .map_err(|_| MediaKeyError::Invalid)?;
    decoder.end().map_err(|_| MediaKeyError::Invalid)?;
    Ok(document)
}

/// Authenticate the selected hash's envelope, then reject malformed or mismatched
/// semantic metadata. Size and chunk policy must come from immutable selected
/// custody, not from this document or untrusted Queue hints. The caller separately
/// authenticates the selected media header/key with posted_media::open.
pub fn open_validated(
    keks: &impl KekSource,
    plaintext_sha256: &str,
    stored: &SealedManifest,
    plaintext_bytes: u64,
    chunks_per_article: u64,
) -> Result<ValidatedManifest, MediaKeyError> {
    if plaintext_bytes == 0 {
        return Err(MediaKeyError::Invalid);
    }
    let spans = media_layout::plan_articles(plaintext_bytes, chunks_per_article)
        .filter(|spans| spans.len() <= nzb::MAX_SEGMENTS_PER_FILE)
        .ok_or(MediaKeyError::Invalid)?;
    let json = open(keks, plaintext_sha256, stored)?;
    let document = decode_document(&json)?;
    if document.format != 1
        || document.plaintext_ranges.0.len() != spans.len()
        || document.ciphertext_offsets.0.len() != spans.len()
        || spans.iter().enumerate().any(|(i, span)| {
            document.plaintext_ranges.0[i] != [span.plaintext_start, span.plaintext_end]
                || document.ciphertext_offsets.0[i] != [span.ciphertext_offset, span.ciphertext_len]
        })
        || document
            .flac_seek_index
            .as_ref()
            .is_some_and(|index| !index.is_object())
        || !valid_labels(&document.posted_via.0)
        || document.nzb_xml_base64.len() > MAX_POSTED_NZB_BYTES.div_ceil(3) * 4
    {
        return Err(MediaKeyError::Invalid);
    }
    // Decode into the final wiping allocation, including partial/error output.
    let mut xml = Zeroizing::new(vec![0; MAX_POSTED_NZB_BYTES]);
    let decoded = STANDARD
        .decode_slice(document.nzb_xml_base64, xml.as_mut_slice())
        .map_err(|_| MediaKeyError::Invalid)?;
    xml.truncate(decoded);
    let parsed = nzb::Parser::new(nzb::Limits {
        files: 1,
        segments_per_file: spans.len(),
        document_bytes: MAX_POSTED_NZB_BYTES as u64,
        ..nzb::Limits::default()
    })
    .and_then(|parser| parser.parse(xml.as_slice()))
    .map_err(|_| MediaKeyError::Invalid)?;
    if parsed.has_password
        || parsed.files.len() != 1
        || parsed.files[0].segments.len() != spans.len()
    {
        return Err(MediaKeyError::Invalid);
    }
    if !valid_identities(
        parsed.files[0]
            .segments
            .iter()
            .map(|segment| &segment.message_id)
            .chain(document.par2_message_ids.0.iter()),
    ) {
        return Err(MediaKeyError::Invalid);
    }
    // Serde's borrowed base64 view must be released before moving its owner.
    drop(document);
    Ok(ValidatedManifest {
        json,
        nzb: xml,
        spans,
    })
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
        let duplicate_labels = vec!["backbone-a".into(), "backbone-a".into()];
        assert!(seal(
            &keys,
            1,
            &row,
            &ManifestInput {
                posted_via: &duplicate_labels,
                ..valid
            },
        )
        .is_err());
        let invalid_ids = vec!["not-an-identity".into()];
        assert!(seal(
            &keys,
            1,
            &row,
            &ManifestInput {
                par2_message_ids: &invalid_ids,
                ..valid
            },
        )
        .is_err());
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

    fn seal_document(keys: &Keys, row: &str, bytes: &[u8]) -> SealedManifest {
        SealedManifest {
            envelope_version: 2,
            payload_format: 1,
            kek_version: 1,
            envelope: field_envelope::seal(keys.kek(1).unwrap(), 1, &context(row).unwrap(), bytes)
                .unwrap(),
        }
    }

    fn valid_document() -> serde_json::Value {
        let keys = Keys::new();
        let row = "a".repeat(64);
        let file = file(2);
        let labels = vec!["backbone-a".into()];
        let sealed = seal(
            &keys,
            1,
            &row,
            &ManifestInput {
                posted_via: &labels,
                ..input(&file)
            },
        )
        .unwrap();
        serde_json::from_slice(&open(&keys, &row, &sealed).unwrap()).unwrap()
    }

    #[test]
    fn validated_open_binds_exact_geometry_and_preserves_wire_sizes() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        let document = Zeroizing::new(serde_json::to_vec(&valid_document()).unwrap());
        let sealed = seal_document(&keys, &row, &document);
        let opened = open_validated(&keys, &row, &sealed, 65_537, 1).unwrap();
        assert!(*opened.json == *document);
        assert_eq!(opened.article_spans()[1].ciphertext_len, 17);
        let nzb = nzb::parse(opened.nzb_xml()).unwrap();
        assert_eq!(nzb.files[0].segments[1].encoded_bytes, 125);
        for (bytes, chunks) in [(0, 1), (65_536, 1), (65_537, 0), (65_537, 2)] {
            assert!(open_validated(&keys, &row, &sealed, bytes, chunks).is_err());
        }
        assert!(open_validated(&keys, &"b".repeat(64), &sealed, 65_537, 1).is_err());
    }

    #[test]
    fn authenticated_but_invalid_schema_and_coordinates_refuse() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        for (field, value) in [
            ("format", serde_json::json!(2)),
            (
                "plaintext_ranges",
                serde_json::json!([[0, 65536], [65535, 65537]]),
            ),
            (
                "ciphertext_offsets",
                serde_json::json!([[32, 65552], [65584, 17]]),
            ),
            (
                "ciphertext_offsets",
                serde_json::json!([[48, 65552], [65600, 65552]]),
            ),
            ("plaintext_ranges", serde_json::json!([[0, 65536]])),
            ("ciphertext_offsets", serde_json::json!([])),
            ("flac_seek_index", serde_json::json!([])),
            ("posted_via", serde_json::json!([])),
            (
                "posted_via",
                serde_json::json!(["backbone-a", "backbone-a"]),
            ),
            ("posted_via", serde_json::json!(["a", "b", "c"])),
            ("posted_via", serde_json::json!(["not/a/label"])),
            ("nzb_xml_base64", serde_json::json!("invalid base64")),
            ("unexpected", serde_json::json!(true)),
        ] {
            let mut document = valid_document();
            document[field] = value;
            let bytes = Zeroizing::new(serde_json::to_vec(&document).unwrap());
            let sealed = seal_document(&keys, &row, &bytes);
            assert!(open(&keys, &row, &sealed).is_ok());
            assert!(matches!(
                open_validated(&keys, &row, &sealed, 65_537, 1),
                Err(MediaKeyError::Invalid)
            ));
        }
    }

    #[test]
    fn duplicate_fields_trailers_and_positional_documents_refuse() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        let valid = serde_json::to_string(&valid_document()).unwrap();
        let duplicate = valid.replacen('{', "{\"format\":1,", 1);
        let value = valid_document();
        let positional = serde_json::json!([
            value["format"],
            value["nzb_xml_base64"],
            value["plaintext_ranges"],
            value["ciphertext_offsets"],
            value["flac_seek_index"],
            value["par2_message_ids"],
            value["posted_via"],
        ])
        .to_string();
        for bytes in [duplicate, format!("{valid} null"), positional, "[]".into()] {
            let bytes = Zeroizing::new(bytes);
            let sealed = seal_document(&keys, &row, bytes.as_bytes());
            assert!(open_validated(&keys, &row, &sealed, 65_537, 1).is_err());
        }
    }

    #[test]
    fn invalid_nzb_sets_and_recovery_identities_refuse() {
        let keys = Keys::new();
        let row = "a".repeat(64);
        for files in [vec![file(1)], vec![file(2), file(2)]] {
            let mut document = valid_document();
            document["nzb_xml_base64"] =
                serde_json::json!(STANDARD.encode(nzb_writer::write_nzb(&files).unwrap()));
            let bytes = Zeroizing::new(serde_json::to_vec(&document).unwrap());
            let sealed = seal_document(&keys, &row, &bytes);
            assert!(open_validated(&keys, &row, &sealed, 65_537, 1).is_err());
        }
        for ids in [
            vec!["not-an-identity".to_string()],
            vec![format!("{:032x}@postie", 1)],
            vec!["recovery@postie".into(), "recovery@postie".into()],
        ] {
            let mut document = valid_document();
            document["par2_message_ids"] = serde_json::json!(ids);
            let bytes = Zeroizing::new(serde_json::to_vec(&document).unwrap());
            let sealed = seal_document(&keys, &row, &bytes);
            assert!(open_validated(&keys, &row, &sealed, 65_537, 1).is_err());
        }
    }

    #[test]
    fn bounded_semantic_arrays_reject_before_full_decode() {
        assert!(serde_json::from_str::<BoundedList<u64, 2>>("[1,2]").is_ok());
        assert!(serde_json::from_str::<BoundedList<u64, 2>>("[1,2,3]").is_err());
        let keys = Keys::new();
        let row = "a".repeat(64);
        let mut document = valid_document();
        document["nzb_xml_base64"] =
            serde_json::json!("a".repeat(MAX_POSTED_NZB_BYTES.div_ceil(3) * 4 + 4));
        let bytes = Zeroizing::new(serde_json::to_vec(&document).unwrap());
        assert!(bytes.len() <= MAX_POSTED_MANIFEST_BYTES);
        let sealed = seal_document(&keys, &row, &bytes);
        assert!(open_validated(&keys, &row, &sealed, 65_537, 1).is_err());
    }

    #[test]
    fn bounded_writers_refuse_before_copying_over_limit() {
        let mut writer = SecretWriter::new(3);
        writer.write_all(&[1, 2, 3]).unwrap();
        assert!(writer.write_all(&[4]).is_err());
        assert_eq!(writer.bytes.as_slice(), &[1, 2, 3]);
    }
}
