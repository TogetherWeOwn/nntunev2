# First-release handoff after switch-off

This synthetic example starts after the documented
[switch-off and verification](../first-private-music-launch.md#switch-off-and-verify)
have finished. All values and evidence references are illustrative.

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
