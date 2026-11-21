# Inert Subsonic staging fixtures

These recipes contain only authored synthetic data. The measurement mount
remains off by default. In a separately authorized window, the trusted
`SUBSONIC_MEASUREMENT_LEASE` appends `;` and a nonempty comma-separated
allowlist of case IDs to the existing v1 lease fields. The entire value is
bounded to 640 bytes. Unknown, duplicate or empty entries invalidate the whole
schedule. The SHA, expiry, exact HTTPS staging host and runner bearer checks
remain in force. The measurement mount admits only the three POST routes
below. The runner selects one listed case with the
`X-Subsonic-Measurement-Case` header; missing, malformed and unlisted selectors
on an armed mount receive the same fixed 404 as a missing runner bearer.
Fixture selection follows real auth against the dedicated binding and the
lease-principal match. No request can supply a recipe, ceiling or identity;
no case falls back to ordinary staging Hyperdrive or a catalogue database.

## Draft: dedicated staging runner identity and Access scope

Review only. Do not apply this draft, provision or bind credentials, arm the
mount, extend a lease, or make live requests as part of this change. This uses
existing staging infrastructure with no new service or spend.

| Literal Access application path | Measurement admission |
| --- | --- |
| `staging.nntune.com/rest/getArtists` | `POST /rest/getArtists` |
| `staging.nntune.com/rest/getArtist` | `POST /rest/getArtist` |
| `staging.nntune.com/rest/getAlbum` | `POST /rest/getAlbum` |

- Add the same **one dedicated service-token identity** as a Service Auth entry
  on each of these literal paths only. No broad `/rest/*`, alias, bypass,
  host-wide edit or production change. The [Access path rules][access-paths]
  can cover descendants, and a more specific application takes precedence
  rather than inheriting the parent's policies. Before any placement, compare
  effective policies on all three paths and their covered descendants. Retain
  the existing Allowed identities (including the Owner OTP and agent QA
  identity documented in [staging Access](../ops.md#cloudflare-access-staging)),
  other applicable policy conditions, order and session behavior. Do not change
  any pre-existing stricter descendant or encode a guessed copy of the baseline.
- Access is not an exact HTTP method/path guard: it cannot distinguish the
  allowed POST from another method on the same path, and a literal app can
  also match subpaths. The Worker checks the exact `POST` and serialized
  pathname and HTTPS staging host before body reads or fixture DB access. No
  alias or pathname decoding in the mount. Off/unsupported requests stay with
  the ordinary adapter, so Worker refusal of measurement admission does not
  restrict the dedicated identity's access to ordinary routes. Before placement,
  verify effective Access denies that identity on other methods, descendants,
  aliases and sibling paths while preserving existing callers' access. Check
  both edge normalization and Fetch URL parsing, which can normalize a path
  before the Worker compares it. The runner independently pins the whole HTTPS
  URL and method before curl spawns, without redirects. If either boundary
  cannot be enforced, leave the draft unapplied.
- Name-only placement metadata (source task, after reviews and separate
  authorization): service token `nntune-subsonic-staging-runner`, runner inputs
  `access_client_id`, `access_client_secret` and `bearer`, Worker staging secret
  `SUBSONIC_MEASUREMENT_RUNNER`. No workflow consumes these inputs in this slice.
  The Operator stages the dedicated Worker binding and runner inputs through
  approved secret handling, never in repo config, logs, argv or cards. No
  fallback to shared QA, health or deploy credentials; those stay unchanged.
- Use the shortest practical lifetime, at most 30 days for **each** of the
  Access token and the independent bearer. The Worker secret has the shape
  `v1:<issued-utc-seconds>:<expires-utc-seconds>:<bearer>`, with a fresh
  32-byte random bearer encoded as 43 unpadded base64url characters. Send
  only the bearer field in Authorization. Worker UTC enforces the half-open
  window and rejects durations over 30 days. Do not extend the same bearer or
  renew the Access token. Verify that Access session settings do not grant the
  new identity access beyond its credential window without changing existing
  callers' session behavior; otherwise leave the draft unapplied. Expiry does
  not disarm the deployment, remove bindings, or prove drain.
- Rollback (only if later placed): remove only the added Service Auth entries
  and any new path applications created for them so the original effective
  policies resume. Revoke only the dedicated Access token and remove only the
  dedicated Worker secret and runner copies. Leave existing identities,
  applications, secrets, and the separate off/drain procedure untouched.

[access-paths]: https://developers.cloudflare.com/cloudflare-one/access-controls/policies/app-paths/

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
The read-only observer exposes a one-shot pressure sample for a future
executor. It reuses the fixed query, validates the database clock and counts,
and limits the call to the earlier of the caller's monotonic deadline and the
three-second freshness bound. It reports the start of the query's local time
bracket, not its receipt time: a query started at or before a drain fence
cannot clear it by returning later. Nothing calls this helper in a live load
window yet. Health and resource collectors, live provider coverage and live
cleanup remain unverified.

Dispatch `subsonic-staging-health` on protected `main` with `expected_health_sha`
from the staging API deployment record. Require the probe step to report the
verified SHA, not a green run with a skipped job. This proves build liveness only.

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
a fence that covers all writers.

### Read-only off and drain verifier

`verify_cleanup` in [the staging cleanup module](../../.github/scripts/subsonic_staging_cleanup.py)
accepts a trusted deployment ID, version ID and build SHA. It checks fresh
provider deployment and version results (not saved receipts) for 100% traffic
on that exact deployment and for the default-off configuration. It then takes
one independent `sample_pressure` reading. Its query start must be strictly
later than the off readback, and a fresh zero measurement-session count must
still hold as evidence when the final deployment read matches the pin.

On a protected `main` push, [`deploy.yml`](../../.github/workflows/deploy.yml)
checks off and drained state after `staging-api` deploys successfully and the
same job validates a matched provider identity receipt for that build. A
failed or skipped deploy (including a schema-gate failure) or a missing or bad
pin skips the readback. The job records the readback step's raw outcome apart
from the deploy result; readback failure does not change E2E eligibility. Do
not dispatch or rerun a workflow to fill in missing post-deploy evidence.

The deploy step and the separate manual
[`subsonic-staging-off-drain`](../../.github/workflows/subsonic-staging-off-drain.yml)
workflow both use the read-only adapter on protected `main`. It uses a dedicated
`SUBSONIC_STAGING_WORKERS_READ_TOKEN` in the `staging` environment with Workers
Scripts Read on the intended account, the configured `CLOUDFLARE_ACCOUNT_ID`, and
that environment's `PLANETSCALE_OBSERVER_URL_STAGING`. Do not substitute the
staging deploy token or the health-only Access token. The provider reads target
only `nntune-api-staging`. All reads share a 30-second monotonic deadline;
Actions also bounds the command and its children with an outer timeout.

For a separate manual observation, dispatch once on `main` with
`expected_deployment_id`, `expected_version_id`, and `expected_build_sha` from
the **same** trusted staging deployment receipt. The manual workflow SHA can
differ from the staging build SHA.

In either lane, the receipt supplies the pin, not the evidence. Require the
actual readback step to report `subsonic-staging-off-drain: verified` and have
a successful raw outcome. Record the run/job link and the pinned receipt,
or record that readback did not verify. This is point-in-time evidence only:
it neither fences concurrent writers nor restores a deployment, cancels
clients, proves recovery after runner loss, or authorizes measurement.

### Actions step finalization (synthetic only)

The `public-text` CI job arms a synthetic deployment and starts a simulated
client in two disposable cases: a failing step and an Actions step timeout.
The same-job `always()` finalizer requires the raw failure outcome and a
fixture receipt (intentional exit or native interrupt), runs the restore
rehearsal, and checks that the synthetic deployment is off and drained. The
job uses no staging credentials, provider calls or live traffic.

The same CI job also kills a synthetic producer with `SIGKILL` after it writes
its active state, before it can write a stop receipt. A fresh process reads that
state and must observe synthetic off readback and a zero backend count after
client cancellation. The backend remains busy through the first sample, so
clearing local clients alone cannot pass the rehearsal.

Neither rehearsal cancels the whole workflow or loses its runner. The
synthetic state stays on the same filesystem; a live recovery path would need
state available after runner loss and a restore fence that protects newer
staging deployments. UTC expiry does not establish live restore or drain.

### Independent UTC expiry (disposable release Fetch)

The disposable release Fetch test checks the Worker's UTC clock:
measurement remains enabled and the binding and exact SHA remain configured,
but a valid lease with a past expiry must reach the inert staging response
without opening the measurement socket. This checks expiry without running
cleanup or injecting a runner clock. Expiry stops admission; it does not remove
the lease or binding, restore a deployment, or prove DB drain.

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
