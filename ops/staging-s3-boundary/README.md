# Disposable staging fixture storage

The boundary accepts HTTP/1.1 HEAD and DELETE for exact fixture keys in
`nntunev2-private-fixtures-staging`. It checks the original origin-form target,
a single nonempty `versionId` query and the configured SigV4 access key ID.
Caddy forwards the signed request to SeaweedFS for signature and policy
verification. A boundary denial is an empty 403 with
`X-NNTune-Boundary: deny`. The boundary sets `Cache-Control: no-store` and
passes through backend status and payload.

Go's HTTP server handles invalid wire framing and the server-wide `OPTIONS *`
before Caddy runs the route. The contract verifies that they send no upstream
traffic. Routable OPTIONS requests receive the boundary denial.

Compose gives SeaweedFS one isolated, internal network and publishes only
Caddy's loopback HTTP port. The internal network is shared by the two services.
The provisioning identity is used via `docker compose exec -T backend`.
See the [provider-only preflight](../../docs/ops/private-staging-trial.md#provider-only-preflight)
for protected dispatch and custody.

## Render the credential's policy

Use an absolute destination inside a directory restricted to the operator.
Set the rendered file to mode 0644 so the Caddy process can read the bind mount
with its capabilities dropped. Supply the access key ID and one or two exact
fixture keys. The renderer accepts the same compact UUIDv4 key shapes as the
fixture retirement checker. Qualify each credential's scope separately.

```sh
umask 077
python3 -B ops/staging-s3-boundary/render.py "$fixture_access_id" "$fixture_key" > "$fixture_caddyfile"
chmod 0644 "$fixture_caddyfile"
sha256sum "$fixture_caddyfile"
```

Compose expects `FIXTURE_CADDYFILE` and `FIXTURE_DATA_DIR` as absolute paths,
`FIXTURE_BOUNDARY_PORT` as the loopback port, and `FIXTURE_ADMIN_ENV` as the
absolute path to the provisioner's private env file containing
`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`. Create the dedicated fixture
data directory before starting the services. Record the SHA-256 of the
mounted Caddyfile and compare it with the reviewed render.
Use the [qualification's managed policy](fixtures/storage.py) as the
one-key reference when checking the backend policy and its attachment.

## Reproduce the disposable qualification

Use a clean checkout at the reviewed commit, Python 3.10+, Node 24+, OpenSSL,
a recent local Docker Engine with isolated bridge gateway support, and Docker
Compose v2.30+. Build the standalone probe from that same commit using the
[provider-only preflight build](../../docs/ops/private-staging-trial.md#provider-only-preflight)
in the controlled qualification environment. With `probe_dir` pointing to the
absolute disposable directory holding the build, run from the repository root.

```sh
python3 -B ops/staging-s3-boundary/fixtures/qualify.py \
  --bundle "$probe_dir/nntune_ingest.js"
```

The harness creates fresh loopback ports, a Docker project, credentials and
fixtures for the recording and SeaweedFS tests. It verifies the deny matrix
against upstream traffic and independent inventory, exercises the reviewed
HTTPS launcher, confirms exact absence and sibling preservation, and repeats
the list-all and bulk assertions directly against the isolated backend as a
negative control. The verdict requires checked disposal of the credentials,
fixture state, containers, networks, ports and scratch files.

For focused reproduction, use `--contract` for the recording upstream or
`--storage` for the SeaweedFS lifecycle with the synthetic wire client. The
hosted `worker release builds` job runs `--bundle` and records the reviewed
launcher verdict.

## Check DNS convergence for the disposable ingress

For an authorized attempt, import [the policy](dns_convergence.py) in the protected executor.
Hold the exclusive writer from write through closeout. If lost, reconcile record identity first.

1. After creation, read back the recorded ID and require exactly that one
   record by exact name, matching the approved type, origin, proxy state, TTL,
   tags and comment. After deletion, require `404` by recorded ID and an empty
   exact-name provider list. Bind direction, name, ID and the original
   monotonic completion time in `Readback`. Run the check once per write.
2. Use the two distinct authorities from the approved zone inputs. Pin each
   query endpoint before the write. Every two seconds `read_pair` starts one
   direct, non-recursive `IN A` query per endpoint together under one timeout.
   Match each reply to its query and server. Return one observation per
   attempt, using a non-authoritative failure if a valid reply is unavailable.
   Count exact-owner A answers. Proxied answers are edge IPs.
3. For create, require `NOERROR` with at least one exact-owner A answer on two
   consecutive reads per authority before 90 seconds, then proceed to HTTPS
   readiness. On expiry or stop, keep the trial disarmed and dispose as scoped.
4. For delete, require `NXDOMAIN` with zero answers on two consecutive reads
   from each authority before 90 seconds from delete readback. On a miss,
   keep reading for containment up to 300 seconds total from that readback.
   Keep the writer held and return the receipt to Engineering even if both
   authorities converge. Keep exact provider absence proof. Perform no second write.
5. In either direction, stop when an authority regresses after two matching
   reads, sends a non-authoritative response, reports `SERVFAIL` or `REFUSED`,
   or otherwise cannot supply a valid observation. Log each read's UTC time,
   server, response code, AA flag, answer count and seconds since REST readback
   to the protected receipt with the merged SHA and executor diff SHA-256.
   Determine delete qualification by reply arrival time. On callback failure
   or interruption, keep the trial disarmed. On delete failure, keep DNS
   cleanup pending and retain the writer while other scoped closeout continues.

Exercise the policy with synthetic observations only.

```sh
python3 -B ops/staging-s3-boundary/test_dns_convergence.py -v
```

## Recover ingress before hosted qualification starts

This tabletop uses placeholder records. The protected rollout owner handles
any live recovery in the coordinated shared-writer window. The dedicated
shared Caddy site and proxied DNS record were created, then setup stopped
before [provider-only preflight](../../docs/ops/private-staging-trial.md#provider-only-preflight)
started. First reconcile the rollout's dispatch record with workflow run
metadata. In this example no matching run was queued or started. If either
occurred or the status is unclear, defer to the rollout owner for
reconciliation. Keep the trial disarmed.

| Readback | Synthetic result |
| --- | --- |
| Pre-change backup and DNS baseline | Caddy revision `<before>` has no site for `fixture.example.invalid`. Zone `<zone>` has no record for that name. |
| Existing-site checks | `other.example.invalid/healthz` and `app.example.invalid/healthz` return `200`. `admin.example.invalid/healthz` returns `401`. |
| Rollout receipt | The rollout added one site for `fixture.example.invalid`, with `import <origin-snippet>` and `reverse_proxy 127.0.0.1:18080`. It created DNS record `<record-id>` in `<zone>` for that name, with type `A`, origin `192.0.2.10`, proxy on, TTL automatic and no tags or comment. |
| Current Caddy source and active route | The exact site block and active route match the rollout receipt. Revision `<current>` also contains a separately deployed `other.example.invalid` route update, confirmed by its owner's change receipt. |
| Current DNS control plane | Reading `<record-id>` in `<zone>` confirms the recorded name, type, origin, proxy state and TTL. It still has no tags or comment. |

1. Compare the live readbacks to the recorded additions, including the full
   Caddy site block and exact DNS record ID. A hostname match alone is
   insufficient. If the hostname now has `<replacement-id>`, leave that
   record in place and stop. Stop before removal if either identity or
   ownership is ambiguous or either addition differs from its receipt.
2. In the rollout owner's shared-writer window, prepare the inverse against
   `<current>` by removing just the rollout's site block. The diff must leave
   the separate `other.example.invalid` route update in place. Restoring
   `<before>` here would undo that update. Recheck both targets against the
   receipt before applying. Validate the resulting Caddy config. Delete the
   recorded DNS ID in its zone and record absence by exact ID and name. Run the
   [delete convergence check](#check-dns-convergence-for-the-disposable-ingress)
   and save its verdict before attempting the Caddy reload. On failure, record
   what applied and read both targets again before any retry.
3. Use the original delete receipt for provider and authoritative absence.
   Read the Caddy source and active route to confirm the rollout block is
   absent, the concurrent route update is still loaded and all three existing
   sites still return their expected responses. Record those results and leave
   hosted qualification marked pending.
   Separately staged fixtures, credentials or protected inputs belong to the
   rollout owner's existing closeout.

If DNS deletion succeeded and the Caddy reload failed, resume with Caddy only
in the rollout owner's shared-writer window. Match the deletion receipt to
`<record-id>` and `<zone>`, then confirm that ID and the fixture hostname are
both absent in the DNS control plane. Stop on missing proof or a replacement
record. Carry forward the original delete verdict and authoritative receipt
for step 3. A Caddy-only resume cannot start a new DNS qualification window.
Read the latest Caddy source and active route. If the rollout block is
already absent from both, continue to step 3. Otherwise require the active
site to match the rollout receipt and the source to show either that same
block or its recorded removal. Stop on unexplained differences between source
and active configuration. Build the removal from the latest verified source,
keeping the unrelated route update, then validate, reload and apply step 3.

After recovery, [disposable qualification](#reproduce-the-disposable-qualification)
can use fresh synthetic credentials and versions with `--bundle` on loopback.
It checks the deny matrix, exact-version deletion and absence, sibling
preservation and disposal without shared ingress. Its result supplies isolated
evidence. Hosted provider qualification still requires a separately prepared
and verified ingress under the protected rollout.
