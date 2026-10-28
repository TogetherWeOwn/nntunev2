# Returning-listener evidence contract v1 (proposal)

**Status: PROPOSED. Real-data collection and counting: BLOCKED.** This is a review artifact, not permission to instrument, collect, retain, access, or report listener data. Product and Security must independently approve the exact contract and its unresolved decisions before any real-data implementation is proposed. Legal or other policy review may also be required. There are no real participant records or secrets in this document. The human baseline is **UNKNOWN / NOT MEASURABLE**, not zero; there is no verified listener demand.

## Purpose and boundaries

Determine whether an eligible, consenting human independently returns after a first journey, with the least evidence necessary. The roadmap's proposed measure is the number of distinct eligible humans with a qualifying return completion in `(as_of − 30 days, as_of]` UTC and `0 < return_time − first_time ≤ 7 days`. A first journey is authenticated provider setup, selection of eligible account-owned content, verified advancing decoded audio, and a successful HTTP Range seek. A return requires advancing decoded audio in a separate listener-initiated session. Distinct accounts are not necessarily distinct humans. The first qualifying journey must be demonstrably earlier; a return receipt alone is insufficient. The return completion time determines the rolling window.

The synthetic-only funnel validation is a separate workstream. Its labeled fixtures may model these logical fields and outcome states without real identities or data. This document neither implements that query nor authorizes turning synthetic examples into participant collection. Any mismatch in definitions must be resolved in review, not silently treated as agreement.

## Minimal proposed logical evidence

These are logical requirements for discussion, not an implemented schema, a permission to emit events, or instructions to copy secrets into logs. All identifiers below are opaque, purpose-scoped, random or securely keyed pseudonyms; they are still potentially personal data. Exact encoding, issuer, assurance and trust boundaries require review. A restricted receipt can attest a fact without embedding raw evidence. Each evidence item must have a schema version and an integrity-protected provenance reference to a trusted issuer; a client assertion alone is not proof.

| Record | Minimum required fields / attested facts |
| --- | --- |
| Journey receipt | `schema_version`, opaque `receipt_id` for deduplication, `purpose_id`, pseudonymous `human_key`, `account_key`, `session_key`, `item_key`, and `build_id`; UTC timestamps for first-journey completion and the required steps; authenticated provider setup, selection of eligible account-owned content, verified decoded-audio advancement, and successful HTTP Range seek; an issuer and evidence/provenance reference for each attestation. No provider credentials or content titles. |
| Return receipt | Same version, receipt, purpose, human/account/session/item/build pseudonyms and trusted provenance; UTC return-playback completion timestamp; attested authenticated listener-initiated session and verified decoded-audio advancement. The session identifier must differ from the first journey's and be supported by independent initiation evidence, not merely a rotated identifier. No seek claim is required for the return. |
| Consent decision | Restricted opaque consent reference, purpose and version of notice/terms, trusted decision issuer/source, effective time, scope, and status including withdrawal/revocation effective time. Only a valid affirmative consent covering this specific measurement at the relevant time is eligible; absence, uncertainty, expiry or withdrawal must never be inferred as consent. |
| Account and human classification | Restricted opaque classification reference and trusted issuer/source, decision and effective time, evidence/assurance category, and review/revocation status. A positive eligible-human determination and a reviewed human-level correlation are required, not simply a non-test account or account identifier. Eligibility must cover both receipt times. |
| Reconciliation decision | Version, receipt references, `as_of` UTC, decision state and reason code, consent/classification decision references and their effective times; reviewer/audit reference where manual reconciliation is used. Do not include raw identifying material in the result. |

Define trusted sources, issuer authentication, receipt integrity and the actual technical proof of decoded advancement and listener initiation before collection. Do not treat server byte delivery, a client-provided boolean, a changed session ID, or a cache hit as sufficient proof by itself. Safeguards against replay, forged or duplicate receipts, clock skew, supervised-session reuse, and changes to consent or classification remain review decisions.

## Correlation, eligibility and dispositions

- Keep any mapping from a real identity to `human_key` in a separate restricted service/store; it is not a public analytics field. Use a purpose-specific keyed pseudonym, with key storage and rotation separated from receipts. An unsalted deterministic hash is vulnerable to guessing and cross-dataset linkage. Never put keys, raw IDs, IP addresses, provider credentials, URLs, content names, or free-text participant notes into receipts, logs, PRs or synthetic fixtures.
- Do not assume one account equals one person. An approved, privacy-preserving process must establish human-level deduplication across accounts without merging uncertain identities. If not demonstrable, the human metric remains UNKNOWN; do not substitute account counts. How to handle shared accounts, multiple accounts, and deletion while maintaining correctness is unresolved.
- Exclude agents, automated traffic, synthetic/test accounts, demonstrations, unconsented identities, and unclassified or uncertain humans from the human count. Synthetic records must be unmistakably labeled and segregated; no synthetic receipt is a real human journey.
- Keep outcome classes separate: **excluded** for an ineligible or insufficiently evidenced identity or invalid/withdrawn consent; **pending** for an otherwise eligible first journey while its seven-day follow-up remains open with no verified return; **non-return** for an otherwise eligible first journey whose seven-day window has closed without a verified return; **return** only when all required evidence qualifies. Unresolved proof cannot be called a non-return or a return. Excluded, pending and non-return are not zero-valued proof of observed human demand.
- Duplicate receipts must not inflate counts; a person with multiple qualifying returns counts at most once per rolling window. Corrections and consent withdrawal must invalidate affected derived results and trigger recomputation/deletion as policy requires; do not keep an identifiable audit shadow merely to preserve an old count.

## Proposed access and lifecycle — not approved

Evidence, restricted consent/classification references and the identity mapping are separate access domains. Propose least-privilege, named Product and Security reviewers only where a specific task requires it, with time-limited access and access audit; engineers and broad analytics consumers receive no row-level access by default. Any reporting would use reviewed aggregate-only results with a separately approved small-count suppression policy. No external vendor or third-party sharing is authorized. Encryption in transit and at rest, audited access and incident response are prerequisites, not substitutes for minimization.

For review, propose a **maximum 45-day** receipt lifetime after the relevant completion, sufficient in principle to cover the seven-day return interval and 30-day reporting window plus limited reconciliation time; the exact anchor, clock, retention per record class, derived-result lifetime and operational feasibility are unresolved. Retain no longer than the approved purpose requires. This number is a proposal, not existing policy or permission to store anything. Consent withdrawal, deletion request, expiry, or classification revocation must stop further use and trigger deletion or irreversible de-identification of relevant records and derived copies; Product/Security must specify deadlines, exceptions, and how to invalidate aggregates. The deletion design must inventory primary stores, identity mappings, caches, logs, exports, replicas and backups, including a bounded backup expiry and suppression of restored deleted data. It must specify how a request locates all copies without retaining a new permanent tracking identity. No collection may begin until the actual mechanism is reviewed and verified.

## Why cache telemetry is not the metric

`workers/stream/src/observability.rs` describes cache status, a SHA-256 user hash, request ID, TTFT and bytes; it states Analytics Engine writes may be sampled and weighted for aggregate queries, and the writer is not wired in that crate. Sampling and `_sample_interval` weighting cannot recover an exact set of distinct humans or prove that an omitted event did not occur. The projection does not establish purpose-specific consent, eligible human classification, first-journey completion, independent listener initiation or a distinct return session. A deterministic hash alone is not proof of a person or adequate privacy protection. These rows must not be repurposed as exact returning-listener evidence.

## Required decisions and review record

All decisions below remain **BLOCKED / undecided** until independently reviewed. Record named reviewer, role, date, exact contract revision, decision (`approve`, `revise`, or `block`), rationale and any conditions in the review system; do not infer approval from silence or a merge. A revise or block decision keeps real-data collection blocked. An approval of this proposal is not itself authorization for a production deployment or for a specific participant.

| Decision | Product | Security |
| --- | --- | --- |
| Purpose, necessity, scope and eligible participants | PENDING | PENDING |
| Receipt fields and trusted proof of each journey step, decoded advancement and independent session | PENDING | PENDING |
| Affirmative consent provenance, validity, withdrawal and applicable policy/legal review | PENDING | PENDING |
| Classification provenance, uncertain identities and cross-account human deduplication | PENDING | PENDING |
| Pseudonym design, key custody, access, audit and sharing | PENDING | PENDING |
| Retention, deletion across copies/backups, correction and reporting/suppression | PENDING | PENDING |
| Reconciliation, replay/duplicates, window boundaries, late data and consent/classification changes | PENDING | PENDING |

Independent Product review: **PENDING; no decision recorded.** Independent Security review: **PENDING; no decision recorded.** Required reviewers must assess their own authority and whether additional legal/privacy approval is needed. This document records no approval.

At the **2026-11-02 checkpoint**, record a continue/stop/reframe decision based on credible participant availability, demonstrated need and a safe test. If any is absent, pause feature expansion rather than invent activity. By **2026-12-15**, record a dated Product and Security disposition for this contract and every decision above; if not approved, keep collection blocked and the human baseline UNKNOWN. Synthetic-only validation may continue under its own controls and must not be represented as a human count.
