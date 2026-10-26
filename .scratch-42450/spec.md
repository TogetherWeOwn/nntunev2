# Migrate playlists from streaming-service exports via MusicBrainz matching — design proposal

Status: **PROPOSED, NOT IMPLEMENTED.** This document is a design only. It authorizes no parser, endpoint, migration, or UI build. Baseline citations are `main` at `d2b4ff9bb` (2026-10-26).

## 1. User value, evidence, and limits of the claim

**User value (one line):** a listener switching from Spotify (or similar) can bring their playlists with them in minutes instead of rebuilding years of curation by hand.

Evidence for the value: playlist lock-in is the named top barrier to switching services. Spotify ships a built-in TuneMyMusic import, Apple Music ships a transfer tool, and paid third parties (Soundiiz, TuneMyMusic, FreeYourMusic) exist solely for this job. All of them do the same two things: match tracks across catalogues and honestly report the misses. NNTune has no migration path at all today.

Limits of the claim, stated up front:

- This proposal moves a *track list*, not audio, likes, play history, folders, or collaborative state. A migrated entry becomes queueable only if that recording is already in the listener's NNTune library (§8); this feature never downloads or ingests music.
- We cannot promise a match rate before the labelled corpus exists (§7). Any percentage quoted before then is marketing, not engineering.
- Spotify's own account-data export carries no ISRCs (§4.3), so the strongest matching tier only fires for exports that include them.

## 2. Scope and explicit non-goals

**In scope (v1 design):**

- Accept a service-export file the user already owns (CSV or plain-text track list), chosen locally in the browser.
- Match each row against our own MusicBrainz mirror in confidence tiers (§6), ISRC-first where data exists.
- Preview every outcome before anything changes; append confirmed matches through the existing queue model (§8); let the user adjudicate low-confidence rows from a bounded candidate list; produce a downloadable unmatched-track report the user can keep.

**Explicit non-goals (v1, and not implied by anything below):**

- **No third-party account login.** No Spotify/Apple/YouTube OAuth, no credential collection, no session cookies, no "paste your playlist URL and we fetch it."
- **No scraping** of any service, and no calls to third-party music APIs. Matching reads only our own mirror.
- **No silent auto-import of low-confidence matches.** Anything below the auto-append tiers waits for an explicit user pick or stays unmatched (§6.4). There is no threshold the user can lower in v1.
- **No auto-ingest.** A mirror match that is not in the user's library is reported, never turned into a download/indexer job by this flow.
- No server playlists, no named/saved playlists, no cross-device sync (server `/v1/playlists` is future/core-only, `docs/api-delivery-map.md` "Library genres/playlists core" row).
- No new persistent store of user playlists or export files server-side (§9, §10).
- No support promise for formats not in §4.1; anything else is refused, not best-efforted.

## 3. Relationship to [TOG-41848](/TOG/issues/TOG-41848)

[TOG-41848](/TOG/issues/TOG-41848) (spec saved, in review) designs the *downstream* half: a local playlist-**file** is previewed against the already-loaded library snapshot and appendable entries are appended to the queue. This proposal is the *upstream* half: an **external export** whose entries have no NNTune IDs must first be resolved to recordings. The two specs share a contract; neither duplicates the other's scope.

**Shared — reused verbatim from the [TOG-41848](/TOG/issues/TOG-41848) spec, not re-specified here:**

- The journey shape: file choice → bounded local parse → classified preview → explicit Append or Cancel; file selection and preview mutate nothing (its §8.1).
- Append semantics: one immutable queue update, file order preserved, current track/position untouched, no autoplay, total dedupe (its §6, over `addTrack` at `apps/web/src/lib/player/queue.ts:111-118`).
- Preview generations and invalidation: catalogue/auth/file/queue generations captured at preview, rechecked at Append, single-use preview token (its §7). This proposal adds one more captured value: the mirror-match generation (§8.2).
- Hostile-input handling: no authority from file content, no raw-line echo in UI/logs/errors, sanitized inert labels, generated (never echoed) feedback vocabulary (its §5 hard rules).
- Bounds philosophy: whole-file refusal over silent truncation; content validation only, filename/MIME never consulted (its §3.4, §4).
- Accessible status/count wording rules: partial success announced as partial, never "playlist imported" (its §8.3).

**New in this proposal (not in [TOG-41848](/TOG/issues/TOG-41848)):**

- Two new input formats (service-export CSV, plain-text list) and their parsers (§4). [TOG-41848](/TOG/issues/TOG-41848) accepts only NNTune's own `queue.m3u` ID list and explicitly forbids title-based matching *for that flow*; this proposal is the lane where title/artist matching is the entire point, which is why it needs its own tiers, corpus, and precision gates instead of borrowing that flow's ID resolution.
- The MusicBrainz matching engine, confidence tiers, disambiguation rules, and human-review list (§5–§7).
- A stateless authenticated match endpoint and one mirror extension (§9).
- The `matched_not_in_library` outcome and the downloadable unmatched report (§8).

**Non-overlap guarantee:** [TOG-41848](/TOG/issues/TOG-41848) never leaves the device and never matches by metadata; this proposal never accepts `queue.m3u` and never appends except through the shared append contract. If [TOG-41848](/TOG/issues/TOG-41848)'s build lands first, this proposal's UI is a second producer into its preview/append surface. If it doesn't, this proposal's preview must implement the same contract — the spec text is shared either way, the code is written once. Neither card is a build prerequisite of the other; the shared contract is.

Other lanes checked for overlap: saved named queues ([TOG-39755](/TOG/issues/TOG-39755)), favorites backup ([TOG-40833](/TOG/issues/TOG-40833)), cross-device handoff ([TOG-37559](/TOG/issues/TOG-37559)) — all independent per [TOG-41848](/TOG/issues/TOG-41848) §2; this proposal touches none of them.

## 4. Input contract

### 4.1 Accepted formats (exactly two)

**A. Delimited text with a header row (CSV/TSV).** The shape produced by export tools a user already has (e.g. Exportify-style Spotify CSV exports). Delimiter is detected from the header line only (`,` or tab); quoting follows RFC 4180. Column mapping is by header name, case-insensitive, from a fixed alias table; unknown columns are ignored and counted, never guessed positionally.

| Field | Required | Accepted header aliases (examples) | Cap |
|---|---|---|---|
| title | yes | `Track Name`, `Title`, `Name` | 512 chars |
| artist | yes | `Artist Name(s)`, `Artist`, `Artists` | 512 chars |
| album | no | `Album Name`, `Album` | 512 chars |
| duration_ms | no | `Duration (ms)`, `Duration`, `Length` | integer 1..=7,200,000 |
| isrc | no | `ISRC` | §4.2 grammar |

If no title or no artist column resolves, the whole file is refused as `unsupported_format` with `next_action` naming the two accepted shapes. A row with an empty title or artist is a **rejected record**, not a file refusal.

**B. Plain-text lines.** One track per line, `Artist - Title` (the shape TuneMyMusic-style text exports and hand-kept lists use). Split on the *first* ` - `; a line without it is a rejected record. No duration, album, or ISRC is available in this format, so its rows can never reach the top tiers (§6) — the preview says so before matching starts, not after.

### 4.2 Bounds and validation (whole-file refusal unless stated)

- File size ≤ 2 MiB; rows ≤ 10,000; line length ≤ 2,048 chars. (A 10k-row export CSV is ~1–2 MiB; the cap pair is sized to admit real exports while keeping the [TOG-41848](/TOG/issues/TOG-41848) whole-file-refusal rule. This is larger than that spec's 256 KiB/4,096 because the input here is foreign bulk data, not our own export — the build card must re-derive these from corpus fixtures, §7.)
- UTF-8 only, decoded fatally; BOM/UTF-16/invalid sequences refuse the file as `unsupported_encoding`. No sniffing or transcoding.
- ISRC grammar: exactly `^[A-Z]{2}[A-Z0-9]{3}[0-9]{7}$` after upper-casing and removing hyphens; anything else makes the *field* absent (record proceeds without it), never a fuzzy ISRC.
- Control/bidi/format characters in any field are stripped for display per [TOG-41848](/TOG/issues/TOG-41848) §5 sanitization; matching uses the normalized form (§5), never the raw bytes.
- CSV formula-injection: fields beginning with `=`, `+`, `-`, `@`, tab, or CR are data, not formulas — they are matched and displayed as text, and the generated report (§8.3) neutralizes them on output.
- Duplicate rows (same normalized artist+title+duration, or same ISRC) classify once; later occurrences are `duplicate_in_file`. Order of first occurrence is the order that matters everywhere below.
- The file is parsed **entirely in the browser**. The raw file is never uploaded (§10).

### 4.3 Explicitly not accepted in v1

- Spotify's official account-data JSON (`YourLibrary.json`, `Playlist*.json`): it carries `spotify:track:` URIs and names but no ISRC and no duration, and a JSON schema-version surface of its own. Plausible v2; in v1 it is refused as `unsupported_format`, honestly, rather than half-parsed.
- XSPF/XML, M3U/M3U8 from foreign players, PLS, ZIP/compressed archives, pasted playlist URLs (see non-goals: nothing is fetched).

## 5. Normalization (query-side, deterministic, shared by matcher and corpus)

Normalization turns a parsed row into match keys. It is a pure function, versioned (`norm_v1`), and the corpus (§7) pins its behavior. Rules, in order:

1. Unicode NFC, case-fold, collapse interior whitespace, trim.
2. Strip bracketed/parenthesized edition suffixes from the title into a separate `edition_tokens` set rather than deleting them: `(Remastered 2011)`, `[Live]`, `- Single Version`, `(feat. X)` etc. The bare title is the match key; edition tokens are disambiguation evidence in §6.3, never silently discarded.
3. Artist: split multi-artist cells on the producing exporter's known separators (`,` / `;` for CSV artist lists) into an ordered artist list; `feat.`/`ft.` credits move to a secondary list. Primary-artist matching is required; featured artists are corroborating evidence only.
4. Punctuation and diacritics: fold for the *comparison key* only (the mirror's `pg_trgm` search is over raw titles, `workers/mb/src/lib.rs` `search_sql`); originals are retained for display and for the report.
5. Duration: integer ms as given; absent stays absent. No duration is ever inferred from the album or estimated.

## 6. Matching design

### 6.0 What exists today (baseline the design must fit)

- The mirror has `mb_artist`, `mb_release` (with `artist_mbid`), `mb_recording` (`mbid`, `release_mbid`, `title`, `track_no`, `duration_ms`), and `mb_track` (`migrations/0001_mb_mirror.sql:11-54`, `migrations/0016_mb_track.sql:23`). There is **no ISRC data in the mirror**: the import ETL projects only `recording(id, gid, name, length)` (`containers/mb-import/etl.py:181`). Tier 0 below is therefore *designed now, available only after build item B1 (§12)*; until then ISRC-bearing rows degrade to Tier 1+ and the preview must say ISRC matching is unavailable rather than quietly skipping it.
- The MB worker's recording search returns **no artist and no duration** (the `Recording` arm of `search_sql` selects `NULL` artist fields from `mb_recording`). Artist/title/duration matching needs a new bounded read surface (§9.2); the existing one cannot support it.
- Precedent for threshold discipline: ingest's release matcher uses `MIN_MATCH_SCORE = 0.6` and `MIN_MATCH_MARGIN = 0.1` with artist verification for "Artist - Album" shapes (`workers/ingest/src/matching.rs:36-52`). Recording-level matching keeps the *shape* of that discipline (score + margin + artist verification) but gets its own constants, set by the corpus (§7), not copied.
- A recording match is not yet a queueable track: the web `Track` carries no MBID (`apps/web/src/lib/player/queue.ts:8-16`), and catalog reads resolve only through caller library membership (`docs/adr/0006-api-surface.md:157-165`). Resolution from recording MBID → the caller's library track happens server-side (§9.2).

### 6.1 Candidate generation

Per row, the server returns a bounded candidate set (≤ 10) from the mirror: recordings whose title passes the trigram predicate against the normalized title, each joined through `mb_track` → `mb_release` → `mb_artist` for artist name/MBID, plus recording `duration_ms` and (post-B1) ISRCs. One recording appearing on several releases collapses to one candidate (recording-level identity; release choice does not matter for queueing). Candidate generation never calls out to musicbrainz.org or any third party.

### 6.2 Confidence tiers

| Tier | Rule (all conditions required) | Disposition |
|---|---|---|
| **T0 ISRC-exact** | Row ISRC (§4.2 grammar) resolves to exactly one recording MBID in `mb_isrc`. If one ISRC maps to multiple recordings (real-world ISRC reuse), the artist+title must also verify against exactly one of them, else T3. | Auto-append eligible |
| **T1 Exact metadata** | Exactly one candidate with: normalized title equal, primary artist equal (normalized), and (duration present on both sides and \|Δ\| ≤ 2 s). If the row has no duration, T1 additionally requires the candidate to be the *only* recording by that artist with that normalized title. | Auto-append eligible |
| **T2 Strong fuzzy** | Top candidate: title trigram similarity ≥ `T2_MIN_SCORE`, lead over runner-up ≥ `T2_MIN_MARGIN`, primary artist verifies (exact normalized primary, or artist similarity ≥ its own corpus-set threshold), duration (when both present) \|Δ\| ≤ 5 s, no edition-token conflict (§6.3). | Auto-append eligible **only after** the §7 precision gate passes; before that, T2 presents as T3 |
| **T3 Review** | Anything with plausible candidates that fails a higher tier: ties, near-ties, edition conflicts, missing-duration ambiguity, multi-recording ISRC. User sees ≤ 5 candidates (title, artist, album/release, duration, why it's in review) and picks one or "none of these". | Human decision; default is *no match*, never pre-selected |
| **T4 Unmatched** | No candidate passes candidate generation, or the user picks "none" in T3. | Goes to the unmatched report (§8.3) with the reason |

"Auto-append eligible" still means *shown in the preview and appended only on the user's explicit Append* (shared contract, §3). No tier appends on file selection, on parse, or in the background.

### 6.3 Duplicate and edition disambiguation

- **Same recording, many releases:** not ambiguity. The match target is the recording MBID; which release it came from is display metadata (the earliest release is shown and labelled as display-only).
- **Same artist + title, several recordings** (studio vs live vs remaster vs explicit/clean vs re-recording): edition tokens (§5 rule 2) must not conflict — a row whose title carries `Live` never auto-matches a candidate whose release/recording carries no live marker, and vice versa. Duration within tier tolerance can separate them; if two or more recordings still survive all checks, the row is T3 with the differing fields highlighted. Never first-wins, never popularity-wins (the mirror has no popularity data anyway).
- **Same title, different artist** (the classic trap): artist verification is a hard gate at every tier; a title-perfect candidate by the wrong artist cannot auto-match and appears in T3 only if the artist is at least plausibly related (e.g. featured-artist overlap), otherwise T4.
- **Same row claimed twice:** two distinct file rows may legitimately match the same recording (a playlist can list a track twice); the second append is suppressed by the shared queue dedupe and reported as `already_queued`/`duplicate_in_file`, not treated as a matcher error.
- **Various-artists / soundtrack rows:** artist `Various Artists` never satisfies artist verification by itself; such rows need title+duration uniqueness or fall to T3/T4.

### 6.4 Human-review list

T3 rows are presented as a review list *within* the preview, grouped separately from auto-append rows, each row showing the original (sanitized) export text, up to 5 candidates, the concrete reason it needs a decision ("two recordings with this title and artist", "duration differs by 9 s"), and three actions: pick a candidate, mark unmatched, or leave undecided. Undecided = unmatched for this Append (stated in the count wording), and the row still lands in the report with `review_skipped`. Review picks join the append set under the same preview generation; they never retroactively change a consumed preview. Keyboard/focus behavior follows [TOG-41848](/TOG/issues/TOG-41848) §8.2.

### 6.5 Measurable precision goals

Measured on the labelled corpus (§7), per tier, before that tier may auto-append in production:

- **T0 precision ≥ 99.9%** (ISRC collisions are the only expected error source, and §6.2 already routes collisions to T3).
- **T1 precision ≥ 99.5%**, and **zero** same-title/different-artist false appends.
- **T2 precision ≥ 98%** with the margin rule active; if the corpus cannot support 98% at any threshold pair, T2 auto-append does not ship and those rows stay in review. This is a kill rule for the tier, not a tuning aspiration.
- Reported alongside, never gated on: coverage (share of rows reaching T0–T2), review burden (T3 share), and per-trap-category error counts (§7). A tier that buys coverage by spending precision fails.

## 7. Labelled test-corpus plan

The corpus is a versioned fixture set in the repo (synthetic and hand-built rows only — **no real user exports, ever**), owned as acceptance material by the Test Author per company practice; the Builder never edits it to make the matcher pass.

- **Form:** one row per case: input fields (title/artist/album/duration/isrc) → expected outcome (`recording_mbid` | `review` | `unmatched`) + expected tier + trap category. Mirror fixtures are a small seeded subset (the existing test-mirror pattern), so the corpus runs offline in CI with no live database beyond the standard test services.
- **Size:** ≥ 300 cases at first gate, ≥ 40 per trap category below; grows by accretion — every matcher bug found later becomes a corpus case before its fix.
- **Trap categories (required):** same-title/different-artist; same-artist/same-title studio-vs-live and remaster pairs; explicit-vs-clean; `feat.` credit present/absent/moved; remaster/year suffix churn; duration collisions (two recordings within 2 s); missing duration; missing/unknown artist (`Various Artists`, soundtrack casts); ISRC reused across recordings; ISRC present but recording absent from mirror; diacritics/transliteration variants; punctuation-only title differences; artist name order/`The` variants; duplicate rows in file; CSV formula-leading and bidi/control-laden fields (must classify safely, not match weirdly).
- **Runner:** a single command executes the matcher against the corpus and prints per-tier precision/coverage, exiting non-zero below the §6.5 gates. The runner's output is the evidence a build PR cites; thresholds are changed only by a PR that also changes corpus or matcher and re-publishes the numbers.
- **Honesty rule:** corpus precision is *fixture* precision. The spec claims nothing about real-world match rates until staging evidence with synthetic full-playlist fixtures exists (test plan, §11).

## 8. Output contract

### 8.1 Classification (every input row lands in exactly one)

`appendable` (T0–T2, or user-picked in T3, and resolved to a library track) · `matched_not_in_library` (recording matched, but no track in the caller's library for it — reported with the matched title/artist so the user can act elsewhere; never auto-ingested) · `already_queued` · `duplicate_in_file` · `review_undecided` · `unmatched` (T4, with reason code) · `rejected` (row-level parse failure, §4). Counts satisfy `rows = Σ classifications`, asserted by tests exactly as [TOG-41848](/TOG/issues/TOG-41848) §5 asserts its invariant.

### 8.2 Append path

Append uses the shared semantics (§3): appendable library tracks only, file order, one immutable update, current track untouched, truthful partial wording ("Appended 84 of 100 — 9 not in your library, 4 unmatched, 3 need review"). One addition to [TOG-41848](/TOG/issues/TOG-41848) §7: the preview captures the **match generation** (the batch identity returned by §9.2); if the library snapshot, auth, or queue generation moved, or the match results were recomputed, Append is disabled as `preview_stale`. Library resolution is rechecked server-side at append time only in the sense that the queue accepts the loaded catalogue's own track objects (shared rule) — a recording that left the library between match and Append resolves as missing and is reported, never forced.

### 8.3 Unmatched-track report (user data, user keeps it)

A downloadable file generated **client-side** from preview state, offered on both Append and Cancel paths (a user who cancels keeps the diagnosis). Contents: one line per non-appended row — original artist/title (from their file), classification, reason in plain words, and for `matched_not_in_library` the matched recording's title/artist/duration. Two formats: plain text (readable, re-importable as §4.1-B input) and CSV (same columns; formula-leading cells neutralized with a leading `'`). The report contains no internal IDs beyond recording MBIDs (public identifiers, included deliberately so the user can look a recording up), no account data, and no data from any other user.

## 9. Data model and API surface

### 9.1 Mirror extension (build item B1)

New table `mb_isrc (isrc TEXT, recording_mbid UUID REFERENCES mb_recording(mbid), PRIMARY KEY (isrc, recording_mbid))` plus index on `isrc`, populated by projecting the MusicBrainz `isrc` COPY file in the existing ETL (`containers/mb-import/etl.py`) and kept current by the same replication path as the other mirror tables. Forward-only migration, CC0 core data like the rest of the mirror. **Sizing must be measured by B1's spike before the migration is written** (row count and index size from the actual COPY file); if the table threatens the mirror's disk budget, B1 stops and reports rather than landing a partial import. Until B1 lands, T0 is unavailable and §6.0's honest-degradation rule applies.

### 9.2 Match endpoint (build item B2; stateless, authenticated)

`POST /v1/library/match` (name indicative; final naming follows ADR 0006 conventions) on the API worker, session-authenticated, no new auth surface:

- **Request:** `{ rows: [{ title, artist, album?, duration_ms?, isrc? }] }`, ≤ 200 rows per call (client batches a 10k file into ≤ 50 calls), field caps identical to §4.2, `norm_v1` declared. Rows are the *parsed fields*, never the raw file.
- **Response per row:** `{ tier, recording_mbid?, library_track_id?, candidates?: ≤5 [{ recording_mbid, library_track_id?, title, artist, album, duration_ms, reasons[] }], reason_code }`. Recording→library resolution happens here, scoped to the caller's library membership (§6.0), so MBIDs and mirror internals are never needed by the queue UI.
- **Stateless:** nothing is persisted — no migration sessions table, no row storage. The match generation (§8.2) is a hash of (normalized rows, matcher version, mirror packet `mb_import_state.last_packet`) computed on the fly.
- **Abuse bounds:** per-account rate limit on the existing limiter, request-body cap, per-row candidate cap, and a server-side deadline; oversize batches are refused with `next_action`, not truncated.
- Errors use the canonical envelope (`docs/api-errors.md`); per-row failures are classifications, not HTTP errors. A mirror outage fails the batch closed (preview unavailable), never degrades into title-only guessing.

Rejected alternative: expose recording MBIDs on every library track and match client-side. It widens every library payload forever, pushes tier logic into the browser where the corpus can't gate it, and leaks mirror-match internals into the queue model. Server-side matching keeps one matcher, one corpus gate, one place to fix.

### 9.3 What deliberately does not exist

No `migrations` table, no playlist entity, no stored export, no match-history/analytics record in v1. If resume-a-migration-later is ever wanted, it is a new spec revision with its own retention design — not a column added quietly.

## 10. Security, privacy, and retention

- **The export file is user data and never leaves the device.** Parsing is client-side; the server receives only bounded parsed fields per §9.2, over the existing authenticated session. There is no upload endpoint to abuse.
- **Server retention of match inputs: none.** Rows are processed in-request and not written to any table, queue, or log. Logs carry counts, tier tallies, and error codes only — never titles, artists, ISRCs, or candidate text (same no-hostile-echo rule as [TOG-41848](/TOG/issues/TOG-41848) §5). This must be pinned by a test in B2, not just asserted here.
- **Client retention:** preview state is in-memory only, discarded on Append-consume, Cancel, navigation, or any generation invalidation ([TOG-41848](/TOG/issues/TOG-41848) §7). The only durable artifacts are (a) the queue itself, under the queue's existing persistence, and (b) the report file, if the user downloads it. There is nothing server-side to delete on account deletion because nothing was stored; the account-deletion copy deck needs no change for v1.
- **No credential surface:** no third-party login, no tokens requested, no playlist URLs fetched — SSRF and credential-theft classes are absent by construction, not mitigated after the fact. File fields that look like tokens/URLs are rejected records, never dereferenced, rendered, or logged.
- **Parser hardening:** RFC 4180 parsing with row/field caps enforced during parse (no unbounded read-then-check), fatal UTF-8, no HTML rendering of any file-derived string (inert text only, sanitized per §4.2), report generation neutralizes spreadsheet formulas (§8.3).
- **Mirror data is public** (MusicBrainz CC0 core); returning candidate metadata discloses nothing user-specific. `library_track_id` in responses is scoped to the caller's own library — B2 must prove a row can never resolve into another user's library track (IDOR test in its acceptance set).
- **Cost-abuse:** the endpoint is the only new compute surface; rate limits in §9.2 keep one account from turning migrations into a mirror-scanning oracle. Candidate responses are capped so the endpoint can't be used to bulk-extract the mirror faster than existing search already allows.

## 11. Cost and test plan

**Cost:** zero third-party spend (own mirror, no external APIs, no new services, no new dependencies assumed — CSV parsing must use an already-vendored parser or a small reviewed hand-rolled RFC 4180 reader, decided in B4 against the actual workspace). Recurring: mirror disk for `mb_isrc` (measured in B1, one-time + replication growth) and bounded Hyperdrive queries per batch (≤ 50 calls × ≤ 200 rows for the largest supported file; candidate SQL rides the existing `mb_recording_title_trgm_idx` and `mb_track` joins — B2 must publish a staging timing for a synthetic 10k-row file before merge). No egress, no per-match fees.

**Test plan (per layer; build items in §12 carry the concrete sketches):**

- *Corpus gate (B3):* §7 runner in CI; tier thresholds config-pinned; precision below §6.5 fails the build. This is the heart of the feature's honesty.
- *Parser (B4):* unit fixtures for valid/empty/header-alias/missing-column/oversize/wrong-encoding/formula-leading/bidi/duplicate cases; whole-file refusals assert `error_code` + `next_action`; fuzz-ish table of malformed CSV quoting.
- *Endpoint (B2):* contract tests for batch caps, auth requirement, caller-library scoping (IDOR), no-persistence and no-log-echo assertions, mirror-outage fail-closed, rate-limit refusal shape.
- *Mirror (B1):* ETL projection test on fixture COPY rows, migration up/down discipline per repo convention, replication-packet fixture proving `mb_isrc` stays current, size/timing readout attached to the PR.
- *UI (B5/B6):* component tests for preview classification counts invariant (§8.1), generation invalidation incl. match generation, review-pick flow, partial-success wording, report content (incl. formula neutralization), keyboard/focus per [TOG-41848](/TOG/issues/TOG-41848) §8.2; e2e synthetic playlist (≥ 200 rows, all trap categories) against staging with the seeded mirror, ending in a queue the test reads back.
- *Non-goals regression:* tests asserting no network call leaves the app during parse, no ingest job is enqueued by any classification, and T2 rows present as review until the precision gate flag is on.

## 12. Proposed one-PR build items (cut-ready; creation waits on §14 disposition)

- **B1 — Mirror: `mb_isrc` table + ETL projection + replication.** *Sketch:* migration, `etl.py` projection, validator coverage; acceptance = §11 mirror tests + measured size/timing readout. Independent; lands first or in parallel.
- **B2 — Match endpoint + recording-candidates query.** *Sketch:* §9.2 route on the API worker, candidate SQL, library scoping, rate limit; matcher called as a pure function from B3 (stubbed tiers until B3 merges). Acceptance = §11 endpoint tests incl. IDOR/no-log-echo. Depends on B3's interface, not its thresholds.
- **B3 — Matcher core + normalization `norm_v1` + corpus runner.** *Sketch:* pure module (no I/O), tier logic §6.2–6.3, corpus fixtures + runner wired to CI with §6.5 gates (T2 flag off until green). Acceptance = corpus readout in PR; thresholds set from evidence, not copied from ingest. Test Author owns the corpus file; Builder owns matcher + runner.
- **B4 — Export parsers (CSV/TSV header-mapped, plain-text) in the web app.** *Sketch:* §4 contract, pure parse-to-rows with classifications, no UI. Acceptance = §11 parser fixtures. Independent of B1–B3.
- **B5 — Migration preview + review UI on the shared append contract.** *Sketch:* §6.4/§8 flow wired B4 → B2 → append, generations incl. match generation, accessible wording. Acceptance = §11 UI tests + staging e2e. Depends on B2, B4; shares (does not fork) the [TOG-41848](/TOG/issues/TOG-41848) append surface.
- **B6 — Unmatched report download (text + CSV).** *Sketch:* §8.3 generation from preview state. Acceptance = report-content tests incl. round-trip of the text form through B4's parser. Depends on B5 state shape; smallest, last.

Sequencing: B1 ∥ B3 ∥ B4 → B2 → B5 → B6. Each is one PR-sized slice with its own acceptance card when cut; none is authorized by this spec alone.

## 13. Owner-only open questions

**None that block design disposition.** The following are recorded as *decisions taken in this spec* that the CEO disposition (§14) can overturn — they are not questions to the owner:

- v1 input limited to CSV/text; Spotify official JSON deferred (§4.3). *(Revisit if real users arrive with only the official export.)*
- `matched_not_in_library` is report-only; this flow never triggers ingest (§8.1). *(A future "add matched albums to my library" action would be its own card through existing ingest surfaces.)*
- Zero server retention, hence no resumable migrations (§9.3).

## 14. Disposition required before any build

1. CEO design disposition on this spec (accept / accept-with-changes / reject), including the §6.5 precision gates and the T2 kill rule.
2. Confirmation that B1's mirror growth fits the host disk budget (B1 spike readout feeds this; disposition of B1 specifically may wait for the measurement).
3. Only after 1–2: cut B1–B6 as separate build cards, each with a Test-Author acceptance card per company practice. This design card creates no build cards and no implementation.
