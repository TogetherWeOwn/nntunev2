"""Disposable PostgreSQL qualification. No connection or credential inputs."""

import base64
import errno
import io
import ipaddress
import json
import os
import secrets
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager, nullcontext, redirect_stderr, redirect_stdout, suppress
from pathlib import Path

import database_bootstrap_setup as setup
import database_bootstrap_baseline as baseline
from database_bootstrap_reader_role import MAX_READER_SECONDS, ReaderRoleLifecycle
from database_bootstrap_role import SetupRoleLifecycle
from fixtures.database_bootstrap.authorization import caller_env, plan_binding
from database_bootstrap_preflight import (
    BootstrapPreflightError, catalog_probe_sql, require_absent_catalog_result,
)
from test_database_bootstrap_admission import check_handoff_snapshot_branch, check_snapshot, snapshot_record
from test_database_bootstrap_environment_readback import readback, save_complete_readback

IMAGE = "postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336"
PORT = 5432
DRILL_SIGNAL = signal.SIGUSR1
CREATOR, OWNER, DECOY = "fixture_creator", "fixture_owner", "fixture_decoy"
MIGRATOR, RUNTIME, READER = "fixture_migrator", "fixture_runtime", "fixture_reader"
STAGING = "fixture_staging"
ISSUER = "fixture_issuer"
BASELINE_READER, READER_ISSUER = "Fixture-Protected-Reader", "fixture_reader_issuer"
MAINTENANCE, LAUNCH, SIBLING = "postgres", "launch_fixture", "staging_fixture"
received_signals = set()


class FixtureFailure(Exception):
    pass


class Interrupted(Exception):
    pass


class PublicationFailure(Exception):
    pass


def remember_signal(signum, _frame):
    # Let a Docker mutation finish before entering teardown.
    received_signals.add(signum)


def check_interruption(include_drill=False):
    pending = received_signals if include_drill else received_signals - {DRILL_SIGNAL}
    if pending:
        raise Interrupted(frozenset(pending))


def require(condition, verdict):
    if not condition:
        raise FixtureFailure(verdict)


def command(*args, input_text=None, timeout=20, cleanup=False, interrupt=False):
    if not cleanup:
        check_interruption()
    with subprocess.Popen(
        args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env={"PATH": os.defpath, "LC_ALL": "C"}, start_new_session=True,
    ) as process:
        try:
            if interrupt:
                readable, _, _ = select.select([process.stdout], [], [], timeout)
                require(readable and process.stdout.readline() == "ready\n" and process.poll() is None,
                        "start an in-flight Docker interruption drill")
                check_interruption()
                received_signals.discard(DRILL_SIGNAL)
                signal.raise_signal(DRILL_SIGNAL)
            stdout, stderr = process.communicate(input_text, timeout=timeout)
        finally:
            if process.poll() is None:
                process.kill()
    if interrupt:
        canary = input_text.rstrip("\n")
        require(process.returncode == 0 and stdout == canary and canary in stderr,
                "capture the in-flight command output and trace during interruption")
    if not cleanup:
        check_interruption(include_drill=interrupt)
    return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)


def docker(*args, **kwargs):
    return command("docker", "--host", "unix:///var/run/docker.sock", *args, **kwargs)


def checked(result, verdict, expected=None):
    require(result.returncode == 0 and (expected is None or result.stdout == expected), verdict)


class Cluster:
    def __init__(self, directory, branch_id):
        token = secrets.token_hex(8)
        self.network = "bootstrap-" + token
        self.server = self.network + ".test"
        self.client = self.network + "-client"
        self.branch_id = branch_id
        roles = ("postgres", CREATOR, DECOY, MIGRATOR, RUNTIME, READER, STAGING, ISSUER)
        self.passwords = {role: secrets.token_hex(24) for role in roles}
        self.expiry = int(time.time()) + 300
        self.directory = directory

    def start(self):
        server, client = self.directory / "server", self.directory / "client"
        server.mkdir()
        client.mkdir()
        checked(docker("network", "create", "--internal", self.network), "create internal network")
        subnet = docker("network", "inspect", "--format", "{{(index .IPAM.Config 0).Subnet}}", self.network)
        checked(subnet, "read the disposable subnet")
        self.address = str(ipaddress.IPv4Network(subnet.stdout.strip())[2])
        for ca in ("trusted", "other"):
            checked(command("openssl", "req", "-x509", "-newkey", "ec",
                            "-pkeyopt", "ec_paramgen_curve:P-256", "-noenc", "-days", "1",
                            "-subj", f"/CN=fixture-{ca}", "-addext", "basicConstraints=critical,CA:TRUE",
                            "-keyout", self.directory / f"{ca}.key", "-out", client / f"{ca}.crt"),
                    "generate fixture CA")
        request = self.directory / "server.csr"
        checked(command("openssl", "req", "-new", "-newkey", "ec", "-pkeyopt",
                        "ec_paramgen_curve:P-256", "-noenc", "-subj", f"/CN={self.server}",
                        "-addext", f"subjectAltName=DNS:{self.server},IP:{self.address}",
                        "-keyout", server / "server.key", "-out", request), "generate TLS request")
        checked(command("openssl", "x509", "-req", "-in", request, "-days", "1",
                        "-CA", client / "trusted.crt", "-CAkey", self.directory / "trusted.key",
                        "-copy_extensions", "copy", "-out", server / "server.crt"),
                "sign fixture server certificate")
        (server / "password").write_text(self.passwords["postgres"] + "\n")
        (server / "pg_hba.conf").write_text("local all all trust\nhostssl all all all scram-sha-256\n")
        common = ["run", "--detach", "--pull=never", "--log-driver=none",
                  "--network", self.network, "--tmpfs", "/var/lib/postgresql"]
        checked(docker(*common, "--name", self.server, "--network-alias", f"wrong-{self.server}",
                       "--mount", f"type=bind,src={server},dst=/fixture,readonly",
                       "-e", "POSTGRES_PASSWORD_FILE=/fixture/password", "--entrypoint", "sh", IMAGE,
                       "-ec", """
                       install -o postgres -g postgres -m 0600 /fixture/server.key /fixture/server.crt /fixture/pg_hba.conf /tmp/
                       exec docker-entrypoint.sh postgres -c ssl=on -c ssl_key_file=/tmp/server.key \
                           -c ssl_cert_file=/tmp/server.crt -c hba_file=/tmp/pg_hba.conf
                       """), "start disposable server")
        checked(docker(*common, "--name", self.client, "--entrypoint", "sleep",
                       "--mount", f"type=bind,src={client},dst=/fixture,readonly", IMAGE, "infinity"),
                "start isolated client")
        checked(docker("network", "inspect", "--format", "{{.Internal}}", self.network),
                "verify an internal network", "true\n")
        checked(docker("inspect", "--format", "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", self.server),
                "verify the certificate address belongs to the disposable server", self.address + "\n")
        for name in (self.server, self.client):
            checked(docker("inspect", "--format", "{{.HostConfig.PublishAllPorts}} {{len .HostConfig.PortBindings}}",
                           name), "verify no published PostgreSQL ports", "false 0\n")

        for _ in range(20):
            result = self.sql("SELECT current_setting('server_version_num')::int / 10000 = 18"
                              " AND inet_server_port() = 5432"
                              " AND (SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid());")
            if result.returncode == 0:
                checked(result, "verify PostgreSQL 18 and encrypted TCP on the pinned port", "t\n")
                return
            time.sleep(0.25)
        raise FixtureFailure("verify TLS readiness")

    def sql(self, sql, *, role="postgres", database=MAINTENANCE, host=None, ca="trusted", switch="",
            trace=False):
        sql = (f"SET ROLE {switch};\n" if switch else "") + sql
        return docker("exec", "-i", self.client, "env", "-i",
                      "PATH=/usr/local/bin:/usr/bin:/bin", "LC_ALL=C",
                      f"PGHOST={host or self.server}", f"PGPORT={PORT}", f"PGDATABASE={database}",
                      f"PGUSER={role}", "PGSSLMODE=verify-full", "PGGSSENCMODE=disable",
                      f"PGSSLROOTCERT=/fixture/{ca}.crt", "PGCONNECT_TIMEOUT=2",
                      "PGOPTIONS=-c statement_timeout=5000 -c lock_timeout=2000",
                      "sh", "-exc" if trace else "-ec", """
                      IFS= read -r PGPASSWORD
                      export PGPASSWORD="$PGPASSWORD"
                      exec psql -XqAtw -v ON_ERROR_STOP=1 -v VERBOSITY=verbose
                      """, input_text=self.passwords[role] + "\n" + sql)

    def ok(self, verdict, sql, expected="", **connection):
        checked(self.sql(sql, **connection), verdict, expected)

    def denied(self, verdict, sql, reason, **connection):
        result = self.sql(sql, **connection)
        require(result.returncode != 0 and result.stdout == "" and reason in result.stderr, verdict)

    def create_fenced(self, target=LAUNCH, expected_refusal=None, *, port=PORT, **connection):
        require(port == PORT, "refuse unapproved port")
        connection.setdefault("role", CREATOR)
        probe = catalog_probe_sql(MAINTENANCE, target, CREATOR)
        result = self.sql(probe, **connection)
        try:
            require_absent_catalog_result(result.returncode, result.stdout)
        except BootstrapPreflightError:
            refused = (
                result.returncode != 0 and result.stdout == "" and expected_refusal in result.stderr
                if expected_refusal in ("does not match host name", "certificate verify failed")
                else result.returncode == 0 and result.stdout == expected_refusal
            )
            require(refused, "verify expected catalog or TLS result")
            return
        require(expected_refusal is None, "preflight unexpectedly admitted creation")
        self.ok("create fenced database", f"CREATE DATABASE {target} TEMPLATE template0 ALLOW_CONNECTIONS false;",
                **connection)
        self.ok("verify creation fence and creator ownership", f"""
            SELECT NOT datallowconn AND datdba = '{CREATOR}'::regrole
            FROM pg_database WHERE datname = '{target}';
        """, "t\n", role=CREATOR)
        self.denied("refuse connections while fenced", "SELECT 1;", "is not currently accepting connections",
                    role=MIGRATOR, database=target)

    def teardown(self):
        # Attempt every removal. Only successful empty listings prove absence.
        for kind, name in (("container", self.client), ("container", self.server), ("network", self.network)):
            flags = ("--force", "--volumes") if kind == "container" else ()
            with suppress(OSError, subprocess.TimeoutExpired):
                docker(kind, "rm", *flags, name, cleanup=True)
        for name in (self.client, self.server):
            checked(docker("container", "ls", "-aq", "--filter", f"name=^/{name}$", cleanup=True),
                    "verify container teardown", "")
        checked(docker("network", "ls", "-q", "--filter", f"name=^{self.network}$", cleanup=True),
                "verify network teardown", "")


def provision_roles(cluster):
    valid_until = time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime(cluster.expiry))
    cluster.ok("create distinct fixture roles", f"""
        CREATE ROLE {DECOY} LOGIN CREATEDB PASSWORD '{cluster.passwords[DECOY]}' VALID UNTIL '{valid_until}';
        CREATE ROLE {OWNER} NOLOGIN;
        GRANT {OWNER} TO {DECOY} WITH INHERIT FALSE, SET TRUE;
        CREATE ROLE {ISSUER} LOGIN CREATEDB CREATEROLE PASSWORD '{cluster.passwords[ISSUER]}';
        GRANT pg_signal_backend TO {ISSUER};
        ALTER ROLE {ISSUER} SET createrole_self_grant = 'set, inherit';
        """ + "\n".join(
            f"CREATE ROLE {role} LOGIN PASSWORD '{cluster.passwords[role]}';"
            for role in (MIGRATOR, RUNTIME, READER, STAGING)
        ))


def issue_role(cluster, lifecycle, expiry):
    result = cluster.sql(lifecycle.issue(expiry, cluster.passwords[CREATOR]), role=ISSUER)
    checked(result, "issue the restricted setup role")
    receipt, separator, rest = result.stdout.partition("\n")
    require(separator and not rest and receipt.isascii() and receipt.isdigit(),
            "read back one private issuance receipt")
    return int(receipt)


def prove_issuance(cluster, lifecycle):
    issue = lifecycle.issue(cluster.expiry, secrets.token_hex(24))
    cluster.denied("roll back role creation when the issuer cannot grant the owner", issue,
                   "ERROR:  42501:", role=ISSUER)
    cluster.ok("verify failed issuance left no role", f"""
        SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{CREATOR}');
        """, "t\n")
    cluster.ok("authorize the non-superuser issuer to grant only the owner membership", f"""
        GRANT {OWNER} TO {ISSUER} WITH ADMIN TRUE, SET FALSE, INHERIT FALSE;
        """)
    for connection in ({"role": ISSUER, "database": "template1"}, {"switch": ISSUER}):
        cluster.denied("refuse an eligible issuer in the wrong custodian session", issue,
                       "ERROR:  P0001: wrong custodian session", **connection)
    for expiry in (int(time.time()) - 1, int(time.time()) + 7300):
        cluster.denied("refuse issuance outside the native expiry window",
                       lifecycle.issue(expiry, cluster.passwords[CREATOR]),
                       "ERROR:  P0001:", role=ISSUER)
    oid = issue_role(cluster, lifecycle, cluster.expiry)
    cluster.denied("refuse repeated issuance without rotating the credential", issue,
                   "ERROR:  P0001:", role=ISSUER)
    cluster.ok("read back both setup SQL identities, attributes, membership and expiry", f"""
        SELECT session_user = '{CREATOR}' AND current_user = '{CREATOR}'
           AND (SELECT rolcanlogin AND rolcreatedb AND rolconnlimit = 1 AND NOT
                    (rolinherit OR rolsuper OR rolcreaterole OR rolreplication OR rolbypassrls)
                    AND rolvaliduntil = to_timestamp({cluster.expiry}) FROM pg_roles WHERE oid = {oid})
           AND (SELECT count(*) = 1 AND bool_and(roleid = '{OWNER}'::regrole AND set_option
                    AND NOT (admin_option OR inherit_option)) FROM pg_auth_members WHERE member = {oid})
           AND (SELECT count(*) = 1 AND bool_and(member = '{ISSUER}'::regrole AND admin_option
                    AND NOT (set_option OR inherit_option)) FROM pg_auth_members WHERE roleid = {oid});
        """, "t\n", role=CREATOR)
    cluster.ok("allow switching to the loginless owner without inheriting its rights", f"""
        SELECT session_user = '{CREATOR}' AND current_user = '{OWNER}';
        """, "t\n", role=CREATOR, switch=OWNER)
    for role, sql in (
        (CREATOR, "CREATE ROLE fixture_denied"),
        (CREATOR, f"GRANT {OWNER} TO {READER}"),
        (CREATOR, f"SET ROLE {READER}"),
        (READER, f"SET ROLE {CREATOR}"),
        (READER, "CREATE DATABASE fixture_denied"),
    ):
        cluster.denied("deny authority outside the setup and reader roles", sql, "ERROR:  42501:", role=role)
    cluster.denied("refuse retirement before the login fence", lifecycle.retire(oid),
                   "ERROR:  P0001:", role=ISSUER)
    return oid


def prove_expiry_and_drain(cluster, lifecycle, old_oid):
    # Reuse the name only in the fixture to prove that an old receipt is harmless.
    cluster.passwords[CREATOR] = secrets.token_hex(24)
    expiry = int(time.time()) + 10
    oid = issue_role(cluster, lifecycle, expiry)
    cluster.denied("refuse to fence a replacement role with a stale receipt", lifecycle.fence(old_oid),
                   "ERROR:  P0001:", role=ISSUER)
    env = {"PATH": os.defpath, "LC_ALL": "C", "PGHOST": cluster.address, "PGPORT": str(PORT),
           "PGDATABASE": MAINTENANCE,
           "PGSSLMODE": "verify-full", "PGGSSENCMODE": "disable", "PGCONNECT_TIMEOUT": "2",
           "PGSSLROOTCERT": str(cluster.directory / "client" / "trusted.crt")}

    def start_session(role):
        return subprocess.Popen(["/usr/bin/psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"],
                                env=env | {"PGUSER": role, "PGPASSWORD": cluster.passwords[role]},
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def readback(session, sql, expected, timeout=3):
        session.stdin.write((sql + "\n").encode())
        session.stdin.flush()
        ready, _, _ = select.select([session.stdout], [], [], timeout)
        require(ready and session.stdout.readline() == expected, "read back a held TLS session")

    with start_session(CREATOR) as holder, start_session(READER) as reader:
        try:
            readback(holder, f"SELECT session_user = '{CREATOR}' AND current_user = '{CREATOR}';", b"t\n")
            reader_identity = f"SELECT session_user = '{READER}' AND current_user = '{READER}';"
            readback(reader, reader_identity, b"t\n")
            cluster.denied("cap concurrent setup sessions", "SELECT 1;",
                           "too many connections for role", role=CREATOR)
            # PostgreSQL checks password validity only at authentication.
            readback(holder, f"SELECT pg_sleep(GREATEST(0, extract(epoch FROM to_timestamp({expiry})"
                     " - clock_timestamp()) + 0.1));", b"\n", timeout=12)
            cluster.denied("deny fresh password authentication after native expiry", "SELECT 1;",
                           "password authentication failed", role=CREATOR)
            readback(holder, f"SET ROLE {OWNER}; SELECT current_user = '{OWNER}';", b"t\n")
            for _ in range(2):
                cluster.ok("commit the login fence independently of session cleanup", lifecycle.fence(oid),
                           role=ISSUER)
            readback(holder, "SELECT 1;", b"1\n")
            for _ in range(2):
                cluster.ok("terminate existing sessions and verify role removal", lifecycle.retire(oid),
                           "retired\n", role=ISSUER)
            stdout, _ = holder.communicate(b"SELECT 1;\n", timeout=3)
            require(holder.returncode != 0 and not stdout, "observe the terminated TLS client")
            readback(reader, reader_identity, b"t\n")
        finally:
            for session in (holder, reader):
                if session.poll() is None:
                    session.kill()


def refuse_before_create(cluster):
    cluster.ok("create and restrict the synthetic staging sibling",
               f"CREATE DATABASE {SIBLING} OWNER {STAGING} TEMPLATE template0;\n"
               f"REVOKE ALL ON DATABASE {SIBLING} FROM PUBLIC;")
    cluster.create_fenced(host=f"wrong-{cluster.server}", expected_refusal="does not match host name")
    cluster.create_fenced(ca="other", expected_refusal="certificate verify failed")
    try:
        cluster.create_fenced(port=PORT + 1)
    except FixtureFailure as error:
        require(error.args == ("refuse unapproved port",), "verify explicit port refusal")
    else:
        raise FixtureFailure("wrong port was admitted")
    cluster.create_fenced(switch=OWNER, expected_refusal="wrong-session\n")
    cluster.create_fenced(role=MIGRATOR, expected_refusal="wrong-session\n")
    cluster.create_fenced(target=SIBLING, expected_refusal="already-exists\n")
    cluster.ok("verify refusals left launch absent and staging ownership intact", f"""
        SELECT NOT EXISTS (SELECT FROM pg_database WHERE datname = '{LAUNCH}')
           AND EXISTS (SELECT FROM pg_database WHERE datname = '{SIBLING}'
                       AND datdba = '{STAGING}'::regrole AND datallowconn);
        """, "t\n", role=CREATOR)


def verify_quiet_failure(cluster):
    token = secrets.token_hex(20)
    result = cluster.sql(f"SELECT '{token}';\nSELECT missing_{token};", role=CREATOR, trace=True)
    require(result.returncode != 0 and token in result.stdout and token in result.stderr
            and cluster.passwords[CREATOR] in result.stderr,
            "capture result, error and credential trace without publishing them")


def start_caller(env, script=setup.__file__):
    return subprocess.Popen([sys.executable, "-EsB", script], env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def caller_verdict(process, expected, timeout=45):
    stdout, stderr = process.communicate(timeout=timeout)
    status = f"database-bootstrap-setup: {expected}\n".encode()
    result = (0, status, b"") if expected == "complete" else (1, b"", status)
    require((process.returncode, stdout, stderr) == result, f"emit only the caller's {expected} verdict")


def routed_client(cluster, mode, role=CREATOR):
    shim = cluster.directory / "psql"
    shutil.copyfile(Path(__file__).parent / "fixtures/database_bootstrap/psql", shim)
    shim.chmod(0o700)
    (cluster.directory / "route").write_text(
        f"{mode} {cluster.canary} {cluster.address} {role}.{cluster.branch_id} {role} {DECOY}"
    )
    return f"{cluster.directory}:/usr/bin:/bin"


def wait_for(cluster, verdict, sql):
    until = time.monotonic() + 5
    while time.monotonic() < until:
        check_interruption()
        if cluster.sql(sql).stdout == "t\n":
            return
        time.sleep(0.1)
    raise FixtureFailure(verdict)


def prove_setup_caller(cluster, *, interrupt, reader):
    calls = cluster.directory / "invocations"
    calls.touch()

    def run(env, expected, invocations):
        before = calls.stat().st_size
        with start_caller(env) as process:
            try:
                caller_verdict(process, expected)
            finally:
                if process.poll() is None:
                    process.kill()
        require(calls.stat().st_size - before == invocations, "verify the expected routed client calls")

    cluster.canary = secrets.token_hex(20)
    roles = CREATOR, OWNER, MIGRATOR, reader
    env = caller_env(cluster, roles)
    if not interrupt:
        route = {"PATH": routed_client(cluster, "route")}
        env |= route
        for overrides in (
            {"GITHUB_RUN_ATTEMPT": "2"}, {"GITHUB_RUN_ID": "0"}, {"BOOTSTRAP_STEP_DEBUG": "true"},
            {"BOOTSTRAP_PLAN_DIGEST": secrets.token_hex(32)},
            {"GITHUB_SHA": secrets.token_hex(20)}, {"GITHUB_REPOSITORY": "fixture/fork"},
        ):
            run(env | overrides, "refused", 0)
        branch = cluster.branch_id
        other_branch = ("1" if branch.startswith("0") else "0") + branch[1:]
        login = f"{CREATOR}.{branch}"
        for changes in (
            {"version": 1}, {"version": "2"}, {"version": 3},
            {"setup_login": f"{CREATOR}.{other_branch}"}, {"setup_login": f"{DECOY}.{branch}"},
            {"setup_login": CREATOR}, {"setup_login": f"{login}|replica"},
            {"setup_login": login + "\n"}, {"setup_login": None},
            {"setup_role": login, "setup_login": f"{login}.{branch}"},
            {"port": PORT + 1}, {"reader_role": CREATOR}, {"launch": MAINTENANCE},
        ):
            run(caller_env(cluster, roles, **changes) | route, "refused", 0)
        for malformed in ("a" * 9, "a" * 11, "a" * 13, "A" * 12, "á" * 12):
            run(caller_env(cluster, roles, branch_id=malformed, setup_login=f"{CREATOR}.{malformed}") | route,
                "refused", 0)
        changed_branch = caller_env(cluster, roles, branch_id=other_branch, setup_login=f"{CREATOR}.{other_branch}")
        run(env | {"BOOTSTRAP_PLAN_B64": changed_branch["BOOTSTRAP_PLAN_B64"]}, "refused", 0)
        raw = base64.b64decode(env["BOOTSTRAP_PLAN_B64"])
        run(env | plan_binding(b'{"setup_login":"ambiguous",' + raw[1:]), "refused", 0)
        plan = json.loads(raw)
        del plan["branch_id"]
        run(env | plan_binding(json.dumps(plan).encode()), "refused", 0)
        del plan["setup_login"]
        plan["version"] = 1
        run(env | plan_binding(json.dumps(plan).encode()), "refused", 0)

        for changes in (
            {"reader_role": "missing_reader"},
            {"reader_role": READER},
            {"ca_pem": (cluster.directory / "client" / "other.crt").read_text()},
        ):
            run(caller_env(cluster, roles, **changes) | route, "refused", 1)
        reader_identifier = f'"{reader}"'
        cluster.ok("grant the pending reader an unrelated SET-only membership",
                   f"GRANT {STAGING} TO {reader_identifier} WITH SET TRUE, INHERIT FALSE;")
        run(env, "refused", 1)
        cluster.ok("restore the reader to its pending state", f"REVOKE {STAGING} FROM {reader_identifier};")
        cluster.ok("raise the pending reader's connection limit",
                   f"ALTER ROLE {reader_identifier} CONNECTION LIMIT 2;")
        run(env, "refused", 1)
        cluster.ok("restore the one-connection reader limit", f"ALTER ROLE {reader_identifier} CONNECTION LIMIT 1;")
        password = secrets.token_hex(24)
        cluster.ok("enable a synthetic reader session",
                   f"ALTER ROLE {reader_identifier} PASSWORD '{password}' VALID UNTIL 'infinity';")
        run(env, "refused", 1)
        tls = setup.tls_environment(cluster.directory, setup.setup_plan(raw), MAINTENANCE, reader, password)
        del tls["PGOPTIONS"]
        with subprocess.Popen(["/usr/bin/psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"], env=tls,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as holder:
            try:
                holder.stdin.write(f"SELECT session_user = '{reader}';\n".encode())
                holder.stdin.flush()
                ready, _, _ = select.select([holder.stdout], [], [], 3)
                require(ready and holder.stdout.readline() == b"t\n", "hold an authenticated reader session")
                cluster.ok("restore the expired, passwordless reader while its session is open",
                           f"ALTER ROLE {reader_identifier} PASSWORD NULL VALID UNTIL 'epoch';")
                run(env, "refused", 1)
                holder.communicate(timeout=3)
                require(holder.returncode == 0, "close the held reader session")
            finally:
                if holder.poll() is None:
                    holder.kill()
        cluster.ok("remove the canonical role's database creation privilege", f"ALTER ROLE {CREATOR} NOCREATEDB;")
        run(env, "refused", 1)
        cluster.ok("restore the canonical role's database creation privilege", f"ALTER ROLE {CREATOR} CREATEDB;")
        run(env | {"PATH": routed_client(cluster, "wrong-role"),
                   "BOOTSTRAP_SETUP_PASSWORD": cluster.passwords[DECOY]}, "refused", 1)
        require((cluster.directory / "probe-result").read_bytes() == b"wrong-session\nroles-ready\n",
                "observe a different eligible SQL identity on the approved route")
        run(env | {"PATH": routed_client(cluster, "standby")}, "refused", 1)
        require((cluster.directory / "standby-result").read_bytes() == b"",
                "refuse the recovery-state probe without a SQL error")
        run(env | {"PATH": routed_client(cluster, "error")}, "refused", 1)
        diagnostics = (cluster.directory / "diagnostics").read_bytes()
        require(all(value.encode() in diagnostics for value in
                    (cluster.passwords[CREATOR], cluster.address, "ERROR:", cluster.canary)),
                "exercise generated credential trace, endpoint and SQL error containment")
        dns = {"PATH": routed_client(cluster, "resolve")}
        run(caller_env(cluster, roles, host=f"wrong-{cluster.server}") | dns, "refused", 1)
        env = caller_env(cluster, roles, host=cluster.server) | dns
        run(env | {"BOOTSTRAP_EXPIRY": str(cluster.expiry - 60)}, "refused", 1)
        cluster.ok("verify all caller refusals leave the target absent",
                   f"SELECT NOT EXISTS (SELECT FROM pg_database WHERE datname = '{LAUNCH}');", "t\n")
        sibling_sql = (f"SELECT to_jsonb(d) FROM pg_database d WHERE datname IN ('{MAINTENANCE}', '{SIBLING}') "
                       "ORDER BY datname;")
        siblings = cluster.sql(sibling_sql)
        checked(siblings, "record the staging and reserved database identities and ACLs")
        require(len(siblings.stdout.splitlines()) == 2, "observe both sibling databases before setup")
        sibling_env = caller_env(cluster, roles, host=cluster.server, launch=SIBLING)
        run(sibling_env | {"PATH": env["PATH"]}, "refused", 1)
        run(env, "complete", 3)
        identity_sql = f"""SELECT oid FROM pg_database WHERE datname = '{LAUNCH}' AND datallowconn
            AND NOT datistemplate AND datdba = '{OWNER}'::regrole
            AND NOT has_database_privilege('{reader}', oid, 'CONNECT');"""
        identity = cluster.sql(identity_sql)
        checked(identity, "read back the new database identity with the reader still fenced")
        require(identity.stdout.strip().isdigit(), "observe a single launch database OID")
        run(env, "refused", 1)  # The name collision stops any second write.
        cluster.ok("preserve the new database OID after collision refusal", identity_sql, identity.stdout)
        cluster.ok("preserve both sibling database identities and ACLs", sibling_sql, siblings.stdout)
        return int(identity.stdout)

    sessions = f"SELECT FROM pg_stat_activity WHERE usename = '{CREATOR}'"
    with start_caller(env | {"PATH": routed_client(cluster, "interrupt")}) as process:
        try:
            wait_for(cluster, "observe the caller inside the setup transaction",
                     f"SELECT EXISTS ({sessions} AND wait_event = 'PgSleep');")
            process.terminate()
            caller_verdict(process, "outcome-uncertain", timeout=3)
        finally:
            if process.poll() is None:
                process.kill()
    wait_for(cluster, "verify the interrupted SQL session closed", f"SELECT NOT EXISTS ({sessions});")
    cluster.ok("verify interruption rolled back the grants and left the create fenced", f"""
        SELECT NOT datallowconn AND datdba = '{CREATOR}'::regrole AND datacl IS NULL
        FROM pg_database WHERE datname = '{LAUNCH}';
        """, "t\n")
    require(not list(cluster.directory.glob("database-bootstrap-ca-*")), "remove the caller's temporary CA")


def prove_baseline(cluster):
    reader = "Fixture-Reader"
    result = cluster.sql(f"SELECT oid, '{READER}'::regrole::oid FROM pg_database WHERE datname = '{LAUNCH}';")
    checked(result, "identify the dedicated fixture database")
    raw_env = caller_env(cluster, (CREATOR, OWNER, MIGRATOR, READER))
    setup_plan = json.loads(base64.b64decode(raw_env["BOOTSTRAP_PLAN_B64"]))
    plan = {key: setup_plan[key] for key in ("host", "port", "ca_pem", "branch_id", "launch", "reader_role")}
    database_oid, reader_oid = map(int, result.stdout.strip().split("|"))
    plan.update(version=2, database_oid=database_oid, reader_oid=reader_oid, reader_role=reader)
    env = raw_env | {
        "BASELINE_ENABLED": "true",
        "BASELINE_READER_PASSWORD": cluster.passwords[READER],
        "PATH": routed_client(cluster, "route", reader),
        "GITHUB_WORKFLOW_REF": f'{raw_env["GITHUB_REPOSITORY"]}/{baseline.WORKFLOW}@refs/heads/main',
    }
    del env["BOOTSTRAP_SETUP_PASSWORD"]

    def run(expected, *, overrides=None, **plan_changes):
        binding = plan_binding(json.dumps(plan | plan_changes).encode())
        with start_caller(env | binding | (overrides or {}), baseline.__file__) as process:
            stdout, stderr = process.communicate(timeout=20)
            line = f"database-bootstrap-baseline: {expected}\n".encode()
            result = (1, b"", line) if expected == "refused" else (0, line, b"")
            require((process.returncode, stdout, stderr) == result, f"baseline emits only {expected}")

    cluster.ok("grant baseline reader only CONNECT to the dedicated database",
               f'GRANT CONNECT ON DATABASE {LAUNCH} TO {READER};', role=CREATOR, switch=OWNER)
    fixture_plan = baseline.baseline_plan(json.dumps(plan | {"reader_role": READER}).encode())
    prove_missing_baseline_relation(cluster, fixture_plan)
    cluster.ok("use the exact mixed-case SQL reader name", f'ALTER ROLE {READER} RENAME TO "{reader}";')
    calls = cluster.directory / "invocations"
    before = calls.stat().st_size
    run("refused", overrides={"BASELINE_ENABLED": ""})
    run("refused", overrides={"GITHUB_WORKFLOW_REF": raw_env["GITHUB_WORKFLOW_REF"]})
    run("refused", launch=MAINTENANCE)
    for invalid_oid in (True, "1", 0, -1, 4294967296):
        run("refused", reader_oid=invalid_oid)
    run("refused", version=1)
    require(calls.stat().st_size == before, "refuse disabled, cross-workflow and invalid plans before SQL")
    run("refused", database_oid=plan["database_oid"] + 1)
    run("empty")
    with tempfile.TemporaryDirectory(dir=cluster.directory, prefix="handoff-environment-") as evidence:
        directory = Path(evidence)
        save_complete_readback(directory)
        require(readback(directory).returncode == 0, "verify both saved Environment readbacks for the handoff")
        check_handoff_snapshot_branch(plan["branch_id"])
    run("refused", overrides={"PATH": routed_client(cluster, "standby", reader)})
    require(b"baseline scope refused" in (cluster.directory / "standby-result").read_bytes(),
            "refuse the simulated PostgreSQL recovery verdict for an otherwise eligible reader")
    routed_client(cluster, "route", reader)
    cluster.ok("prepare a partition with hidden child grants", f"""
        CREATE SCHEMA baseline_fixture;
        CREATE TABLE baseline_fixture.parent (id integer) PARTITION BY RANGE (id);
        CREATE TABLE baseline_fixture.child PARTITION OF baseline_fixture.parent FOR VALUES FROM (0) TO (10);
        INSERT INTO baseline_fixture.parent VALUES (5);
        GRANT USAGE ON SCHEMA baseline_fixture TO "{reader}";
        GRANT SELECT ON baseline_fixture.parent TO "{reader}";
        """, role=MIGRATOR, database=LAUNCH)
    run("refused")
    cluster.ok("grant direct leaf coverage", f'GRANT SELECT ON baseline_fixture.child TO "{reader}";', database=LAUNCH)
    run("rows-present")
    cluster.ok("hide leaf rows with RLS", "ALTER TABLE baseline_fixture.child ENABLE ROW LEVEL SECURITY;", database=LAUNCH)
    run("refused")
    cluster.ok("restore visibility and grant a column write", f"""
        ALTER TABLE baseline_fixture.child DISABLE ROW LEVEL SECURITY;
        GRANT UPDATE (id) ON baseline_fixture.child TO "{reader}";
        """, database=LAUNCH)
    run("refused")
    cluster.ok("replace the fixture tables with one large object",
               "DROP SCHEMA baseline_fixture CASCADE; SELECT lo_create(900001);", "900001\n", database=LAUNCH)
    run("rows-present")
    cluster.ok("grant large-object write access", f'GRANT UPDATE ON LARGE OBJECT 900001 TO "{reader}";', database=LAUNCH)
    run("refused")
    cluster.ok("retire the baseline fixture's grants and object", f"""
        SELECT lo_unlink(900001);
        ALTER ROLE "{reader}" RENAME TO {READER};
        REVOKE CONNECT ON DATABASE {LAUNCH} FROM {READER};
        """, "1\n", database=LAUNCH)


def prove_missing_baseline_relation(cluster, plan):
    launch = {"database": LAUNCH, "role": MIGRATOR}
    reader = {"database": LAUNCH, "role": READER}
    # Rehearse the operator's reviewed inventory comparison. The protected
    # baseline itself only scans relations that exist in the catalog.
    begin = "\nBEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;\n"
    sql = baseline.baseline_sql(plan).decode("ascii")
    require(sql.startswith(begin), "use the baseline's read-only snapshot for inventory reconciliation")
    reconciled_sql = begin + """
        DO $inventory$
        BEGIN
            IF ARRAY(
                SELECT c.relname::text
                FROM pg_catalog.pg_class c
                JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                WHERE n.nspname = 'baseline_readiness_fixture' AND c.relkind = 'r'
                ORDER BY c.relname
            ) <> ARRAY['required', 'retained'] THEN
                RAISE EXCEPTION 'reviewed baseline inventory differs';
            END IF;
        END
        $inventory$;
    """ + sql.removeprefix(begin)

    cluster.ok("prepare the complete reviewed fixture inventory with no rows", f"""
        CREATE SCHEMA baseline_readiness_fixture;
        CREATE TABLE baseline_readiness_fixture.required (id integer);
        CREATE TABLE baseline_readiness_fixture.retained (id integer);
        GRANT USAGE ON SCHEMA baseline_readiness_fixture TO {READER};
        GRANT SELECT ON ALL TABLES IN SCHEMA baseline_readiness_fixture TO {READER};
    """, **launch)
    try:
        cluster.ok("accept the exact reviewed empty control", reconciled_sql, "empty\n", **reader)
        cluster.ok("remove the required empty table", "DROP TABLE baseline_readiness_fixture.required;", **launch)
        cluster.ok("confirm the remaining inventory still has no rows", sql, "empty\n", **reader)
        cluster.denied("refuse readiness despite the empty census", reconciled_sql,
                       "ERROR:  P0001: reviewed baseline inventory differs", **reader)
    finally:
        cluster.ok("remove the reviewed inventory fixture", "DROP SCHEMA baseline_readiness_fixture CASCADE;", **launch)


def prepare_baseline_readers(cluster, names, *, host):
    cluster.passwords[READER_ISSUER] = secrets.token_hex(24)
    cluster.ok("scope reader issuance and signalling separately from setup", f"""
        CREATE ROLE {READER_ISSUER} LOGIN CREATEROLE PASSWORD '{cluster.passwords[READER_ISSUER]}';
        GRANT pg_signal_backend TO {READER_ISSUER};
    """)
    pending = []
    for name in names:
        env = caller_env(cluster, (CREATOR, OWNER, MIGRATOR, name), host=host)
        raw = base64.b64decode(env.pop("BOOTSTRAP_PLAN_B64"))
        env.pop("BOOTSTRAP_SETUP_PASSWORD")
        lifecycle = ReaderRoleLifecycle(raw, READER_ISSUER)
        prepare = lifecycle.prepare()
        if not pending:
            cluster.denied("refuse the setup issuer as the reader issuer", prepare,
                           "wrong custodian session", role=ISSUER)
        result = cluster.sql(prepare, role=READER_ISSUER)
        checked(result, "prepare an unusable reader for the setup caller")
        oid = int(result.stdout)
        cluster.ok("read back pending reader attributes and a NULL password", f"""
            SELECT rolcanlogin AND rolconnlimit = 1 AND rolpassword IS NULL AND NOT
                (rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)
                AND rolvaliduntil = 'epoch'::timestamptz FROM pg_authid WHERE oid = {oid};
        """, "t\n")
        pending.append((lifecycle, oid, env))
    return pending


def start_sql_holder(cluster, plan, role, password):
    env = setup.tls_environment(cluster.directory, plan, LAUNCH, role, password)
    env["PGHOSTADDR"] = cluster.address
    return subprocess.Popen(
        ["/usr/bin/psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"], env=env,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )


@contextmanager
def block_fixture_probe(cluster, plan):
    # A premature row probe needs ACCESS SHARE on the leaf and hits the lock timeout.
    with start_sql_holder(cluster, plan, "postgres", cluster.passwords["postgres"]) as locker:
        try:
            locker.stdin.write(b"BEGIN; LOCK TABLE ONLY reader_scope.leaf IN ACCESS EXCLUSIVE MODE; SELECT 1;\n")
            locker.stdin.flush()
            require(select.select([locker.stdout], [], [], 3)[0] and locker.stdout.readline() == b"1\n",
                    "hold the leaf lock before launching the wrong-OID reader")
            yield
            locker.communicate(b"ROLLBACK;\n", timeout=3)
            require(locker.returncode == 0, "release the synthetic leaf lock")
        finally:
            if locker.poll() is None:
                locker.kill()


def publish_handoff_snapshot(path, record, *, partial=False):
    payload = json.dumps(record).encode()
    try:
        with path.open("xb") as destination:
            if partial:
                destination.write(payload[:len(payload) // 2])
                raise OSError(errno.EIO, "synthetic upload interrupted")
            destination.write(payload)
    except OSError as error:
        raise PublicationFailure(error.errno) from error


def prove_publication_recovery(cluster, pending, *, partial, database_oid):
    lifecycle, reader_oid, _ = pending
    branch = lifecycle.plan.branch_id
    record = snapshot_record(branch)
    record["review"].update(phase="pre-migration", setup_completed_at=record["snapshot"]["captured_at"] - 1)
    record["baseline_branch_id"] = branch
    with tempfile.TemporaryDirectory(dir=cluster.directory, prefix="handoff-publication-") as temp:
        publication = Path(temp) / "published" / "snapshot.json"
        if partial:
            publication.parent.mkdir()
        try:
            prove_reader_lifecycle(
                cluster, pending, scenario="success", database_oid=database_oid,
                publish=lambda: publish_handoff_snapshot(publication, record, partial=partial),
            )
        except PublicationFailure as error:
            expected = errno.EIO if partial else errno.ENOENT
            require(error.args == (expected,), "observe the expected publication fault")
        else:
            raise FixtureFailure("fail evidence publication after an empty baseline")

        cluster.ok("verify reader, sessions and shared grants are absent after publication failure", f"""
            SELECT NOT EXISTS (SELECT FROM pg_roles WHERE oid = {reader_oid} OR rolname = '{lifecycle.name}')
                AND NOT EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = {reader_oid})
                AND NOT EXISTS (SELECT FROM pg_shdepend WHERE refclassid = 'pg_authid'::regclass
                                AND refobjid = {reader_oid});
        """, "t\n")
        saved = publication.read_bytes() if publication.exists() else b""
        require(bool(saved) == partial, "observe the intended missing or partial artifact")
        args = ("--baseline-handoff",)
        check_snapshot("failed publication keeps the migration handoff closed", saved, args=args)

        # Reobserve the target over the setup endpoint using the original database OID as the pin.
        readback = f"""
            BEGIN READ ONLY;
            SELECT oid FROM pg_database WHERE datname = current_database() AND oid = {{expected_oid}}
                AND datname = '{lifecycle.plan.launch}'
                AND datallowconn AND NOT datistemplate AND datdba = '{lifecycle.plan.owner_role}'::regrole;
            COMMIT;
        """
        sibling = cluster.sql(f"SELECT oid FROM pg_database WHERE datname = '{SIBLING}';")
        checked(sibling, "read back the sibling database OID")
        sibling_oid = int(sibling.stdout)
        require(sibling_oid != database_oid, "distinguish the sibling database from the pinned launch target")
        launch = {"database": lifecycle.plan.launch, "host": lifecycle.plan.host}
        cluster.ok("refuse a different OID on the launch route", readback.format(expected_oid=sibling_oid), **launch)
        pinned_readback = readback.format(expected_oid=database_oid)
        cluster.ok("refuse to reconcile a sibling at the setup endpoint", pinned_readback,
                   database=SIBLING, host=lifecycle.plan.host)
        cluster.ok("reconcile the database from setup without reprovisioning", pinned_readback,
                   f"{database_oid}\n", **launch)
        other_branch = branch[:-1] + ("1" if branch.endswith("0") else "0")
        other_record = record | {
            "review": record["review"] | {"branch_id": other_branch},
            "snapshot": record["snapshot"] | {"branch_id": other_branch},
        }
        publication.parent.mkdir(exist_ok=True)
        other_artifact = publication.with_name("wrong-target.json")
        publish_handoff_snapshot(other_artifact, other_record)
        other_bytes = other_artifact.read_bytes()
        check_snapshot("retry with a complete snapshot for a different branch", other_bytes, args=args)
        retry = publication.with_name("retry.json")
        publish_handoff_snapshot(retry, record)
        check_snapshot("retry with matching evidence after retirement", retry.read_bytes(), accept=True, args=args)


def prove_reader_lifecycle(cluster, pending, *, scenario, database_oid, publish=None):
    lifecycle, oid, env = pending
    launch = {"database": LAUNCH, "role": READER_ISSUER}
    plan = {key: getattr(lifecycle.plan, key) for key in ("host", "port", "ca_pem", "branch_id", "launch", "reader_role")}
    result = cluster.sql(f"SELECT '{DECOY}'::regrole::oid;")
    checked(result, "identify the other fixture reader OID")
    other_oid = int(result.stdout)
    plan.update(version=2, database_oid=database_oid, reader_oid=oid)
    raw = json.dumps(plan).encode()
    cluster.ok("delegate only launch CONNECT and schema/table grant options", f"""
        GRANT CONNECT ON DATABASE {LAUNCH} TO {READER_ISSUER} WITH GRANT OPTION;
        REVOKE USAGE ON SCHEMA public FROM PUBLIC;
        CREATE SCHEMA reader_scope;
        CREATE TABLE reader_scope.root (id integer) PARTITION BY RANGE (id);
        CREATE TABLE reader_scope.leaf PARTITION OF reader_scope.root FOR VALUES FROM (0) TO (10);
        GRANT USAGE ON SCHEMA reader_scope TO {READER_ISSUER} WITH GRANT OPTION;
    """, database=LAUNCH)
    if publish is None:
        cluster.ok("populate the reader fixture", "INSERT INTO reader_scope.root VALUES (5);", database=LAUNCH)
    password = secrets.token_hex(24)
    expiry = int(time.time()) + MAX_READER_SECONDS
    activate = lifecycle.activate(raw, oid, expiry, password)
    cluster.denied("refuse the wrong catalog OID", lifecycle.activate(
        json.dumps(plan | {"database_oid": plan["database_oid"] + 1}).encode(), oid, expiry, password),
        "wrong custodian session", **launch)
    cluster.denied("refuse inherited PUBLIC access to unrelated databases", activate, "reader scope refused", **launch)
    cluster.ok("reconcile the disposable cluster's PUBLIC database grants explicitly", f"""
        GRANT CONNECT ON DATABASE postgres TO {READER_ISSUER};
        REVOKE CONNECT ON DATABASE postgres, template1 FROM PUBLIC;
    """)
    cluster.denied("refuse reader activation from maintenance with the correct issuer and plan", activate,
                   "wrong custodian session", role=READER_ISSUER, database=MAINTENANCE)
    cluster.ok("model a wider PUBLIC column grant", "GRANT UPDATE (id) ON reader_scope.leaf TO PUBLIC;", database=LAUNCH)
    cluster.denied("refuse the wider PUBLIC column grant", activate, "reader scope refused", **launch)
    cluster.ok("remove the synthetic wider grant", "REVOKE UPDATE (id) ON reader_scope.leaf FROM PUBLIC;", database=LAUNCH)
    cluster.denied("roll back partial grants if a required grant option is missing", activate,
                   "ERROR:  42501:", **launch)
    cluster.ok("verify refusal preserved both credential and CONNECT fences", f"""
        SELECT rolpassword IS NULL AND NOT has_database_privilege(oid, '{LAUNCH}', 'CONNECT')
        FROM pg_authid WHERE oid = {oid};
        GRANT SELECT ON reader_scope.leaf TO {READER_ISSUER} WITH GRANT OPTION;
    """, "t\n", database=LAUNCH)
    cluster.denied("refuse an overlong credential", lifecycle.activate(raw, oid, expiry + 300, password),
                   "reader scope refused", **launch)
    wrong_binding = json.dumps(plan | {"reader_oid": other_oid}).encode()
    try:
        lifecycle.activate(wrong_binding, oid, expiry, password)
    except ValueError:
        pass
    else:
        raise FixtureFailure("refuse activation with a plan pinned to another role")

    holder = None
    try:
        expiry = int(time.time()) + 15
        cluster.ok("activate with direct physical leaf coverage", lifecycle.activate(raw, oid, expiry, password), **launch)
        with tempfile.NamedTemporaryFile(dir=cluster.directory, prefix="reader-reference-") as reference:
            reference.write(password.encode("ascii"))
            reference.seek(0)
            require(os.fstat(reference.fileno()).st_mode & 0o777 == 0o600, "keep the generated secret private")
            mode = {"success": "resolve", "wrong-oid": "baseline-wrong-oid", "interrupt": "baseline-interrupt"}[scenario]
            env.update(
                plan_binding(wrong_binding if scenario == "wrong-oid" else raw),
                BASELINE_ENABLED="true", BASELINE_READER_PASSWORD=reference.read().decode("ascii"),
                BOOTSTRAP_EXPIRY=str(expiry),
                PATH=routed_client(cluster, mode, lifecycle.name),
                GITHUB_WORKFLOW_REF=f'{env["GITHUB_REPOSITORY"]}/{baseline.WORKFLOW}@refs/heads/main',
            )
            calls = cluster.directory / "invocations"
            before = calls.stat().st_size
            sessions = f"SELECT FROM pg_stat_activity WHERE usesysid = {oid} AND datid = {database_oid}"
            lock = block_fixture_probe(cluster, lifecycle.plan) if scenario == "wrong-oid" else nullcontext()
            with lock, start_caller(env, baseline.__file__) as process:
                try:
                    if scenario == "interrupt":
                        wait_for(cluster, "observe the bound reader inside the baseline transaction",
                                 f"SELECT EXISTS ({sessions} AND wait_event = 'PgSleep');")
                        process.terminate()
                    stdout, stderr = process.communicate(timeout=5)
                    if scenario == "success":
                        verdict = "rows-present" if publish is None else "empty"
                        expected = (0, f"database-bootstrap-baseline: {verdict}\n".encode(), b"")
                    else:
                        expected = (1, b"", b"database-bootstrap-baseline: refused\n")
                    require((process.returncode, stdout, stderr) == expected, "publish only the fixed reader verdict")
                finally:
                    if process.poll() is None:
                        process.kill()
            require(calls.stat().st_size == before + 1, "attempt exactly one routed reader session")
            if scenario == "wrong-oid":
                result = cluster.directory / "baseline-result"
                require(result.is_file() and result.read_text() == "reader-oid-refused\n",
                        "observe a PostgreSQL rejection at the issued reader OID guard")
            wait_for(cluster, "verify the reader caller closed its SQL session", f"SELECT NOT EXISTS ({sessions});")
        if scenario == "success":
            cluster.denied("refuse to rotate an active reader", activate, "reader is no longer pending", **launch)
            cluster.denied("refuse to retire before fencing", lifecycle.retire(raw, oid), "reader retirement refused", **launch)
            holder = start_sql_holder(cluster, lifecycle.plan, lifecycle.name, password)
            holder.stdin.write(
                f"SELECT (rolvaliduntil = to_timestamp({expiry}))::int FROM pg_roles WHERE oid = {oid};\n".encode()
            )
            holder.stdin.flush()
            require(select.select([holder.stdout], [], [], 3)[0] and holder.stdout.readline() == b"1\n",
                    "observe a surviving reader session before retirement")
        if publish is not None:
            publish()
    finally:
        env.pop("BASELINE_READER_PASSWORD", None)
        try:
            cluster.ok("fence the reader from maintenance", lifecycle.fence(oid), role=READER_ISSUER)
            cluster.ok("read back the cleared credential", f"""
                SELECT NOT rolcanlogin AND rolpassword IS NULL AND rolvaliduntil = 'epoch'::timestamptz
                FROM pg_authid WHERE oid = {oid};
            """, "t\n")
            cluster.ok("revoke the scoped grants and read back role and session absence", lifecycle.retire(raw, oid),
                       "retired\n", **launch)
            if holder is not None:
                stdout, _ = holder.communicate(b"SELECT 1;\n", timeout=3)
                require(holder.returncode != 0 and not stdout, "observe the reader server session terminate")
        finally:
            if holder is not None and holder.poll() is None:
                holder.kill()
                holder.communicate(timeout=3)
        require(not list(cluster.directory.glob("reader-reference-*")), "remove the generated connection reference")
        require(not list(cluster.directory.glob("database-baseline-ca-*")), "remove the caller's temporary CA")
        cluster.ok("restore the disposable maintenance grants and remove fixture relations", f"""
            GRANT CONNECT ON DATABASE postgres, template1 TO PUBLIC;
            REVOKE CONNECT ON DATABASE postgres FROM {READER_ISSUER};
            DROP SCHEMA reader_scope CASCADE;
        """, database=LAUNCH)


def verify_launch_grants(cluster):
    cluster.ok("verify revoked PUBLIC access and transferred ownership on admission", f"""
        SELECT datallowconn AND datdba = '{OWNER}'::regrole
           AND NOT EXISTS (SELECT FROM aclexplode(datacl) WHERE grantee = 0)
        FROM pg_database WHERE datname = '{LAUNCH}' AND datacl IS NOT NULL;
        """, "t\n", role=CREATOR)
    for role in (RUNTIME, READER, STAGING):
        cluster.denied("refuse connection during migrator-only setup", "SELECT 1;",
                       "permission denied for database", role=role, database=LAUNCH)
    cluster.ok("establish migrator-owned schema and default privileges", f"""
        CREATE SCHEMA app;
        GRANT USAGE ON SCHEMA app TO {RUNTIME}, {READER};
        ALTER DEFAULT PRIVILEGES IN SCHEMA app
            GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {RUNTIME};
        ALTER DEFAULT PRIVILEGES IN SCHEMA app GRANT SELECT ON TABLES TO {READER};
        ALTER DEFAULT PRIVILEGES IN SCHEMA app GRANT USAGE ON SEQUENCES TO {RUNTIME};
        ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
        CREATE TABLE app.tracks (id serial PRIMARY KEY, title text);
        CREATE TABLE app.ledger (version int);
        REVOKE ALL ON app.ledger FROM {RUNTIME};
        CREATE TABLE app.hidden (id int);
        ALTER TABLE app.hidden ENABLE ROW LEVEL SECURITY;
        CREATE FUNCTION app.private_fn() RETURNS int LANGUAGE sql AS 'SELECT 1';
        INSERT INTO app.tracks (title) VALUES ('fixture');
        INSERT INTO app.ledger VALUES (1);
        INSERT INTO app.hidden VALUES (1);
        """, role=MIGRATOR, database=LAUNCH)
    cluster.ok("withdraw setup authority and admit serving roles", f"""
        REVOKE CREATE ON DATABASE {LAUNCH} FROM {MIGRATOR};
        GRANT CONNECT ON DATABASE {LAUNCH} TO {RUNTIME}, {READER};
        """, role=CREATOR, switch=OWNER)
    cluster.ok("retire creator membership", f"REVOKE {OWNER} FROM {CREATOR};", role=ISSUER)
    cluster.ok("verify creator cannot reassume ownership", f"""
        SELECT NOT pg_has_role(current_user, '{OWNER}', 'SET')
           AND NOT has_database_privilege(current_user, '{LAUNCH}', 'CONNECT');
        """, "t\n", role=CREATOR)
    cluster.ok("verify runtime CRUD and sequence defaults", f"""
        BEGIN;
        SELECT session_user = '{RUNTIME}' AND current_user = '{RUNTIME}'
           AND NOT has_table_privilege(current_user, 'app.tracks', 'SELECT WITH GRANT OPTION');
        INSERT INTO app.tracks (title) VALUES ('runtime');
        UPDATE app.tracks SET title = 'updated' WHERE title = 'runtime' RETURNING title;
        DELETE FROM app.tracks WHERE title = 'updated' RETURNING title;
        ROLLBACK;
        """, "t\nupdated\nupdated\n", role=RUNTIME, database=LAUNCH)
    for role, sql in (
        (READER, "INSERT INTO app.tracks (title) VALUES ('denied')"),
        (READER, "UPDATE app.tracks SET title = 'denied'"),
        (READER, "DELETE FROM app.tracks"),
        (READER, "SELECT nextval('app.tracks_id_seq')"),
        (READER, "CREATE TABLE app.denied(id int)"),
        (READER, "SET ROLE " + MIGRATOR),
        (RUNTIME, "UPDATE app.ledger SET version = 2"),
        (RUNTIME, "TRUNCATE app.tracks"),
        (RUNTIME, "CREATE TEMP TABLE denied(id int)"),
        (RUNTIME, "CREATE SCHEMA denied"),
        (RUNTIME, "SELECT app.private_fn()"),
        (MIGRATOR, "CREATE SCHEMA denied"),
    ):
        cluster.denied("verify scoped SQL refusal", f"BEGIN; {sql}; ROLLBACK;", "ERROR:  42501:",
                       role=role, database=LAUNCH)

    cluster.ok("verify migrator sees the hidden sentinel and owns the ledger", f"""
        SELECT NOT row_security_active('app.hidden') AND EXISTS (SELECT FROM app.hidden)
           AND (SELECT relowner = current_user::regrole FROM pg_class WHERE oid = 'app.ledger'::regclass);
        """, "t\n", role=MIGRATOR, database=LAUNCH)
    cluster.ok("verify reader census distinguishes population from RLS-obscured absence", f"""
        BEGIN READ ONLY;
        SELECT session_user = '{READER}' AND current_user = '{READER}'
           AND NOT row_security_active('app.tracks') AND EXISTS (SELECT FROM app.tracks)
           AND EXISTS (SELECT FROM app.ledger)
           AND row_security_active('app.hidden') AND NOT EXISTS (SELECT FROM app.hidden);
        COMMIT;
        """, "t\n", role=READER, database=LAUNCH)

    cluster.ok("verify staging login on its own database", "SELECT 1;", "1\n", role=STAGING, database=SIBLING)
    cluster.denied("refuse staging login on launch", "SELECT 1;", "permission denied for database",
                   role=STAGING, database=LAUNCH)
    for role in (CREATOR, MIGRATOR, RUNTIME, READER):
        cluster.denied("refuse launch login on staging", "SELECT 1;", "permission denied for database",
                       role=role, database=SIBLING)
    cluster.ok("verify the launch owner also lacks staging access", f"""
        SELECT NOT has_database_privilege('{OWNER}', '{SIBLING}', 'CONNECT');
        """, "t\n", role=CREATOR)


def exercise(*, interrupt):
    try:
        with tempfile.TemporaryDirectory(prefix="database-bootstrap-") as temp:
            cluster = Cluster(Path(temp), branch_id=secrets.token_hex(5 if interrupt else 6))
            try:
                cluster.start()
                provision_roles(cluster)
                env = caller_env(cluster, (CREATOR, OWNER, MIGRATOR, READER))
                lifecycle = SetupRoleLifecycle(base64.b64decode(env["BOOTSTRAP_PLAN_B64"]))
                oid = prove_issuance(cluster, lifecycle)
                if not interrupt:
                    refuse_before_create(cluster)
                    verify_quiet_failure(cluster)
                scenarios = ("success", "wrong-oid", "interrupt")
                names = (BASELINE_READER,) if interrupt else (
                    BASELINE_READER, "Fixture-Wrong-OID", "Fixture-Interrupted", "Fixture-Unavailable", "Fixture-Partial",
                )
                pending = prepare_baseline_readers(cluster, names, host=cluster.address if interrupt else cluster.server)
                database_oid = prove_setup_caller(cluster, interrupt=interrupt, reader=BASELINE_READER)
                if interrupt:
                    (reader_lifecycle, reader_oid, _), = pending
                    cluster.ok("fence the pending reader", reader_lifecycle.fence(reader_oid), role=READER_ISSUER)
                    cluster.ok("discard the pending reader", reader_lifecycle.retire(None, reader_oid),
                               "retired\n", role=READER_ISSUER)
                else:
                    prove_baseline(cluster)
                    for reader, scenario in zip(pending[:3], scenarios, strict=True):
                        prove_reader_lifecycle(cluster, reader, scenario=scenario, database_oid=database_oid)
                    for reader, partial in zip(pending[3:], (False, True), strict=True):
                        prove_publication_recovery(cluster, reader, partial=partial, database_oid=database_oid)
                    verify_launch_grants(cluster)
                cluster.ok("revoke setup login and password", lifecycle.fence(oid), role=ISSUER)
                cluster.ok("read back the login fence and cleared password", f"""
                    SELECT NOT rolcanlogin AND NOT rolcreatedb AND rolpassword IS NULL
                       AND rolvaliduntil = 'epoch'::timestamptz FROM pg_authid WHERE oid = {oid};
                    """, "t\n")
                cluster.denied("refuse setup authentication after revocation", "SELECT 1;",
                               "password authentication failed", role=CREATOR)
                if interrupt:
                    cluster.denied("preserve the interrupted database on failed retirement",
                                   lifecycle.retire(oid), "ERROR:  2BP01:", role=ISSUER)
                    cluster.denied("retain revocation after failed retirement", "SELECT 1;",
                                   "password authentication failed", role=CREATOR)
                    cluster.ok("reconcile only the known fenced fixture database", f"""
                        ALTER DATABASE {LAUNCH} OWNER TO {OWNER};
                        SELECT NOT datallowconn AND datdba = '{OWNER}'::regrole
                        FROM pg_database WHERE datname = '{LAUNCH}';
                        """, "t\n")
                cluster.ok("read back setup role removal and zero sessions", lifecycle.retire(oid),
                           "retired\n", role=ISSUER)
                if interrupt:
                    docker("exec", "-i", cluster.client, "sh", "-exc",
                           'echo ready; read -r line; printf "%s" "$line"',
                           input_text=secrets.token_hex(24) + "\n", timeout=3, interrupt=True)
                    raise FixtureFailure("interruption did not stop the fixture")
                prove_expiry_and_drain(cluster, lifecycle, oid)
            finally:
                cluster.teardown()
    except Interrupted as error:
        if error.args != (frozenset({DRILL_SIGNAL}),):
            raise
    require(not Path(temp).exists(), "verify generated files were removed")


def main():
    require(len(sys.argv) == 1, "fixture accepts no arguments")
    os.umask(0o077)
    for signum in (signal.SIGINT, signal.SIGTERM, DRILL_SIGNAL):
        signal.signal(signum, remember_signal)
    if docker("image", "inspect", IMAGE).returncode != 0:
        checked(docker("pull", IMAGE, timeout=180), "pull pinned PostgreSQL image")
    exercise(interrupt=False)
    exercise(interrupt=True)
    check_interruption()
    if output := os.environ.get("GITHUB_OUTPUT"):
        Path(output).write_text("verdict=passed\n", encoding="utf-8")
        check_interruption()


if __name__ == "__main__":
    try:
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            main()
        require(output.tell() == 0, "keep child diagnostics out of the fixture output")
    except FixtureFailure as error:
        sys.exit(f"database-bootstrap-fixture: failed to {error}")
    except Exception:
        sys.exit("database-bootstrap-fixture: failed")
    print("database-bootstrap-fixture: routed TLS, setup role lifecycle, baseline, isolation and teardown passed")
