# Listen history

## Retention and data boundary

**Listen history is retained for the life of the account unless the user clears
it first.** There is no age-based expiry or background history purge. Final
account erasure deletes the history via `listen_events.user_id ON DELETE CASCADE`.
Scheduling account deletion prevents new history writes during the existing grace
period; it does not change that period or create a new erasure workflow.

These records are client-reported playback observations, not billing receipts.
They cannot authorize playback, renew a lease, or prove seconds heard. `POST
/v1/plays` still issues grants, and the existing lease, heartbeat and meter
contracts are unchanged. History stores the owner, an opaque track reference,
a playback UUID, the event kind and server acceptance time. It does not collect
media URLs, provider accounts, credentials, titles or artwork.

## Recording

`POST /v1/plays/events` accepts a canonical lowercase `playback_id` UUID, an
opaque `track_id` (1–128 UTF-8 bytes with no control characters), and `event`
(`start`, `complete` or `skip`). Browser requests also send `context` from
`POST /v1/plays/events/context`, bound to the verified session. Both browser
endpoints require the configured Origin and a valid cookie session. Native
REST writes require `plays:write` and omit `context`.

Encode the 16 bytes of the server-issued grant `play_id` as `playback_id`
using lowercase UUID formatting without changing any bits. The first `start`
requires a live lease for that account, play and track. A play marked private
at grant time is never recorded, even if private listening is later switched
off. Observations without a matching grant identity are not admitted.

Send `start` when audio actually plays, not on click. Pause and buffering
preserve the play identity. Natural end is `complete`; abandoning started
playback is `skip`. A new grant play (not a continuation) has a different ID.
Identical retries do not add rows or refresh timestamps. Terminals and retries
can refer to an accepted start after the lease changes. Ineligible or
conflicting observations, including a terminal without a matching start,
return 422. Delivery is best effort; a missing terminal is not evidence that
a listen completed. There is no official video embed producer in this build.

## Export and clear

`POST /v1/plays/export` with `{}` gives the first page. Pass the returned
`next_cursor` as `{"before_id":"<next_cursor>"}` for subsequent pages; stop
when it is null. Up to 100 events per page are returned newest-inserted first.
Each row includes `playback_id`, `track_id`, `event`, and `recorded_at` (Unix
seconds). The response includes `retention: "account_lifetime"`. Cursors are
opaque to clients, resolve only within the requesting account, and expose no
internal sequence number. A missing, cleared or foreign cursor anchor yields an
empty page; start again without a cursor if needed. Pages are not a snapshot
across concurrent writes or clears.

`DELETE /v1/plays/events` clears only the requesting account's history. It
requires a fresh session and returns `200 {"ok":true}` even if already empty.
Clear serializes with recording for that account; it does not stop playback,
revoke grants, erase billing or opt the account out of future history. Stop
playback and discard pending deliveries before clearing if a quiet boundary is
needed.

Both routes require a cookie session and exact Origin; a native bearer does not
grant either permission. Responses use `Cache-Control: no-store`. Export allows
120 requests per minute per account; clear allows six, both returning 429 with
`Retry-After: 60` when limited. The export endpoint is the history-only API;
it does not implement the separate whole-account [offline archive
format](account-export-format.md) or an export UI.

## Verification and rollout

The [Clean-room, YouTube and MetaBrainz guardrails](../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails)
apply. No provider telemetry, media extraction, artwork caching or catalogue
licence changes are introduced. Use the [tracked staging migration
process](ops.md#staging-migrations) before merge. Roll back the application by
redeploying the previous build; leave the additive history table intact.

Use synthetic accounts on the normal staging deployment. Record the deployed
merge SHA and run, confirm the playback lifecycle on the available audio paths,
page the export, verify owner isolation and clear, and verify that account
erasure reaches history. The real ingest → play → meter journey must remain
unchanged. Hosted fixtures test the code paths; they do not replace this
served-build smoke.
