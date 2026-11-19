# ADR 0014: Backend provider account pool

- **Status:** Superseded by [ADR 0015](0015-standard-s3-storage-boundary.md)
  (2026-11-13); retained as historical design, not new provider-pool authority.
- **Date:** 2026-11-03
- **Replaces:** the per-user provider model of the BYOP era (one
  `ProviderConfig` in a user's vault, a `providers` row minted per user
  save, a per-user NNTP pool keyed to that account).

- **Amended by:** [ADR 0015](0015-standard-s3-storage-boundary.md) (approved
  target, 2026-11-13): the storage provider is to own NNTP access, posting,
  accounts and repair, and NNTune is to hold no NNTP accounts. This record
  stays as history of the merged design; nothing is migrated or removed yet.

## Context

Users no longer bring Usenet providers. All NNTP traffic — streaming
misses, admin ingest, popularity-cache fills (ADR 0013), health probes,
and later re-posting of the curated collection — goes through provider
accounts the service itself holds, spread across several providers. Only
some of those accounts may post. The concrete provider list, per-account
connection allowances and posting rules are still being researched by the
operator, so this record designs for a pool of **unknown size and mixed
capability**: everything provider-specific must land as configuration,
never as schema or code changes.

Existing pieces this design reuses rather than replaces:

- The NNTP transport and protocol core (`crates/nntune-core`, now a
  re-export of the velmora engine: `nntp.rs`, `transport.rs`,
  `pinned_transport.rs`, yEnc/PAR2/segment identity) —
  transport-injected, no user coupling.
- The envelope encryption built for provider credentials (KEK-versioned,
  ADR 0008 lineage) — the same envelope homes pool account secrets; only
  the owner changes, from per-user vault rows to service accounts.
- The connection-pool and circuit-breaker logic in the stream worker
  (`workers/stream/src/pool.rs`: capacity clamp, 3-failures-in-30s
  breaker) and the reserve/finalize/invalidate shape modelled in
  `provider_authority_store.rs` (currently inert and not installed —
  this ADR is the design that installs a production version of that
  idea, it does not bless the model file as-is).
- The synthetic NNTP test server (`crates/nntune-test-nntp`, a re-export
  of velmora's `velmora-test-nntp`) for pool
  behaviour tests without real Usenet access.

What does not exist yet: any pool registry, any account selection, any
posting support in the transport (no `POST`/`IHAVE`), and any
cross-account failover. The stream worker's provider lookup is a stub.

## Decision

**A config-driven registry of service-owned provider accounts, with
per-account connection budgets enforced by lease, work-class
reservations so background traffic can never starve playback, and
segment-level failover across accounts and backbones.**

### Account registry

One row per account in `pool_providers`:

| Column | Meaning |
|---|---|
| `id` | stable account identity used for attribution and stats |
| `label` | operator-facing name (no secret material) |
| `host`, `port`, `tls` | endpoint; TLS-first, ports restricted to the existing NNTP allowlist |
| `username` | account login (not secret on its own, still never logged) |
| `secret_ref` | reference to the envelope-encrypted credential; plaintext never in the DB, config files, or logs |
| `can_read`, `can_post` | capability flags; posting is a per-account opt-in subset |
| `max_connections` | hard cap, set at or below what the provider allows this account |
| `backbone` | operator-assigned tag; accounts on one backbone share retention/completion behaviour |
| `tier` | preference order for ordinary reads (lower is preferred) |
| `enabled` | operator kill switch, independent of health state |

Adding, disabling or re-capping an account is a data change. The pool
must work correctly with exactly one read account (the launch profile)
and with many; no code path may assume a second account exists.

Credentials are service credentials: created and rotated by the operator
through an admin-only path, stored only as envelope ciphertext, decrypted
in memory at connection time, and never exposed to users, clients or the
Subsonic surface. KEK rotation re-wraps pool rows the same way it did
vault rows.

### Connection budgeting

- The unit of budgeting is a **leased connection slot** on one account,
  held for one fetch session with a TTL and released on completion.
  Leases are coordinated by a Durable Object per account (evolving the
  existing per-user pool DO into a per-account one), so the cap holds
  across concurrent Worker invocations rather than per isolate.
- Work is divided into budget classes with configurable shares of each
  account's slots:

  | Class | Traffic | Rule |
  |---|---|---|
  | `stream` | user playback misses | reserved headroom on every read account; never borrowed by other classes |
  | `ingest` | admin ingest fetches | bounded share |
  | `cache_fill` | ADR 0013 popularity fills | lowest priority; only uses slots idle beyond the `stream` reserve; preemptible between articles |
  | `health` | probes and post verification reads | small fixed share |
  | `post` | re-post uploads | posting-capable accounts only; never borrows the `stream` reserve |

- If no slot is available in class, `stream` requests wait briefly then
  fail as a retryable error (never as "not found"); background classes
  back off and retry later. Queueing is bounded — the pool sheds
  background load before it ever degrades playback.
- Per-account caps are hard. No class arithmetic, retry loop or failover
  path may open more simultaneous connections to an account than
  `max_connections`. This is both a provider-terms rule and the
  anti-hammering guarantee behind ADR 0013.

### Selection and failover

- A fetch (an article or a segment batch) chooses among **healthy,
  enabled** accounts by tier, then fewest in-flight leases. Per-article
  stats (completion, latency, error class) are recorded per account and
  feed health scoring; they contain no user data.
- A missing article (430) is not immediately "unavailable": the fetch
  retries on another account, preferring a different `backbone`, before
  the track is declared unavailable upstream. Segment-level repair
  (PAR2) runs only after cross-account attempts, as today.
- Failure handling is per error class:
  - **Auth failure** quarantines the account (breaker open, operator
    alert) and never retry-loops credentials — repeated bad logins are
    how accounts get banned.
  - **Connection/timeout/TLS failures** open the per-account circuit
    breaker with exponential cooldown and half-open probes (the existing
    3-in-30s breaker is the starting shape).
  - **Rate/abuse responses** (for example 481/482-class denials) open
    the breaker for a long cooldown and alert; the pool treats them as a
    budgeting bug to fix, not a condition to push through.
- Health probes (scheduled, `health` class) exercise group/stat against
  a known-article set per account so a dead account is discovered by the
  probe, not by a user's play.

### Posting subset

- Only `can_post` accounts are eligible for the `post` class; the read
  path is never blocked on posting state, and read-only accounts are
  never attempted for posting even if the provider would accept it.
- Post verification reads the article back through a *different* account
  where one exists (same-account readback is the fallback, recorded as
  such), under the `health` class.
- Posting support in the transport (`POST` with the existing yEnc
  encoder's counterpart) is new build work gated on the re-post design;
  this ADR fixes only the pool contract it will consume: capability
  flags, budget class, and verification. Nothing here requires posting
  to exist at launch.

### Attribution and audit

Every ingest job, cache fill and stream fetch records which account(s)
served it (the existing `provider_id` attribution in ingest moves from
user-minted provider rows to `pool_providers.id`). Account create /
disable / cap-change / credential-rotation events go to the audit log.
Per-account daily counters (articles, bytes, error classes) are the
operator's evidence that budgeting works and the input to resizing the
pool; they are service telemetry, never user-visible.

### Playback source order (consumption contract)

This record states its own consumption contract, self-contained, so
it can be read without following the cross-reference below. The pool
serves the NNTP step of this playback resolver order: **curated FLAC — R2 popularity
cache (ADR 0013) first, then NNTP through this pool — then the YouTube
embed, then unavailable**. A pool miss after cross-account failover is
what "unavailable over NNTP" means in that order; it falls through to
YouTube, never to a user-supplied source (none exist). The
PlayableSource record (ADR 0012) is accepted and published (merged in
#886) and is the authoritative home for this order; the order stated
above matches it. If ADR 0012 is ever revised to state a different
order, this section is revised to match in the same change that
adopts it.

**REA angle:** study how shipped open-source Usenet downloaders
operate multi-server pools (per-server connection caps, priority and
failover across servers, handling of missing-article and rate/abuse
responses) — source first, REA only for behaviour the source does not
settle — before the selection/failover implementation slice fixes its
breaker and backoff parameters. That study informs those parameters;
it is not a blocker for this record.

## Alternatives

- **One hard-coded account:** rejected as the design (it remains the
  launch configuration). Single-account completion and outages would
  make catalogue availability a single provider's property, and adding
  an account later would be a redesign instead of a row insert.
- **Keep per-user pools pointed at one service account:** rejected. It
  preserves BYOP shapes (per-user caps as `min(provider, plan)`,
  user-minted provider rows) whose semantics no longer exist, and makes
  the account cap unenforceable globally.
- **Third-party download/indexer service instead of raw NNTP:** rejected
  for now — new spend and a new trust boundary to solve a problem the
  existing transport already solves. Revisit only with measured pool
  pain.

## Consequences and implementation alignment

Accepted design, not shipped behaviour. Required follow-up, each its own
gated change:

1. schema + admin-only registry (`pool_providers`, audit events);
2. per-account lease DO with class shares, replacing the per-user pool
   keying; stream/ingest/cache-fill call sites acquire by class;
3. selection, breaker and cross-backbone failover, with per-account
   stats; ingest attribution re-pointed at pool accounts;
4. health probes and operator alerting on quarantine;
5. posting transport support only when the re-post design lands.
6. Tests (against the synthetic NNTP server) must cover: cap never
   exceeded under class contention, `stream` reserve respected while
   cache-fill saturates, auth-failure quarantine without credential
   retry, 430 failover across backbones, single-account degenerate
   pool, and kill-switch behaviour mid-flight. Security review covers
   credential storage/rotation and log redaction before launch.

Open inputs that arrive as configuration, not redesign: the provider
list, each account's allowed connection count, backbone grouping, and
which accounts may post.
