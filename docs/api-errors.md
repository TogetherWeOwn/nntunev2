# API error taxonomy

The `api`, `ingest`, `mb`, and `stream` crates define mappings to the same
JSON error envelope. In this slice, `ingest` and `mb` serialize the envelope
on their wired HTTP error paths; `api` emits it for session configuration and
storage failures, while its other mappings and the `stream` mappings are
pinned for HTTP wiring as those paths are integrated. Clients can handle auth,
validation, billing limits, and upstream failures without per-worker special
cases.

## Envelope

```json
{ "code": "rate_limited", "message": "too many requests", "retryable": true }
```

- `code` — stable machine-readable identifier from the registry below.
  Clients must switch on `code`, never on `message` text.
- `message` — short user-facing text. It never carries internal detail
  (hosts, upstream replies, SQL, secrets) on auth, vault, or 5xx paths.
- `retryable` — whether retrying the same request may succeed without
  changing it. `true` means retry with backoff (honouring `Retry-After`
  when present); `false` means fix the request, credentials, or plan first.
- The HTTP status is the registry status for the `code`. Workers may add
  extra fields (for example `job_id` on an ingest 503 so a retry can be
  correlated), but the three envelope fields are always present.

## Fallback for unknown codes

Codes are only ever added, never renamed or reused. A client that receives
a code it does not know — and a worker asked to emit an unregistered code —
must fall back to the generic:

```json
{ "code": "internal_error", "message": "unexpected error", "retryable": true }
```

with HTTP status 500. The worker implementations seal 5xx messages to fixed public text in their
`by_code` lookup; caller-supplied details are never serialized on those paths. Clients map an unknown envelope
`code` to `request_failed`, keeping the server `message` and boolean
`retryable` for display/policy. Legacy `{"error": "..."}` bodies
(pre-taxonomy) derive their code from unambiguous HTTP statuses (400, 401,
402, 403, 404, 413, 416, 422, 429, 503); all other statuses use
`request_failed`. The web client continues to support these legacy bodies
during migration.

## Registry

`retryable` is `true` or `false`. Owning worker lists every worker that may
emit the code.

| code | HTTP status | retryable | owning worker | meaning |
| --- | --- | --- | --- | --- |
| `unauthorized` | 401 | false | api, stream | Missing, invalid, or expired credentials/session/grant. Deliberately generic: never says which check failed. |
| `plan_limit_exceeded` | 402 | false | api, ingest, stream | The plan quota (streams, library, ingest jobs) is exhausted; upgrade or wait for the period reset. Defined ahead of server-side enforcement so clients can build the upgrade path now. |
| `forbidden` | 403 | false | api, stream | Authenticated, but the resource belongs to someone else (or the grant names a different track). |
| `not_found` | 404 | false | api, ingest, mb, stream | No such (visible) resource. Invisible private rows are 404, never 403. |
| `invalid_request` | 400 | false | ingest, mb, stream | Malformed body, bad identifier shape, or out-of-range parameter. |
| `payload_too_large` | 413 | false | ingest | Body, NZB, or element counts exceed the intake caps. |
| `range_not_satisfiable` | 416 | false | stream | The requested byte range lies outside the track. |
| `validation_failed` | 422 | false | api | Well-formed request whose values fail validation (names, items, provider config). |
| `rate_limited` | 429 | true | api, ingest, stream | Too many requests or no pool connection free right now; retry after backoff. |
| `internal_error` | 500 | true | api, ingest, mb, stream | Generic unexpected failure, including the unknown-code fallback. |
| `corrupt_content` | 502 | false | stream | A fetched segment could not be decoded; retrying the same bytes will not help. |
| `upstream_unavailable` | 502 | true | ingest, stream | A provider/indexer connection failed or the pool is quarantined after provider errors. |
| `service_unavailable` | 503 | true | api, ingest | A backing binding (queue, payload store) is unavailable; nothing was accepted. |

## Billing-limit shapes (402 / 429)

Server-side limit enforcement lands separately, but the wire shapes are
pinned here and covered by per-worker tests so the client upgrade and
retry UX can be built against them today:

```json
{ "code": "plan_limit_exceeded", "message": "plan limit reached", "retryable": false }
```

with HTTP 402 when a plan quota is exhausted for the period, and

```json
{ "code": "rate_limited", "message": "too many requests", "retryable": true }
```

with HTTP 429 (and `Retry-After` where the worker knows a delay) for
short-window throttling. `402` is never retryable: only an upgrade or the
period reset changes the answer. `429` is always retryable.

## Adding a code

1. Add a registry row here (code, status, retryable, owning worker).
2. Add the same entry to the owning worker's `api_error` registry. The
   per-worker doc-lint tests fail if code, status, or retryable drift
   between this table and the worker.
3. Never rename or reuse an existing code, and never change a code's
   status or retryable flag in place — add a new code instead.
