# Decision 0004 — Owner product model (2026-11-03, final)

Date: 2026-11-03. Provenance: owner product-model decisions recorded
in the approved programme document (version 1.4) and the near-term
roadmap revision of 2026-11-03. Final; not re-litigated by the
roadmap revisions built on it.

## Context

Earlier plans assumed users bring their own provider accounts and
upload their own archives. The owner replaced that model with a
shared, curated catalogue.

## Decision

- **Backend-only providers.** Users never see providers, archives or
  indexers. The owner supplies curated archives of lossless albums via
  admin ingest; the backend account pool fetches through service-owned
  accounts. No provider wizard, no user provider credentials, no user
  archive upload or URL intake — those surfaces are removed, not
  deferred.
- **Playback order.** (1) lossless audio from the curated collection
  over the backend collection, (2) official video embed, (3)
  unavailable (hidden by default). The video embed is also the
  music-video source for every track.
- **Invite-only paywall.** At first only the owner invites; later,
  paid members with sufficient membership may receive invites valid
  only during scheduled windows. Staging subscriptions use payment
  test mode; nothing activates live billing. Plans carry stream-hour
  caps per month; at the cap, paid playback stops.
- **Availability states.** Unavailable tracks are hidden by default.
  An admin setting may show them greyed out, with a placeholder where
  a future request action goes.
- **Backend additions.** Invite management, admin ingest (upload,
  public-database auto-match, review list for unmatched items),
  re-post (M2 — download, match, tag, rename, re-post; keep the
  original until the new post verifies), backend account pool, and a
  shared popularity cache (after M1; supersedes per-user cache scope).
- **Removed from the plan records.** Free trial, library-track and
  ingest-job caps, per-user cache budgets and grants. Billing
  provider plus stream-hour meter plus single-stream concurrency
  (set to one) are kept.

## Why

A shared curated catalogue removes user-provider complexity and keeps
licence, cost and abuse controls inside the service boundary. The
re-post and popularity-cache work is sequenced behind M1 so the exit
journey does not wait on it.
