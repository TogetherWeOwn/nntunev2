# Profile-artifact chooser first-slice acceptance handoff

**Test-only handoff, not a merge candidate or runtime authorization.** The chooser
seam does not exist on the inspected base. Do not open an intentionally failing
PR. Independent acceptance ownership stays with the test author; the paired
builder carries these files unchanged. Corrections to acceptance files belong to
the test author, never the builder.

## Oracle and scope

Expectations are independently transcribed from the pinned contracts below:

- The accepted end-to-end stream plus Opus/Ogg bot profile spec, section 4.2
  (producer and artifacts): one `opus-64` profile (Opus in Ogg, 64 kbps);
  publication is a `cache_objects` row plus a per-user `track_media` row with
  `format='opus'`, `bitrate=64`, `content_hash=<artifact hash>`; format is
  distinguished by `track_media.format/bitrate`, never by key prefix; knowing
  the hash confers nothing (own-provider proof still required).
- The same spec, section 6 (data model): selection between source and `opus-64`
  is by `track_media(format, bitrate, content_hash)`, authorized separately by
  `user_cache_grants(user_id, content_hash)`. This slice owns selection only;
  authorization stays outside.
- Migration `0022_library_artifacts`: `track_media.content_hash` references
  `cache_objects(content_hash)` (which requires `^[0-9a-f]{64}$`),
  `track_media.bitrate` requires `bitrate > 0`, `format` is non-null text.
  Malformed-row rejection mirrors exactly these three constraints.
- The build contract for this slice: a pure module exposing one choice
  function over plain row structs; exact-profile hit preferred with a
  documented fallback order; cross-profile serve refused with a typed miss;
  an empty candidate set returns a final typed miss carrying a stable code
  and a next action; no lease, dial or IO happens before the choice.

Only inert synthetic row structs (format text, integer bitrates, opaque
hash-shaped strings) occur here. There are no database rows, R2 objects,
grants, HTTP requests, sockets, clocks, bindings or network calls. A
well-shaped row is **not** an authorized artifact.

## Smallest existing harness and proposed seam

Use the existing native Rust integration harness in `nntune-stream`, with its
existing `syn` and `quote` dev-dependencies for the guard file. There are no
manifest, lockfile, product-behavior, dependency or CI changes.

No exported builder seam exists yet. Consequently the following is an
**intended adapter contract**, not a claim to have consumed an existing export:

- Pure portable module `nntune_stream::choose`, no production callers; the
  only permitted production reference is the out-of-line `pub mod choose;`
  export. No DB, R2, grant, HTTP, socket, clock, env, async or `unsafe`.
- `MediaRow<'a> { format: &'a str, bitrate: i32, content_hash: &'a str }`
  mirroring the `track_media` triple; `StreamProfile::{Opus64}` (one profile,
  no negotiation matrix); all seam types `Debug + Clone + Copy + PartialEq
  + Eq` so outcomes compare by value.
- `choose_profile_artifact(rows: &[MediaRow], profile: StreamProfile)
  -> Result<ChosenArtifact, ChoiceMiss>`: synchronous, total, deterministic.
  `ChosenArtifact { content_hash, format, bitrate, match_level }` echoes the
  winning row plus `MatchLevel::{Exact, Fallback}`.
- Decision table for `Opus64` (evaluated in this order):
  1. Empty slice -> `ChoiceMiss::EmptyCandidateSet`, code
     `profile_artifact_empty`, next `TranscodeThenRetry`.
  2. First well-formed row with `format == "opus"` and `bitrate == 64` ->
     `Exact` (case-sensitive; `OPUS` is not the opus family).
  3. First well-formed row with `format == "opus"` (any positive bitrate) ->
     `Fallback`. Candidate order, not bitrate, breaks ties: the caller
     orders candidates, the chooser scans them.
  4. Else, if any well-formed row exists (all outside the opus family) ->
     `ChoiceMiss::CrossProfileRefused`, code
     `profile_artifact_cross_profile`, next `TranscodeThenRetry`.
  5. Else (non-empty, none well-formed) ->
     `ChoiceMiss::NoWellFormedCandidate`, code `profile_artifact_malformed`,
     next `RepairCandidates`.
- Well-formed mirrors the migration constraints: non-empty `format`,
  `bitrate > 0`, `content_hash` exactly 64 lowercase hex characters.
- `ChoiceMiss::code() -> &'static str` is the stable wire code;
  `next_action() -> NextAction` is the typed next step
  (`TranscodeThenRetry`: produce the missing opus-64 artifact, then retry;
  `RepairCandidates`: discard the malformed rows and re-read the set).
- Precedence note: a well-formed cross-profile row beats malformed
  opus-shaped rows (rule 4 before rule 5), so the actionable
  "source exists, transcode it" verdict wins over "repair".

## Must-pass matrix once the seam exists

| Area | Cases | Oracle |
| --- | --- | --- |
| Exact hit | opus-64 wins in two orderings incl. surrounded by fallback and source rows; echoes hash/format/bitrate/`Exact` | Spec 4.2, build contract |
| Ladder fallback | No exact row -> first opus row in candidate order, both orderings; echoes `Fallback` | Build contract |
| Empty miss | Empty slice -> `EmptyCandidateSet` + code + next action | Build contract |
| Cross-profile refusal | flac/mp3/wav sets and singleton -> `CrossProfileRefused` + code + next; never served | Spec 4.2/6 |
| Case | `OPUS` is cross-profile, not opus | Spec 4.2 (`format='opus'`) |
| Malformed skipped | Empty-hash and zero-bitrate rows skipped for well-formed exact and fallback rows, both positions | Migration 0022 |
| All malformed | 8 shapes (empty format, zero/negative bitrate, empty/short/uppercase/non-hex/long hash) + all-malformed set -> `NoWellFormedCandidate` + code + next | Migration 0022 |
| Precedence | Malformed opus-shaped + well-formed flac -> `CrossProfileRefused` | Decision table |
| Determinism | Same slice twice -> equal hit and equal miss | Purity |
| Containment | No production seam caller; chooser purity fence + sensitivity; no tracker IDs/UI paths/private IPv4 in these files | Slice posture |

## Intended runners (worktree root)

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test choose_profile_acceptance
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test choose_slice_guard_acceptance
rustfmt --check --edition 2021 workers/stream/tests/choose_profile_acceptance.rs workers/stream/tests/choose_slice_guard_acceptance.rs
```

Compiling commands go through the bounded pool wrapper only; never a
per-worktree target, manual `CARGO_TARGET_DIR`, or a direct compiling cargo
command. Exit 75 is refusal/wait, not product RED or PASS.

## Initial evidence and limits

Recorded at handoff on the test-only branch (see the authoring card): base
commit, presence preflight output, pool outcome, rustfmt outcome, diff
hygiene. Reproduce the noncompiling missing-seam RED preflight:

```sh
python3 - <<'PY'
from pathlib import Path
source = Path('workers/stream/src/choose.rs').is_file()
export = 'pub mod choose;' in Path('workers/stream/src/lib.rs').read_text()
print(f'choose seam preflight: source_present={source}; export_present={export}')
raise SystemExit(0 if source and export else 1)
PY
```

The behavioral suite RED is the missing seam: the test target fails to
compile with an unresolved `nntune_stream::choose` import until the builder
lands the module. Assertion sensitivity was proven separately against a
temporary uncommitted scratch implementation (kept out of the handoff):
correct scratch -> all green; ladder broken (first row wins) -> exact and
fallback tests fail; malformed accepted -> malformed tests fail;
cross-profile served -> refusal test fails; codes swapped -> code asserts
fail. The scratch was deleted before handoff; the branch carries test files
only.

The cross-package spelling scan is deliberately conservative and permits
only the out-of-line library export; it is **not** a whole-program
reachability proof. The chooser-purity fence walks code idents (doc prose
and plain strings skipped, macro inputs word-scanned) with clean/dirty
sensitivity cases; it is a spelling fence, not a capability proof — review,
dependency gates and the wasm32 build still own the full supply-chain
picture. Nothing here touches a database, R2, grants, HTTP, sockets, clocks
or staging/production.

## Explicitly NOT proven by this slice

Grant issuance/verification, R2 serving, Range/206 behavior, runtime
adapters, DB writers, ingest/transcode execution, metering/ledger writes,
bot consumption, staging or production readiness. A chosen artifact grants
no authority to serve bytes: authorization stays with the grant check at
serve time.
