# Listen history

Listen events are retained for the life of the account unless the user clears
history first. Final account erasure removes the rows via foreign-key cascade.

- `POST /v1/plays/export` with `{}` returns the first page. Send `next_cursor`
  back as `{"before_id":"<next_cursor>"}`; stop when it is null. Pages contain
  up to 100 events, newest inserted first, and `retention: "account_lifetime"`.
  Rows contain `playback_id`, `track_id`, `event` (`start`, `complete` or `skip`),
  and `recorded_at` (Unix seconds). Treat cursors as opaque: they expose no
  internal sequence number and resolve only within the requesting account.
  Missing, cleared or foreign anchors return an empty page. Pagination is not
  a snapshot across concurrent writes or clears.
- `DELETE /v1/plays/events` requires a fresh session and clears only the
  requesting account's listen events. It returns `200 {"ok":true}` even when
  empty. Clear serializes with writes; it neither opts out of future recording
  nor changes playback, grants, leases, metered history, or billing.

Both routes require a cookie session and the exact configured Origin, reject
`Authorization`, and use `Cache-Control: no-store`. Export and clear have
separate per-account limits of 120 and six requests per minute respectively;
exhaustion returns `429` with `Retry-After: 60`.

These routes do not mount recording, change `/v1/me/history`, or provide a UI
or a [whole-account archive](account-export-format.md).
