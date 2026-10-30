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
`workers/api/src/handlers.rs:700`, handled by `qa_test_codes` at
`workers/api/src/handlers.rs:2418`, dispatched at
`workers/api/src/handlers.rs:2535`, deliberately excluded from
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

## Verification note

On the examined head, the wired-route table in `workers/api/README.md:37-49`
matched the dispatcher (`handle` in `workers/api/src/handlers.rs:2516-2538`,
known-path set in `is_known_path` at `workers/api/src/handlers.rs:2471-2490`)
exactly, apart from the deliberate staging-seam omission: `GET /healthz`,
the four passkey routes, the two magic-link routes, the three session
routes, the two reauthentication routes, the three vault routes,
`POST /v1/vault/test` plus its `/v1/vault/test-connection` compatibility
alias, `OPTIONS` on known paths, and a `404` fallback. (`POST
/api/test-codes` is dispatched at `workers/api/src/handlers.rs:2535` but is
not a listed route: it is excluded from `is_known_path` by design.) Both sides of
the web contract are pinned by `workers/api/tests/fixtures/api-web-contract.json`:
the web client asserts the exact wire bodies it sends and the Rust handlers
suite asserts each route is served (`workers/api/README.md:54-58`).

Line anchors above are for the examined head; prefer the cited symbols
(`handle`, `is_known_path`) after main moved.

## Route-family map

| Family | Current on this head (handler + test evidence) | Target (`docs/adr/0006-api-surface.md:22-112`) | Status |
|---|---|---|---|
| Liveness `GET /healthz` | Wired in `handle` (`workers/api/src/handlers.rs:2517` on examined head); dispatched in `workers/api/tests/request_body_transport_acceptance.rs:1074` (via `handlers::handle`) | Not specified in ADR 0006 (no `healthz`/`liveness` text anywhere in `docs/adr`); the versioned resource list (`docs/adr/0006-api-surface.md:26-28`) omits `/healthz`, so treated as transport liveness outside the versioned surface (author inference, not an ADR statement) | Shipped |
| Passkey registration (`register/start`, `register/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2518-2519` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:846-892` (harness calls `handlers::handle` at `workers/api/tests/pr30_findings_acceptance.rs:609`) | Launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:102-103`); ADR 0006 names no `/v1/auth/*` resource, and these unauthenticated calls mint the cookie (author inference — the cited lines state only the launch-auth default) | Shipped |
| Passkey login (`login/start`, `login/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2520-2521` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:893-907` | Same as registration: launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:102-103`); no `/v1/auth/*` resource is named in the ADR (author inference) | Shipped |
| Email magic link (`magic/start`, `magic/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2524-2525` on examined head); exercised through `handlers::handle` in `workers/api/tests/pr30_findings_acceptance.rs:637-653` and module-level issuance/consumption in `workers/api/tests/api_tests.rs` (`auth::issue_magic_link`, no `handlers` dispatch) | Same as registration: launch auth is the existing session cookie (`docs/adr/0006-api-surface.md:102-103`); no `/v1/auth/*` resource is named in the ADR (author inference) | Shipped |
| Session rotation/revocation (`refresh`, `logout`, `revoke-all`) | Wired in `handle` (`workers/api/src/handlers.rs:2526-2528` on examined head); route-level rotation/revocation in `workers/api/src/handlers.rs:3931` (`refresh_rotates_server_row_and_revoke_all_kills_sessions`, via test harness `call_full` through dispatch); contract routes listed in `workers/api/tests/fixtures/api-web-contract.json`; module-level cookie behavior in `workers/api/tests/session_cookie_hardening_acceptance.rs` (`nntune_api::session`, no `handlers` import — not route evidence) | One auth/session layer serves both surfaces (`docs/adr/0006-api-surface.md:116-117`) | Shipped |
| Session-bound reauthentication (`reauthenticate/start`, `reauthenticate/finish`) | Wired in `handle` (`workers/api/src/handlers.rs:2522-2523` on examined head); helper-level session behavior in `workers/api/tests/privilege_reauth_acceptance.rs` (direct `nntune_api::session` calls, no `handlers::handle` dispatch — not route evidence); route-level proof in `workers/api/src/handlers.rs:4404` (`passkey_reauthentication_replaces_only_the_authenticated_session`) and freshness gating in `workers/api/src/handlers.rs:4142` (`vault_mutations_require_fresh_session_but_reads_do_not`) | This is the current freshness proof for vault writes. The separate account-deletion step-up pair (`POST /v1/account/step-up/begin`, `POST /v1/account/step-up/complete`) is target-only (`docs/adr/0006-api-surface.md:51-77`) | Shipped (current scope); account step-up is future |
| Vault (`GET`/`PUT`/`DELETE /v1/vault`, `POST /v1/vault/test` + alias) | Wired in `handle` (`workers/api/src/handlers.rs:2529-2534` on examined head); booleans-only CRUD pinned in `workers/api/src/handlers.rs:3968` (`vault_crud_is_booleans_only_and_owner_scoped`); dispatched reads/writes in `workers/api/tests/request_body_transport_acceptance.rs:1085-1132` and `workers/api/tests/pr30_findings_acceptance.rs:987-1042` (both via `handlers::handle`) | Advanced BYOP management stays REST-only (`docs/adr/0006-api-surface.md:209-210`); compat credentials never reach provider credentials (`docs/adr/0006-api-surface.md:154`); note the naming gap: the ADR resource list (`docs/adr/0006-api-surface.md:26-28`) names `/v1/providers` and `/v1/indexers` (Future below) with no direct `/v1/vault` entry | Shipped |
| CORS preflight (`OPTIONS` on known paths) | Wired in `handle` (`workers/api/src/handlers.rs:2536` on examined head); known-path set in `is_known_path` (`workers/api/src/handlers.rs:2471-2490`); origin rules in `workers/api/README.md:50-53` | Not specified in ADR 0006 (no `preflight`/CORS text in `docs/adr`); the versioned resource list (`docs/adr/0006-api-surface.md:26-28`) covers resources only, so preflight is treated as transport behavior outside the versioned surface (author inference, not an ADR statement) | Shipped |
| Library genres/playlists core | Core rules in `workers/api/src/library.rs` (page cap `workers/api/src/library.rs:51`); behavior tests in `workers/api/tests/library_tests.rs` and `workers/api/tests/library_single_row_visibility.rs`; **no** library path appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2516-2538` on examined head) | Mapped onto `/v1/playlists` with the metadata-only public rule; no parallel `/api/playlists` contract survives (`docs/adr/0006-api-surface.md:172-174`); catalog reads resolve only through caller library membership (`docs/adr/0006-api-surface.md:157-165`) | Core-only; HTTP routes future |
| Account deletion (`DELETE /v1/account`, `cancel-deletion`, `GET /v1/operations/{id}`) | No account/operations path appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2516-2538` on examined head); SQL/constant pins only in `workers/api/src/account_deletion.rs` and `workers/api/tests/account_deletion_acceptance.rs`; the deletion fence is already enforced on vault writers (`workers/api/src/handlers.rs:2234` for upsert, `workers/api/src/handlers.rs:2394` for test-connection, envelope in `workers/api/src/handlers.rs:654`) | `DELETE /v1/account`, `DELETE /v1/libraries/{id}`, `DELETE /v1/playlists/{public_id}`, `POST /v1/account/cancel-deletion`, `GET /v1/operations/{operation_id}` with `202` + operation id, grace window, and typed `409` fences (`docs/adr/0006-api-surface.md:29-101`) | HTTP future; writer fence shipped |
| Native catalog REST (`/v1/{me,library,artists,releases,tracks,playlists,plays,providers,indexers,search,stream}`) | None of these paths appears in the dispatcher (`handle` at `workers/api/src/handlers.rs:2516-2538` on examined head) | The canonical API; `apps/web` uses only this (`docs/adr/0006-api-surface.md:26-28`); versioned `/v1`, breaking changes ship `/v2` alongside (`docs/adr/0006-api-surface.md:104`) | Future |
| Scoped native REST credentials (bearer keys) | Wired issuance surface (route table `workers/api/README.md:61-63`): `POST /v1/credentials` → `credentials_mint` (`workers/api/src/handlers.rs:3646`, fn at `:3129-3245`), `GET /v1/credentials` → `credentials_list` (`:3647`), `DELETE /v1/credentials/{id}` → `credentials_revoke` (`:3652-3654`); exact path in `is_known_path` (`:3524-3544`), dispatched by `handle` (`:3601-3661`). Mint rejects `ttl_secs: 0` (`:3154-3156`) and raw scope lists longer than `ALL_SCOPES.len()` (`:3171-3173`) before any row, audit entry, or secret exists; both map through `validation_failed` (`:3098-3106`) → `ApiError::validation_failed` (`workers/api/src/api_error.rs:163-165`) → 422 `validation_failed` (`workers/api/src/api_error.rs:65-69`; `docs/api-errors.md:85`). Minting itself is `issue_credential` (`workers/api/src/native_rest.rs:242-265`), TTL clamped to `MAX_CREDENTIAL_TTL_SECS` (`:106`, `:258`). Pinned by `zero_ttl_mint_is_validation_failed_and_mints_nothing` and `over_long_scope_list_is_validation_failed_and_mints_nothing` (`workers/api/tests/credential_mint_validation_acceptance.rs:1064-1106`). Mint-time checks gate issuance only: use-time authorization stays with the bearer gate (`authorize` + `scope_for_operation`, `workers/api/src/native_rest.rs:332-366` + `:458-579`), and all three issuance routes are `SessionOnly` in that map (`:488`, `:531`, `:573`), so bearer keys can never mint, list, or revoke. (All `handlers.rs` anchors measured on the merged head: atomic enrollment plus refresh-retry; dispatcher arms and function definitions are cited separately.) | Issuance requires a privilege-fresh session and reuses `reauth_required` (`docs/adr/0010-native-rest-credentials.md:129-132`); expiry is mandatory, user-chosen up to the 365-day maximum (`:133-135`); issuance/revocation of any credential is session-only by construction (`:113-117`) | Issuance shipped; bearer use future (data routes not yet wired — default-deny holds, `workers/api/src/handlers.rs:3659`) |
| Subsonic subset (`/rest/*`: `ping`, `getLicense`, `getMusicFolders`, `getIndexes`, `getMusicDirectory`, `getAlbumList2`, `search3`, `getArtist`, `getAlbum`, `getSong`, `stream`, `getCoverArt`, `getPlaylist(s)`, `scrobble`) | No `/rest/*` path appears anywhere in `workers/api/src/`; the dispatcher fallback is `404` (`handle` at `workers/api/src/handlers.rs:2537` on examined head) | Subsonic subset with error `0` (or `70` where the resource is missing) plus an `nntune-native` hint header (`docs/adr/0006-api-surface.md:105-112`); adapters over the same handlers, REST first (`docs/adr/0006-api-surface.md:203-204`); acceptance contract in `docs/adr/0006-api-surface.md:217-226` | Future (separate compat lane) |
| Generated OpenAPI (`openapi.yaml`) + CI diff | No `openapi.yaml` exists under `workers/api/` on the examined head | Spec is generated from code (Rust types to schema), checked in, CI-diffed so drift fails the build (`docs/adr/0006-api-surface.md:201-202`) | Future (explicitly marked) |

## Deploy and staging seams (not public API)

These exist on the examined head and are deploy/staging seams, cited so no
reader mistakes them for versioned surface. Only the first bullet is
staging-only; the rest are every-environment deploy gates:

- Staging-only (shipped in #224, present on the examined head):
  `POST /api/test-codes` (`QA_TEST_CODES_PATH` at
  `workers/api/src/handlers.rs:700` → `qa_test_codes` at
  `workers/api/src/handlers.rs:2418`, dispatched at
  `workers/api/src/handlers.rs:2535`, deliberately excluded from
  `is_known_path` at `workers/api/src/handlers.rs:2471-2490` so its
  `OPTIONS` never answers 204 — proven by the `OPTIONS`-answers-404 gate
  tests at `workers/api/src/handlers.rs:3409-3415`). Capture requires the
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
- Staging deploy plumbing (worker-build plus per-env wrangler config from
  secrets) is an explicit follow-up slice (`workers/api/README.md:87-88`).

## Source index

- Wired surface: `workers/api/src/handlers.rs:2471-2539` (`is_known_path`, `handle`, plus the `POST /api/test-codes` staging-seam arm; examined-head lines), `workers/api/README.md:37-88`
- Web contract: `workers/api/tests/fixtures/api-web-contract.json`
- Route-level tests (via `handlers::handle`): `workers/api/tests/pr30_findings_acceptance.rs`, `workers/api/tests/request_body_transport_acceptance.rs`, route-level unit tests in `workers/api/src/handlers.rs:5808` (`refresh_rotates_server_row_and_revoke_all_kills_sessions`), `workers/api/src/handlers.rs:5845` (`vault_crud_is_booleans_only_and_owner_scoped`), `workers/api/src/handlers.rs:6332` (`vault_mutations_require_fresh_session_but_reads_do_not`), `workers/api/src/handlers.rs:7297` (`passkey_reauthentication_replaces_only_the_authenticated_session`)
- Module-level tests (no `handlers` import — session/auth/rate-limit/vault behavior, not route evidence): `workers/api/tests/privilege_reauth_acceptance.rs` (`nntune_api::session`), `workers/api/tests/session_cookie_hardening_acceptance.rs` (`nntune_api::session`), `workers/api/tests/rate_limits_acceptance.rs` (`nntune_api::auth`, `nntune_api::rate_limit`), `workers/api/tests/security_regressions.rs` (`nntune_api::{auth, session, vault}`)
- Unit-level tests: `workers/api/tests/api_tests.rs` (`auth::issue_magic_link`, no dispatch)
- Core-only library: `workers/api/src/library.rs`, `workers/api/tests/library_tests.rs`, `workers/api/tests/library_single_row_visibility.rs`
- Deletion constants/fence: `workers/api/src/account_deletion.rs`, `workers/api/tests/account_deletion_acceptance.rs`
- Target surface: `docs/adr/0006-api-surface.md`, `docs/adr/0010-native-rest-credentials.md`
- Native REST credential issuance (function-definition anchors on the merged head): `workers/api/src/handlers.rs:3129-3245` (`credentials_mint`), `:3263-3283` (`credentials_list`), `:3285-3325` (`credentials_revoke`), `workers/api/src/native_rest.rs:242-265` (`issue_credential`), `workers/api/tests/credential_mint_validation_acceptance.rs:1064-1106` (zero-TTL + over-long scope-list pins)
- Staging seam (on the examined head): `docs/security/threat-model.md` section 14, `QA_TEST_CODES_PATH` (`workers/api/src/handlers.rs:700`) / `qa_test_codes` (`workers/api/src/handlers.rs:2418`)
