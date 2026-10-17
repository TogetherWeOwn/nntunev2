//! Fixture-schema validation for the synthetic legacy-playlist corpus
//! (`tests/fixtures/legacy-playlists-v1.json`, contract in the sibling
//! `legacy-playlists-v1.contract.md`).
//!
//! This is test support only: it validates inert fixture data for a later
//! playlist cutover. It is not wired into any handler, read path, SQL, or
//! schema, and loading a corpus here never touches a database. Validation
//! is strict — unknown keys are rejected at every level so a stray field
//! can never smuggle data past a reader — and each rejection is classified
//! into exactly one [`FixtureErrorKind`] per the contract.
//!
//! Run it with:
//!
//! ```text
//! cargo test --locked --manifest-path workers/api/Cargo.toml \
//!     --test legacy_playlist_fixtures_acceptance
//! ```
//!
//! Provenance and expected shape: every identifier, name, and legacy ref
//! in the corpus is synthetic and invented for tests — nothing comes from
//! staging or production data. Each case pins the reads a future cutover
//! must produce (owner reads: resolved entries in original order/positions
//! plus the owner-only unresolved remainder; non-owner reads: metadata and
//! the resolved-only count for public playlists, `not_found` for private
//! ones). Those are future cutover inputs, not delivered reads: no current
//! endpoint returns them, and this validator changes no runtime behavior.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

const SCHEMA_NAME: &str = "legacy-playlist-fixtures/v1";
const PUBLIC_HANDLE_LEN: usize = 22;
const TRACK_SHAPED_KEYS: [&str; 4] = ["track_public_id", "track_id", "locality", "title"];

/// Why a fixture corpus or case was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureErrorKind {
    /// Structure, types, or schema constants do not match the contract.
    MalformedSchema,
    /// Internal BIGSERIAL ids and 22-character public handles are confused,
    /// malformed, colliding, or resolved inconsistently.
    IdentityConfusion,
    /// Positions, stored order, or expected-entry membership/order differ.
    BadOrdering,
    /// The non-owner read leaks more than metadata, hides a public
    /// playlist, exposes a private one, or counts unresolved refs.
    BadVisibility,
    /// An unresolved legacy ref was invented into a track.
    GuessedResolution,
}

/// A single fixture validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureValidationError {
    pub kind: FixtureErrorKind,
    pub message: String,
}

impl fmt::Display for FixtureValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for FixtureValidationError {}

type Result<T> = std::result::Result<T, FixtureValidationError>;

fn err(kind: FixtureErrorKind, message: impl Into<String>) -> FixtureValidationError {
    FixtureValidationError {
        kind,
        message: message.into(),
    }
}

fn malformed(message: impl Into<String>) -> FixtureValidationError {
    err(FixtureErrorKind::MalformedSchema, message)
}

fn as_object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| malformed(format!("{what} must be an object")))
}

/// Reject unknown keys and require every allowed key, as `MalformedSchema`.
fn require_exact_keys(obj: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<()> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(malformed(format!("{what}: unknown key `{key}`")));
        }
    }
    for key in allowed {
        if !obj.contains_key(*key) {
            return Err(malformed(format!("{what}: missing key `{key}`")));
        }
    }
    Ok(())
}

fn is_public_handle(value: &str) -> bool {
    value.chars().count() == PUBLIC_HANDLE_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One validated source entry, projected for the expected-read checks.
struct SourceEntry {
    position: u64,
    legacy_ref: String,
    /// `Some((track_public_id, locality))` when resolved.
    resolved: Option<(String, String)>,
}

/// Parse and validate a whole corpus document, including the corpus-wide
/// uniqueness and identity-space rules.
pub fn validate_corpus(json: &str) -> Result<()> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| malformed(format!("unparsable JSON: {e}")))?;
    let obj = as_object(&root, "corpus")?;
    for key in obj.keys() {
        if !["$comment", "schema", "version", "cases"].contains(&key.as_str()) {
            return Err(malformed(format!("corpus: unknown key `{key}`")));
        }
    }
    if let Some(comment) = obj.get("$comment") {
        if !comment.is_string() {
            return Err(malformed("corpus `$comment` must be a string"));
        }
    }
    if obj.get("schema").and_then(Value::as_str) != Some(SCHEMA_NAME) {
        return Err(malformed(format!(
            "corpus `schema` must be `{SCHEMA_NAME}`"
        )));
    }
    let version_ok = obj
        .get("version")
        .map(|v| v.as_i64() == Some(1) && v.is_i64() || v.as_u64() == Some(1))
        .unwrap_or(false);
    if !version_ok {
        return Err(malformed("corpus `version` must be integer 1"));
    }
    let cases = obj
        .get("cases")
        .and_then(Value::as_array)
        .filter(|cases| !cases.is_empty())
        .ok_or_else(|| malformed("corpus `cases` must be a non-empty array"))?;

    let mut case_ids: Vec<&str> = Vec::new();
    for case in cases {
        if let Some(id) = case.get("id").and_then(Value::as_str) {
            if case_ids.contains(&id) {
                return Err(malformed(format!("duplicate case id `{id}`")));
            }
            case_ids.push(id);
        }
    }
    for case in cases {
        validate_case(case)?;
    }

    // Corpus-wide identity rules: internal ids and public handles are
    // unique, disjoint identity spaces across every case, and no track
    // handle collides with any of them.
    let mut internal_ids: Vec<i64> = Vec::new();
    let mut public_ids: Vec<&str> = Vec::new();
    for case in cases {
        let playlist = &case["playlist"];
        let internal = playlist["internal_id"].as_i64().unwrap_or_default();
        if internal_ids.contains(&internal) {
            return Err(err(
                FixtureErrorKind::IdentityConfusion,
                format!("duplicate internal_id {internal} across cases"),
            ));
        }
        internal_ids.push(internal);
        let public = playlist["public_id"].as_str().unwrap_or_default();
        if public_ids.contains(&public) {
            return Err(err(
                FixtureErrorKind::IdentityConfusion,
                format!("duplicate public_id `{public}` across cases"),
            ));
        }
        public_ids.push(public);
    }
    let internal_texts: Vec<String> = internal_ids.iter().map(i64::to_string).collect();
    for public in &public_ids {
        if internal_texts.iter().any(|t| t == public) {
            return Err(err(
                FixtureErrorKind::IdentityConfusion,
                format!("public_id `{public}` is an internal id rendered as text"),
            ));
        }
    }
    for case in cases {
        for entry in case["entries"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(track) = entry.get("track_public_id").and_then(Value::as_str) {
                if public_ids.contains(&track) || internal_texts.iter().any(|t| t == track) {
                    return Err(err(
                        FixtureErrorKind::IdentityConfusion,
                        format!("track handle `{track}` collides with a playlist identity"),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Validate one case object (an element of the corpus `cases` array).
pub fn validate_case(case: &Value) -> Result<()> {
    let case_id = case
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or("<unknown case>");
    validate_case_inner(case).map_err(|e| FixtureValidationError {
        kind: e.kind,
        message: format!("case {case_id}: {}", e.message),
    })
}

fn validate_case_inner(case: &Value) -> Result<()> {
    let obj = as_object(case, "case")?;
    require_exact_keys(
        obj,
        &["id", "description", "playlist", "entries", "expected"],
        "case",
    )?;
    match obj["id"].as_str() {
        Some(id) if !id.is_empty() => {}
        _ => return Err(malformed("case `id` must be a non-empty string")),
    }
    match obj["description"].as_str() {
        Some(description) if !description.is_empty() => {}
        _ => return Err(malformed("case `description` must be a non-empty string")),
    }

    // --- playlist ---------------------------------------------------------
    let playlist = as_object(&obj["playlist"], "playlist")?;
    require_exact_keys(
        playlist,
        &[
            "internal_id",
            "public_id",
            "owner_user_id",
            "name",
            "is_public",
            "items",
        ],
        "playlist",
    )?;
    let internal_id = match &playlist["internal_id"] {
        Value::Number(n) if n.as_i64().is_some_and(|v| v > 0) => n.as_i64().unwrap(),
        Value::Number(n) if n.as_u64().is_some_and(|v| v > 0 && v <= i64::MAX as u64) => {
            n.as_u64().unwrap() as i64
        }
        _ => {
            return Err(err(
                FixtureErrorKind::IdentityConfusion,
                "playlist `internal_id` must be an integer > 0",
            ))
        }
    };
    let internal_text = internal_id.to_string();
    let public_id = match playlist["public_id"].as_str() {
        Some(handle) if is_public_handle(handle) && handle != internal_text => handle,
        _ => {
            return Err(err(
                FixtureErrorKind::IdentityConfusion,
                "playlist `public_id` must be a 22-character [A-Za-z0-9_-] handle \
                 distinct from the internal id",
            ))
        }
    };
    match playlist["owner_user_id"].as_str() {
        Some(owner) if owner.starts_with("fixture-user-") => {}
        _ => {
            return Err(malformed(
                "playlist `owner_user_id` must start with `fixture-user-`",
            ))
        }
    }
    let name = match playlist["name"].as_str() {
        Some(name) if !name.is_empty() => name,
        _ => return Err(malformed("playlist `name` must be a non-empty string")),
    };
    let is_public = playlist["is_public"]
        .as_bool()
        .ok_or_else(|| malformed("playlist `is_public` must be a boolean"))?;
    let items_value = playlist["items"]
        .as_array()
        .ok_or_else(|| malformed("playlist `items` must be an array"))?;
    let mut items: Vec<&str> = Vec::with_capacity(items_value.len());
    for item in items_value {
        match item.as_str() {
            Some(r) if !r.is_empty() && r.starts_with("legacy-ref-") => items.push(r),
            _ => {
                return Err(malformed(
                    "playlist `items` must be non-empty `legacy-ref-` strings",
                ))
            }
        }
    }

    // --- entries ----------------------------------------------------------
    let entries_value = obj["entries"]
        .as_array()
        .ok_or_else(|| malformed("case `entries` must be an array"))?;
    if entries_value.len() != items.len() {
        return Err(err(
            FixtureErrorKind::BadOrdering,
            "case `entries` must have one entry per playlist item",
        ));
    }
    let mut source: Vec<SourceEntry> = Vec::with_capacity(entries_value.len());
    // Determinism: one resolution per legacy ref; one ref per track.
    let mut by_ref: BTreeMap<&str, Option<(&str, &str)>> = BTreeMap::new();
    let mut by_track: BTreeMap<&str, &str> = BTreeMap::new();
    for (index, entry) in entries_value.iter().enumerate() {
        let entry_obj = as_object(entry, "entry")?;
        let status = match entry_obj.get("status").and_then(Value::as_str) {
            Some("resolved") => true,
            Some("unresolved") => false,
            _ => return Err(malformed("entry `status` must be resolved|unresolved")),
        };
        let position = entry_obj.get("position").and_then(Value::as_u64);
        if position != Some(index as u64) {
            return Err(err(
                FixtureErrorKind::BadOrdering,
                format!("entry {index}: `position` must be {index} in stored order"),
            ));
        }
        let legacy_ref = entry_obj.get("legacy_ref").and_then(Value::as_str);
        if legacy_ref != Some(items[index]) {
            return Err(err(
                FixtureErrorKind::BadOrdering,
                format!("entry {index}: `legacy_ref` must equal items[{index}]"),
            ));
        }
        let legacy_ref = items[index];
        let resolved = if status {
            for key in entry_obj.keys() {
                if ![
                    "position",
                    "legacy_ref",
                    "status",
                    "track_public_id",
                    "locality",
                ]
                .contains(&key.as_str())
                {
                    return Err(malformed(format!("resolved entry: unknown key `{key}`")));
                }
            }
            let track = match entry_obj.get("track_public_id").and_then(Value::as_str) {
                Some(handle)
                    if is_public_handle(handle)
                        && handle != public_id
                        && handle != internal_text =>
                {
                    handle
                }
                _ => {
                    return Err(err(
                        FixtureErrorKind::IdentityConfusion,
                        format!(
                            "entry {index}: `track_public_id` must be a 22-character handle \
                             distinct from the playlist identities"
                        ),
                    ))
                }
            };
            let locality = match entry_obj.get("locality").and_then(Value::as_str) {
                Some(locality @ ("catalog" | "owner_local")) => locality,
                _ => {
                    return Err(malformed(format!(
                        "entry {index}: `locality` must be catalog|owner_local"
                    )))
                }
            };
            Some((track, locality))
        } else {
            if entry_obj
                .keys()
                .any(|k| !["position", "legacy_ref", "status"].contains(&k.as_str()))
            {
                return Err(err(
                    FixtureErrorKind::GuessedResolution,
                    format!("entry {index}: unresolved ref carries track-shaped data"),
                ));
            }
            None
        };
        match by_ref.get(legacy_ref) {
            Some(previous) if *previous != resolved => {
                return Err(err(
                    FixtureErrorKind::IdentityConfusion,
                    format!("ref `{legacy_ref}` resolves inconsistently within the case"),
                ));
            }
            Some(_) => {}
            None => {
                by_ref.insert(legacy_ref, resolved);
            }
        }
        if let Some((track, _)) = resolved {
            match by_track.get(track) {
                Some(previous) if *previous != legacy_ref => {
                    return Err(err(
                        FixtureErrorKind::IdentityConfusion,
                        format!("distinct refs resolve to the same track `{track}`"),
                    ));
                }
                Some(_) => {}
                None => {
                    by_track.insert(track, legacy_ref);
                }
            }
        }
        source.push(SourceEntry {
            position: index as u64,
            legacy_ref: legacy_ref.to_string(),
            resolved: resolved.map(|(t, l)| (t.to_string(), l.to_string())),
        });
    }
    let resolved_entries: Vec<&SourceEntry> =
        source.iter().filter(|e| e.resolved.is_some()).collect();
    let unresolved_entries: Vec<&SourceEntry> =
        source.iter().filter(|e| e.resolved.is_none()).collect();

    // --- expected ---------------------------------------------------------
    let expected = as_object(&obj["expected"], "expected")?;
    require_exact_keys(expected, &["owner_read", "non_owner_read"], "expected")?;

    let owner = as_object(&expected["owner_read"], "owner_read")?;
    require_exact_keys(
        owner,
        &[
            "id",
            "name",
            "is_public",
            "legacy_item_count",
            "entries",
            "unresolved_legacy_remainder",
        ],
        "owner_read",
    )?;
    if owner["id"].as_str() != Some(public_id) {
        return Err(err(
            FixtureErrorKind::IdentityConfusion,
            "owner_read `id` must equal the playlist `public_id`",
        ));
    }
    if owner["name"].as_str() != Some(name)
        || owner["is_public"].as_bool() != Some(is_public)
        || owner["legacy_item_count"].as_u64() != Some(items.len() as u64)
    {
        return Err(malformed(
            "owner_read name/is_public/legacy_item_count must equal the playlist fields",
        ));
    }
    let owner_entries = owner["entries"]
        .as_array()
        .ok_or_else(|| malformed("owner_read `entries` must be an array"))?;
    for entry in owner_entries {
        if let Some(r) = entry.get("legacy_ref").and_then(Value::as_str) {
            if unresolved_entries.iter().any(|e| e.legacy_ref == r) {
                return Err(err(
                    FixtureErrorKind::GuessedResolution,
                    format!("owner entries invent a track for unresolved ref `{r}`"),
                ));
            }
        }
        require_exact_keys(
            as_object(entry, "owner entry")?,
            &["position", "legacy_ref", "track_public_id", "locality"],
            "owner entry",
        )?;
    }
    let owner_matches = owner_entries.len() == resolved_entries.len()
        && owner_entries
            .iter()
            .zip(&resolved_entries)
            .all(|(got, want)| {
                let (track, locality) = want.resolved.as_ref().unwrap();
                got["position"].as_u64() == Some(want.position)
                    && got["legacy_ref"].as_str() == Some(want.legacy_ref.as_str())
                    && got["track_public_id"].as_str() == Some(track.as_str())
                    && got["locality"].as_str() == Some(locality.as_str())
            });
    if !owner_matches {
        return Err(err(
            FixtureErrorKind::BadOrdering,
            "owner_read `entries` must be the resolved entries in original order/positions",
        ));
    }
    let remainder = owner["unresolved_legacy_remainder"]
        .as_array()
        .ok_or_else(|| malformed("owner_read `unresolved_legacy_remainder` must be an array"))?;
    for entry in remainder {
        let entry_obj = as_object(entry, "remainder entry")?;
        if entry_obj
            .keys()
            .any(|k| TRACK_SHAPED_KEYS.contains(&k.as_str()))
        {
            return Err(err(
                FixtureErrorKind::GuessedResolution,
                "unresolved remainder carries track-shaped data",
            ));
        }
        require_exact_keys(entry_obj, &["position", "legacy_ref"], "remainder entry")?;
    }
    let remainder_matches = remainder.len() == unresolved_entries.len()
        && remainder
            .iter()
            .zip(&unresolved_entries)
            .all(|(got, want)| {
                got["position"].as_u64() == Some(want.position)
                    && got["legacy_ref"].as_str() == Some(want.legacy_ref.as_str())
            });
    if !remainder_matches {
        return Err(err(
            FixtureErrorKind::BadOrdering,
            "owner_read `unresolved_legacy_remainder` must be the unresolved entries \
             in original order/positions",
        ));
    }

    let non_owner = as_object(&expected["non_owner_read"], "non_owner_read")?;
    if is_public {
        let keys_ok = non_owner.len() == 5
            && ["visibility", "id", "name", "is_public", "entry_count"]
                .iter()
                .all(|k| non_owner.contains_key(*k));
        if !keys_ok || non_owner["visibility"].as_str() != Some("metadata_only") {
            return Err(err(
                FixtureErrorKind::BadVisibility,
                "public non-owner read must be exactly metadata_only + id/name/is_public/entry_count",
            ));
        }
        if non_owner["id"].as_str() != Some(public_id) {
            return Err(err(
                FixtureErrorKind::IdentityConfusion,
                "non_owner_read `id` must equal the playlist `public_id`",
            ));
        }
        if non_owner["name"].as_str() != Some(name)
            || non_owner["is_public"].as_bool() != Some(true)
            || non_owner["entry_count"].as_u64() != Some(resolved_entries.len() as u64)
        {
            return Err(err(
                FixtureErrorKind::BadVisibility,
                "public non-owner read must carry the playlist metadata and the \
                 resolved-only entry count",
            ));
        }
    } else if non_owner.len() != 1 || non_owner["visibility"].as_str() != Some("not_found") {
        return Err(err(
            FixtureErrorKind::BadVisibility,
            "private non-owner read must be exactly visibility:not_found",
        ));
    }
    Ok(())
}
