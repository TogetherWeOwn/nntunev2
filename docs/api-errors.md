# API error taxonomy

The `api`, `ingest`, `mb`, and `stream` crates define mappings to the same
JSON error envelope. In this slice, `ingest` and `mb` serialize the envelope
on their wired HTTP error paths, including ingest's search-throttle 429,
which carries `{code: rate_limited, message, retryable: true}` with
`Retry-After` (`search_rate_limited_body` in
`workers/ingest/src/rate_limit.rs`, served by
`search_rate_limited_response` in `workers/ingest/src/lib.rs`). The API emits
`reauth_required` from privilege-gated vault mutations (`vault_put` and
`vault_delete` via `authenticate_privileged`) and `account_deleting` when a
vault write is refused by the account-deletion fence (the `vault_put` save and
the test-connection lease path); a misconfigured session service additionally
surfaces as the `internal_error` envelope via `session_error_response`
(`workers/api/src/handlers.rs`). The API's auth-throttle 429 likewise
carries `{code: rate_limited, message, retryable: true}` with `Retry-After`
(`throttled` in `workers/api/src/handlers.rs`, rendering
`ApiError::rate_limited`). Other API and stream mappings are pinned
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
| `invalid_request` | 400 | false | api, ingest, mb, stream | Malformed body, bad identifier shape, or out-of-range parameter. |
| `MB_SEARCH_EMPTY_QUERY` | 400 | false | mb | Empty or whitespace-only `q` on `GET /v1/mb/search`; returned before `type` validation without touching the mirror or the cache. Send a non-empty `q` of at least 2 chars. |
| `payload_too_large` | 413 | false | ingest | Body, NZB, or element counts exceed the intake caps. |
| `range_not_satisfiable` | 416 | false | stream | The requested byte range lies outside the track. |
| `validation_failed` | 422 | false | api | Well-formed request whose values fail validation (names, items, provider config). |
| `rate_limited` | 429 | true | api, ingest, stream | Too many requests or no pool connection free right now; retry after backoff. |
| `internal_error` | 500 | true | api, ingest, mb, stream | Generic unexpected failure, including the unknown-code fallback. |
| `corrupt_content` | 502 | false | stream | A fetched segment could not be decoded; retrying the same bytes will not help. |
| `upstream_unavailable` | 502 | true | ingest, stream | A provider/indexer connection failed or an upstream request failed. |
| `upstream_circuit_open` | 503 | true | stream | The user's provider pool circuit breaker is open; retry after the supplied `Retry-After` delay. |
| `service_unavailable` | 503 | true | api, ingest | A backing binding (queue, payload store) is unavailable; nothing was accepted. |

## Pool error-wire codes (stream NntpPool → ingest, Lane::Ingest)

The stream `NntpPool` Durable Object serves the ingest pipeline on
`Lane::Ingest` (`POST /probe` in `workers/stream/src/probe.rs`, `POST /body`
in `workers/stream/src/body.rs`) with legacy `{"error": <code>}` bodies, not
the `{code, message, retryable}` envelope above. These seven codes are the
complete served set; they intentionally carry no `api_error` registry entry
(no prod-code change on this card), so the envelope doc-lint tests
(`registry_matches_docs_api_errors`, `docs_registry_matches_all_worker_registries`)
ignore this table by design: its rows use bare codes, never the `| \``
envelope-registry prefix. Ingest consumes them through `classify_reply` in
`workers/ingest/src/pool_probe.rs:69-99` (400/413/401/403 are `Rejected`,
everything else is `Retry` with clamped `Retry-After`; a 401/403's
`Retry-After` is deliberately unread) and `classify_body_reply` in
`workers/ingest/src/pool_body.rs:83-118` (exact `{"error":"missing"}` 404 is
`Missing` data, 413 is `TooLarge` data, 400/401/403 are `Rejected`,
everything else is `Retry` with clamped `Retry-After`, including non-exact
404s at the default delay). The pool wire never emits 401/403 today (auth
collapses to 502 `provider_error`); the 401/403 arms fail closed so a
future auth refusal does not burn all 5 job attempts plus DLQ. `retryable`
below mirrors that ingest verdict: 4xx request/data answers are `false`,
redeliverable 5xx answers are `true`.

| code | HTTP status | retryable | owning lane | meaning | served by |
| --- | --- | --- | --- | --- | --- |
| bad_request | 400 | false | Ingest | Malformed envelope: bad JSON, batch size, ids, wrong user, unreadable body, or owner-less instance. Ingest treats it as `Rejected`. | probe.rs:111 `ProbeError::BadRequest`, status probe.rs:100; parse probe.rs:185-208; capped read runtime.rs:188-200 maps unreadable/over-cap to `BadRequest`/`TooLarge`; route OWNER_MISSING routes.rs:33-35 served routes.rs:75 and runtime.rs:84-86 |
| too_large | 413 | false | Ingest | Body over `MAX_PROBE_BODY_BYTES` (probe) / `MAX_BODY_REQUEST_BYTES` / `MAX_BODY_ARTICLE_BYTES` (body). Ingest treats probe 413 as `Rejected` and body 413 as `TooLarge` data. | probe.rs:112, status probe.rs:101; body.rs:233/275; capped reads runtime.rs:194-196 and runtime.rs:130-133 |
| busy | 503 | true | Ingest | Ingest lane full (`PoolError::Exhausted`) or pool quarantined (`PoolError::Quarantined`); carries `Retry-After` (`retry_after_secs`, at least 1 s). Ingest redelivers after the hint. | probe.rs:113, status probe.rs:102; run_probe probe.rs:280-285, run_body body.rs:194-199; hint LANE_FULL_RETRY_MS probe.rs:57, clamp pool_probe.rs:87-89 |
| provider_unavailable | 503 | true | Ingest | The user's provider settings could not be loaded (pre-wiring `provider()` stub). Ingest retries after `DEFAULT_RETRY_SECS`. | probe.rs:114, status probe.rs:102; runtime.rs:95-103 and runtime.rs:173-175; pre-wiring route PROVIDER_UNAVAILABLE routes.rs:37-39 served routes.rs:76 |
| provider_error | 502 | true | Ingest | Provider failure (connection, auth, protocol): the pool is quarantined, the connection dropped, provider text never leaves the module. Ingest retries through the job attempt budget. | probe.rs:115, status probe.rs:103; provider_failed probe.rs:357-367 called from run_probe probe.rs:316/322/342 and run_body body.rs:247/283 |
| missing | 404 | false | Ingest | Provider answered `430`: the article is gone. Data for the par2 block count, not an error; ingest folds it as `Missing`, never a retry. | body.rs:85-90 `BodyOutcome::Missing`; fetched body.rs:159-162 |
| internal | 500 | true | Ingest | `ProbeOutcome` serialization failure (infallible for `Vec<String>` outcomes; fail-closed generic). Ingest classifies any other 5xx as `Retry`. | probe.rs:148-152 `ProbeReply::from` `Err` arm on `serde_json::to_string` failure |

## Repair taxonomy status (no registry rows)

The repair worker defines its own stable fault shape in
`workers/repair/src/contract.rs` (`ApiFault { status, code }`,
`ApiFault::new`/`invalid`): an HTTP status plus a machine-readable code
for logs and operators, carrying no user data. Ingest maps those faults
in `workers/ingest/src/repair.rs` (`classify_repair_reply`): replies
with status 400, 401, 403, 404, 409, or 413 are `Unrepairable`
(permanent — a retry would send the same bytes, so the job fails
terminally; the 401/403 arms are auth parity with the pool
classifiers, since a refusal of the repair caller can never be fixed
by redelivery), provided
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
container-origin repair-only codes with no registry row (two of the
six — `invalid_manifest` and `scratch_input_mismatch` — are also
emitted directly by the repair worker itself:
`workers/repair/src/contract.rs:123,129` and
`workers/repair/src/runtime.rs:358,429`). The repair worker also emits
`container_unavailable` (502), `scratch_unavailable` (503),
`repair_unavailable` (500/503), and `scratch_missing` (404) directly
(not through `leak_code`); those have no registry row either.

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

## Sources

Each rule above traces to these worker sources (verified at the current
head; the per-worker `registry_matches_docs_api_errors` tests fail on
code, status, or retryable drift):

- Envelope and fallback: `workers/api/src/api_error.rs`,
  `workers/ingest/src/api_error.rs`, `workers/mb/src/api_error.rs`,
  `workers/stream/src/api_error.rs` (`ApiError::body_json`, `by_code`,
  `spec`/`FALLBACK`), pinned by each module's
  `envelope_is_exactly_code_message_retryable`,
  `server_error_messages_are_sealed_to_public_text` (stream:
  `registered_server_errors_use_fixed_public_messages`; mb: sealing is
  asserted inside `envelope_is_exactly_code_message_retryable`), and
  `unknown_code_falls_back_to_the_documented_generic` tests.
- Intake refusals and ingest extras: `workers/ingest/src/nzb_intake.rs`
  (intake codes and caps), `intake_rejection_parts` and `queued_body` in
  `workers/ingest/src/lib.rs`, and the search-throttle 429 envelope in
  `workers/ingest/src/rate_limit.rs`.
- API and stream wiring scope: `authenticate_privileged`,
  `account_deleting_response`, `throttled` (the auth-throttle 429
  envelope), and `session_error_response` in
  `workers/api/src/handlers.rs`; stream serves pre-wiring bodies from
  `workers/stream/src/routes.rs` while `PlayError::api_error` in
  `workers/stream/src/play.rs` is covered by tests only.
- Client fallback: `apps/web/src/lib/api/errors.ts`
  (`KNOWN_ERROR_CODES`, `codeFromStatus`, `parseErrorBody`); the
  provider/indexer signals in `workers/api/src/vault.rs`
  (`PROVIDER_AUTH_EXPIRED_CODE`) and `workers/ingest/src/newznab.rs`
  (`INDEXER_AUTH_EXPIRED_CODE`).
- Limit refusal: `assertPlanLimitRefusal` in `e2e/helpers/contract.ts`,
  pinned offline in `e2e/tests/helpers.spec.ts`.
- Repair: `workers/repair/src/contract.rs`,
  `workers/repair/src/runtime.rs`, and `classify_repair_reply` in
  `workers/ingest/src/repair.rs`.
- Pool error-wire codes: `ProbeError::status`/`code` and `ProbeReply::from`
  in `workers/stream/src/probe.rs:98-161`, `BodyReply::from` in
  `workers/stream/src/body.rs:73-101`, route constants and decisions in
  `workers/stream/src/routes.rs:29-107`, pool fetch in
  `workers/stream/src/runtime.rs:79-206`, consumed by `classify_reply` in
  `workers/ingest/src/pool_probe.rs:69-99` and `classify_body_reply` in
  `workers/ingest/src/pool_body.rs:83-118`, pinned by
  `workers/stream/tests/pool_error_wire_codes.rs` (served set ⊆ pool table).

## Adding a code

1. Add a registry row here (code, status, retryable, owning worker).
2. Add the same entry to the owning worker's `api_error` registry. The
   per-worker doc-lint tests fail if code, status, or retryable drift
   between this table and the worker.
3. Never rename or reuse an existing code, and never change a code's
   status or retryable flag in place — add a new code instead.
