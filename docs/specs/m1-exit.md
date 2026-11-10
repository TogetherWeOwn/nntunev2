# M1 exit specification (approved 2026-11-03)

Status: approved requirement. M1 exit is required before a production
launch is considered. It is not a launch date or an availability
guarantee.

M1 exits when a real invited user can, on an exact-head staging deploy
with access control enabled, complete the journey below, with rollback
evidence recorded. Steps cite the [traceability matrix](../m1-exit-traceability.md)
for covering end-to-end specs.

1. **Invite-gated signup → paywall/plan page.** An owner-issued invite
   is required. The user signs up and lands on the plan picker. Later,
   paid members may receive invites valid only during owner-scheduled
   windows; M1 needs owner-issued invites only.
2. **Subscription active.** The user holds an active subscription in
   payment test mode (test checkout or an authorized staging seam).
   Playback is allowed only after this step. Live billing is not
   activated by this milestone.
3. **Admin ingest.** An admin uploads one curated album archive; it is
   matched to the public music database and its tracks appear in the
   shared catalogue as playable. Users never upload archives or
   provide provider credentials. For M1, the collection may stream from
   the original archive; the re-post pipeline is a later milestone.
4. **Play and seek from the curated collection.** The existing play
   and seek legs, with the source set to the curated collection. A track the collection
   does not have plays from the official video embed; an unavailable
   track is hidden by default. The stream-hour meter accrues for the
   play.
5. **Access enforcement.** The journey passes with access control
   enabled and denies anonymous traffic to health, readiness and the
   app shell.
6. **Rollback evidence.** Recorded per the staging promotion
   checklist: last good staging revision and its deploy run, plus a
   successful redeploy of that revision.
7. **Exact-head deploy.** The deployed build's identity matches the
   promoted revision at both the start and the end of the journey.
8. **Evidence gate.** A recorded pass cites the green post-deploy run
   and the deployed head revision. Skip-green without configured
   staging is not evidence.

Metering: stream hours only. At the monthly cap for a paid plan,
playback stops. A future free tier would drop to video-only at its
cap. No free plan exists at launch; do not add a free-plan video path
without a product/legal decision.

Player deferrals (accepted 2026-11-04): audio leveling,
verified played-rendition quality details and speculative next-track
prefetch remain deferred and undelivered. Helper presence or a source
type label is not delivery. See
[docs/plans/r1-r4-programme.md](../plans/r1-r4-programme.md).
