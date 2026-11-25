# First-release handoff after switch-off

This synthetic example starts after the documented
[switch-off and verification](../first-private-music-launch.md#switch-off-and-verify)
have finished. All values and evidence references are illustrative.

| Handoff field | Example entry |
| --- | --- |
| Attempted candidate | `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`, rollout `101/1`. Health watch failed at 12:10 UTC. |
| Recovery outcome | Switch-off complete at 12:14 UTC. Recovery job `102/1` restored the recorded maintenance version. See [allocation](#evidence-allocation) and [denials](#evidence-denials). |
| Serving Web build | Maintenance seed `bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb` at 100% on deployment `33333333-3333-4333-8333-333333333333`, observed at 12:14 UTC. See [allocation](#evidence-allocation). |
| First-release decision | **Pending** for proposed candidate `cccccccccccccccccccccccccccccccccccccccc`. Its exact-build staging journey and admission evidence are pending. Complete its [candidate qualification and release review](../first-private-music-launch.md#qualify-the-candidate) before recording a release decision. |

### Evidence allocation

The deployment readback showed `33333333-3333-4333-8333-333333333333` serving
100% of traffic on version `22222222-2222-4222-8222-222222222222`. That version
matches the verified maintenance seed record for
`bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb`. The seed record's original
deployment was `11111111-1111-4111-8111-111111111111`.

### Evidence denials

The authorized probe read `503`, `Cache-Control: no-store` and
`X-Robots-Tag: noindex` from the maintenance response. Private Access and
direct API and media denials passed, including the existing-session and
previously issued media-grant checks. The database and snapshot were preserved.
