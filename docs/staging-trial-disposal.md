# Stage C private trial disposal preview

Stage C is the protected rollout of the dedicated PostgreSQL trial. Use its
provisioning record to plan disposal. Dispose the same day unless the lead
retains the trial for reruns for up to seven days. Report the disposal outcome
to the Chief of Staff. This example uses synthetic identifiers and contains no
executable commands.

## Partial setup inventory

In this example, both Access routes and the Hyperdrive origin CA upload were
created. Setup stopped before sending the Hyperdrive config create request.
The containers used only the recorded bind mounts. No serving Worker used
the trial and no GitHub `staging` Environment inputs were set. Every
identifier below is a placeholder, including account, zone, host and path
references.

| Resource | Recorded identifier and scope | Setup result |
| --- | --- | --- |
| PostgreSQL and tunnel connector containers | `<trial-pg-container-id>`, `<trial-connector-container-id>` on `<trial-docker-host>` | Created |
| Internal and connector egress networks | `<trial-internal-network-id>`, `<trial-egress-network-id>` on `<trial-docker-host>` | Created |
| Local data and files | `<trial-data-dir>` (database and TLS files), `<trial-tunnel-dir>` (connector config and credential file) on `<trial-docker-host>` | Created |
| Cloudflare Tunnel | `<trial-tunnel-id>` in `<trial-account-id>` | Created |
| Operator and Hyperdrive DNS CNAMEs | `<trial-op-record-id>` for `<trial-op-hostname>`, `<trial-hd-record-id>` for `<trial-hd-hostname>` in `<trial-zone-id>`, both pointing to `<trial-tunnel-id>` | Created |
| Operator Access app, app-scoped policy and service token | `<trial-op-app-id>`, its child `<trial-op-policy-id>`, and `<trial-op-token-id>` in `<trial-account-id>` | Created |
| Hyperdrive Access app, app-scoped policy and service token | `<trial-hd-app-id>`, its child `<trial-hd-policy-id>`, and `<trial-hd-token-id>` in `<trial-account-id>` | Created |
| Hyperdrive origin CA upload | `<trial-origin-ca-id>` in `<trial-account-id>` | Created |
| Dedicated Hyperdrive config | `<trial-hd-config-name>` in `<trial-account-id>`, no config ID issued | Planned only |
| Trial-specific GitHub Environment inputs | `staging` in `<trial-repo>`, no input names created | Planned only |

## Teardown preview and absence checks

Build the preview from IDs recorded for this trial in their original account,
zone or Docker host. Use the same record for readback. Keep the shared
PostgreSQL clusters, existing Hyperdrive configs and staging inputs, other
Access apps and tokens, existing tunnels and DNS records outside the preview.
If a create timed out or its result is unknown, reconcile it in that scope
before adding a target. A name match alone does not establish ownership.

| Order | Targets in this example | Readback after disposal |
| --- | --- | --- |
| 1 | Stop and remove `<trial-connector-container-id>`. Remove `<trial-op-record-id>` and `<trial-hd-record-id>` and delete `<trial-tunnel-id>` after its connector disconnects. | Verify the connector container ID is absent, including among stopped containers, and both DNS record IDs are absent in the recorded zone. Verify the tunnel ID is absent from the account's active tunnel inventory or is explicitly marked deleted. |
| 2 | Once step 1 is verified, remove `<trial-op-app-id>` and `<trial-hd-app-id>` with their app-scoped policies, and revoke `<trial-op-token-id>` and `<trial-hd-token-id>`. | Verify both app IDs are absent. Their recorded child policies are removed with the apps. Check that both token IDs are absent from the account's service-token inventory. |
| 3 | Remove the origin CA upload `<trial-origin-ca-id>`. | Check that the exact CA ID is absent in the account's origin certificate inventory. |
| 4 | Stop and remove `<trial-pg-container-id>`, then remove `<trial-internal-network-id>` and `<trial-egress-network-id>`. | Check the exact container ID among running and stopped containers and both exact network IDs on the recorded Docker host. All three IDs should be absent. |
| 5 | Securely delete the database files and generated TLS material under `<trial-data-dir>` and the connector config and credential file under `<trial-tunnel-dir>`, then remove the two dedicated directories. | Check that both exact paths are absent on the recorded host, including any leftover symlink entries. Record path status only. |

Keep Access in place while route disposal is unverified. For any trial that
reaches serving, disarm its rollout and unbind its dedicated config through
the protected lane before disposal. When setup created trial-specific
Environment inputs, remove those and read back their absence by Environment
and input name, using metadata for secrets. If setup created a Hyperdrive
config, remove it and verify its exact ID is absent in the recorded account
before removing the origin CA, even if the trial never reached serving.

If container mount metadata identifies a volume created by the trial, record
its exact Docker volume name and host before removing the container. Include
that volume in the preview and verify its absence on the same host. Remove
any trial credentials held in secure custody by their recorded references.
Record whether those references are absent without reading secret values.

The table records expected checks. For an actual disposal, record each
observed result against its ID or path. A timeout, denied lookup or wrong
account, zone or host leaves the item unverified. Report any remaining or
unverified items and retry only those after reconciling their current state.
