# Worker logging standard (target contract)

This document defines the worker-logging contract implemented by `crates/nntune-logging`. The `ingest` and `mb` workers emit these records and propagate trace IDs end to end (routes, queue messages, consumer); `stream` threads the trace through the play path with the same envelope renderer while keeping its Analytics Engine row shape; the `api` fetch-boundary wiring is the remaining follow-up — `api` exposes the emitter with conformance tests until then. The MUST requirements below apply to every worker that adopts this standard.

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
