# Live-loader first-slice acceptance handoff

**Test-only handoff, not a merge candidate or runtime authorization.** The loader
seam does not exist on the inspected base. Do not open an intentionally failing
PR. Independent acceptance ownership stays with the test author; the paired
builder carries these files unchanged. Corrections to acceptance files belong to
the test author, never the builder.

## Oracle and scope

Expectations are independently transcribed from the pinned contracts below.
Every body was hash-verified over stored UTF-8 bytes (revision, byte length,
SHA-256) before authoring; all seven pins matched. P2/M2 apply only as amended
by S/R; no moving-latest fallback was used.

| Key | Contract | Revision (short) | Bytes | SHA-256 |
| --- | --- | --- | --- | --- |
| S | provider/manifest seam v3, 2026-10-20 | `73f360cd` | 77379 | `6930ae92d37cc739873c79c024687aab5dbb65ef8edbb684546b3a5c32227145` |
| R | lost-admission discovery addendum R1 | `7020d5b1` | 29883 | `1011398a3eaa10f469dce75cddef98ca2fa35da1672df9e790bcdf6b796d7ecc` |
| P2 | provider correction | `ae9b8936` | 78119 | `86580ead28f0c18b2616b70d292f9b4f3809547cad2e9967a086ec712756b124` |
| M2 | manifest correction | `62137dc1` | 94988 | `11630b2e9d3ea74f626c4406ef5483eb646291888ae587cca325f560eba11b59` |
| C1/C2 | seam verdict (approve-design) | `8d700799` | 33136 | `3bbccd465a8c65a959fb4bd38b433ad4e6eff21c7c839f8dde2d3419bef0c89f` |
| B1 | unpublished-verifier BODY contract | `8b345004` | 97081 | `fee5902a00510d0ca0e15f900d59668a8e6cd6563c18725f9c0df4745bbef761` |
| B1v | BODY verdict (approve-design, A1-A10) | `c53b74a6` | 36794 | `e7d9bdb3602c917a53ca8f701aca2814bb616f58b553d562e241cfa45ebf15f5` |

Decision rules come from S:283 (ProviderOnly shape, 119/563, cap validation),
S:343 (whole-object deny, never truncate), S:345-354 (bounds table), S:356
(exact error shape and 403/409/503/404 mapping), S:360 (resolve-all/vet-all,
exact vetted peer, original-hostname TLS, no second resolution, preserved
pool properties), P1:184/186 (cap formula, per-dial guard), P1:226 (quotas,
90% refusal), P2:404 (preserved vetting/cap/pool properties) and B1:38/197/211
(amendment A5: one connect/checkout pair per window, pre-content dial failure
aborts) with B1:319/321 (separate admissions, vet-all dial). R amends nothing
in this slice (import-admission discovery only); the C1/C2 and B1 verdicts
decide design, not execution, and grant no implementation authority by
themselves — this suite exists under the separate bounded test allocation.

Only inert synthetic hostnames (`.invalid`), pre-resolved candidate text,
opaque UUID-shaped strings, integer sizes/counts/caps and boolean policy
inputs occur here. There are no keys, signatures, cryptographic fixtures,
DNS lookups, sockets, worker bindings, vault/DB access, clocks, network or
live requests. A well-shaped fixture is **not** an authorized response.

## Smallest existing harness and proposed seam

Use the existing native Rust integration harness in `nntune-stream`, with its
existing `serde_json`, `syn` and `quote` dependencies and the existing
`pool::Lane` / `pool::effective_cap` / `routes::ProbeDecision` imports. There
are no manifest, lockfile, product-behavior, dependency or CI changes. v4
fixtures are built from octets (`Ipv4Addr::new`) so no dotted private quad
appears in tracked files; v6 fixtures are literals.

No exported builder seam exists yet. Consequently the following is an
**intended adapter contract**, not a claim to have consumed an existing export:

- Pure portable module `nntune_stream::loader`, no production callers; the
  only permitted production reference is the out-of-line `pub mod loader;`
  export. No DNS, sockets, vault/KEK, DB, clock, env, bindings or `unsafe`.
- `vet_dial_plan(req: &VetDialPlanRequest) -> Result<DialPlan, LoaderDenial>`
  with `VetDialPlanRequest { host: &str, port: u16, tls: bool,
  allow_insecure_plaintext: bool, candidates: &[&str] }`. Vets **all**
  candidates: non-empty, every entry parses as an IP and is publicly
  routable, else deny (mixed, empty and unparseable sets all deny). Host
  mirrors the existing SSRF host validator (1..=253 ASCII, no scheme, path,
  userinfo, port suffix or space; no per-label rule is invented). Port must
  be exactly 119 or 563. Plaintext requires the explicit acknowledgement.
  Every denial is `LoaderDenial::ProviderUnavailable`: the stored endpoint
  is unusable; the caller's own authorization is not decided here.
- `DialPlan` is an **inert vetted destination**, not a socket or dial
  permission: `peer() -> SocketAddr` pins the exact vetted IP and requested
  port (membership in the vetted set; no selection order is fixed),
  `tls_server_name() -> &str` carries the original hostname byte-exact for
  TLS validation, `tls() -> bool` echoes the transport mode. It implements
  no `Debug` (checked at compile time): dial material stays out of
  capture/debug/tail/trace surfaces. There is no hostname dial input, so a
  transport has nothing to re-resolve.
- `admit_connection_step(window: &DialWindow, step: DialStep, warm: bool)
  -> Result<(), LoaderDenial>` with `DialStep::{Connect, Checkout}` and
  `StepState::{Unadmitted, Admitted, Completed, Failed}`. Fresh windows
  admit connect; checkout needs a completed connect, or a fresh window with
  `warm` (which spends the skipped connect); every step consumes exactly
  once; completed pairs admit no second pair or reconnect; any `Failed`
  state terminally denies every further step. Denials are
  `ProviderUnavailable`: the window cannot serve the step.
- `derive_capacity(req: &CapacityRequest) -> Result<CapacityGrant,
  LoaderDenial>` with `CapacityRequest { object_bytes: u64, bound_bytes:
  u64, provider_cap: i64, plan_cap: i64, lane: Lane, in_flight: u32,
  quarantined: bool, global_used: u64, global_bound: u64, owner_used: u32,
  owner_bound: u32, resolve_used: u32 }`. One generic inclusive
  (actual, bound) mechanism backs every byte/shape ceiling: fit admits,
  bound+1 denies, never a partial grant. Caps validate positive with the
  provider cap at most 50 before `min(provider, plan, 50)`; a
  zero/disabled plan denies as `AuthorizationDenied` (entitlement) before
  any clamp, every other violation as `ProviderUnavailable`. Global
  90%-of-bound refuses new work (overflow-safe; saturated counters deny);
  owner ceilings deny at the bound; a recorded prior resolve denies the
  retry; quarantine denies before slot accounting; lanes mirror the pool's
  exact reserve rule (Playback 0, others 1); at-cap denies as backpressure.
  `CapacityGrant` echoes the whole admitted size and the metered slot
  (`effective_max_connections()`, `object_bytes()`, `admitted_slot()`).
  When several violations coincide, any fixed code is acceptable; tests
  isolate single violations and never pin an evaluation order.
- Bounds-table constants pin S:345-354 byte/count/state rows
  (`MAX_PROVIDER_REQUEST_BYTES` … `MAX_ACTION_ROWS`,
  `MAX_EFFECTIVE_CONNECTIONS = 50`, `REFUSAL_USE_PERMILLE = 900`,
  `MIN_PLAN_ACTIONS = 1`). Timing/lifetime rows need a clock and stay
  runtime gates outside this pure slice; shape-floor and closed-schema
  validation stay with the closed validators.
- `LoaderDenial::{AuthorizationDenied, ProviderNotConfigured,
  ProviderUnavailable, NotFound}` with `code()` (`authorization_denied`,
  `provider_not_configured`, `provider_unavailable`, `not_found`),
  `status()` (403, 409, 503, 404) and `Display` rendering exactly the code.
- `map_loader_error(denial: LoaderDenial, request_id: &str, fresh_id: &str,
  authorized: bool) -> MappedLoaderError`. The 409 maps only after full
  authorization (otherwise 403, no existence disclosure); unknown routes
  map to 404 regardless; 503 needs authorization too. The body is exactly
  `{"v":3,"request_id":"<id>","error":"<code>"}`: integer 3, that key
  order, no extra keys. A valid lowercase UUIDv4 echoes; anything else is
  replaced by the caller-supplied authority UUID verbatim, never echoed.
  Configured-ness detection itself needs the vault and is outside this
  slice; the mapping gate is the pure part tested here.

The forbidden-address families follow the contract (loopback, private,
link-local including the metadata endpoint, carrier-grade NAT,
unspecified/this-network, multicast, broadcast, benchmarking, IETF
assignments, reserved, v6 non-global-unicast and special-use space).
Long-term classification stays single-owned by the shared SSRF guard
crate; this suite pins vet-ALL decision semantics (every answer vetted,
mixed sets deny) over mainstream families, not a forked table. Public
controls use routable fixtures only.

## Must-pass matrix once the seam exists

| Area | Cases | Oracle |
| --- | --- | --- |
| Exact peer/port/TLS | Single v4/v6 pins peer+port+SNI; 119+563 admit; 12 other ports deny; SNI byte-exact incl. case | S:360, S:283 |
| Vet-all | Mixed sets deny whichever answer is last; every forbidden family denies alone; empty denies; unparseable denies alone/mixed; multi-public admits a member | S:360, P1:186 |
| Host shape | DNS + v4-literal admit; 12 malformed shapes deny; v6-literal denies as a mirrored validator quirk | P1:184, S:283 |
| Plaintext ack | Unacknowledged TLS-off denies; acked admits; TLS admits | P1:184 |
| No second resolution | Plan exposes only concrete `SocketAddr` + SNI; loader source fence bans DNS idents | S:360, B1:321 |
| One pair/abort | Connect-first; warm skip spends connect; no second pair/reconnect; consume-once; any failure terminal for both steps × warm/cold | B1:211/321 |
| Whole object | 13 byte bounds fit-at-bound/deny-over; plaintext ceiling; shape counts ride the same mechanism | S:343-354 |
| Caps | min/provider/plan/50 table; nonpositive/oversize deny pre-clamp; disabled-plan 403 precedes the clamp helper (contrast pinned) | P1:184, S:360 |
| Quotas | 90% boundary + saturation; owner ceilings; second-resolve denies | S:353, P1:226 |
| Slots/lanes | Playback last-slot; prefetch/ingest headroom; quarantine-first; at-cap backpressure; saturated in-flight denies | P2:404, S:360 |
| Error shape | Code/status table; exact 403/409/503/404 bodies; 409-gate both directions; 404 regardless of authorization | S:356 |
| Fresh UUID | Valid echoes; 9 invalid shapes substitute fresh; credential-bearing IDs compare equal and never echo | S:356 |
| Containment | `provider()` AST unchanged + decoys; fixed 503; no production seam caller; loader purity fence + sensitivity; KEK absent; no tracker IDs/UI paths/private IPv4 in these files | S:360 posture |

## Intended runners (worktree root)

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test loader_dial_plan_acceptance
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test loader_capacity_acceptance
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test loader_error_mapping_acceptance
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test loader_slice_guard_acceptance
rustfmt --check --edition 2021 workers/stream/tests/loader_dial_plan_acceptance.rs workers/stream/tests/loader_capacity_acceptance.rs workers/stream/tests/loader_error_mapping_acceptance.rs workers/stream/tests/loader_slice_guard_acceptance.rs
```

Compiling commands go through the bounded pool wrapper only; never a
per-worktree target, manual `CARGO_TARGET_DIR`, or a direct compiling cargo
command. Exit 75 is refusal/wait, not product RED or PASS.

## Initial evidence and limits

Recorded at handoff on the test-only branch (see the authoring card): base
commit, presence preflight output, pool outcome, rustfmt outcome, diff
hygiene. Reproduce the noncompiling missing-seam RED preflight:

```sh
python3 - <<'PY'
from pathlib import Path
source = Path('workers/stream/src/loader.rs').is_file()
export = 'pub mod loader;' in Path('workers/stream/src/lib.rs').read_text()
print(f'loader seam preflight: source_present={source}; export_present={export}')
raise SystemExit(0 if source and export else 1)
PY
```

The `provider()` guard uses parsed Rust with comment-decoy/partial-enable
sensitivity cases. The cross-package spelling scan is deliberately
conservative and permits only the out-of-line library export; it is **not**
a whole-program reachability proof. The loader-purity fence walks code
idents (doc prose and plain strings skipped, macro inputs word-scanned)
with clean/dirty sensitivity cases; it is a spelling fence, not a
capability proof — review, `cargo-deny`-style dependency gates and the
wasm32 build still own the full supply-chain picture. Nothing here
instantiates a Worker/DO, resolves DNS, opens sockets, reads the vault or
touches staging/production.

## Explicitly NOT proven by this slice

Issuer/cnf verification, key custody/rotation, SQL ownership/ledger
transactions, DO identity/routing, lease issuance or current-state checks,
audit acknowledgement, configured-ness detection, stored-plaintext-ack
provenance, plan eligibility before clamp, actual DNS resolution/dial/TLS
transport behavior, timing/lifetime/deadline enforcement, unpublished
verification BODY authority, signer/audit/publication/play/probe runtime
wiring, E2E import/streaming, deployment or staging/production readiness.
A vetted dial plan, capacity grant or mapped error grants no authority to
connect, decrypt, deliver credentials or serve content.
