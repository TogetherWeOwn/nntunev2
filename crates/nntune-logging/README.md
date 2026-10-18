# nntune-logging

Shared structured worker log emitter and trace-ID propagation helper.
Compiles for native targets **and** `wasm32-unknown-unknown` (Cloudflare
Workers): it takes no Worker runtime imports — callers pass a unix-millis
timestamp in and emit the rendered line through `console_log!` themselves.
(Sources: crate docs `src/lib.rs:1-11`.)

The normative contract lives in `../../docs/logging-standard.md`; this file
pins what the emitter guarantees and cites the code. Adoption — which
workers emit these records and where the wiring lands — is recorded in the
standard doc, not here, and is not duplicated below.

## Record shape

Each record renders as one newline-delimited JSON object with exactly the
six shared fields — no more, no fewer:

| Field | Type | Rule |
|---|---|---|
| `ts` | string | UTC timestamp rendered by the emitter (see timestamp boundary). |
| `level` | string | One of `debug`, `info`, `warn`, `error`. |
| `worker` | string | One of `api`, `ingest`, `mb`, `stream`. |
| `trace_id` | string | 32 lowercase hex chars (128 bits). |
| `event` | string | Worker-namespaced event name (legacy `stream` vocabulary excepted). |
| `fields` | object | Flat map of string, number, boolean, or `null`; no nesting. |

(Sources: record struct `src/lib.rs:439-446`, six-key render
`src/lib.rs:479-502`, levels `src/lib.rs:65-93`, worker names
`src/lib.rs:30-61`.)

- `level` parses only the four lowercase names; anything else is `None`,
  never coerced (`src/lib.rs:84-92`).
- `worker` wire names are fixed (`api`, `ingest`, `mb`, `stream`); `parse`
  is for config/CLI surfaces, never for trusting a caller-supplied label
  inside a record (`src/lib.rs:49-60`).
- `event` must be non-empty, at most 96 chars, lowercase `[a-z0-9_.]`
  with no leading/trailing `.`/`_`. `api`, `ingest`, and `mb` events
  additionally require their `<worker>.` namespace prefix; `stream`
  keeps its legacy vocabulary with no dot required during migration
  (`src/lib.rs:227-256`).
- `fields` must be a JSON object whose values are flat primitives only;
  nested objects and arrays are rejected (`src/lib.rs:395-419,424-433`).

## Trace-ID propagation

- The header name is `x-trace-id` (`TRACE_HEADER`, `src/lib.rs:21`). The
  same ID travels unchanged in downstream HTTP requests, queued messages,
  and consumers (`src/lib.rs:19-20`).
- A trace ID is exactly 32 lowercase hexadecimal characters (128 bits).
  Absent, short, long, uppercase, or non-hex values are invalid and must
  never be trusted or logged (`src/lib.rs:138-147`).
- At an inbound boundary, `resolve_trace_id` accepts the header value only
  when valid and otherwise generates a fresh ID; an invalid value is
  dropped, never trusted or logged (`src/lib.rs:164-173`).
- Fresh IDs come from `getrandom` (Web Crypto via the `js` feature on
  wasm targets, OS RNG natively) and encode as 32 lowercase hex chars
  (`src/lib.rs:149-162`).
- If entropy is unavailable, a last-resort `f`-prefixed ID keeps the
  handler total instead of panicking the isolate; the fixed prefix marks
  the weakness so a flood of them points at the RNG, and a process-local
  counter keeps IDs distinct on every target (`src/lib.rs:175-188`).
- A built record exposes its trace via `trace_id()` (and its event via
  `event()`) for downstream propagation (`src/lib.rs:504-514`).

## Privacy validation at construction

Validation runs at construction: `LogRecord::new` checks the trace ID,
the worker-namespaced event, and every field, so anything invalid is an
`Err`, never a log line (`src/lib.rs:435-474`).

- A `fields` key is banned when it names personal data or credentials:
  an `ip` path component (without flagging `ship_id`), a name component
  (`name`, `*_name`, known full-name variants), or any credential
  fragment from the banned list (`src/lib.rs:258-330`). The list
  deliberately has no bare `payment` entry so `payment_id` stays loggable
  (`src/lib.rs:283`).
- A string value is rejected when it opens with a known credential prefix
  or embeds a `key=value` credential marker; benign labels such as
  `task-queued` pass (`src/lib.rs:346-358`).
- Email-shaped values (`addr@domain.tld`) are rejected as personal data
  (`src/lib.rs:368-381`); dotted-quads of valid octets are rejected as
  IPv4 (`src/lib.rs:360-366`); hex-colon literals are rejected as IPv6
  while colon-joined operational refs (`nzb:sha256:…`, `12:00:00`) still
  pass (`src/lib.rs:383-393`).
- Failure variants name the offending key or event only — values are
  never echoed, so an error path cannot leak what it rejected
  (`src/lib.rs:95-134`).
- If a value must be represented for diagnosis but may hold secret or
  personal data, the caller substitutes the fixed `[REDACTED]` marker
  before logging; the emitter never invents redaction at the sink
  (`REDACTED`, `src/lib.rs:24-26`).

## Caller-supplied timestamp boundary

The emitter owns formatting, the caller owns time: `LogRecord::new`
takes `ts_millis` (unix millis — production workers pass their runtime
clock, tests pass fixed values) and renders an RFC 3339 UTC timestamp
ending in `Z` with millisecond precision (`2026-10-15T20:00:00.123Z`)
via pure civil-date math with no date library, so native and wasm
targets behave identically (`src/lib.rs:190-225,449-474`).

`to_json_line` returns one line with no trailing newline; the caller
joins lines (`src/lib.rs:476-479`).

## Checks

```sh
cargo test -p nntune-logging
cargo clippy -p nntune-logging --all-targets    # clean, `-D warnings` in CI
```

Link hygiene for this file:

```sh
python3 .github/scripts/docs_link_lint.py --root . --files crates/nntune-logging/README.md
```
