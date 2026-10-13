# NNTune v2 operations

Provisioned resource IDs for staging + prod. **IDs only — never secrets.**
Secret values live exclusively in GitHub Actions secrets, Cloudflare, and
`~/secure-drop` on the host. This file must never contain a token, password,
or connection string (the `gitleaks` gate enforces this).

Provisioning card: internal tracker `TOG-23628` (see board; not for PR titles).

## Cloudflare

- Account: `Rick.dugger@gmail.com's Account`
- Account ID: `209cf7dd678adfb683947dd7874d05af`
  (short prefix `209cf7dd`; full ID is the `CLOUDFLARE_ACCOUNT_ID` secret value)
- Plan check: Workers Paid + R2 Paid entitlements — verified via host executor
  (see card; operator MCP token is read/write-scoped, not billing-scoped).

### R2 buckets (all created 2026-10-13, TOG-23628)

| Bucket | Env | Purpose |
|---|---|---|
| `nntunev2-audio-staging` | staging | audio cache |
| `nntunev2-audio-prod` | prod | audio cache |
| `nntunev2-art-staging` | staging | artwork cache |
| `nntunev2-art-prod` | prod | artwork cache |
| `nntunev2-archive-staging` | staging | archive cache |
| `nntunev2-archive-prod` | prod | archive cache |

UNDO: `wrangler r2 bucket delete nntunev2-<name>` (or API
`DELETE /accounts/<acct>/r2/buckets/<name>`), one per bucket.

### Queues (created 2026-10-13, TOG-23628)

| Queue | ID | Env |
|---|---|---|
| `nntunev2-ingest-staging` | `c9e6afdea0ca487c8d20f799a052b045` | staging |
| `nntunev2-ingest-prod` | `2bf7e1a0edd347c9aada2afbc0c24ae5` | prod |

UNDO: API `DELETE /accounts/<acct>/queues/<queue_id>`.

### KV namespaces (created 2026-10-13, TOG-23628)

| Namespace | ID | Env |
|---|---|---|
| `nntunev2-config-staging` | `1d4f21508a214ebfa8ff7b594989d6f6` | staging |
| `nntunev2-config-prod` | `9664ceed71464521bb6baae2d9374006` | prod |

UNDO: API `DELETE /accounts/<acct>/storage/kv/namespaces/<id>`.

### DNS (zone `nntune.com`, id `97e268d5c1d540d42545ed8ab386b9c4`)

- `v2-staging.nntune.com` → `AAAA 100::`, proxied, TTL auto.
  Record ID `97670fbad3e38106c28f71d1df89175e`.
  Placeholder address (`100::` is reserved discard space); the scaffold card's
  staging Worker claims this hostname via a Workers custom domain and the
  placeholder record is then removed. v1 `staging.nntune.com` is untouched.
- Prod hostname: not created yet — decided at launch (sibling scaffold card).

UNDO: API `DELETE /zones/<zone>/dns_records/<record_id>`.

### Cloudflare Access (staging)

- Target: `v2-staging.nntune.com` behind Access, mirroring v1
  `staging.nntune.com`: owner OTP (email `rick.dugger@gmail.com` via the
  account's cloudflare IdP `a290d6fa-9235-4ce3-921f-0c9a67916c20`) + QA
  service token `two-web staging QA (Paperclip agents)`
  (id `efe71168-f5e9-4f42-83bf-340473e76e01`, client id suffix `.access` —
  ID only, not the token secret; reused, never rotated).
- Status: creation via operator MCP failed twice on a tag policy error
  (`tags must be created before assigning`); queued as a privileged host
  action (see card). App ID recorded here once created.

UNDO: delete the Access app `NNTune v2 staging (nntunev2-staging)` in
Zero Trust → Access → Applications.

### CI API token + GitHub secrets

- Token name `nntunev2-ci`, scoped: Workers Scripts/Routes edit, R2 edit,
  Queues edit, KV edit, Hyperdrive edit; D1 none; Pages none; account-scoped
  to `209cf7dd…`. Created fresh — existing keys never rotated.
- Wired to repo secrets `CLOUDFLARE_API_TOKEN` + `CLOUDFLARE_ACCOUNT_ID`
  by the privileged host executor (token value never touches chat, cards,
  or this repo). Status: queued — see card.

UNDO: delete token `nntunev2-ci` in Cloudflare → Manage Account →
API Tokens; delete the two repo secrets if decommissioning.

## PlanetScale Postgres (blocked: creds file missing as of 2026-10-13)

Expected input: `~/secure-drop/planetscale-api.env` on host `rbx1`.
Until the owner places it, nothing below can run.

Plan (unchanged):

- Region: US (owner users).
- Databases: `nntunev2` (branches `staging`, `main`) and `nntunev2-mb`
  (MusicBrainz mirror, sized ~100 GB).
- Roles per database: `migrator`, `runtime`, `ro` (least privilege;
  Hyperdrive binds the `runtime` role only).
- Hyperdrive configs per env (`nntunev2-staging`, `nntunev2-prod`,
  `nntunev2-mb-staging`, `nntunev2-mb-prod`) bound to the `runtime` role;
  IDs recorded here once created.

UNDO: delete Hyperdrive configs first (unbind), then PlanetScale branches/
databases. Never delete `main` data without a fresh backup + owner sign-off.

## Environments matrix

| Concern | staging | prod |
|---|---|---|
| R2 audio | `nntunev2-audio-staging` | `nntunev2-audio-prod` |
| R2 art | `nntunev2-art-staging` | `nntunev2-art-prod` |
| R2 archive | `nntunev2-archive-staging` | `nntunev2-archive-prod` |
| Queue | `nntunev2-ingest-staging` | `nntunev2-ingest-prod` |
| KV | `nntunev2-config-staging` | `nntunev2-config-prod` |
| Hostname | `v2-staging.nntune.com` (+ Access) | TBD at launch |
| Hyperdrive | `nntunev2-staging` (pending) | `nntunev2-prod` (pending) |
| PlanetScale branch | `staging` (pending) | `main` (pending) |
