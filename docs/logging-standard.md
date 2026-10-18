# Worker logging standard (target contract)

This document defines the worker-logging contract implemented by `crates/nntune-logging`. The `ingest` and `mb` workers emit these records and propagate trace IDs end to end (routes, queue messages, consumer); `stream` threads the trace through the play path through a dependency-free mirror renderer that agrees on the same six-key contract (see verification note below) while keeping its Analytics Engine row shape; the `api` fetch-boundary wiring is the remaining follow-up — `api` exposes the emitter with conformance tests until then. The MUST requirements below apply to every worker that adopts this standard.

Target worker log records MUST be newline-delimited JSON (one object per line). Each record includes the following shared fields:

| Field | Type | Requirement |
| --- | --- | --- |
| `ts` | string | RFC 3339 timestamp in UTC, ending in `Z`. |
| `level` | string | One of `debug`, `info`, `warn`, or `error`. |
| `worker` | string | Stable worker name: `api`, `ingest`, `mb`, or `stream`. (`workers/repair` exists on main but is out of scope for this slice; see scope note below.) |
| `trace_id` | string | 32 lowercase hexadecimal characters (128 bits). |
| `event` | string | Lowercase event name (`[a-z0-9_.]`, non-empty, no leading/trailing `.`/`_`, max 96 chars). New `api`, `ingest`, and `mb` events are namespaced with their worker name and contain a dot (for example, `api.auth.login`). Existing `stream` event vocabulary is preserved as-is (a dot is not required during migration). |
| `fields` | object | Flat map of string, number, boolean, or `null` values; no nested objects or arrays. |

When implemented, `api`, `ingest`, and `mb` records MUST use exactly these six top-level fields. `stream` records MUST carry these six fields as a superset (legacy extras allowed during migration): a future stream implementation adopts the six-key envelope and moves the existing observability keys (`request_id`, `user_hash`, `cache_status`, `ttft_ms`, `bytes`, `pool_error_code` from `CacheObserveEvent::to_json_log`) under `fields`, while the Analytics Engine row stays as-is (it is not a JSON log shape). The stream play path threads the trace through responses, streams, and orphaned plays; log-line emission from the Worker entry point lands with the deploy-config slice. The synthetic `stream` JSON example below illustrates the target envelope only; it is not an Analytics Engine record and does not assert that `stream.play.start` exists in production.

Scope: `workers/repair` (crate `nntune-repair`, called by ingest) is out of scope for this slice. A future extension will add `repair` to the worker enum with a `repair.*` event namespace plus a matching golden fixture and acceptance test; until then no `worker: "repair"` record is claimed conforming under this standard.

## Trace ID propagation

1. At an inbound HTTP boundary, accept `x-trace-id` only if it is exactly 32 lowercase hexadecimal characters. If absent or invalid, generate a new cryptographically random 128-bit ID and encode it as 32 lowercase hexadecimal characters. Never trust or log an invalid value.
2. Use that ID as the request's `trace_id` in every log line. Pass it unchanged to downstream HTTP requests in the `x-trace-id` header.
3. Put the same ID in queued work messages and preserve it through each consumer and any subsequent worker call. A worker receiving a valid ID MUST NOT replace it.
4. Do not use a trace ID as an authorization or user-identity value.

## Privacy and redaction

`fields` MUST contain only operational facts needed to diagnose the event. Opaque internal identifiers (for example, a generated job ID) are allowed; direct personal identifiers are not. Do not log passwords, secrets, tokens, API keys, cookies, authorization values, private keys, mnemonics, email addresses, IP addresses, names, postal addresses, payment details, session identifiers, request/response bodies, credentials, raw query strings, NZB contents, or complete headers. Do not add fields whose keys imply these values (including variants such as `api_key`, `grant_token`, or `cookie`).

If a value may contain a secret or personal data, omit it; if it must be represented for diagnosis, replace it with the fixed marker `[REDACTED]` before logging. Never serialize an untrusted value and rely on the log sink to redact it later. Golden fixtures must use synthetic, non-sensitive values only.

## Examples

Each example is one JSON log line; timestamps and trace IDs are static fixture values.

```json
{"ts":"2026-10-15T20:00:00.123Z","level":"info","worker":"api","trace_id":"9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c","event":"api.auth.login","fields":{"status_code":200,"duration_ms":12}}
{"ts":"2026-10-15T20:00:00.123Z","level":"info","worker":"ingest","trace_id":"9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c","event":"ingest.job.queued","fields":{"job_id":"job_01","stage":"queued"}}
{"ts":"2026-10-15T20:00:00.123Z","level":"info","worker":"mb","trace_id":"9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c","event":"mb.lookup.hit","fields":{"kind":"artist","cache":"hit"}}
{"ts":"2026-10-15T20:00:00.123Z","level":"info","worker":"stream","trace_id":"9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c","event":"stream.play.start","fields":{"track_id":"track-a","cache_hit":false}}
```

## Verification (verified 2026-10-18)

Cross-checked against main on 2026-10-18; line numbers are that revision. This section records evidence only and changes no emitter, schema, matcher, or binding behavior.

Adoption claims:

- `ingest` emits via `LogRecord::new(LogWorker::Ingest, …)` and propagates end to end: `inbound_trace_id` / `emit` / `with_trace` (`workers/ingest/src/lib.rs:126,139,159`), `trace_id` carried on queue messages (`workers/ingest/src/queue.rs:60`, `None` for pre-trace messages), and consumer re-resolve plus emit (`workers/ingest/src/queue_consumer.rs:1405-1412`).
- `mb` emits via `LogRecord::new(LogWorker::Mb, …)` (`workers/mb/src/lib.rs:589-595`) with `inbound_trace_id` (`:575`) and an `x-trace-id` echo on served responses (`with_trace`, `:616-619`); edge-cache hits still echo the trace and log one line (`lookup_route`).
- `stream` threads the trace through the play path: `PlayInput::trace_id` (`workers/stream/src/play.rs:201`), normalize-before-observe (`:483-486`), `PlayStream::trace_id` (`:229-232`), orphan carry (`:283-308`, `trace_id()` at `:1060-1065`), and `OrphanedPlay::trace_id` (`workers/stream/src/pool.rs:63`). The envelope is rendered by a dependency-free mirror (`render_log_line`, `workers/stream/src/trace.rs:368-397`); that crate stays free of `serde` and `nntune-logging` by design (`:4-13`), so "same renderer" was drift — the two renderers agree on the contract instead. No code in `workers/stream` constructs `nntune_logging::LogRecord::new(Worker::Stream, …)` yet; only the mirror renderer and the trace plumbing above exist.
- `api` exposes the emitter (`pub use nntune_logging as logging`, `workers/api/src/lib.rs:25`) with unit coverage (`logging_tests`, `:28-91`); the fetch entry (`worker_entry::dispatch`) carries no `x-trace-id` handling, so the fetch-boundary wiring remains the follow-up. Per-worker conformance suites plus golden fixtures exist at `workers/{api,ingest,mb,stream}/tests/logging_standard.rs` and `workers/{api,ingest,mb,stream}/tests/fixtures/logging_golden.json`.
- The worker enum is exactly `api | ingest | mb | stream` (`Worker::as_str` / `Worker::parse`, `crates/nntune-logging/src/lib.rs:30-58`); `parse("repair")` is `None` (pinned at `:811`). Repair is the `nntune-repair` crate (`workers/repair/Cargo.toml:2`), invoked by ingest through the `REPAIR_CONTAINER` service binding fed by verify's staged inputs (`workers/ingest/src/repair.rs:18-24`), with no logging-crate usage — the scope note holds.

Envelope, propagation, and redaction:

- The six keys render in `LogRecord::to_json_line` (`crates/nntune-logging/src/lib.rs:479-502`): `ts` via `unix_millis_to_rfc3339` (`:194`), `level` (`Level`, `:65`), `worker` (`Worker`, `:30`), `trace_id` (`is_valid_trace_id`, `:142`), `event` (`is_valid_event_for_worker`, `:232-256`; the stream legacy no-dot rule is mirrored in `workers/stream/src/trace.rs:79-87`, vocabulary at `:38-51`), `fields` (`fields_from_json` plus `validate_fields`, `:424,:395-419`).
- Propagation rules 1–4 map to `resolve_trace_id` (`crates/nntune-logging/src/lib.rs:168`), the `with_trace` echo (ingest `:159-162`, mb `:616-619`), the queue carry (`workers/ingest/src/queue.rs:60`, `workers/ingest/src/queue_consumer.rs:1405-1406`), and constructor rejection of invalid IDs (`LogRecord::new`, `:459-461`).
- Redaction maps to `BANNED_KEY_PARTS` / `BANNED_NAME_PARTS` / `is_banned_field_key` (`:258,:296,:313`), `looks_like_secret_value` / `looks_like_email_value` / `looks_like_ipv4_value` / `looks_like_ipv6_value` (`:350,:373,:363,:388`), the `[REDACTED]` marker (`REDACTED`, `:26`), and construction-time refusal (`validate_fields`, `:395-419`). Matcher agreement is pinned by `banned_key_lists_agree_with_shared_validator` in the api/ingest/mb suites (`workers/api/tests/logging_standard.rs:898`; ingest/mb `:887`) and by the stream mirror matchers (`workers/stream/src/trace.rs:110-228`).
- `CacheObserveEvent::to_json_log` carries exactly `request_id`, `user_hash`, `cache_status`, `ttft_ms`, `bytes`, `pool_error_code` (`workers/stream/src/observability.rs:146`); the Analytics Engine mapping is unchanged (`analytics_blobs` / `analytics_doubles` / `analytics_index`), and the trace never enters the AE row (`workers/stream/src/play.rs:283-286`).
- The `stream` example above is byte-identical to the stream golden fixture (`workers/stream/tests/fixtures/logging_golden.json`); the api/ingest/mb examples likewise match their golden fixtures. The stream conformance header states the fixture event is illustrative, not a claim about deployed Analytics Engine output (`workers/stream/tests/logging_standard.rs:1-12`).
