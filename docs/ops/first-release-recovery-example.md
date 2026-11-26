# First-release switch-off handoff

This synthetic example follows the documented
[switch-off and verification](../first-private-music-launch.md#switch-off-and-verify).
All values and evidence references are illustrative.

## Recover the run reference after a lost start response

The operator recorded the repository, time and inputs of the recovery request
before sending it at 12:10 UTC. The request targeted `deploy-production.yml`
on `main` with `mode=recover-worker`, reviewed
`sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` and `recovery_run_id=101` for
rollout attempt `1`. The request timed out before returning a run ID. Mark the
start **Pending readback** and keep the switch-off fences in place. Do not
dispatch again or rerun a workflow while resolving the start. An empty list
can mean the first request is still becoming visible.

Use authenticated Actions records to list `deploy-production.yml` workflow
dispatches on `main` in the same repository for the recorded request window.
Verify the workflow ID and path and that the run is on attempt `1`. Verify
the submitted mode, candidate SHA and original rollout run ID for that same
attempt. In this example those values come from `DEPLOY_MODE`, `INPUT_SHA`
and `RECOVERY_RUN_ID` on the **Resolve and validate the promotion target**
step in the attempt's `production-preflight` job log. The Actions `head_sha`
identifies the workflow checkout. Verify the reviewed candidate against the
submitted `sha` input.

The complete readback identifies only run `102/1` and confirms all three
inputs. Record its run link and continue verification on `102/1`.
This confirms the start. Switch-off stays pending until the recovery job,
serving allocation and denial readbacks agree.

If the listing is incomplete or ambiguous, the input evidence is unavailable,
or the readback points to a different candidate or rollout, leave the start
pending and retain the original request pins. Continue read-only reconciliation
without recording a run as confirmed or issuing a second dispatch.

## Resume verification when the recorded run status is unavailable

Run `102/1` is already recorded. A temporary failure reading its Actions
status leaves the status and switch-off **Pending**. Keep the recorded run
link, attempt and writer freeze. The next action is a read of that same attempt
in the recorded repository (`GET /repos/example/fixture/actions/runs/102/attempts/1`).
Do not list candidate runs again, dispatch another recovery or rerun the workflow.

The next read reports `completed` with conclusion `success` for `102/1`.
Record the status and continue with that attempt's recovery job, serving
allocation and denial evidence before marking switch-off complete.

## Record the completed switch-off

| Handoff field | Example entry |
| --- | --- |
| Attempted candidate | `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`, rollout `101/1`. Health watch failed at 12:10 UTC. |
| Recovery outcome | Switch-off complete at 12:14 UTC. The `recover-worker` job in run `102`, attempt `1`, recovered rollout `101/1`. See the [job record](#evidence-recovery-job), [allocation](#evidence-allocation) and [denials](#evidence-denials). |
| Serving Web build | Maintenance seed `bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb` at 100% on deployment `33333333-3333-4333-8333-333333333333`, observed at 12:14 UTC. See [allocation](#evidence-allocation). |
| First-release decision | **Pending** for proposed candidate `cccccccccccccccccccccccccccccccccccccccc`. Its exact-build staging journey and admission evidence are pending. Complete its [candidate qualification and release review](../first-private-music-launch.md#qualify-the-candidate) before recording a release decision. |

### Evidence recovery job

Synthetic Actions job `103` (`recover-worker`), run `102`, attempt `1`.
The **Restore or verify the recorded allocation** step succeeded.

### Evidence allocation

The pinned maintenance seed record lists the original deployment
`11111111-1111-4111-8111-111111111111`, version `22222222-2222-4222-8222-222222222222`,
seed SHA `bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb` and module SHA-256
`dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd`.
The recovery created deployment `33333333-3333-4333-8333-333333333333`.
Its readback matched the recovery marker and recorded rollout history and
showed 100% of traffic on the pinned maintenance version.

### Evidence denials

The authorized probe read `503`, `Cache-Control: no-store` and
`X-Robots-Tag: noindex` from the maintenance response. Private Access and
direct API and media denials passed, including the existing-session and
previously issued media-grant checks. The database and snapshot were preserved.
