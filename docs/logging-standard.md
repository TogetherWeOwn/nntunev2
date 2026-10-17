# Worker logging standard (target contract)

This document defines the target contract for a future worker-logging implementation. It does not claim that current production workers emit these records or propagate trace IDs; existing runtime logging remains unchanged by this documentation slice. The MUST requirements below apply when a worker adopts this standard.

Target worker log records MUST be newline-delimited JSON (one object per line). Each record includes the following shared fields:

| Field | Type | Requirement |
| --- | --- | --- |
| `ts` | string | RFC 3339 timestamp in UTC, ending in `Z`. |
| `level` | string | One of `debug`, `info`, `warn`, or `error`. |
| `worker` | string | Stable worker name: `api`, `ingest`, `mb`, or `stream`. |
| `trace_id` | string | 32 lowercase hexadecimal characters (128 bits). |
| `event` | string | Lowercase dotted event name. New `api`, `ingest`, and `mb` events are namespaced with their worker name (for example, `api.auth.login`). Existing `stream` event names remain unchanged. |
| `fields` | object | Flat map of string, number, boolean, or `null` values; no nested objects or arrays. |

When implemented, `api`, `ingest`, and `mb` records MUST use exactly these six top-level fields. This slice makes no change to `stream` production output. A future stream implementation must preserve its actual Analytics Engine payload shape and event set; the synthetic `stream` JSON example below is not an Analytics Engine record and does not assert that `stream.play.start` exists in production.

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
