//! Curated mbdump TSV validation for the mb-import container job.
//!
//! The importer chunks each dump file and applies one chunk per database
//! transaction. This crate decides, per physical line, whether the line is
//! safe to feed to Postgres `COPY (FORMAT text)` plus the typed upsert, and
//! if not, why it belongs in the dead-letter table. It never touches the
//! database: `import.sh` drives it as the `mb-validate` binary.
//!
//! Curated column contracts (see `containers/mb-import/import.sh`):
//! - artist:    mbid, name, sort_name, type, country, updated_at
//! - release:   mbid, artist_mbid, title, release_type, year, caa_url, updated_at
//! - recording: mbid, release_mbid, title, track_no, duration_ms, updated_at
//! - track:     release_mbid, recording_mbid, medium_no, track_no, updated_at
//!
//! Normalization: empty nullable fields are rewritten to the `COPY` null
//! marker `\N`; typed columns are validated on their COPY-decoded value
//! and emitted canonical and COPY-safe (lowercase UUIDs, plain integers,
//! re-escaped countries, decoded timestamps); and the 1-based dump line
//! number is appended as the final column (the loader COPYs it into
//! `line_no`). Text columns pass through byte-equal: Postgres decodes COPY
//! text escapes on import, so a `SELECT` after import returns the decoded
//! meaning of the dump's encoded bytes. Poison rows always keep the exact
//! original dump bytes.

/// Which curated dump file a line comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    Artist,
    Release,
    Recording,
    Track,
}

impl Table {
    /// Parse the `--table` CLI value.
    pub fn parse(raw: &str) -> Option<Table> {
        match raw {
            "artist" => Some(Table::Artist),
            "release" => Some(Table::Release),
            "recording" => Some(Table::Recording),
            "track" => Some(Table::Track),
            _ => None,
        }
    }

    /// Expected field count for one TSV line.
    pub fn fields(self) -> usize {
        match self {
            Table::Artist => 6,
            Table::Release => 7,
            Table::Recording => 6,
            Table::Track => 5,
        }
    }

    /// Table name as stored in the cursor and dead-letter tables.
    pub fn as_str(self) -> &'static str {
        match self {
            Table::Artist => "artist",
            Table::Release => "release",
            Table::Recording => "recording",
            Table::Track => "track",
        }
    }
}

/// Machine-readable poison reasons. Lowercase snake_case, no tabs: they are
/// copied verbatim into the dead-letter table.
pub mod reason {
    pub const COLUMN_COUNT: &str = "column_count";
    pub const EMPTY_LINE: &str = "empty_line";
    pub const BAD_UUID: &str = "bad_uuid";
    pub const BAD_INT: &str = "bad_int";
    pub const BAD_TIMESTAMP: &str = "bad_timestamp";
    pub const BAD_ESCAPE: &str = "bad_escape";
    pub const NULL_IN_NOT_NULL: &str = "null_in_not_null";
    pub const BAD_COUNTRY: &str = "bad_country";
    pub const NEGATIVE_DURATION: &str = "negative_duration";
    pub const NEGATIVE_POSITION: &str = "negative_position";
    pub const INVALID_ENCODING: &str = "invalid_encoding";
}

/// Validate one physical dump line.
///
/// `line_no` is the 1-based number in the whole dump file (chunk offset
/// added by the caller). On success returns the normalized line to feed to
/// `COPY` — curated fields plus the appended `line_no` — without trailing
/// newline. On failure returns `(reason, line_no)` for the dead-letter
/// table.
pub fn validate_line(table: Table, line_no: u64, raw: &str) -> Result<String, (&'static str, u64)> {
    let err = |why: &'static str| Err((why, line_no));
    if raw.is_empty() {
        return err(reason::EMPTY_LINE);
    }
    let fields: Vec<&str> = raw.split('\t').collect();
    if fields.len() != table.fields() {
        return err(reason::COLUMN_COUNT);
    }
    let mut out: Vec<String> = Vec::with_capacity(fields.len());
    // (field index, kind) per table; kinds below are the only ones COPY needs.
    let spec: &[(FieldKind, bool)] = match table {
        Table::Artist => &[
            (FieldKind::Uuid, true),
            (FieldKind::Text, true),
            (FieldKind::Text, true),
            (FieldKind::Text, false),
            (FieldKind::Country, false),
            (FieldKind::Timestamp, true),
        ],
        Table::Release => &[
            (FieldKind::Uuid, true),
            (FieldKind::Uuid, false),
            (FieldKind::Text, true),
            (FieldKind::Text, false),
            (FieldKind::Smallint, false),
            (FieldKind::Text, false),
            (FieldKind::Timestamp, true),
        ],
        Table::Recording => &[
            (FieldKind::Uuid, true),
            (FieldKind::Uuid, false),
            (FieldKind::Text, true),
            (FieldKind::Smallint, false),
            (FieldKind::Duration, false),
            (FieldKind::Timestamp, true),
        ],
        Table::Track => &[
            (FieldKind::Uuid, true),
            (FieldKind::Uuid, true),
            (FieldKind::Position, true),
            (FieldKind::Position, true),
            (FieldKind::Timestamp, true),
        ],
    };
    for (value, (kind, not_null)) in fields.iter().zip(spec.iter()) {
        match kind.check(value, *not_null) {
            Ok(normalized) => out.push(normalized),
            Err(why) => return err(why),
        }
    }
    out.push(line_no.to_string());
    Ok(out.join("\t"))
}

/// Escape a poison raw line so `reason \t line_no \t raw` round-trips
/// through `COPY (FORMAT text)`: backslash first, then tab/CR/LF and NUL.
pub fn escape_poison_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\\\x00"),
            _ => out.push(c),
        }
    }
    out
}

/// Chunk ranges still pending, as 1-based inclusive `(start, end)` line
/// spans over the whole dump file. `completed` is the cursor value
/// (lines already committed); `total` the current file line count.
pub fn plan_chunks(completed: u64, total: u64, size: u64) -> Vec<(u64, u64)> {
    if size == 0 || completed >= total {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = completed + 1;
    while start <= total {
        let end = start.saturating_add(size - 1).min(total);
        chunks.push((start, end));
        if end == total {
            break;
        }
        start = end + 1;
    }
    chunks
}

/// Column kinds the importer validates before COPY. Every `Ok` value is
/// already COPY-safe (empty nullable values are rewritten to `\N`); every
/// `Err` is a `reason::` poison code for the dead-letter table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Uuid,
    Smallint,
    Timestamp,
    Country,
    Duration,
    Position,
    Text,
}

impl FieldKind {
    fn check(self, value: &str, not_null: bool) -> Result<String, &'static str> {
        // `COPY (FORMAT text)` reads a bare `\N` as NULL. A dump line that
        // holds it verbatim in a NOT NULL text column would otherwise load
        // as NULL and violate the constraint inside the transaction: catch
        // it here so the row goes to the dead-letter table instead, while
        // `\N` in a nullable column normalizes to the same NULL marker.
        // This gate sees the RAW token on purpose: an ENCODED backslash-N
        // (e.g. `\134N`, which decodes to the two characters `\` + `N`) is
        // data, never NULL, and keeps flowing to the checks below.
        if value == "\\N" {
            if not_null {
                return Err(reason::NULL_IN_NOT_NULL);
            }
            return Ok("\\N".to_string());
        }
        // One bounded COPY text decoder feeds both jobs: it rejects
        // malformed/unsupported escapes (plus NUL and non-UTF-8 decodings)
        // exactly as before, and its decoded value is what the typed
        // checks below validate, so `\x55S` and `US` take the same path.
        // Only `\\`, `\b`, `\f`, `\n`, `\r`, `\t`, `\v`, octal `\` + 1-3
        // octal digits, and hex `\x` + 1-2 hex digits may follow a
        // backslash, else the row would load mangled or was never a
        // canonical dump escape. (Run before numeric/uuid/timestamp checks
        // so a bad escape reports as an escape, not as a bad value.)
        let decoded = decode_copy_field(value)?;
        if decoded.is_empty() {
            if not_null {
                // `updated_at` defaults to now() on NOT NULL violation inside
                // COPY, which would silently stamp rows: poison instead so
                // the dump stays explicit. Text NOT NULL columns take "".
                return match self {
                    FieldKind::Timestamp => Err(reason::BAD_TIMESTAMP),
                    FieldKind::Uuid => Err(reason::BAD_UUID),
                    // An empty NOT NULL numeric would COPY as "" and abort
                    // the chunk transaction; poison it like a bad value.
                    FieldKind::Smallint | FieldKind::Duration | FieldKind::Position => {
                        Err(reason::BAD_INT)
                    }
                    _ => Ok(String::new()),
                };
            }
            return Ok("\\N".to_string());
        }
        // Typed columns validate the DECODED value and emit it canonical
        // and COPY-safe, so every spelling of one value (literal, octal,
        // hex, padded, cased) loads the same key Postgres would store.
        // Semantic failures keep the literal reason taxonomy; only text
        // keeps the original token.
        match self {
            FieldKind::Uuid => {
                if is_uuid(&decoded) {
                    Ok(decoded.to_ascii_lowercase())
                } else {
                    Err(reason::BAD_UUID)
                }
            }
            FieldKind::Smallint => match decoded.parse::<i16>() {
                Ok(n) => Ok(n.to_string()),
                Err(_) => Err(reason::BAD_INT),
            },
            FieldKind::Timestamp => {
                // A validated timestamp holds no backslash, tab, CR or LF,
                // so the decoded value is already COPY-safe as emitted.
                if is_timestamp(&decoded) {
                    Ok(decoded)
                } else {
                    Err(reason::BAD_TIMESTAMP)
                }
            }
            FieldKind::Country => {
                // Character count on the DECODED value: `\x55S` is the two
                // characters `US`, while `\t` is one character and poison.
                // Re-escape so the decoded meaning round-trips through COPY
                // instead of becoming a separator or NULL: a decoded tab
                // emits as `\t`, a decoded backslash-N as `\\N`.
                if decoded.chars().count() == 2 {
                    Ok(escape_copy_text(&decoded))
                } else {
                    Err(reason::BAD_COUNTRY)
                }
            }
            FieldKind::Duration => match decoded.parse::<i32>() {
                Ok(n) if n >= 0 => Ok(n.to_string()),
                Ok(_) => Err(reason::NEGATIVE_DURATION),
                Err(_) => Err(reason::BAD_INT),
            },
            // Medium/track positions: SMALLINT in the schema, and a
            // negative position would trip the mb_track CHECK inside the
            // chunk transaction, so it is poison here instead.
            FieldKind::Position => match decoded.parse::<i16>() {
                Ok(n) if n >= 0 => Ok(n.to_string()),
                Ok(_) => Err(reason::NEGATIVE_POSITION),
                Err(_) => Err(reason::BAD_INT),
            },
            // Text passes through byte-equal: Postgres decodes COPY text
            // escapes on import, so the stored value is the decoded meaning
            // of the dump's encoded bytes. The decoder above already refused
            // anything COPY would load mangled.
            FieldKind::Text => Ok(value.to_string()),
        }
    }
}

/// Decode one `COPY (FORMAT text)` field, mirroring exactly what `COPY
/// FROM` documents: `\b \f \n \r \t \v \\`, backslash plus 1-3 octal
/// digits, and `\x` plus 1-2 hex digits. Anything else after a backslash
/// (including a dangling backslash, `\o`, `\u`, a bare `\x`, or `\x` with
/// no hex digits) is poison: COPY would either load a different value than
/// the dump holds (`\q` loads as `q`, dropping the backslash) or the row
/// was never a canonical dump escape, so it goes to the dead-letter table
/// for operator review instead of being mangled into the mirror.
/// Deliberately stricter than COPY, which accepts any other backslashed
/// character as itself: the importer only passes through escapes whose
/// stored value is the documented control character or byte.
///
/// The single decoder feeds both byte safety and typed validation: text
/// columns validate through it and keep the original token, while typed
/// columns validate the decoded value it returns.
fn decode_copy_field(value: &str) -> Result<String, &'static str> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            return Err(reason::BAD_ESCAPE);
        }
        if bytes[i] != b'\\' {
            decoded.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            return Err(reason::BAD_ESCAPE);
        }
        match bytes[i] {
            b'b' => decoded.push(0x08),
            b'f' => decoded.push(0x0c),
            b'n' => decoded.push(b'\n'),
            b'r' => decoded.push(b'\r'),
            b't' => decoded.push(b'\t'),
            b'v' => decoded.push(0x0b),
            b'\\' => decoded.push(b'\\'),
            b'0'..=b'7' => {
                // Octal byte: up to 3 octal digits total (COPY is greedy).
                let mut byte = (bytes[i] - b'0') as u32;
                i += 1;
                for _ in 0..2 {
                    if i < bytes.len() && bytes[i].is_ascii_digit() && bytes[i] < b'8' {
                        byte = byte * 8 + (bytes[i] - b'0') as u32;
                        i += 1;
                    } else {
                        break;
                    }
                }
                if byte > 0o377 {
                    return Err(reason::BAD_ESCAPE);
                }
                decoded.push(byte as u8);
                continue;
            }
            b'x' => {
                // Hex byte: `\x` requires 1-2 hex digits.
                i += 1;
                let start = i;
                let mut byte = 0;
                while i < bytes.len() && i - start < 2 && bytes[i].is_ascii_hexdigit() {
                    byte = byte * 16 + (bytes[i] as char).to_digit(16).unwrap() as u8;
                    i += 1;
                }
                if i == start {
                    return Err(reason::BAD_ESCAPE);
                }
                decoded.push(byte);
                continue;
            }
            _ => return Err(reason::BAD_ESCAPE),
        }
        i += 1;
    }
    if decoded.contains(&0) {
        return Err(reason::BAD_ESCAPE);
    }
    String::from_utf8(decoded).map_err(|_| reason::BAD_ESCAPE)
}

/// Re-escape a decoded typed value so it round-trips through
/// `COPY (FORMAT text)` as data: backslash, tab, LF, CR and the other C0
/// controls COPY names get their short spellings, everything else
/// (including multi-byte UTF-8) passes through raw. NUL cannot appear:
/// the decoder already refused it.
fn escape_copy_text(decoded: &str) -> String {
    let mut out = String::with_capacity(decoded.len());
    for c in decoded.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            _ => out.push(c),
        }
    }
    out
}

/// Lowercase or uppercase hex UUID `8-4-4-4-12`, no braces, no `urn:`.
fn is_uuid(value: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    parts
        .iter()
        .zip(GROUPS)
        .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Timestamps COPY accepts for `timestamptz`: date, or date + time, with an
/// optional zone (`Z` or `±hh[:mm]`). Rejects impossible months/days and
/// leap-day on non-leap years so poison never reaches the typed upsert.
fn is_timestamp(value: &str) -> bool {
    // Split optional zone: trailing Z, or ±hh[:mm].
    let bare = match split_zone(value) {
        Some(bare) => bare,
        None => return false,
    };
    if bare.is_empty() {
        return false;
    }
    let split = bare.find('T').or_else(|| bare.find(' '));
    let (date, time) = match split {
        Some(i) => (&bare[..i], Some(&bare[i + 1..])),
        None => (bare.as_str(), None),
    };
    if !is_date(date) {
        return false;
    }
    match time {
        None => true,
        Some(t) => is_time(t),
    }
}

/// Strip one trailing `Z` or `±hh[:mm]` zone; `None` means malformed zone.
fn split_zone(value: &str) -> Option<String> {
    if let Some(bare) = value.strip_suffix('Z').or_else(|| value.strip_suffix('z')) {
        return Some(bare.to_string());
    }
    // Find a trailing ±hh or ±hh:mm, scanning from the end.
    let bytes = value.as_bytes();
    let mut i = bytes.len();
    while i > 0 && (bytes[i - 1].is_ascii_digit() || bytes[i - 1] == b':') {
        i -= 1;
    }
    // A zone only follows a time: without a `T`/space separator the trailing
    // `-DD` is the date's own day (e.g. `2024-05-01`), never a zone offset.
    if i > 0 && (bytes[i - 1] == b'+' || bytes[i - 1] == b'-') && i > 1 {
        let bare = &value[..i - 1];
        if !bare.bytes().any(|b| b == b'T' || b == b't' || b == b' ') {
            return Some(value.to_string());
        }
        let zone = &value[i - 1..];
        let digits = match zone.len() {
            3 if zone[1..].bytes().all(|b| b.is_ascii_digit()) => zone[1..].to_string(),
            5 if zone[1..].bytes().all(|b| b.is_ascii_digit()) => zone[1..].to_string(),
            6 if zone.as_bytes()[3] == b':'
                && zone[1..3].bytes().all(|b| b.is_ascii_digit())
                && zone[4..6].bytes().all(|b| b.is_ascii_digit()) =>
            {
                format!("{}{}", &zone[1..3], &zone[4..6])
            }
            _ => return None,
        };
        let hh = digits[..2].parse::<u32>().ok()?;
        let mm = if digits.len() == 4 {
            digits[2..].parse::<u32>().ok()?
        } else {
            0
        };
        // PostgreSQL rejects timezone displacements above 15 hours.
        if hh <= 15 && mm <= 59 {
            return Some(bare.to_string());
        }
        return None;
    }
    Some(value.to_string())
}

fn is_date(date: &str) -> bool {
    let p: Vec<&str> = date.split('-').collect();
    if p.len() != 3 || p[0].len() != 4 || p[1].len() != 2 || p[2].len() != 2 {
        return false;
    }
    let (Ok(y), Ok(m), Ok(d)) = (
        p[0].parse::<i32>(),
        p[1].parse::<u32>(),
        p[2].parse::<u32>(),
    ) else {
        return false;
    };
    if y < 1 || !(1..=12).contains(&m) || d < 1 {
        return false;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let max = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    d <= max
}

fn is_time(time: &str) -> bool {
    // Canonical dump shape requires seconds (`HH:MM:SS[.ffffff]`); a bare
    // `HH:MM` is valid Postgres but not a shape the dumps emit, so it goes
    // to the dead-letter table for operator review instead.
    let p: Vec<&str> = time.split(':').collect();
    if p.len() != 3 {
        return false;
    }
    if p[0].len() != 2 || p[1].len() != 2 {
        return false;
    }
    let (Ok(h), Ok(mi)) = (p[0].parse::<u32>(), p[1].parse::<u32>()) else {
        return false;
    };
    if h > 23 || mi > 59 {
        return false;
    }
    {
        // Seconds with optional fractional part.
        let (sec, frac) = match p[2].split_once('.') {
            Some((s, f)) => (s, Some(f)),
            None => (p[2], None),
        };
        if sec.len() != 2 {
            return false;
        }
        let Ok(sec) = sec.parse::<u32>() else {
            return false;
        };
        if sec > 60 {
            return false;
        }
        if let Some(f) = frac {
            if f.is_empty() || f.len() > 6 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2024-05-01T12:00:00Z";
    const MBID: &str = "652b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d1";
    const MBID2: &str = "752b9a5c-ee58-41f5-bb9e-b0d0a5a7c9d2";

    fn artist_line(mbid: &str, ts: &str) -> String {
        format!("{}\tThe Band\tBand, The\tGroup\tUS\t{}", mbid, ts)
    }

    #[test]
    fn valid_artist_passes_through_byte_equal() {
        let raw = artist_line(MBID, TS);
        // Curated fields pass through untouched; the dump line number is
        // appended for the loader's line_no column.
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Ok(format!("{raw}\t1"))
        );
    }

    #[test]
    fn valid_release_and_recording_shapes() {
        let rel = format!("{}\t{}\tAlbum Title\tAlbum\t2024\t\\N\t{}", MBID2, MBID, TS);
        assert_eq!(
            validate_line(Table::Release, 7, &rel),
            Ok(format!("{rel}\t7"))
        );
        let rec = format!("{}\t{}\tTrack One\t3\t180000\t{}", MBID, MBID2, TS);
        assert_eq!(
            validate_line(Table::Recording, 9, &rec),
            Ok(format!("{rec}\t9"))
        );
    }

    #[test]
    fn wrong_column_count_is_poison_with_line_no() {
        let raw = format!("{}\tOnly Two", MBID);
        assert_eq!(
            validate_line(Table::Artist, 42, &raw),
            Err((reason::COLUMN_COUNT, 42))
        );
    }

    #[test]
    fn poison_field_encodes_nul_as_copy_safe_text() {
        assert_eq!(escape_poison_field("bad\0row"), "bad\\\\x00row");
    }

    #[test]
    fn empty_line_is_poison() {
        assert_eq!(
            validate_line(Table::Artist, 3, ""),
            Err((reason::EMPTY_LINE, 3))
        );
    }

    #[test]
    fn bad_uuid_is_poison() {
        let raw = artist_line("not-a-uuid", TS);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Err((reason::BAD_UUID, 1))
        );
        let raw = artist_line("652b9a5c-ee58-41f5-bb9e-b0d0a5a7c9ZZ", TS);
        assert!(validate_line(Table::Artist, 1, &raw).is_err());
    }

    #[test]
    fn empty_nullable_fk_becomes_copy_null() {
        let raw = format!("{}\t\tAlbum Title\tAlbum\t\t\t{}", MBID2, TS);
        let want = format!("{}\t\\N\tAlbum Title\tAlbum\t\\N\t\\N\t{}\t1", MBID2, TS);
        assert_eq!(validate_line(Table::Release, 1, &raw), Ok(want));
    }

    #[test]
    fn bad_year_is_poison_year_bounds_accepted() {
        let raw = format!("{}\t\\N\tT\tAlbum\t40000\t\\N\t{}", MBID2, TS);
        assert_eq!(
            validate_line(Table::Release, 1, &raw),
            Err((reason::BAD_INT, 1))
        );
        // i16 bounds are the schema bounds (SMALLINT).
        let raw = format!("{}\t\\N\tT\tAlbum\t32767\t\\N\t{}", MBID2, TS);
        assert!(validate_line(Table::Release, 1, &raw).is_ok());
        let raw = format!("{}\t\\N\tT\tAlbum\t-32768\t\\N\t{}", MBID2, TS);
        assert!(validate_line(Table::Release, 1, &raw).is_ok());
    }

    #[test]
    fn valid_track_membership_and_bad_positions() {
        let raw = format!("{}\t{}\t2\t0\t{}", MBID2, MBID, TS);
        assert_eq!(
            validate_line(Table::Track, 4, &raw),
            Ok(format!("{raw}\t4"))
        );
        assert_eq!(Table::parse("track"), Some(Table::Track));
        assert_eq!(Table::Track.as_str(), "track");
        let raw = format!("{}\t{}\t-1\t1\t{}", MBID2, MBID, TS);
        assert_eq!(
            validate_line(Table::Track, 1, &raw),
            Err((reason::NEGATIVE_POSITION, 1))
        );
        // An empty NOT NULL position must poison, not COPY as "".
        let raw = format!("{}\t{}\t\t1\t{}", MBID2, MBID, TS);
        assert_eq!(
            validate_line(Table::Track, 1, &raw),
            Err((reason::BAD_INT, 1))
        );
        let raw = format!("{}\t\\N\t1\t1\t{}", MBID2, TS);
        assert_eq!(
            validate_line(Table::Track, 1, &raw),
            Err((reason::NULL_IN_NOT_NULL, 1))
        );
    }

    #[test]
    fn bad_track_and_negative_duration_are_poison() {
        let raw = format!("{}\t{}\tT\tx\t180000\t{}", MBID, MBID2, TS);
        assert_eq!(
            validate_line(Table::Recording, 1, &raw),
            Err((reason::BAD_INT, 1))
        );
        let raw = format!("{}\t{}\tT\t1\t-5\t{}", MBID, MBID2, TS);
        assert_eq!(
            validate_line(Table::Recording, 1, &raw),
            Err((reason::NEGATIVE_DURATION, 1))
        );
    }

    #[test]
    fn bad_timestamp_is_poison_sane_shapes_pass() {
        let raw = artist_line(MBID, "not-a-date");
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Err((reason::BAD_TIMESTAMP, 1))
        );
        for ts in [
            "2024-05-01",
            "2024-05-01 12:00:00+00",
            "2024-05-01T12:00:00+0530",
            "2024-05-01T12:00:00+05:30",
            "2024-05-01T12:00:00.123456Z",
            "2024-02-29T00:00:00Z", // 2024 is a leap year
        ] {
            assert!(
                validate_line(Table::Artist, 1, &artist_line(MBID, ts)).is_ok(),
                "{ts}"
            );
        }
        for ts in [
            "0000-01-01T00:00:00Z",
            "2023-02-29",
            "2024-13-01",
            "2024-05-01T25:00:00Z",
            "2024-05-01 12:00",
            "2024-05-01T12:00:ABZ",
            "2024-05-01T12:00:00+0560",
            "2024-05-01T12:00:00+05:60",
            "2024-05-01T12:00:00+05::30",
            "2024-05-01T12:00:00+16:00",
        ] {
            assert!(
                validate_line(Table::Artist, 1, &artist_line(MBID, ts)).is_err(),
                "{ts}"
            );
        }
    }

    #[test]
    fn country_must_be_empty_or_two_chars() {
        let raw = artist_line(MBID, TS).replace("\tUS\t", "\tUSA\t");
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Err((reason::BAD_COUNTRY, 1))
        );
        let raw = artist_line(MBID, TS).replace("\tUS\t", "\t\t");
        assert!(validate_line(Table::Artist, 1, &raw).is_ok());
    }

    #[test]
    fn bad_backslash_escape_is_poison_valid_ones_pass() {
        let mut raw = artist_line(MBID, TS);
        raw = raw.replacen("The Band", "Bad \\q Name", 1);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Err((reason::BAD_ESCAPE, 1))
        );
        let mut raw = artist_line(MBID, TS);
        raw = raw.replacen("The Band", "Ends with backslash \\", 1);
        assert!(validate_line(Table::Artist, 1, &raw).is_err());
        // Valid COPY escapes round-trip: literal backslash, tab, newline.
        let raw = artist_line(MBID, TS).replacen("The Band", "A \\\\ B \\t C \\n D", 1);
        assert!(validate_line(Table::Artist, 1, &raw).is_ok());
    }

    #[test]
    fn copy_text_octal_hex_match_postgres_docs() {
        // Octal `\` + 1-3 octal digits and hex `\x` + 1-2 hex digits are
        // real COPY text escapes (sql-copy.html) and must validate.
        for esc in ["\\7", "\\40", "\\101", "\\x4", "\\x41", "\\x4a"] {
            let raw = artist_line(MBID, TS).replacen("The Band", &format!("A {esc} B"), 1);
            assert!(
                validate_line(Table::Artist, 1, &raw).is_ok(),
                "{esc} should validate"
            );
        }
        // `\o` and `\u` are NOT COPY sequences (COPY would load them as the
        // bare letter, mangling the dump), a bare `\x` or `\x` with no hex
        // digits has no byte value, and `\8`/`\9` are not octal digits: all
        // poison for operator review instead of silent mangling.
        for esc in [
            "\\o", "\\o141", "\\u", "\\u0041", "\\x", "\\xZZ", "\\8", "\\9", "\\400", "\\777",
            "\\0", "\\00", "\\000", "\\x0", "\\x00", "\\xff", "\\200", "\\377", "\\xc3",
        ] {
            let raw = artist_line(MBID, TS).replacen("The Band", &format!("A {esc} B"), 1);
            assert_eq!(
                validate_line(Table::Artist, 1, &raw),
                Err((reason::BAD_ESCAPE, 1)),
                "{esc} should be poison"
            );
        }
        let raw = artist_line(MBID, TS).replacen("The Band", "A embedded\r carriage return B", 1);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Err((reason::BAD_ESCAPE, 1)),
            "an embedded carriage return should be poison"
        );
    }

    #[test]
    fn copy_null_marker_rejected_in_not_null_text() {
        let raw = format!("{}\t\\N\t\t\tUS\t{}", MBID, TS);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Err((reason::NULL_IN_NOT_NULL, 1))
        );
    }

    #[test]
    fn poison_escape_round_trips_copy() {
        // Tab, CR, LF and backslash in raw lines must survive the poison
        // file's COPY load and come back byte-equal.
        let raw = "x\\\ty\r\nz";
        let esc = escape_poison_field(raw);
        assert!(!esc.contains('\t') && !esc.contains('\n') && !esc.contains('\r'));
        // Unescape the way COPY (FORMAT text) would.
        let mut back = String::new();
        let mut it = esc.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next().unwrap_or('?') {
                    '\\' => back.push('\\'),
                    't' => back.push('\t'),
                    'n' => back.push('\n'),
                    'r' => back.push('\r'),
                    other => {
                        back.push('\\');
                        back.push(other);
                    }
                }
            } else {
                back.push(c);
            }
        }
        assert_eq!(back, raw);
    }

    #[test]
    fn plan_chunks_covers_tail_and_skips_done() {
        assert_eq!(
            plan_chunks(0, 5000, 2000),
            vec![(1, 2000), (2001, 4000), (4001, 5000)]
        );
        assert_eq!(
            plan_chunks(2000, 5000, 2000),
            vec![(2001, 4000), (4001, 5000)]
        );
        assert_eq!(plan_chunks(5000, 5000, 2000), vec![]);
        assert_eq!(plan_chunks(6000, 5000, 2000), vec![]);
        assert_eq!(plan_chunks(0, 0, 2000), vec![]);
        assert_eq!(plan_chunks(0, 10, 2000), vec![(1, 10)]);
    }

    #[test]
    fn copy_field_decoder_vectors() {
        // The one decoder both byte safety and typed checks share.
        for (token, want) in [
            ("plain", "plain"),
            (r"\141", "a"),
            (r"\x61", "a"),
            (r"\101", "A"),
            (r"\x41", "A"),
            (r"\7", "\x07"),
            (r"\40", " "),
            (r"\x4", "\x04"),
            (r"\t", "\t"),
            (r"\\", "\\"),
            (r"\125S", "US"),
            (r"\303\251", "é"),
            (r"\xc3\xa9", "é"),
        ] {
            assert_eq!(decode_copy_field(token), Ok(want.to_string()), "{token:?}");
        }
        for token in [
            r"\q",
            r"\o",
            r"\x",
            r"\xZZ",
            r"\8",
            r"\9",
            r"\400",
            r"\777",
            r"\0",
            r"\000",
            r"\x0",
            r"\x00",
            r"\377",
            r"\xc3\x28",
            "trailing\\",
            "raw\rcr",
            "raw\0nul",
        ] {
            assert_eq!(
                decode_copy_field(token),
                Err(reason::BAD_ESCAPE),
                "{token:?}"
            );
        }
    }

    #[test]
    fn decoded_typed_values_emit_canonical_copy_safe_forms() {
        // UUIDs canonicalize to lowercase whatever the spelling.
        let raw = artist_line(&MBID.to_ascii_uppercase(), TS);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Ok(format!("{}\t1", artist_line(MBID, TS)))
        );
        let raw = artist_line(&format!(r"\x36{}", &MBID[1..]), TS);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Ok(format!("{}\t1", artist_line(MBID, TS)))
        );
        // Integers canonicalize: padding, `+` and escapes all collapse.
        for (token, want) in [("+001", "1"), ("0002", "2"), (r"\061", "1"), (r"\x32", "2")] {
            let raw = format!("{MBID2}\t{MBID}\t{token}\t{token}\t{TS}");
            let want = format!("{MBID2}\t{MBID}\t{want}\t{want}\t{TS}\t1");
            assert_eq!(validate_line(Table::Track, 1, &raw), Ok(want), "{token:?}");
        }
        // Countries validate on decoded characters and re-escape COPY-safe.
        for (token, want) in [
            (r"\x55S", "US"),
            (r"\303\251A", "éA"),
            (r"\x09A", r"\tA"),
            (r"\134N", r"\\N"),
        ] {
            let raw = artist_line(MBID, TS).replace("\tUS\t", &format!("\t{token}\t"));
            let want = artist_line(MBID, TS).replace("\tUS\t", &format!("\t{want}\t"));
            assert_eq!(
                validate_line(Table::Artist, 1, &raw),
                Ok(format!("{want}\t1")),
                "{token:?}"
            );
        }
        // Timestamps validate decoded digits and separators.
        let raw = artist_line(MBID, r"\062024-05-01\04012:00:00Z");
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Ok(format!("{}\t1", artist_line(MBID, "2024-05-01 12:00:00Z")))
        );
        // Text still passes through byte-equal, escapes and all.
        let raw = artist_line(MBID, TS).replacen("The Band", r"A \x55 B \\ C", 1);
        assert_eq!(
            validate_line(Table::Artist, 1, &raw),
            Ok(format!("{raw}\t1"))
        );
    }
}
