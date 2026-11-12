# API delivery map: current vs target

Examined head: `d3563f5b` (re-verify with `git rev-parse HEAD`; the previous
verification baselined `d583c9b5`). This map proposes no endpoint, auth, or
policy design: the Current column restates wired code and its tests, the
Target column restates accepted ADRs where they speak (rows the ADR does not
address are marked `Not specified in ADR 0006` with the author's inference
kept separate), and Status names the gap between them.

Delta since the previous examined head `d583c9b5` (no rebase per
review-round rules): exactly one commit touched the dispatcher (#224),
adding a `POST /api/test-codes` arm (`QA_TEST_CODES_PATH` at
`workers/api/src/handlers.rs:715`, handled by `qa_test_codes` at
`workers/api/src/handlers.rs:2438`, dispatched at
`workers/api/src/handlers.rs:2555`, deliberately excluded from
`is_known_path` so its `OPTIONS` never answers 204). That seam is admitted
only behind the staging gates documented in
`docs/security/threat-model.md` section 14. Every other dispatcher arm is
unchanged, shifted in line only; the README route table
(`workers/api/README.md:37-48`) still lists only the versioned surface plus
liveness, so the seam stays a deploy/staging seam covered in the seams
section below, not public API.

Credentials-row delta (anchored to `1059d7f4`, not current main): the
issuance surface in the scoped-credentials row landed after the examined
head (#239) and the mint rejections later still (#544), so that row
alone is anchored to that revision's lines; every other row keeps the
examined-head anchors above.

## User-provider retirement delta

The historical snapshot below is superseded for user providers by ADR 0014.
`/v1/vault`, `/v1/vault/test` and its compatibility alias are now absent from
dispatch, the native scope map, preflight and served-path logging. They return
the normal unknown-route 404. User provider minting and store writers are
removed; envelope crypto and KEK rotation for remaining rows are retained.
The scoped native credential row and source index below use current symbols.

## Verification note

On the examined head, the wired-route table in `workers/api/README.md:37-49`
matched the dispatcher (`handle` in `workers/api/src/handlers.rs:2536-2558`,
known-path set in `is_known_path` at `workers/api/src/handlers.rs:2491-2510`)
exactly, apart from the deliberate staging-seam omission: `GET /healthz`,
the four passkey routes, the two magic-link routes, the three session
routes, the two reauthentication routes, the three vault routes,
`POST /v1/vault/test` plus its `/v1/vault/test-connection` compatibility
alias, `OPTIONS` on known paths, and a `404` fallback. (`POST
/api/test-codes` is dispatched at `workers/api/src/handlers.rs:2540` but is
not a listed route: it is excluded from `is_known_path` by design.) Both sides of
the web contract are pinned by `workers/api/tests/fixtures/api-web-contract.json`:
the web client asserts the exact wire bodies it sends and the Rust handlers
suite asserts each route is served (`workers/api/README.md:54-58`).

Line anchors above are for the examined head; prefer the cited symbols
(`handle`, `is_known_path`) after main moved.

## Route-family map

| Family | Current on this head (handler + test evidence) | Target (`docs/adr/0006-api-surface.md:34-128`, as revised 2026-11-03) | Status |
|---|---|---|---|
| Liveness `GET /healthz` | Wired in `handle` (`workers/api/src/handlers.rs:2537` on examined head); dispatched in `workers/api/tests/request_body_transport_acceptance.rs:1074` (via `handlers::handle`) | Not specified in ADR 0006 (no `healthz`/`liveness` text anywhere in `docs/adr`); the versioned resource list (`docs/adr/0006-api-surface.md:38-44`) omits `/healthz`, so treated as transport liveness outside the versioned surface (author inference, not an ADR statement) | Shipped |
| Passkey registration (`register/start`, `register/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2538-2539` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:846-892` (harness calls `handlers::handle` at `workers/api/tests/pr30_findings_acceptance.rs:609`) | Launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:118-119`); ADR 0006 names no `/v1/auth/*` resource, and these unauthenticated calls mint the cookie (author inference — the cited lines state only the launch-auth default) | Shipped |
| Passkey login (`login/start`, `login/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2540-2541` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:893-907` | Same as registration: launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:118-119`); no `/v1/auth/*` resource is named in the ADR (author inference) | Shipped |
| Email magic link (`magic/start`, `magic/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2544-2545` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:637-653` and module-level issuance/consumption in `workers/api/tests/api_tests.rs` (`auth::issue_magic_link`, no `handlers` dispatch) | Same as registration: launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:118-119`); no `/v1/auth/*` resource is named in the ADR (author inference) | Shipped |
| Session rotation/revocation (`refresh`, `logout`, `revoke-all`) | Wired in `handle` (`workers/api/src/handlers.rs:2546-2548` on examined head); route-level rotation/revocation in `workers/api/src/handlers.rs:4052` (`refresh_rotates_server_row_and_revoke_all_kills_sessions`, via test harness `call_full` through dispatch); contract routes listed in `workers/api/tests/fixtures/api-web-contract.json`; module-level cookie behavior in `workers/api/tests/session_cookie_hardening_acceptance.rs` (`nntune_api::session`, no `handlers` import — not route evidence) | One auth/session layer serves both surfaces (`docs/adr/0006-api-surface.md:132-133`) | Shipped |
| Session-bound reauthentication (`reauthenticate/start`, `reauthenticate/finish`) | Wired in `handle`; route-level proof in `workers/api/src/handlers.rs` (`passkey_reauthentication_replaces_only_the_authenticated_session`); helper-level session behavior in `workers/api/tests/privilege_reauth_acceptance.rs` (direct `nntune_api::session` calls, not route evidence) | The general privilege-change mechanism remains after user vault removal; pool credentials are admin-only and never session-reachable. The separate account-deletion step-up pair (`POST /v1/account/step-up/begin`, `POST /v1/account/step-up/complete`) is target-only (`docs/adr/0006-api-surface.md:67-104`) | Shipped (current scope); account step-up is future |
| Retired user-provider routes (`/v1/vault`, test and alias) | Absent from `handle`, `is_known_path` and `scope_for_operation`; negative route and side-effect matrix in `workers/api/src/handlers.rs` (`retired_vault_routes_are_plain_not_found_without_side_effects`) | **Retired target (2026-11-03):** no user credential endpoints exist in the target surface. Users supply no provider/indexer credentials; pool accounts are operator configuration (ADR 0014) reachable only via an admin-only path, and there is no `/v1/providers`, `/v1/indexers` or `/v1/vault` resource (`docs/adr/0006-api-surface.md:38-44`, [Consequences](adr/0006-api-surface.md#consequences)); compat credentials never reach pool credentials (`docs/adr/0006-api-surface.md:169-170`) | Removed; normal 404 (stored user credentials await purge) |
| CORS preflight (`OPTIONS` on known paths) | Wired in `handle` (`workers/api/src/handlers.rs:2556` on examined head); known-path set in `is_known_path` (`workers/api/src/handlers.rs:2491-2510`); origin rules in `workers/api/README.md:50-53` | Not specified in ADR 0006 (no `preflight`/CORS text in `docs/adr`); the versioned resource list (`docs/adr/0006-api-surface.md:38-44`) covers resources only, so preflight is treated as transport behavior outside the versioned surface (author inference, not an ADR statement) | Shipped |
| Library genres/playlists core | Core rules in `workers/api/src/library.rs` (page cap `workers/api/src/library.rs:51`); behavior tests in `workers/api/tests/library_tests.rs` and `workers/api/tests/library_single_row_visibility.rs`; **no** library path appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2536-2558` on examined head) | Mapped onto `/v1/playlists` with the metadata-only public rule; no parallel `/api/playlists` contract survives (`docs/adr/0006-api-surface.md:188-190`); catalog reads resolve only through caller library membership (`docs/adr/0006-api-surface.md:173-181`) | Core-only; HTTP routes future |
| Account deletion (`DELETE /v1/account`, `cancel-deletion`, `GET /v1/operations/{id}`) | No account/operations path appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2536-2558` on examined head); SQL/constant pins only in `workers/api/src/account_deletion.rs` and `workers/api/tests/account_deletion_acceptance.rs`; the former user vault writers are now removed (see retirement delta above) | `DELETE /v1/account`, `DELETE /v1/libraries/{id}`, `DELETE /v1/playlists/{public_id}`, `POST /v1/account/cancel-deletion`, `GET /v1/operations/{operation_id}` with `202` + operation id, grace window, and typed `409` fences (`docs/adr/0006-api-surface.md:45-117`) | HTTP future; user vault writers removed |
| Native catalog REST (`/v1/{me,library,artists,releases,tracks,playlists,plays,search,stream}`) | None of these paths appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2536-2558` on examined head). Changed after the examined head: the catalogue read arms `GET /v1/library`, `/v1/artists`, `/v1/releases`, `/v1/releases/{id}` and `/v1/tracks` (plus the `GET /api/library` legacy alias) are now dispatched (`catalogue_library`, `catalogue_library_alias`, `catalogue_artists`, `catalogue_releases`, `catalogue_release`, `catalogue_tracks` in `workers/api/src/handlers.rs`); this slice makes no new delivery claim for the remaining paths | The canonical API; `apps/web` uses only this (`docs/adr/0006-api-surface.md:38-44` — `providers`/`indexers` dropped 2026-11-03; no user provider, indexer or NZB resource exists, NZB intake is admin-only ingest); versioned `/v1`, breaking changes ship `/v2` alongside (`docs/adr/0006-api-surface.md:120`) | Catalogue reads wired in source; staging acceptance pending |
| Scoped native REST credentials (bearer keys) | Wired session-only issuance in `workers/api/src/handlers.rs`: `credentials_mint` (`workers/api/src/handlers.rs:4194`), `credentials_list`, `credentials_revoke`; dispatch at `workers/api/src/handlers.rs:7213`. Zero TTL and over-long scope lists fail before storage, audit or secret issuance, pinned by `zero_ttl_mint_is_validation_failed_and_mints_nothing` and `over_long_scope_list_is_validation_failed_and_mints_nothing` (`workers/api/tests/credential_mint_validation_acceptance.rs:1256-1298`). Use-time authorization stays with `authorize` and `scope_for_operation` in `workers/api/src/native_rest.rs`; bearer keys cannot mint, list or revoke credentials. | Fresh-session issuance and revocation, mandatory expiry up to 365 days (`docs/adr/0010-native-rest-credentials.md`) | Issuance shipped; bearer reads wired for the catalogue routes `GET /v1/library`, `/v1/artists`, `/v1/releases`, `/v1/releases/{id}` and `/v1/tracks` (dispatch arms `catalogue_library`, `catalogue_artists`, `catalogue_releases`, `catalogue_release`, `catalogue_tracks`); every other bearer data route default-denies until wired |
| Subsonic subset (`/rest/*`: `ping`, `getLicense`, `getMusicFolders`, `getIndexes`, `getMusicDirectory`, `getAlbumList2`, `search3`, `getArtist`, `getAlbum`, `getSong`, `stream`, `getCoverArt`, `getPlaylist(s)`, `scrobble`) | Changed after the examined head (verified at `82b622f2`): `/rest` and `/rest/*` route to the `nntune-subsonic` dispatcher ahead of the default dispatch (`is_subsonic_request`, `workers/api/src/lib.rs:130-149`), served over `MemStore::empty()` (`workers/api/src/lib.rs:233`), which holds no credentials or catalogue; the `staging-api` generated config routes no `/rest/*` pattern, so the surface is not reachable on staging | Subsonic subset with error `0` (or `70` where the resource is missing) plus an `nntune-native` hint header (`docs/adr/0006-api-surface.md:121-128`); adapters over the same handlers, REST first (`docs/adr/0006-api-surface.md:224-226`); acceptance contract in [ADR 0006 Implementation alignment](adr/0006-api-surface.md#consequences) | Shell dispatched in source only (not routed on staging); persistent library-backed store future |
| Generated OpenAPI (`openapi.yaml`) + CI diff | No `openapi.yaml` exists under `workers/api/` on the examined head | Spec is generated from code (Rust types to schema), checked in, CI-diffed so drift fails the build (`docs/adr/0006-api-surface.md:222-223`) | Future (explicitly marked) |

## Deploy and staging seams (not public API)

These exist on the examined head and are deploy/staging seams, cited so no
reader mistakes them for versioned surface. Only the first bullet is
staging-only; the rest are every-environment deploy gates:

- Staging-only (shipped in #224, present on the examined head):
  `POST /api/test-codes` (`QA_TEST_CODES_PATH` at
  `workers/api/src/handlers.rs:715` → `qa_test_codes` at
  `workers/api/src/handlers.rs:2438`, dispatched at
  `workers/api/src/handlers.rs:2555`, deliberately excluded from
  `is_known_path` at `workers/api/src/handlers.rs:2491-2510` so its
  `OPTIONS` never answers 204 — proven by the `OPTIONS`-answers-404 gate
  tests at `workers/api/src/handlers.rs:3429-3435`). Capture requires the
  explicit `QA_CODE_SEAM` staging flag plus an `example.test` recipient.
  Retrieval requires the `QA_CODE_SEAM` staging flag, the staged
  `QA_AUTH_TOKEN` bearer, and an `example.test` mailbox with an issued,
  unexpired code; every other posture
  answers 404/401 with no detail, and production (flag and bearer never set)
  serves plain 404 (`docs/security/threat-model.md` section 14).
- Magic-link delivery needs the operator's `EMAIL_WEBHOOK_URL`; without it
  links are issued but not delivered (`workers/api/README.md:83-85`). The
  webhook payload shape is a mailer concern, not a `/v1` route. Applies in
  every environment, not staging-only.
- The Worker refuses traffic unless `HYPERDRIVE_CACHING_DISABLED=1` is set for
  the env, after `./check_hyperdrive_cache.sh --config-id <id>` passes for
  that Hyperdrive config (`workers/api/README.md:175-195`; script at
  `workers/api/check_hyperdrive_cache.sh`). This is a deploy gate, not API.
  Applies in every environment, not staging-only.
- Changed after the examined head (verified at `82b622f2`): staging deploy
  runs in the `staging-api` job of `.github/workflows/deploy.yml`.

## Source index

- Wired surface: `workers/api/src/handlers.rs:2491-2559` (`is_known_path`, `handle`, plus the `POST /api/test-codes` staging-seam arm; examined-head lines), `workers/api/README.md:37-88`
- Web contract: `workers/api/tests/fixtures/api-web-contract.json`
- Route-level tests (via `handlers::handle`): `workers/api/tests/pr30_findings_acceptance.rs`, `workers/api/tests/request_body_transport_acceptance.rs`, and `workers/api/src/handlers.rs` (`refresh_rotates_server_row_and_revoke_all_kills_sessions`, `passkey_reauthentication_replaces_only_the_authenticated_session`, `retired_vault_routes_are_plain_not_found_without_side_effects`).
- Module-level tests (no `handlers` import — session/auth/rate-limit/vault behavior, not route evidence): `workers/api/tests/privilege_reauth_acceptance.rs` (`nntune_api::session`), `workers/api/tests/session_cookie_hardening_acceptance.rs` (`nntune_api::session`), `workers/api/tests/rate_limits_acceptance.rs` (`nntune_api::auth`, `nntune_api::rate_limit`), `workers/api/tests/security_regressions.rs` (`nntune_api::{auth, session, vault}`)
- Unit-level tests: `workers/api/tests/api_tests.rs` (`auth::issue_magic_link`, no dispatch)
- Core-only library: `workers/api/src/library.rs`, `workers/api/tests/library_tests.rs`, `workers/api/tests/library_single_row_visibility.rs`
- Deletion constants/fence: `workers/api/src/account_deletion.rs`, `workers/api/tests/account_deletion_acceptance.rs`
- Target surface: `docs/adr/0006-api-surface.md`, `docs/adr/0010-native-rest-credentials.md`
- Native REST credential issuance: `workers/api/src/handlers.rs` (`credentials_mint`, `credentials_list`, `credentials_revoke`), `workers/api/src/native_rest.rs` (`issue_credential`), `workers/api/tests/credential_mint_validation_acceptance.rs:1256-1298` (zero-TTL + over-long scope-list pins).
- Staging seam (on the examined head): `docs/security/threat-model.md` section 14, `QA_TEST_CODES_PATH` (`workers/api/src/handlers.rs:715`) / `qa_test_codes` (`workers/api/src/handlers.rs:2438`)
