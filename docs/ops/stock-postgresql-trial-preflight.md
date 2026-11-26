# Stock PostgreSQL trial preflight

Use this preflight when the reviewed trial plan specifies the dedicated stock
PostgreSQL target. Record C0 account capacity separately from the C2 operator
connection. Keep the trial disarmed until the existing protected staging gates
pass.

## C0 Access and Tunnel capacity

For the same approved account, compare complete, quota-counted inventories
with the applicable Zero Trust limits. Reserve room for two Access apps
(separate operator and serving paths) and one dedicated Tunnel. Record the
used count, limit, required slots and protected evidence references for each.
If an inventory is incomplete or its limit is unconfirmed, mark that row
pending. If confirmed spare capacity is below the required slots, stop
before provisioning.

| Synthetic readback | Used | Limit | Slots needed | Result |
| --- | ---: | ---: | ---: | --- |
| Access apps | 8 | 10 | 2 | Capacity confirmed |
| Dedicated Tunnel | Unknown | 5 | 1 | Pending |

The example leaves C0 pending. Complete the other C0 checks in the
[Stage C2 runbook](private-staging-trial.md#stage-c2-hyperdrive-origin-connection)
before C1 provisioning.

## C2 operator target, TLS and role

Use the reviewed [stock PostgreSQL target pins](https://github.com/TogetherWeOwn/nntunev2/pull/1919)
after the stock target's Stage B rehearsal passes. Use the authorized
loopback-only TCP listener for the operator route. Verify
that the listener forwards to the hostname and origin port recorded in
`routes.direct`. Use the pinned hostname for TLS and the listener's local
port for the socket. Record the listener mapping in protected custody. Keep
the mapping and certificate configuration fixed through the probe and both
SQL sessions. Dedicate the CA to this instance and a single server leaf.
A certificate rotation requires new pins and readbacks.

Before supplying any database credentials, confirm the CA file contains only
the dedicated CA certificate and compare its DER SHA-256 digest with
`ca_sha256`. Invoke the approved OpenSSL 3 binary by absolute path. Probe the
same listener with `openssl
s_client -starttls postgres -connect <loopback-address>:<local-port>`, using
`-servername` and `-verify_hostname` with the pinned DNS name, `-tls1_3`,
`-verify 1`, `-verify_return_error`, `-CAfile` with the dedicated CA, `-no-CApath`,
`-no-CAstore` and `-showcerts`. Read stdin from `/dev/null`. Require a
successful handshake and compare the presented leaf's DER SHA-256 digest with
`server_sha256`. `openssl x509 -noout -fingerprint -sha256` reports the digest
for a PEM certificate. Compare the hex digits ignoring case and separators.

The separate probe checks the leaf pin. Each SQL connection verifies the
dedicated CA and pinned hostname with `verify-full`. After the probe passes,
substitute the approved absolute path to PostgreSQL 18 `psql`, hostname,
local port, CA file, assigned login and trial database for the synthetic
values. Enter the password at the terminal prompt. Keep credentials and raw
diagnostics in protected custody.

```sh
/usr/bin/env -i LC_ALL=C \
  PGHOST=operator-db.example.invalid PGHOSTADDR=127.0.0.1 PGPORT=15432 \
  PGSSLROOTCERT=/path/to/approved-instance-ca.pem PGSSLMODE=verify-full \
  PGSSLMINPROTOCOLVERSION=TLSv1.3 PGCHANNELBINDING=require \
  PGGSSENCMODE=disable PGPASSFILE=/dev/null PGCONNECT_TIMEOUT=10 \
  /path/to/approved/postgresql-18/bin/psql -XqAt -v ON_ERROR_STOP=1 \
  -U example_operator -d example_trial <<'SQL'
BEGIN READ ONLY;
SELECT d.datname, d.oid, session_user, login_role.oid,
       current_user, effective_role.oid, s.ssl, s.version
FROM pg_catalog.pg_database d
JOIN pg_catalog.pg_roles login_role ON login_role.rolname = session_user
JOIN pg_catalog.pg_roles effective_role ON effective_role.rolname = current_user
JOIN pg_catalog.pg_stat_ssl s ON s.pid = pg_catalog.pg_backend_pid()
WHERE d.datname = pg_catalog.current_database();
COMMIT;
SQL
```

Require one row with the expected database name and `database_oid`, the
assigned canonical login and effective role names and their pinned OIDs, and
`t` and `TLSv1.3`.

For the cluster readback, require the canonical database owner's existing
`EXECUTE` grant on `pg_control_system()` in the dedicated database. If the
grant is missing, keep the readback pending and reconcile it through the
approved stock target setup. Repeat the connection through the same listener
as that owner. Add the owner-only query before `COMMIT` in the transaction
that reads the owner's database, role and TLS row.

```sql
SELECT system_identifier FROM pg_catalog.pg_control_system();
```

Match the system identifier, database OID and canonical owner role OID to the
reviewed pins. Keep the cluster readback separate from the assigned login's
evidence and keep the owner authority outside the serving bindings.

In the protected handoff, record the reviewed pin reference and separate
target, TLS and assigned-role verdicts with their evidence references. A
missing readback leaves the operator step pending. On a confirmed mismatch,
stop and reconcile the pins before retrying. Compare the operator result with
the [serving readbacks](private-staging-trial.md#stage-c2-hyperdrive-origin-connection)
for the same reviewed stock target.
