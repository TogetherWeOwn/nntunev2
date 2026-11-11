//! Bounded FLAC metadata reader for the accepted-audio producer (ADR 0002 §6).
//!
//! This component reads untrusted hints, not catalog matches. Callers must bind
//! bytes to a verified original selection and enforce the 32 MiB job-wide prefix
//! budget. No network, fingerprint computation, picture validation/staging or
//! checkpoint write happens here; this reader is not yet wired into Matching.
use std::collections::BTreeMap;

use crate::audio_manifest::AudioHints;

pub const MAX_TAG_PREFIX_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMMENTS: usize = 4096;
const MAX_TEXT_BYTES: usize = 4096;
const FIELDS: &[&str] = &[
    "TITLE",
    "ALBUM",
    "ALBUMARTIST",
    "ARTIST",
    "MUSICBRAINZ_ARTISTID",
    "MUSICBRAINZ_ALBUMARTISTID",
    "MUSICBRAINZ_ALBUMID",
    "MUSICBRAINZ_TRACKID",
    "DISCNUMBER",
    "TRACKNUMBER",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagReadError {
    Unsupported,
    TooLarge,
    Malformed,
    Incomplete,
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], TagReadError> {
    if len > input.len() {
        return Err(TagReadError::Incomplete);
    }
    let (value, rest) = input.split_at(len);
    *input = rest;
    Ok(value)
}

fn le_len(input: &mut &[u8]) -> Result<usize, TagReadError> {
    let bytes = take(input, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

fn clean_text(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty() && value.len() <= MAX_TEXT_BYTES && !raw.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn position(raw: Option<String>) -> Option<i32> {
    let raw = raw?;
    let mut parts = raw.split('/');
    let number = parts.next()?;
    if !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number: i32 = number.parse().ok()?;
    if number <= 0 {
        return None;
    }
    if let Some(total) = parts.next() {
        if !total.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let total: i32 = total.parse().ok()?;
        if total < number || parts.next().is_some() {
            return None;
        }
    }
    Some(number)
}

fn mbid(raw: Option<String>) -> Option<String> {
    raw.filter(|value| crate::queue::validate_mbid(value))
}

fn comments(mut body: &[u8], hints: &mut AudioHints) -> Result<(), TagReadError> {
    // An inconsistent length inside a complete block is malformed, not a reason
    // to fetch a bigger prefix. No allocation follows an untrusted length/count.
    let parsed = (|| {
        let vendor_len = le_len(&mut body)?;
        take(&mut body, vendor_len)?;
        let count = le_len(&mut body)?;
        if count > MAX_COMMENTS {
            return Err(TagReadError::Malformed);
        }
        let mut fields = BTreeMap::new();
        for _ in 0..count {
            let len = le_len(&mut body)?;
            let raw =
                std::str::from_utf8(take(&mut body, len)?).map_err(|_| TagReadError::Malformed)?;
            let (name, value) = raw.split_once('=').ok_or(TagReadError::Malformed)?;
            if name.is_empty() || !name.bytes().all(|b| (0x20..=0x7d).contains(&b)) {
                return Err(TagReadError::Malformed);
            }
            if let Some(&key) = FIELDS.iter().find(|key| name.eq_ignore_ascii_case(key)) {
                // Multiple values, even identical ones, are ambiguous claims.
                fields
                    .entry(key)
                    .and_modify(|value| *value = None)
                    .or_insert_with(|| clean_text(value));
            }
        }
        if !body.is_empty() {
            return Err(TagReadError::Malformed);
        }
        let disc_present = fields.contains_key("DISCNUMBER");
        hints.title = fields.remove("TITLE").flatten();
        hints.album = fields.remove("ALBUM").flatten();
        hints.album_artist = fields.remove("ALBUMARTIST").flatten();
        hints.track_artist = fields.remove("ARTIST").flatten();
        hints.artist_mbid = mbid(fields.remove("MUSICBRAINZ_ARTISTID").flatten());
        hints.album_artist_mbid = mbid(fields.remove("MUSICBRAINZ_ALBUMARTISTID").flatten());
        hints.release_mbid = mbid(fields.remove("MUSICBRAINZ_ALBUMID").flatten());
        hints.recording_mbid = mbid(fields.remove("MUSICBRAINZ_TRACKID").flatten());
        hints.track_no = position(fields.remove("TRACKNUMBER").flatten());
        // Preserve presence of a malformed/ambiguous disc tag as a nonpositive
        // hint. ADR 0002 permits implicit disc 1 only if *no* entry has a tag.
        hints.disc_no =
            disc_present.then(|| position(fields.remove("DISCNUMBER").flatten()).unwrap_or(0));
        Ok(())
    })();
    parsed.map_err(|_| TagReadError::Malformed)
}

/// Read a capped FLAC prefix through its last metadata block. An incomplete
/// prefix yields no partial hints, and unknown formats are explicitly refused.
/// Picture blocks are skipped: their presence never implies valid/staged art.
/// STREAMINFO duration is a hint too; no tag can assert a computed fingerprint.
pub fn read_flac_hints(prefix: &[u8]) -> Result<AudioHints, TagReadError> {
    if prefix.len() > MAX_TAG_PREFIX_BYTES {
        return Err(TagReadError::TooLarge);
    }
    let mut input = prefix;
    if take(&mut input, 4)? != b"fLaC" {
        return Err(TagReadError::Unsupported);
    }
    let mut hints = AudioHints::default();
    let mut seen_comments = false;
    for index in 0..MAX_COMMENTS {
        let header = take(&mut input, 4)?;
        let last = header[0] & 128 != 0;
        let kind = header[0] & 127;
        let len = ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
        let body = take(&mut input, len)?;
        if (index == 0 && kind != 0) || (index != 0 && kind == 0) || kind == 127 {
            return Err(TagReadError::Malformed);
        }
        match kind {
            0 => {
                if body.len() != 34 {
                    return Err(TagReadError::Malformed);
                }
                let mut packed = [0; 8];
                packed.copy_from_slice(&body[10..18]);
                let packed = u64::from_be_bytes(packed);
                let rate = packed >> 44;
                let samples = packed & ((1_u64 << 36) - 1);
                if rate == 0 {
                    return Err(TagReadError::Malformed);
                }
                // FLAC's 36-bit sample count cannot overflow this multiplication.
                hints.duration_ms = (samples != 0).then(|| samples * 1000 / rate);
            }
            4 => {
                if seen_comments {
                    return Err(TagReadError::Malformed);
                }
                comments(body, &mut hints)?;
                seen_comments = true;
            }
            _ => {}
        }
        if last {
            return Ok(hints);
        }
    }
    Err(TagReadError::Malformed)
}
