# Offline CC0 genre extract (interchange v1)

This source-independent, stdlib-only Python 3.11+ tool consumes a **local**
Wikidata structured-data dump and writes a small integration boundary. It reuses
mb-import's streaming projection and disposable SQLite scratch conventions, not
its URL-relationship allowlist. There is no network code, download, database
connection, schema/migration allocation, worker route, cadence or enabled runtime.

Compliance: [AGENTS.md “Clean-room, YouTube and MetaBrainz guardrails”](../../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails).
Wikidata structured statements and labels are CC0; see
[Wikidata licensing](https://www.wikidata.org/wiki/Wikidata:Licensing).
Only P434, P136, P279 and the selected language's label are projected. This does
not ingest MusicBrainz genre/tag assignments, NC supplementary data or replication
packets; MLHD+ is never used. No competitor taxonomy/assets/copy are included.
All committed entities and taxonomy labels are invented fixtures, not a dataset.
No roots are configured or inferred here.

## Invocation and input admission

Run from the trusted source tree, never from an untrusted dump's directory:

```sh
python3 -I containers/genre-extract/extract.py \
  --input /path/to/already-local/synthetic.json.bz2 \
  --output /path/to/new-projection --dump-version synthetic-v1
```

This example neither downloads nor commissions a production dump. A future
production extraction needs its own admission, sizing and execution environment.
`--dump-version` is a required caller-supplied snapshot identifier (1–128 ASCII
letters/digits/dots/underscores/hyphens, starting alphanumeric), not verified
publisher provenance. Retain the original source/licence receipt separately.
The manifest binds the actual decompressed input bytes with SHA-256; it does not
claim authenticity or make arbitrary supplied data CC0.

- Input must be a regular local file. `.gz` and `.bz2` suffixes select streaming
  decompression; otherwise bytes are read directly. No archives are extracted.
- Default `--format json-array`: `[` and `]` each on a separate line, exactly
  one complete entity per intervening line, standard comma separation. `[]` is
  also accepted. This is the line-oriented dump layout, not arbitrary pretty JSON.
- `--format jsonl`: one complete object per line, without array/comma framing.
  Empty input is a valid empty projection only in this mode.
- Invalid UTF-8/JSON, duplicate JSON keys, non-finite numbers, bad framing,
  truncated compression, invalid item identity, duplicate item IDs and structural
  errors in selected fields fail the entire extraction. Input is never executed.
  Property/lexeme records are ignored; unknown entity types fail.
- A valid item ID is canonical `Q` plus 1–20 digits (no leading zero).
  Missing P434 or P136 means no membership. Invalid claim values are skipped and
  counted in `counts.invalid_claims`; malformed containers fail instead.
- Statements must have `type=statement`, rank `normal` or `preferred`, matching
  mainsnak property/datatype, `snaktype=value`, and the expected datavalue type.
  Deprecated, unknown-value and no-value claims are skipped. Both normal and
  preferred claims are retained (no best-rank preference). Qualifiers/references
  are not projected; this is a statement-membership extract, not temporal truth.
- P434 must be a hyphenated UUID, normalized lowercase; all distinct valid IDs
  are retained. No MusicBrainz existence/type lookup occurs here: integration
  must join against its CC0 **artist** mirror and decide unmatched-ID policy.
  P136/P279 values must identify items; optional numeric-id must match the QID.
- `--language en` selects exactly that label with no fallback. Missing labels
  become null. Malformed, blank, control-bearing, surrogate or >4096-byte labels
  fail. Strings retain their original Unicode normalization.

## Stable interchange

A successful output directory contains exactly four UTF-8 files, all ending in
LF (empty JSONL tables have zero bytes). JSON keys are lexicographically ordered,
compact, and Unicode is not ASCII-escaped. There are no timestamps or host paths.

| File | Record shape | Unique key / ascending binary text order |
| --- | --- | --- |
| `artist_genres.jsonl` | `{"artist_mbid":"…","genre_qid":"Q101"}` | `(artist_mbid, genre_qid)` |
| `genres.jsonl` | `{"genre_qid":"Q101","label":"synthetic branch beta","present":true}` | `genre_qid` |
| `genre_parents.jsonl` | `{"genre_qid":"Q101","parent_qid":"Q100"}` | `(genre_qid, parent_qid)` |
| `manifest.json` | One JSON object, schema `nntune.genre-extract.v1` | Completion marker |

QIDs sort as **strings**, not numeric IDs. Duplicate claims and cross-entity
memberships deduplicate. Only membership nodes and their full reachable P279
ancestry are emitted; unused nodes are excluded. Each edge endpoint has a genre
row. An absent entity is an explicit `present:false,label:null` placeholder;
a present entity without a requested-language label has `present:true,label:null`.
Missing parents are not guessed. Cycles/self-edges are retained but visited once.
Exceeding the configured ancestry depth fails, never silently truncates.

The manifest includes extractor version `1`, extractor-source SHA-256, source
name/licence/dump version, input SHA-256 and decompressed byte count/format,
language, effective limits, entity/invalid-claim counts, and each row file's
SHA-256, byte length and row count. Same bytes/options/source produce identical
output bytes; entity/claim reordering preserves row files but changes the input
identity (and potentially diagnostic counts). Compression envelopes are not
part of identity. Consumers must reject unknown schema/version, validate every
file's digest/bytes/rows and reference/uniqueness constraints, and retain this
manifest as provenance. Empty results are valid, not proof of coverage.

`--output` must not exist (even as an empty directory or symlink). The process
exclusively reserves it, then writes the manifest via rename **last**. Consumers
must not admit a directory without a complete, validated manifest. Normal errors
remove the run-owned directory and scratch; existing outputs are never changed.
An abrupt kill/power loss can leave partial directories: quarantine them, do not
resume or ingest them. Use a new destination to retry. Do not mutate input/output
paths concurrently; this is trusted offline filesystem tooling, not a multi-user
filesystem security boundary. Error messages do not echo dump text or local paths.
Argument errors, including unknown options and invalid values, exit 2 with a fixed
sanitized diagnostic; `--help` still describes the available options.

## Resource contract

Defaults (all flags explicit in the manifest):

| Flag | Default | Enforcement |
| --- | --- | --- |
| `--max-line-bytes` | 16 MiB | bounded binary readline before JSON parsing |
| `--max-input-bytes` | 8 GiB decompressed | includes all framing/newline bytes |
| `--max-entities` | 10,000,000 | includes ignored non-item records |
| `--max-db-bytes` | 1 GiB | SQLite 4096-byte pages, rounded down; min 64 KiB, max 1 TiB |
| `--max-output-bytes` | 1 GiB | combined row files plus manifest, before writing |
| `--max-depth` | 128 edges | shortest distance from membership nodes; range 0–10000 |

One input pass, memory proportional to one bounded JSON entity plus Python JSON
object overhead (not a hard RSS guarantee), 8 MiB SQLite page cache, at most 64 MiB
hard SQLite heap limit. Admission sets and queries `PRAGMA hard_heap_limit` before
creating tables, selecting memory temp storage or loading input; an unavailable,
nonpositive or above-budget effective limit refuses extraction. An existing lower
positive SQLite limit is retained. Scratch stores all item labels and P279 edges,
not only output nodes. SQLite sorting/temp data stay in its bounded heap; journaling is disabled
for this disposable scratch only. There are no shell/external sort processes.
Scratch file bytes <= DB budget; output bytes <= output budget, excluding source,
filesystem metadata and interpreter overhead. Scratch lives in a temporary sibling
of the output. Multiple independent runs need separate aggregate disk admission.

CPU/I/O depend on admitted input, claim fan-out and graph depth; these are not
fixed-duration guarantees. Execute large admitted jobs with an external deadline,
RSS/filesystem quota and isolated source/output storage. Increase limits only after
sizing; the defaults deliberately refuse a full unfiltered production dump. This
boundary has no production-scale throughput or recall claim.

## Actions verification

The existing required `mb-import-etl` job in `.github/workflows/ci.yml` runs:

```sh
python3 -I containers/genre-extract/tests/test_extract.py
```

Golden fixtures cover P434 joining, missing/malformed IDs, absent P136,
deduplication, missing labels/nodes, transitive parents and cycles. Black-box
CLI tests cover determinism, order/compression/format equivalence, provenance,
malformed input, output refusal/cleanup and resource saturation. Full suites and
CI gates run on Actions, not the shared host. Local execution is only for a
single failing test selected from Actions, per [CONTRIBUTING.md](../../CONTRIBUTING.md#verification-github-actions-first).
