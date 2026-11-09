# Listen history

## Boundary and retention default

**Default: retain history for the lifetime of the account**, until the user
explicitly clears it or the account is erased. There is no automatic age-based
expiry, partition drop, or background history purge. This is the initial product
retention default; a future timed-retention policy must be explicitly reviewed.

`listen_events` contains client-reported playback lifecycle observations. It is
**not billing evidence**, and cannot authorize playback, increase stream usage,
renew a lease, or prove seconds heard. Existing `POST /v1/plays` grant issuance,
`/v1/plays/heartbeat`, stream meter/outbox and `stream_play_ledger` keep their
existing auth, entitlement, lease and replay contracts. In particular, the
append-only metering `play_events` rules in [ADR 0002](adr/0002-data-model.md)
do not authorize a timed purge here or any purge of billing receipts.

History stores only the authenticated owner, a client playback UUID, an opaque
track reference, lifecycle event and server acceptance timestamp. No media URL,
credential, title, artwork, external API metadata or elapsed-second claim is
stored. Track references deliberately have no catalogue FK: catalogue/demo IDs
can be recorded without importing metadata or blocking catalogue erasure.
History is private, not a public catalogue/analytics surface.

## Write contract

`POST /v1/plays/events` accepts JSON:

```json
{"playback_id":"40c325c0-2cee-489d-a8b6-fbe937155671","track_id":"track-1","event":"start"}
```

- `playback_id`: canonical lowercase UUID-shaped string generated once per
  playback; retries reuse it. It is not the server-issued meter `play_id`.
- `track_id`: nonempty opaque reference, at most 128 UTF-8 bytes, no controls.
- `event`: exactly `start`, `complete`, or `skip`; unknown fields are rejected.
- Browser mutations require a valid cookie session and exact configured Origin.
  Native REST credentials require `plays:write`; invalid presented bearers get
  the uniform 404, wrong-scope bearers get 403, never cookie rescue.
- A successful write or identical retry returns `200 {"ok":true}`. Immutable
  slots `(owner, playback_id, phase)` allow one start and one terminal event.
  A changed retry, second terminal kind, or terminal without its matching
  accepted start returns `422 validation_failed`. Retries do not refresh the
  original recorded timestamp. No client-supplied timestamp is trusted.
- A locking user-row fence rejects writes after account deletion is scheduled
  (`409 account_deleting`); storage errors fail with 500, not a false success.
- Admission is limited to 120 events per authenticated owner per 60 seconds
  (429 with `Retry-After: 60`). The existing 32 KiB request cap also applies.

Both existing browser audio elements (library and demo) use the same observer.
`playing`, not play intent, starts history. Pause/resume and buffering retain
identity. Natural end completes; replacing/clearing a started source or disposing
its element skips. Repeat/replay creates a new UUID. Failed-before-playing
attempts generate no history. Anonymous demo audio remains free and playable;
a 401 stops telemetry temporarily, never starts a signup/paywall flow.

Transport is best effort: bounded in-memory requests/retries, ordered start
before terminal, no local storage/offline queue. Authentication transitions drop
pending work. Network failure, process termination, browser unload or missing
client lifecycle events can leave a start without a terminal, or no history.
Do not infer complete listens from starts, fill missing endings, or treat these
observations as meter receipts. No native application or official video embed
is implemented in this repository; future playback producers must use the same
seam and their own authorized event integration.

## Export and erasure reachability

`POST /v1/plays/export` takes `{}` for the first page, then
`{"before_id":"<next_cursor>"}`. A cookie session plus exact Origin is required;
`plays:write` does not grant export access. Responses are:

```json
{"events":[],"next_cursor":null,"retention":"account_lifetime"}
```

Each event has `id`, `playback_id`, `track_id`, `event`, `recorded_at` (Unix
seconds). IDs/cursors are decimal strings to avoid JavaScript integer rounding.
Pages contain at most 100 events in descending ID order, filtered on the
session owner. The cursor is exclusive and does not depend on row existence.
Clients follow `next_cursor` until null; no unbounded full-account read or
cross-owner cursor lookup is exposed. Export is a moving view, not a snapshot
across concurrent writes. API responses use the existing `Cache-Control:
no-store` policy. An account-export flow can page this seam without a separate
history permission or DB scan.

`DELETE /v1/plays/events` clears only the session owner's history and requires
the exact Origin and existing fresh-session gate. It never clears metering or
leases. This is a point-in-time clear, not an opt-out: later playback can record
new history. Clients should stop playback and discard pending telemetry before
clearing if they want a quiet boundary.

Account erasure additionally reaches history through the `user_id` foreign key
with `ON DELETE CASCADE`. The account-erasure executor may use
`listen_history::DELETE_SQL` before deleting the user; the cascade is a final
integrity safeguard, not a new account-deletion workflow. Scheduling the
existing account-deletion fence prevents late writes from repopulating it.
The user's 24-hour account-erasure grace policy is unchanged.

## Migration and verification

`0037_listen_history.sql` adds the owner-scoped table/index only; no backfill,
meter migration, credentials, external service or schema apply is included.
Use the existing tracked migration/receipt process before staging deployment;
never hand-apply or edit an applied migration. Migration numbering must be
rechecked against main before merge if another migration lands. Rollback is
application rollback to the preceding deployed build; leave this additive table
intact until separately authorized data disposition. Removing this table loses
history and is not an automatic rollback step.

CI coverage includes handler authentication, cross-owner export/clear,
validation, throttling, immutable retries and unchanged grant boundaries;
Postgres tests execute the production insert statement for terminal ordering,
dedupe, deletion fence and user cascade. Browser/unit tests cover the shared
observer, sender and route wiring. Full suites run only on GitHub Actions;
these descriptions are coverage intent, not evidence that tests passed.

Before acceptance, record exact-head Actions run/job links and authorized
same-build staging evidence: sign in as a synthetic user, play on each existing
path, pause/resume, skip, and reach natural end/repeat; export should show one
start and one appropriate terminal per UUID. Repeat as another user and verify
isolation. Verify duplicate deliveries, export paging, explicit clear and
account-erasure reachability in disposable/authorized test fixtures. Check the
staging ingest-to-play-to-meter journey remains unchanged. No staging mutation
or live-provider/DB testing is authorized by this document.

Guardrails: [AGENTS.md — Clean-room, YouTube and MetaBrainz
guardrails](../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails) apply.
No video extraction, hidden playback, new metadata ingestion, artwork caching or
competitor-derived assets/styles/copy are introduced. REA angle: reuse the
existing approved contracts and playback lifecycle; no new analysis dossier.
