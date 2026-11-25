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

Use an absolute destination in private custody. Supply the access key ID and
one or two exact fixture keys. The renderer accepts the same compact UUIDv4 key
shapes as the fixture retirement checker. Qualify each credential's scope
separately.

```sh
umask 077
python3 -B ops/staging-s3-boundary/render.py "$fixture_access_id" "$fixture_key" > "$fixture_caddyfile"
sha256sum "$fixture_caddyfile"
```

Compose expects `FIXTURE_CADDYFILE` and `FIXTURE_DATA_DIR` as absolute paths,
`FIXTURE_BOUNDARY_PORT` as the loopback port, and `FIXTURE_ADMIN_ENV` as the
absolute path to the provisioner's private env file containing
`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`. Create the dedicated fixture
data directory before starting the services. Record the SHA-256 of the
mounted Caddyfile and compare it with the reviewed render.

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
