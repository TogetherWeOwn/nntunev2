# Inert provider-v3 acceptance handoff

**Test-only handoff, not a merge candidate or runtime authorization.** The response
seam does not exist on inspected main `978c75987f6de6d225d00fa2c65dc6410975c688`.
Do not open an intentionally failing PR. Independent acceptance ownership stays
with QA; the builder may carry these files unchanged. Interface-only adjustments
and final same-head GREEN/RED challenge evidence return to QA, not the builder.

## Oracle and scope

Expectations are independently transcribed from the pinned provider/manifest seam
S, `nntune.playback-provider-seam/3`, proposal 1, 2026-10-20, §§2, 6.1, 6.2 and 8:
SHA-256 `6930ae92d37cc739873c79c024687aab5dbb65ef8edbb684546b3a5c32227145`.
Its exact UTF-8 body was hash-verified before authoring. Incorporated definitions
were also hash-verified: P1 §7 (`ea37be6a885c70a6a36f17413f26dc4d626031d861b951ba83a54a1c3228066b`)
and M2 §1.2 (`11630b2e9d3ea74f626c4406ef5483eb646291888ae587cca325f560eba11b59`).
Historical proposal headers are not implementation or live-authority grants.

Only synthetic IDs, opaque digest-shaped strings, key **labels**, credential-shaped
strings, and integer time inputs occur here. There are no keys, signatures,
cryptographic fixtures, sockets, bindings, database access or live requests.
A syntactically valid fixture is **not** an authorized response. The tests do not
reimplement a parser, derive an authorization context or synthesize unpublished
verification BODY authority.

## Smallest existing harness and proposed seam

Use the existing native Rust integration harness in `nntune-stream`, with its
existing `serde_json`, `syn` and `quote` dependencies. There are no manifest,
lockfile, product-behavior or CI changes. JSON Value is used only to manufacture
inert input mutations; raw lexical cases bypass Value so duplicate keys and
exponent/float integer forms cannot disappear in an oracle round trip.

No exported builder seam exists yet. Consequently the following is an **intended
adapter contract**, not a claim to have consumed an existing export:

- Pure portable module `nntune_stream::provider_response`, no callers.
- `validate_provider_response(raw: &[u8], expected: &ExpectedProviderResponse)`
  returns `Result<Provider3Success, E>`; `E: Display` renders only a fixed
  `authorization_denied` or `provider_unavailable` code for invalid input.
- `ExpectedProviderResponse` fields follow `expected()` in the test: owned String
  IDs/digests/operation/owner, u64 epochs/time, and `KeyBinding { kid: String,
  kv: u64 }`; both implement PartialEq. These are supplied comparison inputs,
  not authenticated authority.
- `Provider3Success` and `ProviderOnly` are **inert decoded views**, not usable
  lease/authorization handles. Neither implements Debug (checked at compile time).
  Keep fields private rather than exposing an authority constructor.
  `Provider3Success::bindings()` yields `&ExpectedProviderResponse`, and
  `provider()` yields `&ProviderOnly`. Its `host()`, `username()` and `password()`
  yield `&str`; `port()`/`effective_max_connections()` yield unsigned integers
  convertible to u64; `tls()`/`allow_insecure_plaintext()` yield bool. All decoded
  output fields are compared, including exact credential byte preservation.
- The builder can use local names/idiom via a QA-owned interface-only adjustment;
  do not weaken the closed wire schemas or case expectations to satisfy a parser.

The expected tuple includes only fields actually present in Provider3Success.
`context_digest` is compared as an opaque exact value; no extra `context` field
is invented. There is no scope/origin parser, crypto, clock authority, signer,
producer, ledger or runtime loader in this slice. `lease_until` is shape/range and
exact supplied-value validation only, not current-state or lease authorization.
Time zero is accepted only as M2 §1.2 lexical/static validity, never a live lease.
The 8 KiB hard cap is interpreted inclusively: at most 8192 bytes, not less than
8192. Ordinary response whitespace remains legal JSON and counts toward the cap.
S §2 requires JCS for authenticated request/JWS JSON, not these response bytes.

## Must-pass matrix once the seam exists

| Test group | Required observation | Normative source |
| --- | --- | --- |
| Positive native identity, BODY and STAT | Non-UUID UTF-8 owner accepted with exact expected tuple; both defined operations accepted | S §2, §6.2 |
| Recursive closed shapes | Every required root/provider/cnf/issuer field missing or null denies; extras including legacy/combined fields deny | S §2, §6.2 |
| Versions and scalar types | v2/other versions, wrong scalar/container/boolean representations deny | S §2 |
| Raw numeric lexemes | Float/exponent/leading-plus/leading-zero integer forms and negative-zero time deny | S §2, M2 §1.2 |
| Strict JSON | Duplicate keys at each object level, BOM, bad UTF-8, lone surrogate, trailing values, empty/nonobject input deny | S §2 |
| Expected bindings | Individually changed request/lease/provider/owner/scope/operation/digests/cnf/issuer/epochs/time deny | S §6.1, §6.2 |
| Native domain | UTF-8 byte ceilings and control restrictions enforced; no trim/casefold/Unicode-normalization/UUID coercion | S §2 |
| Identity/key/numeric shapes | Lowercase v4 scope ID, lowercase hex digest, key label bounds and positive safe epochs; equal malformed expected values cannot excuse invalid wire shapes | S §2, §6.1 |
| Provider material | Username 1..255/password 1..1024 UTF-8 bytes, no NUL/CR/LF; host 1..253 ASCII shape; invalid values refuse without normalization/truncation | S §6.2, P1 §7 |
| Ports, TLS and capacity | 119/563, TLS or explicit plaintext acknowledgement, every capacity 1..50 positive; zero/51 cannot be clamped | S §6.2, P1 §7 |
| Complete body limit | Complete legal JSON at 8192 bytes fits; 8193 bytes and oversized valid-prefix bodies deny wholly | S §2, §8 |
| Sensitive observations | Fixed error codes for every denial, distinct malformed passwords give identical fixed output; sensitive decoded types have no Debug | S §8 |
| Runtime STOP | Actual provider method remains unconditional ProviderUnavailable; portable route rendering remains fixed 503; no production import/call of inert seam | L0 containment, S §8 |

The capacity test validates the **already-effective response cap**. It cannot
prove original provider/plan caps were positive before producer-side min/clamp;
those inputs do not occur in ProviderOnly. Likewise host shape is not fresh DNS
resolve-all/vetting, and a plaintext flag's syntactic truth is not evidence of
stored user acknowledgement or an authenticated revision.

## Intended runners (worktree root)

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test provider_v3_inert_guard_acceptance
python3 scripts/cargo_cache.py run -- test -p nntune-stream --test provider_v3_response_acceptance
python3 scripts/cargo_cache.py run -- clippy -p nntune-stream --test provider_v3_inert_guard_acceptance --test provider_v3_response_acceptance -- -D warnings
rustfmt --check --edition 2021 workers/stream/tests/provider_v3_inert_guard_acceptance.rs workers/stream/tests/provider_v3_response_acceptance.rs
```

No direct compiling Cargo, private target directory, alternative pool or manual
CARGO_TARGET_DIR is allowed. Exit 75 is refusal/wait, not product RED or PASS.

## Initial evidence and limits

- Main/HEAD: `978c75987f6de6d225d00fa2c65dc6410975c688`; no source module/export.
  Presence preflight below exits **1** with
  `L0 seam preflight: source_present=False; export_present=False`.
- Attempted inert-guard Cargo runner above exits **75** before compiling:
  `{"finding":"refused","reason":"not a real directory: /paperclip/.cache/nntunev2-bounded"}`.
  No pool bypass or host provisioning was attempted. Neither new Rust target has
  compiled/executed; no product-behavior GREEN/RED challenge is claimed.
- Rustfmt validates syntax/format only. It cannot prove imports, types or assertions
  compile. Missing-seam preflight is the only executed RED evidence at handoff.
- No PR, required-CI result, review score, merge, deployment or staging PASS is
  claimed by this test-first branch. Final compiled acceptance and challenge
  evidence must bind the builder's exact implementation head after the normal gates.

Reproduce the noncompiling missing-seam RED preflight:

```sh
python3 - <<'PY'
from pathlib import Path
source = Path('workers/stream/src/provider_response.rs').is_file()
export = 'pub mod provider_response;' in Path('workers/stream/src/lib.rs').read_text()
print(f'L0 seam preflight: source_present={source}; export_present={export}')
raise SystemExit(0 if source and export else 1)
PY
```

The source guard uses parsed Rust for the exact NntpPool provider body, with
comment-decoy/partial-enable sensitivity cases. The cross-package spelling scan
is deliberately conservative, ignores the codec definition and permits only its
out-of-line library export. It is **not** a whole-program reachability proof;
renames/macros/generated/out-of-tree code require independent review. It scans
current `workers/*/src` and `crates/*/src`, not tests, examples or build.rs;
no live wasm32 Worker is instantiated. The spelling guard can conservatively
reject references in doc attributes/string literals too; it does not infer calls.

## Explicitly NOT proven by L0

Issuer/cnf signature or private-signer verification/custody; native SQL ownership
or actual DO routing; producer/link/M2 commitment authenticity; current state,
nonce/revocation/replay/common-writer transactions; stored acknowledgement and
provider/plan eligibility; audit acknowledgement/sink; real lease timing/early stop;
key/credential access or delivery authorization; fresh DNS and vetted-peer dial;
transport/TLS operation; unpublished verification BODY; end-to-end ingestion,
streaming, production/staging readiness or deployment. All runtime STOPs remain.

ACT: author independent tests and push a handoff, no product behavior edits.
WHY: establish the L0 contract before the builder implementation without a duplicate oracle.
UNDO: leave the unmerged test branch unused; no runtime state was changed.

Skills used: paperclip, paperclip-superpowers-adapter, verification-before-completion, model-router-dogfood, qa-acceptance, pre-review. Test-driven-development procedure applied through the enabled adapter: test-first missing-seam RED; implementation/GREEN belongs to the paired builder, final challenge to QA.
