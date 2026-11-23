"""Disposable PostgreSQL qualification. No connection or credential inputs."""

import base64
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
from contextlib import redirect_stderr, redirect_stdout, suppress
from pathlib import Path

import database_bootstrap_setup as setup
from fixtures.database_bootstrap.authorization import caller_env, plan_binding
from database_bootstrap_preflight import (
    BootstrapPreflightError, catalog_probe_sql, require_absent_catalog_result,
)

IMAGE = "postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336"
PORT = 5432
DRILL_SIGNAL = signal.SIGUSR1
CREATOR, OWNER, DECOY = "fixture_creator", "fixture_owner", "fixture_decoy"
MIGRATOR, RUNTIME, READER = "fixture_migrator", "fixture_runtime", "fixture_reader"
STAGING = "fixture_staging"
MAINTENANCE, LAUNCH, SIBLING = "postgres", "launch_fixture", "staging_fixture"
received_signals = set()


class FixtureFailure(Exception):
    pass


class Interrupted(Exception):
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
        roles = ("postgres", CREATOR, DECOY, MIGRATOR, RUNTIME, READER, STAGING)
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
        CREATE ROLE {CREATOR} LOGIN CREATEDB PASSWORD '{cluster.passwords[CREATOR]}' VALID UNTIL '{valid_until}';
        CREATE ROLE {DECOY} LOGIN CREATEDB PASSWORD '{cluster.passwords[DECOY]}' VALID UNTIL '{valid_until}';
        CREATE ROLE {OWNER} NOLOGIN;
        GRANT {OWNER} TO {CREATOR}, {DECOY} WITH INHERIT FALSE, SET TRUE;
        """ + "\n".join(
            f"CREATE ROLE {role} LOGIN PASSWORD '{cluster.passwords[role]}';"
            for role in (MIGRATOR, RUNTIME, READER, STAGING)
        ))
    cluster.ok("verify limited creator and loginless owner", f"""
        SELECT (SELECT rolcanlogin AND rolcreatedb AND NOT rolsuper AND NOT rolcreaterole
                FROM pg_roles WHERE rolname = current_user)
           AND (SELECT NOT rolcanlogin AND NOT rolsuper FROM pg_roles WHERE rolname = '{OWNER}');
        """, "t\n", role=CREATOR)


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


def start_caller(env):
    return subprocess.Popen([sys.executable, "-EsB", setup.__file__], env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def caller_verdict(process, expected, timeout=45):
    stdout, stderr = process.communicate(timeout=timeout)
    status = f"database-bootstrap-setup: {expected}\n".encode()
    result = (0, status, b"") if expected == "complete" else (1, b"", status)
    require((process.returncode, stdout, stderr) == result, f"emit only the caller's {expected} verdict")


def routed_client(cluster, mode):
    shim = cluster.directory / "psql"
    shutil.copyfile(Path(__file__).parent / "fixtures/database_bootstrap/psql", shim)
    shim.chmod(0o700)
    (cluster.directory / "route").write_text(
        f"{mode} {cluster.canary} {cluster.address} {CREATOR}.{cluster.branch_id} {CREATOR} {DECOY}"
    )
    return f"{cluster.directory}:/usr/bin:/bin"


def prove_setup_caller(cluster, *, interrupt):
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
    roles = CREATOR, OWNER, MIGRATOR, READER
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
            {"port": PORT + 1}, {"reader_role": CREATOR},
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
            {"ca_pem": (cluster.directory / "client" / "other.crt").read_text()},
        ):
            run(caller_env(cluster, roles, **changes) | route, "refused", 1)
        run(env | {"PATH": routed_client(cluster, "wrong-role"),
                   "BOOTSTRAP_SETUP_PASSWORD": cluster.passwords[DECOY]}, "refused", 1)
        require((cluster.directory / "probe-result").read_bytes() == b"wrong-session\nroles-ready\n",
                "observe a different eligible SQL identity on the approved route")
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
        run(env, "complete", 3)
        run(env, "refused", 1)  # The name collision stops any second write.
        return

    def wait_for(verdict, sql):
        until = time.monotonic() + 5
        while time.monotonic() < until:
            check_interruption()
            if cluster.sql(sql).stdout == "t\n":
                return
            time.sleep(0.1)
        raise FixtureFailure(verdict)

    sessions = f"SELECT FROM pg_stat_activity WHERE usename = '{CREATOR}'"
    with start_caller(env | {"PATH": routed_client(cluster, "interrupt")}) as process:
        try:
            wait_for("observe the caller inside the setup transaction",
                     f"SELECT EXISTS ({sessions} AND wait_event = 'PgSleep');")
            process.terminate()
            caller_verdict(process, "outcome-uncertain", timeout=3)
        finally:
            if process.poll() is None:
                process.kill()
    wait_for("verify the interrupted SQL session closed", f"SELECT NOT EXISTS ({sessions});")
    cluster.ok("verify interruption rolled back the grants and left the create fenced", f"""
        SELECT NOT datallowconn AND datdba = '{CREATOR}'::regrole AND datacl IS NULL
        FROM pg_database WHERE datname = '{LAUNCH}';
        """, "t\n")
    require(not list(cluster.directory.glob("database-bootstrap-ca-*")), "remove the caller's temporary CA")


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
    cluster.ok("retire creator membership", f"REVOKE {OWNER} FROM {CREATOR};")
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
                if not interrupt:
                    refuse_before_create(cluster)
                    verify_quiet_failure(cluster)
                prove_setup_caller(cluster, interrupt=interrupt)
                if interrupt:
                    docker("exec", "-i", cluster.client, "sh", "-exc",
                           'echo ready; read -r line; printf "%s" "$line"',
                           input_text=secrets.token_hex(24) + "\n", timeout=3, interrupt=True)
                    raise FixtureFailure("interruption did not stop the fixture")
                verify_launch_grants(cluster)
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
    print("database-bootstrap-fixture: routed login, SQL identity, TLS, fencing, grants and teardown passed")
