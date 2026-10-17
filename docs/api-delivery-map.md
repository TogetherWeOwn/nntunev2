# API delivery map: current vs target

Examined head: `d583c9b5` (re-verify with `git rev-parse HEAD`; the card this
was cut from baselined `6b7ddcd7`). This map proposes no endpoint, auth, or
policy design: the Current column restates wired code and its tests, the
Target column restates accepted ADRs, and Status names the gap between them.

## Verification note

The wired-route table in `workers/api/README.md:37-49` matches the dispatcher
in `workers/api/src/handlers.rs:2331-2353` exactly: `GET /healthz`, the four
passkey routes, the two magic-link routes, the three session routes, the two
reauthentication routes, the three vault routes, `POST /v1/vault/test` plus
its `/v1/vault/test-connection` compatibility alias, `OPTIONS` on known paths
(`workers/api/src/handlers.rs:2286-2305`), and a `404` fallback. Both sides of
the web contract are pinned by `workers/api/tests/fixtures/api-web-contract.json`:
the web client asserts the exact wire bodies it sends and the Rust handlers
suite asserts each route is served (`workers/api/README.md:54-58`).

## Route-family map

| Family | Current on this head (handler + test evidence) | Target (`docs/adr/0006-api-surface.md:22-112`) | Status |
|---|---|---|---|
| Liveness `GET /healthz` | Wired in `workers/api/src/handlers.rs:2332`; dispatched in `workers/api/tests/request_body_transport_acceptance.rs:1072` | Unchanged liveness probe; not part of the versioned REST surface (`docs/adr/0006-api-surface.md:26-28`) | Shipped |
| Passkey registration (`register/start`, `register/finish`) | Wired in `workers/api/src/handlers.rs:2333-2334`; exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:845-891` (harness calls `handlers::handle` at `workers/api/tests/pr30_findings_acceptance.rs:608`) | Stays on native REST under the existing session cookie (`docs/adr/0006-api-surface.md:102-103`) | Shipped |
| Passkey login (`login/start`, `login/finish`) | Wired in `workers/api/src/handlers.rs:2335-2336`; exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:892-906` | Same as registration: session-cookie auth at launch (`docs/adr/0006-api-surface.md:102-103`) | Shipped |
| Email magic link (`magic/start`, `magic/finish`) | Wired in `workers/api/src/handlers.rs:2339-2340`; exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:636-652` and unit-pinned in `workers/api/tests/api_tests.rs` | Same as registration: session-cookie auth at launch (`docs/adr/0006-api-surface.md:102-103`) | Shipped |
| Session rotation/revocation (`refresh`, `logout`, `revoke-all`) | Wired in `workers/api/src/handlers.rs:2341-2343`; contract routes listed in `workers/api/tests/fixtures/api-web-contract.json`; cookie hardening in `workers/api/tests/session_cookie_hardening_acceptance.rs` | One auth/session layer serves both surfaces (`docs/adr/0006-api-surface.md:116-117`) | Shipped |
| Session-bound reauthentication (`reauthenticate/start`, `reauthenticate/finish`) | Wired in `workers/api/src/handlers.rs:2337-2338`; covered by `workers/api/tests/privilege_reauth_acceptance.rs` | This is the current freshness proof for vault writes. The separate account-deletion step-up pair (`POST /v1/account/step-up/begin`, `POST /v1/account/step-up/complete`) is target-only (`docs/adr/0006-api-surface.md:51-77`) | Shipped (current scope); account step-up is future |
| Vault (`GET`/`PUT`/`DELETE /v1/vault`, `POST /v1/vault/test` + alias) | Wired in `workers/api/src/handlers.rs:2344-2349`; booleans-only CRUD pinned in `workers/api/src/handlers.rs:3519` (`vault_crud_is_booleans_only_and_owner_scoped`); dispatched reads/writes in `workers/api/tests/request_body_transport_acceptance.rs:1083-1130` and `workers/api/tests/pr30_findings_acceptance.rs:986-1041` | Advanced BYOP management stays REST-only (`docs/adr/0006-api-surface.md:209-210`); compat credentials never reach provider credentials (`docs/adr/0006-api-surface.md:154`) | Shipped |
| CORS preflight (`OPTIONS` on known paths) | Wired in `workers/api/src/handlers.rs:2350`; known-path set in `workers/api/src/handlers.rs:2286-2305`; origin rules in `workers/api/README.md:50-53` | No target change: preflight is transport behavior, outside the versioned resources (`docs/adr/0006-api-surface.md:26-28`) | Shipped |
| Library genres/playlists core | Core rules in `workers/api/src/library.rs` (page cap `workers/api/src/library.rs:51`); behavior tests in `workers/api/tests/library_tests.rs` and `workers/api/tests/library_single_row_visibility.rs`; **no** library path appears in the dispatcher (`workers/api/src/handlers.rs:2331-2353`) | Mapped onto `/v1/playlists` with the metadata-only public rule; no parallel `/api/playlists` contract survives (`docs/adr/0006-api-surface.md:172-174`); catalog reads resolve only through caller library membership (`docs/adr/0006-api-surface.md:157-165`) | Core-only; HTTP routes future |
| Account deletion (`DELETE /v1/account`, `cancel-deletion`, `GET /v1/operations/{id}`) | No account/operations path appears in the dispatcher (`workers/api/src/handlers.rs:2331-2353`); SQL/constant pins only in `workers/api/src/account_deletion.rs` and `workers/api/tests/account_deletion_acceptance.rs`; the deletion fence is already enforced on vault writers (`workers/api/src/handlers.rs:2108` for upsert, `workers/api/src/handlers.rs:2268` for test-connection, envelope in `workers/api/src/handlers.rs:584-588`) | `DELETE /v1/account`, `DELETE /v1/libraries/{id}`, `DELETE /v1/playlists/{public_id}`, `POST /v1/account/cancel-deletion`, `GET /v1/operations/{operation_id}` with `202` + operation id, grace window, and typed `409` fences (`docs/adr/0006-api-surface.md:29-101`) | HTTP future; writer fence shipped |
| Native catalog REST (`/v1/{me,library,artists,releases,tracks,playlists,plays,providers,indexers,search,stream}`) | None of these paths appears in the dispatcher (`workers/api/src/handlers.rs:2331-2353`) | The canonical API; `apps/web` uses only this (`docs/adr/0006-api-surface.md:26-28`); versioned `/v1`, breaking changes ship `/v2` alongside (`docs/adr/0006-api-surface.md:104`) | Future |
| Scoped native REST credentials (bearer keys) | Not wired: launch auth is the session cookie (`docs/adr/0006-api-surface.md:102-103`); compat credentials cannot authenticate native REST (`docs/adr/0006-api-surface.md:226`) | Design accepted in `docs/adr/0010-native-rest-credentials.md`, which states it authorizes no issuance code or storage; implementation needs its own gated card | Future (separate credential lane) |
| Subsonic subset (`/rest/*`: `ping`, `getLicense`, `getMusicFolders`, `getIndexes`, `getMusicDirectory`, `getAlbumList2`, `search3`, `getArtist`, `getAlbum`, `getSong`, `stream`, `getCoverArt`, `getPlaylist(s)`, `scrobble`) | No `/rest/*` path appears anywhere in `workers/api/src/`; the dispatcher fallback is `404` (`workers/api/src/handlers.rs:2351`) | Subsonic subset with error `0` (or `70` where the resource is missing) plus an `nntune-native` hint header (`docs/adr/0006-api-surface.md:105-112`); adapters over the same handlers, REST first (`docs/adr/0006-api-surface.md:203-204`); acceptance contract in `docs/adr/0006-api-surface.md:217-226` | Future (separate compat lane) |
| Generated OpenAPI (`openapi.yaml`) + CI diff | No `openapi.yaml` exists under `workers/api/` on the examined head | Spec is generated from code (Rust types to schema), checked in, CI-diffed so drift fails the build (`docs/adr/0006-api-surface.md:201-202`) | Future (explicitly marked) |

## Staging-only seams (not public API)

These exist on the examined head and are staging/deploy seams, cited so no
reader mistakes them for versioned surface:

- Magic-link delivery needs the operator's `EMAIL_WEBHOOK_URL`; without it
  links are issued but not delivered (`workers/api/README.md:83-85`). The
  webhook payload shape is a mailer concern, not a `/v1` route.
- The Worker refuses traffic unless `HYPERDRIVE_CACHING_DISABLED=1` is set for
  the env, after `./check_hyperdrive_cache.sh --config-id <id>` passes for
  that Hyperdrive config (`workers/api/README.md:153-161`; script at
  `workers/api/check_hyperdrive_cache.sh`). This is a deploy gate, not API.
- Staging deploy plumbing (worker-build plus per-env wrangler config from
  secrets) is an explicit follow-up slice (`workers/api/README.md:87-88`).

## Source index

- Wired surface: `workers/api/src/handlers.rs:2286-2353`, `workers/api/README.md:37-88`
- Web contract: `workers/api/tests/fixtures/api-web-contract.json`
- Handler-level tests: `workers/api/tests/pr30_findings_acceptance.rs`, `workers/api/tests/request_body_transport_acceptance.rs`, `workers/api/tests/privilege_reauth_acceptance.rs`, `workers/api/tests/rate_limits_acceptance.rs`, `workers/api/tests/security_regressions.rs`, `workers/api/tests/session_cookie_hardening_acceptance.rs`
- Unit-level tests: `workers/api/tests/api_tests.rs`
- Core-only library: `workers/api/src/library.rs`, `workers/api/tests/library_tests.rs`, `workers/api/tests/library_single_row_visibility.rs`
- Deletion constants/fence: `workers/api/src/account_deletion.rs`, `workers/api/tests/account_deletion_acceptance.rs`
- Target surface: `docs/adr/0006-api-surface.md`, `docs/adr/0010-native-rest-credentials.md`
