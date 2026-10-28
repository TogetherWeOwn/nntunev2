# Worker logging standard (target contract)

This document defines the worker-logging contract implemented by `crates/nntune-logging`. The `api`, `ingest`, and `mb` workers emit these records and propagate trace IDs end to end (the `api` fetch entry resolves inbound `x-trace-id` and emits a per-request served line; `ingest` carries the trace across routes, queue messages, and consumer); `stream` threads the trace through the play path with a hand-written mirror renderer in `trace.rs` that agrees on the same six-key contract (see verification note below; the renderer is not yet called outside tests) while keeping its Analytics Engine row shape. The MUST requirements below apply to every worker that adopts this standard.

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

## Verification (verified 2026-10-22)

Cross-checked against main on 2026-10-22; every citation below was re-verified against the current tree on that date (no revision pin: branch-head SHAs orphan on squash-merge, and the citation lint now guards drift). This section records evidence only and changes no emitter, schema, matcher, or binding behavior.

Adoption claims:

- `ingest` emits via `LogRecord::new(LogWorker::Ingest, …)` and propagates end to end: `inbound_trace_id` / `emit` / `with_trace` (`workers/ingest/src/lib.rs:130,174,194`), `trace_id` carried on queue messages (`workers/ingest/src/queue.rs:67`, `None` for pre-trace messages), and consumer re-resolve plus emit (`workers/ingest/src/queue_consumer.rs:2094-2105`). Every fetch line (search received, payload store_failed, upload received, status_checked) plus the consumer line carries `build_sha` from the public `BUILD_SHA` var or explicit `"unknown"` (`build_sha_from_env`, `workers/ingest/src/lib.rs:158`, template in `workers/ingest/wrangler.toml.example`).
- `mb` emits via `LogRecord::new(LogWorker::Mb, …)` (`workers/mb/src/lib.rs:1202-1206`) with `inbound_trace_id` (`:1160`) and an `x-trace-id` echo on served responses (`with_trace`, `:1224-1229`); edge-cache hits still echo the trace and log one line (`lookup_route`, `:1561`). Every line (three routes × 2, detached `mb.db.connect_failed` at `:1323`, catch-all `mb.request.not_found` at `:1715`) carries `build_sha` from the public `BUILD_SHA` var or explicit `"unknown"` (`build_sha_from_env`, `:1188`, template in `workers/mb/wrangler.toml.example`).
- `stream` threads the trace through the play path: `PlayInput::trace_id` (`workers/stream/src/play.rs:229`), normalize-before-observe (`:518-521`), `PlayStream::trace_id` (`:314`), orphan carry (`:330-336`, `trace_id()` at `:1239-1240`), and `OrphanedPlay::trace_id` (`workers/stream/src/pool.rs:63`). The envelope is rendered by a hand-written renderer (`render_log_line`, `workers/stream/src/trace.rs:401-430`, not yet called outside tests). Build identity is renderer-ready: `normalize_build_sha` (`:101-106`), `is_build_sha_known` (`:87-92`), and `BUILD_SHA_UNKNOWN` (`:81`) mirror `parse_build_sha` (`crates/nntune-logging/src/lib.rs:201-206`), `is_build_sha_known` (`:211-216`), and `BUILD_SHA_UNKNOWN` (`:194`), pinned by `build_sha_normalizes_to_known_or_unknown` (`trace.rs:478`) and `rendered_line_carries_build_sha_literally` (`:514`); no Worker entry point calls them yet — emission lands with the deploy-config slice; the `stream` crate does not depend on `nntune-logging` (it already uses `serde`/`serde_json` for `body.rs`/`probe.rs`), and only `trace.rs` itself renders by hand — the two renderers agree on the contract instead of sharing code. No code in `workers/stream` constructs `nntune_logging::LogRecord::new(Worker::Stream, …)` yet; only the mirror renderer and the trace plumbing above exist.
- `api` emits via `LogRecord::new(LogWorker::Api, …)` from the fetch entry (`emit_record`, `workers/api/src/worker_entry.rs:2476`): `dispatch` (`:2501`) resolves the inbound `x-trace-id` (`:2504-2509`), logs one `api.request.served` line per request with route/status/latency plus the registry-code `error_class` fallback (`:2605-2624`, mapping at `:2405`, path sentinel at `:2427`), and covers startup misconfiguration (`api.start.misconfigured`, `:2520`) and database failures (`api.db.connection_error`, `:621`). The emitter is re-exported (`pub use nntune_logging as logging`, `workers/api/src/lib.rs:41`) with unit coverage (`logging_tests`, `:44-106`). Per-worker conformance suites plus golden fixtures exist at `workers/{api,ingest,mb,stream}/tests/logging_standard.rs` and `workers/{api,ingest,mb,stream}/tests/fixtures/logging_golden.json`.
- The worker enum is exactly `api | ingest | mb | stream` (`Worker::as_str` / `Worker::parse`, `crates/nntune-logging/src/lib.rs:30-60`); `parse("repair")` is `None` (pinned at `:881`). Repair is the `nntune-repair` crate (`workers/repair/Cargo.toml:2`), invoked by ingest through the `REPAIR_CONTAINER` service binding fed by verify's staged inputs (`workers/ingest/src/repair.rs:21-27`), with no logging-crate usage — the scope note holds.

Envelope, propagation, and redaction:

- The six keys render in `LogRecord::to_json_line` (`crates/nntune-logging/src/lib.rs:507-530`): `ts` via `unix_millis_to_rfc3339` (`:222`), `level` (`Level`, `:65`), `worker` (`Worker`, `:30`), `trace_id` (`is_valid_trace_id`, `:142`), `event` (`is_valid_event_for_worker`, `:260-284`; the stream legacy no-dot rule is mirrored in `workers/stream/src/trace.rs:112-120`, vocabulary at `:39-52`), `fields` (`fields_from_json` plus `validate_fields`, `:452,:423-447`).
- Propagation rules 1–4 map to `resolve_trace_id` (`crates/nntune-logging/src/lib.rs:168`), the `with_trace` echo (ingest `:194-199`, mb `:1224-1229`), the queue carry (`workers/ingest/src/queue.rs:67`, `workers/ingest/src/queue_consumer.rs:2094-2095`), and constructor rejection of invalid IDs (`LogRecord::new`, `:487-489`).
- Redaction maps to `BANNED_KEY_PARTS` / `BANNED_NAME_PARTS` / `is_banned_field_key` (`:286,:324,:341`), `looks_like_secret_value` / `looks_like_email_value` / `looks_like_ipv4_value` / `looks_like_ipv6_value` (`:378,:401,:391,:416`), the `[REDACTED]` marker (`REDACTED`, `:26`), and construction-time refusal (`validate_fields`, `:423-447`). Matcher agreement is pinned by `banned_key_lists_agree_with_shared_validator` in the api/ingest/mb suites (`workers/api/tests/logging_standard.rs:898`; ingest/mb `:887`) and by the stream mirror matchers (`workers/stream/src/trace.rs:143-255`).
- `CacheObserveEvent::to_json_log` carries exactly `request_id`, `user_hash`, `cache_status`, `ttft_ms`, `bytes`, `pool_error_code` (`workers/stream/src/observability.rs:147`); the Analytics Engine mapping is unchanged (`analytics_blobs` / `analytics_doubles` / `analytics_index`), and the trace never enters the AE row (`observe_event` builds the row without it, `workers/stream/src/play.rs:1255-1264`).
- The `stream` example above is byte-identical to the stream golden fixture (`workers/stream/tests/fixtures/logging_golden.json`); the api/ingest/mb examples likewise match their golden fixtures. The stream conformance header states the fixture event is illustrative, not a claim about deployed Analytics Engine output (`workers/stream/tests/logging_standard.rs:1-12`).
