# Inert Subsonic staging fixtures

These recipes contain only authored synthetic data. The measurement mount
remains off by default. In a separately authorized window, the trusted
`SUBSONIC_MEASUREMENT_LEASE` appends `;` and a nonempty comma-separated
allowlist of case IDs to the existing v1 lease fields. The entire value is
bounded to 640 bytes. Unknown, duplicate or empty entries invalidate the whole
schedule. The SHA, expiry, exact HTTPS staging host and runner bearer checks
remain in force. The runner selects one listed case with the
`X-Subsonic-Measurement-Case` header; missing, malformed and unlisted selectors
on an armed mount receive the same fixed 404 as a missing runner bearer.
Fixture selection follows real auth against the dedicated binding and the
lease-principal match. No request can supply a recipe, ceiling or identity;
no case falls back to ordinary staging Hyperdrive or a catalogue database.

## Fixture-to-boundary map

Each triplet uses the suffixes `below`, `exact`, `over` in that order. Sizes are
uncompressed UTF-8 response bytes. `A` is artist ID
`00000000-0000-4000-8000-000000000001`; `R` is release ID
`00000000-0000-4000-8000-000000000002`. Request paths below are under `/rest/`.

| Fixture IDs | Boundary inputs | Request | Expected HTTP status / bytes |
| --- | --- | --- | --- |
| `rows-*` | 1,023 / 1,024 / 1,025 track rows (one artist and release) | `getArtist?id=A&f=json` | 200 / 492; 200 / 492; 503 / 112 |
| `field-*` (WASM only) | 1,023 / 1,024 / 1,025 title bytes | `getAlbum?id=R&f=json` | 200 / 1,991; 200 / 1,992; 503 / 112 |
| `text-*` | 262,143 / 262,144 / 262,145 raw field bytes across 258 rows | `getArtists?f=json` | 200 / 296; 200 / 296; 503 / 112 |
| `output-json-*` | 262,143 / 262,144 / 262,145 encoded bytes | `getAlbum?id=R&f=json` | 200 / 262,143; 200 / 262,144; 413 / 122 |
| `output-xml-*` | Same, including XML escaping | `getAlbum?id=R` | 200 / 262,143; 200 / 262,144; 413 / 130 |
| `output-jsonp-*` | Same, including wrapper and callback | `getAlbum?id=R&f=jsonp&callback=fixture.cb` | 200 / 262,143; 200 / 262,144; 413 / 122 (plain JSON) |
| `large-mixed` | One artist, four releases, 512 track occurrences; 256-byte UTF-8 titles with repeated escaping | First release via the three `getAlbum` requests above | 200 / 106,659 (JSON), 118,091 (XML), 106,676 (JSONP) |

`empty` and `small` are unchanged. The short `rows-*` and `text-*` projections
intentionally avoid the independent output cap while building the full snapshot.
Row/text/charge preflight runs before raw row reservation; the existing counted
renderer rejects over-cap output before allocating the response body.

## Runner crosswalk (not an executable schedule)

| Slots | Case selection and expected outcome |
| --- | --- |
| 01–06 | `empty` artist index, then `small` album in JSON, XML and JSONP (four complete 200s). Withhold the runner bearer for fixed 404; withhold Subsonic credentials but send the runner bearer and `small` for HTTP 200 with protocol error 40 and no catalogue. |
| 07–09 | `rows-{below,exact,over}` as above. |
| 10–12 | `charge-{below,exact,over}` as pinned below. |
| 13–15 | `text-{below,exact,over}` as above. |
| 16–24 | `output-{json,xml,jsonp}-{below,exact,over}`, in format order as above. |
| 25–36 | `large-mixed` first release, four samples per format using the body byte counts above. |
| 37–48 | Six `large-mixed` pairs: two ordinary, two held-reader, two disconnect. Record observed overlap and completion vs. busy refusal; the isolate-local permit does not guarantee a busy response across isolates. |

Slots count starts, not distinct case IDs or retries. The allowlist does not
count starts, schedule pairs, provision the auth binding or authorize live use.

Run `python3 -I scripts/subsonic-staging-runner.py --simulate` offline. This
uses no credentials or network and supplies no evidence about live staging.
The offline request-contract tests also check the exact staging POST target,
form-body credential placement, and fixed application rejections. They use
synthetic credentials and do not exercise live transport or attribute a pair
response to permit contention.

### Read-only staging observer smoke

After the observer workflow ships, manually run `subsonic-staging-observer` on
protected `main`. The job uses only the existing `staging` observer secret. It
makes one fresh PostgreSQL connection with verified TLS and reads the fixed
counts function in a short read-only transaction. The five fields, in order,
are measurement sessions, all client sessions, observer sessions, maximum
connections, and all reserved connections. The observer must see itself once
in the client count; subtract the reserve and client count only once from the
maximum. The smoke requires zero measurement sessions and at least two free
ordinary slots. Missing or unsafe telemetry fails the job with a fixed category.

Record the run, successful job, exact source SHA and UTC sample time. A skipped
job is not evidence. This is point-in-time database telemetry, not lease or
`/rest` readiness, and cannot be reused to admit a future load window. The job
does not deploy a Worker, change bindings or send application traffic.

### In-window telemetry policy (offline)

The offline policy checks staging DB pressure and staging health for the pinned
serving version while clients run, just before each start and at load close.
Collectors must validate the identity and source timestamp first, then map it
onto the runner's monotonic timeline; receipt time cannot make an old signal
fresh. DB and health samples must be inside the window and at most three seconds
old. Keep room for two measurement sessions in total. After all clients in a
request group terminate, record the group's completion on the monotonic timeline
before checking admission for the next group. Only an independently observed
zero DB count strictly after that fence proves drain; a fresh zero from before
or at the fence is insufficient. Only the second client of a scheduled pair
can use the overlap allowance. A violated threshold or missing signal stops
admission for the rest of the window, with a fixed classification.

Resource-event evidence must cover the pinned version continuously from window
open, not just contain the most recent event. An empty or sampled response is
not proof of zero events. If the provider only supplies delayed resource events,
select delayed mode before the window and record the lack of real-time coverage.
It does not waive DB or health checks, and any reported resource exceedance or
termination still stops admission. Reconciliation needs clean coverage through
client termination within two minutes of load close (three seconds in live
mode), without extending the five-minute job deadline or holding up restore.
This policy has no collectors or live entrypoint; it does not prove provider
coverage or cleanup.

### Version-fenced restore rehearsal (offline)

The rehearsal pins a known-off deployment and checks that the serving version
agrees with the deployment version. On cleanup it cancels clients, then attempts
restore only for the experiment's exact deployment identity. The simulated write
compares that identity atomically; even a newer deployment of the same version
must survive. A separately verified newer off deployment is not a restoration
of the pinned baseline.

Off requires disabled measurement, no lease, and no measurement DB binding.
The independent zero DB count must follow the off readback and the deployment
must still match after that sample. Missing proof leaves cleanup unverified.

This is not a Cloudflare adapter. Ordinary staging API deployment and a future
live executor must share a GitHub concurrency group across activation, load,
and cleanup. That group does not fence dashboard or API writes. Cloudflare's
[deployment create API](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/create/)
does not expose an expected-current deployment precondition. A read followed by
an unconditional rollback is not safe; do not arm or attempt that write without
a fence that covers all writers. The rehearsal does not exercise Actions
cancellation or independent UTC lease expiry.

### Charge triplet

Use the same `POST /rest/getArtists` with form-encoded `f=json` and credentials
for all three `small` selectors. Write `P` for the ledger after authenticated
parameter parsing. It includes request collection, credential decode, real
auth and dispatch parameter allocations. That prefix varies with the
credential shape; it is not refunded, reset or assumed to be fixture data.
The lower ceiling is `min(8 MiB, P + headroom)` on that **same** ledger.

| Slot | Case | Headroom (bytes) | Attempted cumulative charge | HTTP status / body bytes |
| --- | --- | ---: | ---: | --- |
| 10 | `charge-below` | 4,025 | P + 4,024 | 200 / 296; complete artist index |
| 11 | `charge-exact` | 4,024 | P + 4,024 | 200 / 296; byte-identical index |
| 12 | `charge-over` | 4,023 | P + 4,024 | 503 / 112; fixed JSON only, `Retry-After: 1` |

The wasm32 ledger delta is **4,024 bytes**: raw rows 768, owned row copy 768,
mapped snapshot 1,040, artist index and response graph 1,136, and exact 296-byte
body buffer 312 after allocator rounding. At one byte short the writer refuses
that final buffer before any response data is sent. This is not the separate
291,928-byte `large-mixed` raw-row phase oracle, and does not cover the Worker
JS transfer copy or isolate memory. Non-charge cases keep the default 8 MiB
ceiling. All cases retain the row, field, aggregate text and document limits.

The `worker-release-builds` job exercises the boundary presets on release
WASM and the charge triplet through the release Worker's real authentication
against disposable PostgreSQL. `api-migrations` separately tests PostgreSQL
membership, mirror row/text preflight and transfer/transaction behavior using
disposable databases, not a shared mirror.
