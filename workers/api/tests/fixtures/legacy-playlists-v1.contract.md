# Legacy-playlist fixture package — schema contract v1

Fixtures: `legacy-playlists-v1.json` (this directory). Acceptance tests:
`workers/api/tests/legacy_playlist_fixtures_acceptance.rs`. The fixtures are
deterministic, synthetic, non-production inputs for a later playlist cutover.
They are inert data: loading them never executes SQL, touches a database, or
changes current runtime behavior.

Semantics follow `docs/adr/0002-data-model.md` (public playlists are
metadata-only for non-owners; unresolved legacy refs stay owner-only and are
excluded from the non-owner count; legacy refs are never guessed into
tracks). This package asserts no new runtime behavior — it only pins the
fixture corpus the later work must satisfy.

## Protected files (Test Author–owned; the paired builder must not edit)

- `workers/api/tests/legacy_playlist_fixtures_acceptance.rs`
- `workers/api/tests/fixtures/legacy-playlists-v1.json`
- `workers/api/tests/fixtures/legacy-playlists-v1.contract.md`

Builder-owned seam (the only files the builder creates/edits):

- `workers/api/src/legacy_playlist_fixtures.rs` (new, minimal validator)
- `workers/api/src/lib.rs` (add `pub mod legacy_playlist_fixtures;` only)

No application behavior, SQL, migration, schema, or route changes.

## Validator seam (builder implements exactly this surface)

In `nntune_api::legacy_playlist_fixtures`, dependency-free beyond the
crate's existing `serde`/`serde_json`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureErrorKind {
    MalformedSchema,
    IdentityConfusion,
    BadOrdering,
    BadVisibility,
    GuessedResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureValidationError {
    pub kind: FixtureErrorKind,
    pub message: String,
}

impl std::fmt::Display for FixtureValidationError { /* kind + message */ }

/// Parse and validate a whole corpus document.
pub fn validate_corpus(json: &str) -> Result<(), FixtureValidationError>;

/// Validate one case object (an element of the corpus `cases` array).
pub fn validate_case(case: &serde_json::Value) -> Result<(), FixtureValidationError>;
```

`validate_corpus` also enforces the corpus-wide rules marked † below.
Unparsable JSON is `MalformedSchema`. Validation is strict: unknown keys are rejected at every level, so a stray
field can never smuggle data past a reader. Error messages should name the
case id when known; tests assert only `kind`, never message text.

## Corpus shape

Top level — only these keys:

| Key | Rule |
| --- | --- |
| `$comment` | Optional string; ignored. |
| `schema` | Exactly `"legacy-playlist-fixtures/v1"`, else `MalformedSchema`. |
| `version` | Exactly integer `1`, else `MalformedSchema`. |
| `cases` | Non-empty array of case objects. Duplicate case `id`s † → `MalformedSchema`. |

Each case — only `id`, `description`, `playlist`, `entries`, `expected`:

- `id`: non-empty string, unique †.
- `description`: non-empty string.
- `playlist`: only `internal_id`, `public_id`, `owner_user_id`, `name`,
  `is_public`, `items`.
  - `internal_id`: integer `> 0` (a BIGSERIAL identity, never a string),
    unique across cases †; violations → `IdentityConfusion`.
  - `public_id`: exactly 22 characters from `[A-Za-z0-9_-]`, unique across
    cases †, and never equal to any `internal_id` rendered as text †;
    violations → `IdentityConfusion`.
  - `owner_user_id`: string starting with `fixture-user-`.
  - `name`: non-empty string. `is_public`: boolean.
  - `items`: array of non-empty strings starting with `legacy-ref-` (the
    stored opaque legacy refs, in stored order; repeats allowed).
- `entries`: one entry per element of `items`, same length. Per entry:
  - `position`: integer; positions are exactly `0..items.len()` in array
    order, and `legacy_ref` equals `items[position]` — violations →
    `BadOrdering`.
  - `status: "resolved"`: only keys `position`, `legacy_ref`, `status`,
    `track_public_id`, `locality`. `track_public_id` is a 22-character
    `[A-Za-z0-9_-]` handle, different from every playlist `public_id` † and
    from every `internal_id` rendered as text †; `locality` is `"catalog"`
    or `"owner_local"` — violations → `IdentityConfusion` (bad/misused
    handle) or `MalformedSchema` (unknown locality value).
  - `status: "unresolved"`: only keys `position`, `legacy_ref`, `status`.
    Any track-shaped field (`track_public_id`, `track_id`, `locality`,
    `title`, …) → `GuessedResolution`. Any other status string →
    `MalformedSchema`.
  - Determinism within a case: the same `legacy_ref` always has the same
    status, and when resolved the same `track_public_id` and `locality`;
    two different refs never resolve to the same `track_public_id`.
    Violations → `IdentityConfusion`.
- `expected`: only `owner_read`, `non_owner_read`.
  - `owner_read`: only `id`, `name`, `is_public`, `legacy_item_count`,
    `entries`, `unresolved_legacy_remainder`.
    - `id` equals the playlist `public_id`; `name`/`is_public` equal the
      playlist fields; `legacy_item_count` equals `items.len()`.
      A wrong `id` → `IdentityConfusion`; other mismatches →
      `MalformedSchema`.
    - `entries` is exactly the resolved subset of the case `entries`, in
      ascending original `position`, each with only `position`,
      `legacy_ref`, `track_public_id`, `locality`, all equal to the source
      entry. Wrong order/positions/membership → `BadOrdering`; an entry
      here for an unresolved ref (inventing a track) →
      `GuessedResolution`.
    - `unresolved_legacy_remainder` is exactly the unresolved subset, in
      ascending original `position`, each with only `position` and
      `legacy_ref`. Wrong order/membership → `BadOrdering`; a
      track-shaped field here → `GuessedResolution`.
  - `non_owner_read`:
    - Public playlist: exactly `visibility: "metadata_only"`, `id` (the
      playlist `public_id`), `name`, `is_public: true`, and `entry_count`
      equal to the number of **resolved** entries (unresolved legacy refs
      are excluded by design). Any extra key — entries, items, track
      handles, remainder, internal id, owner — or a wrong count →
      `BadVisibility`. A wrong `id` → `IdentityConfusion`.
    - Private playlist: exactly `visibility: "not_found"` and no other
      key; anything else → `BadVisibility`.

Required coverage in the corpus (asserted by the acceptance tests through
the fixture itself, not by the validator): fully resolved; mixed
resolved/unresolved; all-unresolved; empty items; a public playlist with
owner-local tracks; repeated refs preserving order/positions; distinct
internal BIGSERIAL ids versus 22-character public handles.

## Expected RED (before the builder's validator exists)

```text
cargo test --locked --manifest-path workers/api/Cargo.toml --test legacy_playlist_fixtures_acceptance
```

fails to compile with an unresolved import of
`nntune_api::legacy_playlist_fixtures` — the feature is missing, which is
the intended RED state. After the builder implements the seam above, the
same command must pass without any edit to the protected files, and the
unchanged baselines `library_tests` and `library_single_row_visibility`
must still pass.
