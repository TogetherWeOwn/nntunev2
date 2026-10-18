# API delivery map: current vs target

Examined head: `d583c9b5` (re-verify with `git rev-parse HEAD`; the card this
was cut from baselined `6b7ddcd7`). This map proposes no endpoint, auth, or
policy design: the Current column restates wired code and its tests, the
Target column restates accepted ADRs where they speak (rows the ADR does not
address are marked `Not specified in ADR 0006` with the author's inference
kept separate), and Status names the gap between them.

Known delta after the examined head (no rebase per review-round rules; main is
now `9de9862e` via #224): main's dispatcher adds a `POST /api/test-codes` arm
(`QA_TEST_CODES_PATH` handled by `qa_test_codes` in `workers/api/src/handlers.rs`,
admitted only behind the staging gates documented in
`docs/security/threat-model.md` section 14). That seam did not exist on the
examined head and is covered in the seams section below so no reader mistakes
this map for main.

## Verification note

On the examined head, the wired-route table in `workers/api/README.md:37-49`
matched the dispatcher (`handle` in `workers/api/src/handlers.rs:2331-2353`,
known-path set in `is_known_path` at `workers/api/src/handlers.rs:2286-2305`)
exactly: `GET /healthz`, the four passkey routes, the two magic-link routes,
the three session routes, the two reauthentication routes, the three vault
routes, `POST /v1/vault/test` plus its `/v1/vault/test-connection`
compatibility alias, `OPTIONS` on known paths, and a `404` fallback. Both sides of
the web contract are pinned by `workers/api/tests/fixtures/api-web-contract.json`:
the web client asserts the exact wire bodies it sends and the Rust handlers
suite asserts each route is served (`workers/api/README.md:54-58`).

Line anchors above are for the examined head; prefer the cited symbols
(`handle`, `is_known_path`) after main moved.

## Route-family map

| Family | Current on this head (handler + test evidence) | Target (`docs/adr/0006-api-surface.md:22-112`) | Status |
|---|---|---|---|
| Liveness `GET /healthz` | Wired in `handle` (`workers/api/src/handlers.rs:2332` on examined head); dispatched in `workers/api/tests/request_body_transport_acceptance.rs:1072` (via `handlers::handle`) | Not specified in ADR 0006 (no `healthz`/`liveness` text anywhere in `docs/adr`); the versioned resource list (`docs/adr/0006-api-surface.md:26-28`) omits `/healthz`, so treated as transport liveness outside the versioned surface (author inference, not an ADR statement) | Shipped |
| Passkey registration (`register/start`, `register/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2333-2334` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:845-891` (harness calls `handlers::handle` at `workers/api/tests/pr30_findings_acceptance.rs:608`) | Launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:102-103`); ADR 0006 names no `/v1/auth/*` resource, and these unauthenticated calls mint the cookie (author inference — the cited lines state only the launch-auth default) | Shipped |
| Passkey login (`login/start`, `login/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2335-2336` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:892-906` | Same as registration: launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:102-103`); no `/v1/auth/*` resource is named in the ADR (author inference) | Shipped |
| Email magic link (`magic/start`, `magic/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2339-2340` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:636-652` and module-level issuance/consumption in `workers/api/tests/api_tests.rs` (`auth::issue_magic_link`, no `handlers` dispatch) | Same as registration: launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:102-103`); no `/v1/auth/*` resource is named in the ADR (author inference) | Shipped |
| Session rotation/revocation (`refresh`, `logout`, `revoke-all`) | Wired in `handle` (`workers/api/src/handlers.rs:2341-2343` on examined head); route-level rotation/revocation in `workers/api/src/handlers.rs:3482` (`refresh_rotates_server_row_and_revoke_all_kills_sessions`, via test harness `call_full` through dispatch); contract routes listed in `workers/api/tests/fixtures/api-web-contract.json`; module-level cookie behavior in `workers/api/tests/session_cookie_hardening_acceptance.rs` (`nntune_api::session`, no `handlers` import — not route evidence) | One auth/session layer serves both surfaces (`docs/adr/0006-api-surface.md:116-117`) | Shipped |
| Session-bound reauthentication (`reauthenticate/start`, `reauthenticate/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2337-2338` on examined head); helper-level session behavior in `workers/api/tests/privilege_reauth_acceptance.rs` (direct `nntune_api::session` calls, no `handlers::handle` dispatch — not route evidence); route-level proof in `workers/api/src/handlers.rs:3951` (`passkey_reauthentication_replaces_only_the_authenticated_session`) and freshness gating in `workers/api/src/handlers.rs:3692` (`vault_mutations_require_fresh_session_but_reads_do_not`) | This is the current freshness proof for vault writes. The separate account-deletion step-up pair (`POST /v1/account/step-up/begin`, `POST /v1/account/step-up/complete`) is target-only (`docs/adr/0006-api-surface.md:51-77`) | Shipped (current scope); account step-up is future |
| Vault (`GET`/`PUT`/`DELETE /v1/vault`, `POST /v1/vault/test` + alias) | Wired in `handle` (`workers/api/src/handlers.rs:2344-2349` on examined head); booleans-only CRUD pinned in `workers/api/src/handlers.rs:3519` (`vault_crud_is_booleans_only_and_owner_scoped`); dispatched reads/writes in `workers/api/tests/request_body_transport_acceptance.rs:1083-1130` and `workers/api/tests/pr30_findings_acceptance.rs:986-1041` (both via `handlers::handle`) | Advanced BYOP management stays REST-only (`docs/adr/0006-api-surface.md:209-210`); compat credentials never reach provider credentials (`docs/adr/0006-api-surface.md:154`); note the naming gap: the ADR resource list (`docs/adr/0006-api-surface.md:26-28`) names `/v1/providers` and `/v1/indexers` (Future below) with no direct `/v1/vault` entry | Shipped |
| CORS preflight (`OPTIONS` on known paths) | Wired in `handle` (`workers/api/src/handlers.rs:2350` on examined head); known-path set in `is_known_path` (`workers/api/src/handlers.rs:2286-2305`); origin rules in `workers/api/README.md:50-53` | Not specified in ADR 0006 (no `preflight`/CORS text in `docs/adr`); the versioned resource list (`docs/adr/0006-api-surface.md:26-28`) covers resources only, so preflight is treated as transport behavior outside the versioned surface (author inference, not an ADR statement) | Shipped |
| Library genres/playlists core | Core rules in `workers/api/src/library.rs` (page cap `workers/api/src/library.rs:51`); behavior tests in `workers/api/tests/library_tests.rs` and `workers/api/tests/library_single_row_visibility.rs`; **no** library path appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2331-2353` on examined head) | Mapped onto `/v1/playlists` with the metadata-only public rule; no parallel `/api/playlists` contract survives (`docs/adr/0006-api-surface.md:172-174`); catalog reads resolve only through caller library membership (`docs/adr/0006-api-surface.md:157-165`) | Core-only; HTTP routes future |
| Account deletion (`DELETE /v1/account`, `cancel-deletion`, `GET /v1/operations/{id}`) | No account/operations path appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2331-2353` on examined head); SQL/constant pins only in `workers/api/src/account_deletion.rs` and `workers/api/tests/account_deletion_acceptance.rs`; the deletion fence is already enforced on vault writers (`workers/api/src/handlers.rs:2108` for upsert, `workers/api/src/handlers.rs:2268` for test-connection, envelope in `workers/api/src/handlers.rs:584-588`) | `DELETE /v1/account`, `DELETE /v1/libraries/{id}`, `DELETE /v1/playlists/{public_id}`, `POST /v1/account/cancel-deletion`, `GET /v1/operations/{operation_id}` with `202` + operation id, grace window, and typed `409` fences (`docs/adr/0006-api-surface.md:29-101`) | HTTP future; writer fence shipped |
| Native catalog REST (`/v1/{me,library,artists,releases,tracks,playlists,plays,providers,indexers,search,stream}`) | None of these paths appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2331-2353` on examined head) | The canonical API; `apps/web` uses only this (`docs/adr/0006-api-surface.md:26-28`); versioned `/v1`, breaking changes ship `/v2` alongside (`docs/adr/0006-api-surface.md:104`) | Future |
| Scoped native REST credentials (bearer keys) | Not wired: launch auth is the session cookie (`docs/adr/0006-api-surface.md:102-103`); compat credentials cannot authenticate native REST (`docs/adr/0006-api-surface.md:226`) | Design accepted in `docs/adr/0010-native-rest-credentials.md`, which states it authorizes no issuance code or storage; implementation needs its own gated card | Future (separate credential lane) |
| Subsonic subset (`/rest/*`: `ping`, `getLicense`, `getMusicFolders`, `getIndexes`, `getMusicDirectory`, `getAlbumList2`, `search3`, `getArtist`, `getAlbum`, `getSong`, `stream`, `getCoverArt`, `getPlaylist(s)`, `scrobble`) | No `/rest/*` path appears anywhere in `workers/api/src/`; the dispatcher fallback is `404` (`handle` at `workers/api/src/handlers.rs:2351` on examined head) | Subsonic subset with error `0` (or `70` where the resource is missing) plus an `nntune-native` hint header (`docs/adr/0006-api-surface.md:105-112`); adapters over the same handlers, REST first (`docs/adr/0006-api-surface.md:203-204`); acceptance contract in `docs/adr/0006-api-surface.md:217-226` | Future (separate compat lane) |
| Generated OpenAPI (`openapi.yaml`) + CI diff | No `openapi.yaml` exists under `workers/api/` on the examined head | Spec is generated from code (Rust types to schema), checked in, CI-diffed so drift fails the build (`docs/adr/0006-api-surface.md:201-202`) | Future (explicitly marked) |

## Deploy and staging seams (not public API)

These exist on or after the examined head and are deploy/staging seams, cited so no
reader mistakes them for versioned surface. Only the first bullet is
staging-only; the rest are every-environment deploy gates:

- Staging-only (on main `9de9862e`, absent on the examined head):
  `POST /api/test-codes` (`QA_TEST_CODES_PATH` → `qa_test_codes` in
  `workers/api/src/handlers.rs`, deliberately excluded from `is_known_path`
  so its `OPTIONS` never answers 204). Capture requires the explicit
  `QA_CODE_SEAM` staging flag plus an `example.test` recipient and retrieval
  requires the staged `QA_AUTH_TOKEN` bearer; every other posture answers
  404/401 with no detail, and production (flag and bearer never set) serves
  plain 404 (`docs/security/threat-model.md` section 14 on main).
- Magic-link delivery needs the operator's `EMAIL_WEBHOOK_URL`; without it
  links are issued but not delivered (`workers/api/README.md:83-85`). The
  webhook payload shape is a mailer concern, not a `/v1` route. Applies in
  every environment, not staging-only.
- The Worker refuses traffic unless `HYPERDRIVE_CACHING_DISABLED=1` is set for
  the env, after `./check_hyperdrive_cache.sh --config-id <id>` passes for
  that Hyperdrive config (`workers/api/README.md:153-161`; script at
  `workers/api/check_hyperdrive_cache.sh`). This is a deploy gate, not API.
  Applies in every environment, not staging-only.
- Staging deploy plumbing (worker-build plus per-env wrangler config from
  secrets) is an explicit follow-up slice (`workers/api/README.md:87-88`).

## Source index

- Wired surface: `workers/api/src/handlers.rs:2286-2353` (`is_known_path`, `handle`; examined-head lines), `workers/api/README.md:37-88`
- Web contract: `workers/api/tests/fixtures/api-web-contract.json`
- Route-level tests (via `handlers::handle`): `workers/api/tests/pr30_findings_acceptance.rs`, `workers/api/tests/request_body_transport_acceptance.rs`, route-level unit tests in `workers/api/src/handlers.rs:3482` (`refresh_rotates_server_row_and_revoke_all_kills_sessions`), `workers/api/src/handlers.rs:3692` (`vault_mutations_require_fresh_session_but_reads_do_not`), `workers/api/src/handlers.rs:3951` (`passkey_reauthentication_replaces_only_the_authenticated_session`)
- Module-level tests (no `handlers` import — session/auth/rate-limit/vault behavior, not route evidence): `workers/api/tests/privilege_reauth_acceptance.rs` (`nntune_api::session`), `workers/api/tests/session_cookie_hardening_acceptance.rs` (`nntune_api::session`), `workers/api/tests/rate_limits_acceptance.rs` (`nntune_api::auth`, `nntune_api::rate_limit`), `workers/api/tests/security_regressions.rs` (`nntune_api::{auth, session, vault}`)
- Unit-level tests: `workers/api/tests/api_tests.rs` (`auth::issue_magic_link`, no dispatch)
- Core-only library: `workers/api/src/library.rs`, `workers/api/tests/library_tests.rs`, `workers/api/tests/library_single_row_visibility.rs`
- Deletion constants/fence: `workers/api/src/account_deletion.rs`, `workers/api/tests/account_deletion_acceptance.rs`
- Target surface: `docs/adr/0006-api-surface.md`, `docs/adr/0010-native-rest-credentials.md`
- Post-head staging seam (main only): `docs/security/threat-model.md` section 14, `QA_TEST_CODES_PATH`/`qa_test_codes` in `workers/api/src/handlers.rs` on main `9de9862e`
