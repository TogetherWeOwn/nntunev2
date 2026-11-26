# Recover interrupted private trial database setup

Use this handoff for the dedicated stock PostgreSQL trial during Stage C.
Keep the trial disarmed. Confirm the original protected authorization,
reviewed revision and staging writer window still cover the work. Confirm the
original executor has stopped before assigning one recovery executor. Follow
the [Stage C entry gates](private-staging-trial.md#stage-c2-hyperdrive-origin-connection)
and retain the original attempt ID, scope and readback references. Confirm
the original apply session and any queued jobs have ended before comparing
database state or resuming writes.

## Start from the provisioning record

Record the attempt ID, reviewed revision, executor and original scope,
last sent action, request and result references, exact scoped IDs or paths,
dependencies, next unsent step and a readback verdict for each item. Distinguish
**unsent**, **sent, unresolved**, **verified present** and **verified absent**.
Before sending the apply, save protected evidence of the reviewed pre-apply
schema, objects, ACLs, ledger and synthetic fixture state for comparison.
Keep connection strings, keys, passwords and raw provider responses in
protected custody. Record evidence references and verdicts in the handoff.

The known partial layout below uses synthetic identifiers. The executor
stopped after both Access routes and the origin CA upload. The database
catalog apply, all Hyperdrive creates, serving binds and Environment inputs
were recorded as unsent. No runtime SQL logins had been issued. The two
containers used the recorded bind mounts.

| Resource | Scoped record at interruption |
| --- | --- |
| PostgreSQL and connector | `<pg-id>`, `<connector-id>` in `<trial-project>` on `<docker-host>` |
| Internal and connector egress networks | `<internal-id>`, `<egress-id>` on `<docker-host>` |
| Local files | `<pg-root>` with dedicated data and TLS files, and `<connector-root>` with dedicated config and credential file on `<docker-host>` |
| Tunnel and DNS | `<tunnel-id>` in `<account>` and `<op-dns-id>`, `<hd-dns-id>` in `<zone>`, with their exact hostnames and tunnel target |
| Operator Access | `<op-app-id>`, its child `<op-policy-id>`, `<op-token-id>` in `<account>` |
| Serving Access | `<hd-app-id>`, its child `<hd-policy-id>`, `<hd-token-id>` in `<account>` |
| Origin CA | `<origin-ca-id>` and fingerprint in `<account>` |
| Hyperdrive configs | `UPLOAD`, `AUTH`, `PUBLISH`, `ENROLL` all unsent, no IDs issued |
| Serving binds and Environment inputs | Unsent, no version bound and no trial-specific inputs created |

## Resume the same attempt

1. Read back every prerequisite in its original scope. Match container,
   network and mount ownership, PostgreSQL version and system identifier, the
   database and issued role OIDs, and CA and leaf fingerprints. Match the
   recorded tunnel, each DNS record, each Access app and its service-token-only
   policy and token, and the origin CA ID. Recheck account capacity before
   creating the remaining configs. Preserve the two pinned TLS hostnames and
   `verify-full`. Use the stock target's reviewed readbacks after Stage B and
   keep the [catalog extension policy](private-staging-trial.md#isolated-database-catalog-preflight)
   intact.
2. Proceed only when the prerequisites match. For an unsent apply, match
   the saved pre-apply state, then use the reviewed migration and synthetic
   fixture inputs. Advance the checkpoint only after checking the full ledger,
   checksums and fixture state. If a sent apply is unresolved, first check for
   that complete state. If it matches, record completion and skip the apply.
   Retry only when the original session has ended, the complete pre-apply
   state matches and the reviewed apply permits replay. An absent ledger
   provides no evidence about earlier committed statements. Hold an
   incomplete or ambiguous catalog for the reviewed recovery or retirement
   path without sending more SQL writes.
3. Complete the remaining approved C1 steps in order. Record each Hyperdrive
   create separately with its assigned scoped SQL role and its request and
   result references. The four configs use the same serving Access client and
   pinned origin CA, with `verify-full` and caching disabled. Report verified
   C1 completion to the Chief of Staff. Keep serving pending until the four
   actual binding checks in Stage C2 pass.

### If a Hyperdrive create acknowledgement is lost

For example, `UPLOAD` is sent with request reference `<request-ref>`. A
separate provider-side result `<result-ref>` binds it to `<upload-config-id>`
in `<account>`. `AUTH`, `PUBLISH` and `ENROLL` remain unsent in this example.
Read back the exact config ID in the same account. Match the binding's
intended origin host, Access client reference, dedicated database and scoped
SQL identity, CA ID and fingerprint, `verify-full` and disabled caching using
protected readback evidence. Require exactly one scoped match. Record that
config ID as verified present in the prepared handoff, skip its create and
resume at the first unsent step. Verify serving separately in Stage C2.

If the result cannot bind the ID to the original request, a required field
differs, or the readback is missing, ambiguous, denied or times out, hold the
create and dependent writes. Use scoped inventory to narrow the search and
recover the protected create result or equivalent evidence that binds a config
ID to the original request. A name match, a new ID in inventory or a negative
lookup alone leaves a sent create unresolved. Preserve the request reference,
bound ID or `unknown`, last readback, affected binding and dependency IDs.
Hand the record to the disposal operator and report the unresolved item to
Engineering for provider-side reconciliation.

## Retire the attempt by exact ID

Build a teardown preview from the protected record. Confirm the listed scope
and the provenance of each created object. Leave shared PostgreSQL, shared
staging settings, existing Hyperdrive configs and unrelated host or account
resources outside the scope. If the original action may still complete,
reconcile it before removing the affected resource or its dependencies.
Before either container is removed, save its inspected bind-mount sources and
volume names privately against its exact ID. Match the sources to the original
roots. Any replacement container requires reconciled provenance, a new exact
ID and the prior ID's observed disposition in the record.
Use the verified current state after an interrupted deletion to work from
the first remaining item. Confirm absence by an authorized lookup in the
original scope. A timeout, denial or scope mismatch leaves the item pending.

### Confirm containment and stock-target withdrawal

For the known partial layout, verify from the original record and scoped
readbacks that no runtime SQL logins were issued, no serving bindings were
deployed and no fixture activity requires cleanup. Record containment and
withdrawal as inapplicable only after those checks pass.

If fixture activity occurred, complete the applicable [protected fixture
containment and cleanup](private-staging-trial.md#retire-and-recover) using
reviewed stock-target access. Keep the selected fixture pins and cleanup
access until cleanup is confirmed.

Reconcile every Worker binding from the attempt, including bindings deployed
for [Stage C2 qualification](private-staging-trial.md#stage-c2-hyperdrive-origin-connection)
while the trial was disarmed. Match the recorded routes, deployments, Worker
versions, binding names and Hyperdrive config IDs. Remove the trial bindings
through the reviewed stock-target deployment path. Verify that no active
deployment or separately reachable Worker version still references any trial
config. Keep step 1 pending if a binding, deployment or removal result is
unresolved. Complete this readback before deleting any Hyperdrive config.

If any runtime SQL login was issued, require a separately reviewed
stock-target procedure and scoped readbacks for fencing, draining and
withdrawing those roles before infrastructure teardown. The linked `fence`
and `retire` workflow accepts PlanetScale targets only. Until the stock
procedure and required evidence are available, keep the trial disarmed,
record disposal as pending and send the exact blocker to Engineering.

| Order | Targets and required readback |
| --- | --- |
| 1 | Verify [containment and withdrawal](#confirm-containment-and-stock-target-withdrawal) before clearing trial-specific Environment inputs. Check absence by Environment and name, using secret metadata only. The known partial layout has no inputs to clear. |
| 2 | Remove each verified trial Hyperdrive config and confirm its exact ID is absent in `<account>`. Treat the four entries independently. In the known partial layout all were unsent. If any sent create or deletion is unresolved, pause before step 3 and retain the CA, route, Access and database resources pending reconciliation. Keep the trial disarmed and report the exact pending IDs or requests. |
| 3 | Stop and remove `<connector-id>`, remove `<op-dns-id>` and `<hd-dns-id>`, then delete `<tunnel-id>` after the connector disconnects. Check the container ID among running and stopped containers on `<docker-host>`, both DNS IDs in `<zone>` and the exact tunnel ID in `<account>` for verified absence or an explicit deleted state. |
| 4 | Once route removal is verified, remove both Access apps with their app-scoped policies and revoke both service tokens. Verify each app and token ID is absent in `<account>` and confirm its child policy was removed with the app. Remove `<origin-ca-id>` after step 2 is verified and confirm its exact ID is absent. |
| 5 | Stop and remove `<pg-id>` and remove `<internal-id>` and `<egress-id>`. Verify the exact container and network IDs are absent on `<docker-host>`. Remove and verify any recorded trial-owned volume by its exact name on the same host. |
| 6 | Match the roots to the saved mount inspection and verify they are dedicated to this attempt before removing the data, TLS, connector files and the two directories. Check the exact paths are absent, including any leftover symlink entries. Never follow a replacement symlink or remove a parent path. Remove trial custody entries by their recorded references after their final required use. Record absence without exposing values. |

Record the observed outcome for each original ID or path. Dispose the same
day unless the lead retains the trial for up to seven days. Report completed,
remaining and unresolved items to the Chief of Staff with the evidence
references and next action. Pending items keep disposal open.

## Offline rehearsal

On a disposable host with the [Stage A image](../../ops/staging-trial-pg/README.md)
already cached, run the supplemental database rehearsal with the stock
PostgreSQL fixture. The existing Stage B evidence remains the C1 entry gate.

```sh
python3 -B workers/api/tests/fixtures/test_private_trial_setup_recovery.py
```

For the provider portion, walk the table with synthetic readbacks. With the
known layout and all prerequisites matching, the first write is the unsent
catalog apply. With a bound `UPLOAD` result and matching readback, the next
create is `AUTH`. A wrong account or timed-out `UPLOAD` lookup leaves its
checkpoint pending. During retirement stop at step 2 and retain the listed
dependencies until the result is reconciled. For disposal of the known layout, the
first removal is the connector. No config or Environment input is a deletion
target. In the C2 case, suppose a deployed Worker version still binds
`UPLOAD` when the read-only check times out. Even with the trial disarmed,
keep step 1 pending until the deployment readback confirms the binding is gone.
After simulated DNS removal with the tunnel deletion still pending,
recheck the two exact DNS IDs and the connector's absence, then remove only
the remaining tunnel before touching Access. The tabletop uses synthetic
provider readbacks and leaves serving qualification to Stage C2.

The [offline binding fixture](../../.github/scripts/test_worker_build_workflow.py)
rehearses an interruption later in C1, after `UPLOAD`, `AUTH` and `PUBLISH`
were verified. The `ENROLL` create succeeds but the executor loses its
response. Two records have the requested settings, and only one has the
config ID tied to the original request in the admitted account. The prepared
handoff supplies that ID as `PRIVATE_TRIAL_ENROLL_DB_STAGING_ID` for the
staging binding renderer and omits an `ENROLL` create. If the same ID is
linked only to a different request, preparation remains pending. The fixture
uses synthetic records and ends at configuration preparation.
