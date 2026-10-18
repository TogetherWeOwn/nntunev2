# API error taxonomy

The `api`, `ingest`, `mb`, and `stream` crates define mappings to the same
JSON error envelope. In this slice, `ingest` and `mb` serialize the envelope
on their wired HTTP error paths. The API emits `reauth_required` from
privilege-gated vault mutations and `account_deleting` when a vault save is
refused by the account-deletion fence; other API and stream mappings are pinned
for future HTTP wiring, not emitted by reachable HTTP paths here. Clients can
handle auth, validation, billing limits, and upstream failures without
per-worker special cases.

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
  correlated), but the three envelope fields are always present. NZB intake
  refusals use `invalid_request` or `payload_too_large` as the registry `code`
  and preserve the stable intake-specific identifier in `intake_code`. During
  migration, these responses also retain `error` as an alias of `message`.

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
402, 403, 404, 409, 413, 416, 422, 429, 503); all other statuses use
`request_failed`. The web client continues to support these legacy bodies
during migration. `provider_auth_expired` is also a registered structured code
so the stream worker can preserve its inline credential-refresh signal in the
shared `{code, message, retryable}` envelope. The separate
`indexer_auth_expired` legacy signal remains outside this registry and is
preserved for the indexer re-key flow.

## Registry

`retryable` is `true` or `false`. Owning worker lists every worker that may
emit the code.

| code | HTTP status | retryable | owning worker | meaning |
| --- | --- | --- | --- | --- |
| `unauthorized` | 401 | false | api, stream | Missing, invalid, or expired credentials/session/grant. Deliberately generic: never says which check failed. |
| `reauth_required` | 401 | false | api | Session is valid for ordinary operations but too old for a vault credential change; complete a fresh authentication proof. |
| `provider_auth_expired` | 401 | false | api, stream | Provider credentials expired; prompts inline credential refresh rather than signing the user out. |
| `plan_limit_exceeded` | 402 | false | api, ingest, stream | The plan quota (streams, library, ingest jobs) is exhausted; upgrade or wait for the period reset. Defined ahead of server-side enforcement so clients can build the upgrade path now. |
| `forbidden` | 403 | false | api, stream | Authenticated, but the resource belongs to someone else (or the grant names a different track). |
| `not_found` | 404 | false | api, ingest, mb, stream | No such (visible) resource. Invisible private rows are 404, never 403. |
| `conflict` | 409 | false | ingest | The request is valid but the resource already exists in a state that refuses it (for example re-posting an ingest job whose previous run failed inside its replay window). Nothing was accepted; inspect the existing resource instead of retrying unchanged. |
| `account_deleting` | 409 | false | api | The caller's account is being deleted (`users.deleting_at` is set), so the write is fenced off (ADR 0002 rule 3). Nothing was written; do not retry unchanged. |
| `invalid_request` | 400 | false | ingest, mb, stream | Malformed body, bad identifier shape, or out-of-range parameter. |
| `payload_too_large` | 413 | false | ingest | Body, NZB, or element counts exceed the intake caps. |
| `range_not_satisfiable` | 416 | false | stream | The requested byte range lies outside the track. |
| `validation_failed` | 422 | false | api | Well-formed request whose values fail validation (names, items, provider config). |
| `rate_limited` | 429 | true | api, ingest, stream | Too many requests or no pool connection free right now; retry after backoff. |
| `internal_error` | 500 | true | api, ingest, mb, stream | Generic unexpected failure, including the unknown-code fallback. |
| `corrupt_content` | 502 | false | stream | A fetched segment could not be decoded; retrying the same bytes will not help. |
| `upstream_unavailable` | 502 | true | ingest, stream | A provider/indexer connection failed or an upstream request failed. |
| `upstream_circuit_open` | 503 | true | stream | The user's provider pool circuit breaker is open; retry after the supplied `Retry-After` delay. |
| `service_unavailable` | 503 | true | api, ingest | A backing binding (queue, payload store) is unavailable; nothing was accepted. |

## Repair taxonomy status (no registry rows)

The repair worker defines its own stable fault shape in
`workers/repair/src/contract.rs` (`ApiFault { status, code }`,
`ApiFault::new`/`invalid`): an HTTP status plus a machine-readable code
for logs and operators, carrying no user data. Ingest maps those faults
in `workers/ingest/src/repair.rs` (`classify_repair_reply`): replies
with status 400, 404, 409, or 413 are `Unrepairable` (permanent — a
retry would send the same bytes, so the job fails terminally), provided
the reply body does not exceed `MAX_REPAIR_REPLY_BYTES` (oversized
bodies are `Retry` before the status is checked); a 200
reply is decided by its `status` field (`repaired` -> `Repaired`,
`unrepairable` -> `Unrepairable` which is also terminal, anything else
or a malformed or oversized body -> `Retry`); every other (non-200)
status is `Retry` (transient — retried through the shared
job attempt budget). Container replies are classified in
`workers/repair/src/contract.rs` (`classify_container_reply`,
`ContainerOutcome::Forward`) and then narrowed in
`workers/repair/src/runtime.rs` (`leak_code`): only a well-formed 200
result completes the repair worker's request (`Done` — persist outputs
and reply 200); on 400/404/409/413/503 a container `error` code of at
most 64 lowercase/digit/underscore characters is passed through
`leak_code`, a closed allow-list (`invalid_request`,
`invalid_manifest`, `not_found`, `payload_too_large`,
`repair_manifest_conflict`, `repair_timeout`, `repair_resource`,
`repair_failed`, `scratch_input_mismatch`). Listed codes reach ingest
unchanged with the container's status; any other code becomes
`container_unavailable` with that same status (a container 409
`some_new_code` reaches ingest as 409 `container_unavailable`). A
missing or unparseable code, or a status outside that list, becomes 502
`container_unavailable`. Six of the listed codes
(`invalid_manifest`, `repair_manifest_conflict`, `repair_timeout`,
`repair_resource`, `repair_failed`, `scratch_input_mismatch`) are
repair-only with no registry row — the unregistered repair taxonomy
this section records.

This registry currently carries no repair rows: every row in the table
above names an owning worker drawn from api, ingest, mb, and stream,
and none names repair. That is a status fact, not a registry change.
Two qualifications apply to that fact. Repair replies use a bare
`{"error": <code>}` body (`workers/repair/src/runtime.rs`,
`fault_response`), not the `{code, message, retryable}` envelope, and
already reuse the registered strings `invalid_request`,
`payload_too_large`, and `not_found` without being listed as an owning
worker on those rows — so the `Owning worker lists every worker that
may emit the code` header is incomplete for those three codes. There is
also no repair-side `api_error` registry or doc-lint test
(`workers/repair/src` holds only `contract.rs`, `lib.rs`, `runtime.rs`;
the `api_error` registries live under api, ingest, mb, and stream), so
step 2 of `Adding a code` and its drift guard cannot be followed for
repair as written.

A future repair row follows the same `Adding a code` rule below once
those prerequisites exist: one registry row here (code, status,
retryable, owning worker `repair`) plus the same entry in a repair-side
error registry covered by a doc-lint test (or an explicit exemption
recorded here), plus a `leak_code` arm in
`workers/repair/src/runtime.rs` for any container-origin code that must
reach the wire (unlisted codes are renamed `container_unavailable`);
never rename or reuse a code, and never change a code's status or
retryable flag in place. No such row is added by this note.

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

### Limit refusal wire (enforcement contract)

When enforcement refuses an over-limit attempt, the refusal carries the
limit facts a client needs to render the upgrade path instead of a
dead-end error — asserted end to end by the over-limit e2e legs
(`assertPlanLimitRefusal` in `e2e/helpers/contract.ts`, pinned offline in
`e2e/tests/helpers.spec.ts`):

- Headers: a header naming the limit (preferred `x-plan-limit`) and a
  header naming the reset or retry (preferred `x-plan-reset`; `retry-after`
  satisfies the retry side and stays required on 429). Both carry values.
- Body: the envelope above plus an upgrade pointer (preferred
  `upgrade_url` with value `/pricing`, the client's pricing route), so any
  client can route the upgrade CTA without hardcoding it:

```json
{
  "code": "plan_limit_exceeded",
  "message": "plan limit reached",
  "retryable": false,
  "upgrade_url": "/pricing"
}
```

Refusal bodies stay names-only: no account, credential, or provider
material — the e2e legs fail a refusal that echoes the capped account's
email/session or carries credential-shaped keys.

## Adding a code

1. Add a registry row here (code, status, retryable, owning worker).
2. Add the same entry to the owning worker's `api_error` registry. The
   per-worker doc-lint tests fail if code, status, or retryable drift
   between this table and the worker.
3. Never rename or reuse an existing code, and never change a code's
   status or retryable flag in place — add a new code instead.
