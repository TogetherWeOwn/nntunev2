# Stock PostgreSQL private trial target

Stage A qualifies this template on disposable PostgreSQL 18.6. The hosted
fixture uses Docker's internal DNS for two synthetic names and the local run
can resolve both names through loopback inside each disposable container.
Both runs use the same pinned image, server configuration and HBA rules.

```sh
python3 -B workers/api/tests/fixtures/test_private_trial_selfhost.py --loopback Qualification.test_catalog_and_continuity
```

The fixture requires the pinned image in the local Docker cache. It creates
two isolated Compose projects without publishing ports, uses only synthetic
logins and removes the projects, private keys and data directories on exit.
It reuses the private trial fixture's clean app migration replay through
the reviewed baseline (`0063` at introduction), verifies every replayed
checksum, and runs the same catalog predicate as the PlanetScale preflight.
An independent cluster with matching database and role OIDs and the same
fixture certificate isolates the system identifier check.

For a separately approved rollout, the Compose input directory is
`/srv/nntune-trial-pg`, with mode `0700` and a dedicated host UID and GID.
The template defaults to `24186:24186`. Supply a `passwd` entry mapping that
identity to `nntune_trial_bootstrap` for Unix peer authentication. Mount
`config`, `tls` and the one-time initialization password file separately
from the data directory. Generate the CA and server material with
`python3 -B ops/staging-trial-pg/create_tls.py <new-private-directory>`.
Keep the CA key outside every container mount. Use the CA for this one
instance and a single leaf. A rotation requires new pins and readback on
both routes. Monitor the database for a 5 GiB size alarm in the rollout.

`pins.example.json` shows the separate stock PostgreSQL pin shape. Read the
system identifier independently with `pg_controldata`. Pin both DNS names
and observed ports, the CA and leaf DER SHA-256 digests, and the database and
role OIDs before running checks. Use `null` for each runtime role OID until
that role is issued. SQL identity checks on both routes use the canonical
owner for `pg_control_system()`. Restrict execution of that routine in the
app database to bootstrap and owner. The executor and runtime roles prove
their own canonical SQL identity and database and role OIDs. The leaf pin is
checked via a separate TLS handshake. Each libpq connection enforces the
dedicated CA, hostname, TLS 1.3 and SCRAM channel binding.

Stage B reruns the merged commit with fresh disposables. Stage C0 checks
account capacity, scoped authority and combined Tunnel/Access and origin
TLS support before any write. The Tunnel/Access documentation omits an
explicit origin port. The origin TLS documentation describes a custom CA
and `verify-full` with a separate invocation. C0 must establish that the
combined configuration works with both controls intact. Stage A does not
establish that capability or admit a live target. [Tunnel/Access documentation](https://developers.cloudflare.com/hyperdrive/configuration/connect-to-private-database/) · [Origin TLS documentation](https://developers.cloudflare.com/hyperdrive/configuration/tls-ssl-certificates-for-hyperdrive/)
