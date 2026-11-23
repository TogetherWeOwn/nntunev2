"""Disposable PostgreSQL qualification. No connection or credential inputs."""

import os
import secrets
import select
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from database_bootstrap_preflight import (
    BootstrapPreflightError, catalog_probe_sql, require_absent_catalog_result,
)

IMAGE = "postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336"
PORT = 5432
DRILL_SIGNAL = signal.SIGUSR1
CREATOR, OWNER = "fixture_creator", "fixture_owner"
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
    require(not interrupt or process.returncode == 0, "finish the in-flight Docker command")
    if not cleanup:
        check_interruption(include_drill=interrupt)
    return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)


def docker(*args, **kwargs):
    return command("docker", "--host", "unix:///var/run/docker.sock", *args, **kwargs)


def checked(result, verdict, expected=None):
    require(result.returncode == 0 and (expected is None or result.stdout == expected), verdict)


class Cluster:
    def __init__(self, directory):
        token = secrets.token_hex(8)
        self.network = "bootstrap-" + token
        self.server = self.network + ".test"
        self.bad_host = "wrong-" + self.server
        self.client = self.network + "-client"
        roles = ("postgres", CREATOR, MIGRATOR, RUNTIME, READER, STAGING)
        self.passwords = {role: secrets.token_hex(24) for role in roles}
        self.directory = directory

    def start(self):
        server, client = self.directory / "server", self.directory / "client"
        server.mkdir()
        client.mkdir()
        for ca in ("trusted", "other"):
            checked(command("openssl", "req", "-x509", "-newkey", "ec",
                            "-pkeyopt", "ec_paramgen_curve:P-256", "-noenc", "-days", "1",
                            "-subj", f"/CN=fixture-{ca}", "-addext", "basicConstraints=critical,CA:TRUE",
                            "-keyout", self.directory / f"{ca}.key", "-out", client / f"{ca}.crt"),
                    "generate fixture CA")
        request = self.directory / "server.csr"
        checked(command("openssl", "req", "-new", "-newkey", "ec", "-pkeyopt",
                        "ec_paramgen_curve:P-256", "-noenc", "-subj", f"/CN={self.server}",
                        "-addext", f"subjectAltName=DNS:{self.server}",
                        "-keyout", server / "server.key", "-out", request), "generate TLS request")
        checked(command("openssl", "x509", "-req", "-in", request, "-days", "1",
                        "-CA", client / "trusted.crt", "-CAkey", self.directory / "trusted.key",
                        "-copy_extensions", "copy", "-out", server / "server.crt"),
                "sign fixture server certificate")
        password_file = server / "password"
        password_file.touch(mode=0o600)
        password_file.write_text(self.passwords["postgres"] + "\n")
        (server / "pg_hba.conf").write_text("local all all trust\nhostssl all all all scram-sha-256\n")
        checked(docker("network", "create", "--internal", self.network), "create internal network")
        common = ["run", "--detach", "--pull=never", "--log-driver=none",
                  "--network", self.network, "--tmpfs", "/var/lib/postgresql"]
        checked(docker(*common, "--name", self.server, "--network-alias", self.bad_host,
                       "--mount", f"type=bind,src={server},dst=/fixture,readonly",
                       "-e", "POSTGRES_PASSWORD_FILE=/fixture/password", "--entrypoint", "sh", IMAGE,
                       "-ec", """
                       install -o postgres -g postgres -m 0600 /fixture/server.key /tmp/server.key
                       exec docker-entrypoint.sh postgres -c ssl=on -c ssl_key_file=/tmp/server.key \
                           -c ssl_cert_file=/fixture/server.crt -c hba_file=/fixture/pg_hba.conf
                       """), "start disposable server")
        checked(docker(*common, "--name", self.client, "--entrypoint", "sleep",
                       "--mount", f"type=bind,src={client},dst=/fixture,readonly", IMAGE, "infinity"),
                "start isolated client")
        checked(docker("network", "inspect", "--format", "{{.Internal}}", self.network),
                "verify an internal network", "true\n")
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

    def sql(self, sql, *, role="postgres", database=MAINTENANCE, host=None, ca="trusted", switch=""):
        sql = (f"SET ROLE {switch};\n" if switch else "") + sql
        return docker("exec", "-i", self.client, "env", "-i",
                      "PATH=/usr/local/bin:/usr/bin:/bin", "LC_ALL=C",
                      f"PGHOST={host or self.server}", f"PGPORT={PORT}", f"PGDATABASE={database}",
                      f"PGUSER={role}", "PGSSLMODE=verify-full", "PGGSSENCMODE=disable",
                      f"PGSSLROOTCERT=/fixture/{ca}.crt", "PGCONNECT_TIMEOUT=2",
                      "PGOPTIONS=-c statement_timeout=5000 -c lock_timeout=2000",
                      "sh", "-ec", """
                      IFS= read -r PGPASSWORD
                      export PGPASSWORD
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
        # Use successful listings as absence evidence. A failed inspect could
        # also mean the daemon was unreachable.
        for name in (self.client, self.server):
            docker("container", "rm", "--force", "--volumes", name, cleanup=True)
        docker("network", "rm", self.network, cleanup=True)
        for name in (self.client, self.server):
            checked(docker("container", "ls", "-aq", "--filter", f"name=^/{name}$", cleanup=True),
                    "verify container teardown", "")
        checked(docker("network", "ls", "-q", "--filter", f"name=^{self.network}$", cleanup=True),
                "verify network teardown", "")


def provision_roles(cluster):
    cluster.ok("create distinct fixture roles", f"""
        CREATE ROLE {CREATOR} LOGIN CREATEDB PASSWORD '{cluster.passwords[CREATOR]}';
        CREATE ROLE {OWNER} NOLOGIN;
        GRANT {OWNER} TO {CREATOR} WITH INHERIT FALSE, SET TRUE;
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
    cluster.create_fenced(host=cluster.bad_host, expected_refusal="does not match host name")
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


def verify_launch_grants(cluster):
    cluster.ok("revoke PUBLIC database privileges and transfer ownership", f"""
        REVOKE ALL ON DATABASE {LAUNCH} FROM PUBLIC;
        ALTER DATABASE {LAUNCH} OWNER TO {OWNER};
        """, role=CREATOR)
    cluster.ok("verify revoked PUBLIC access and transferred ownership before admission", f"""
        SELECT NOT datallowconn AND datdba = '{OWNER}'::regrole
           AND NOT EXISTS (SELECT FROM aclexplode(datacl) WHERE grantee = 0)
        FROM pg_database WHERE datname = '{LAUNCH}' AND datacl IS NOT NULL;
        """, "t\n", role=CREATOR)
    cluster.ok("admit only the migrator for setup", f"""
        GRANT CONNECT, CREATE ON DATABASE {LAUNCH} TO {MIGRATOR};
        ALTER DATABASE {LAUNCH} ALLOW_CONNECTIONS true;
        """, role=CREATOR, switch=OWNER)
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
            cluster = Cluster(Path(temp))
            try:
                cluster.start()
                provision_roles(cluster)
                if not interrupt:
                    refuse_before_create(cluster)
                cluster.create_fenced()
                if interrupt:
                    docker("exec", "-i", cluster.client, "sh", "-ec", "echo ready; read -r line",
                           input_text="continue\n", timeout=3, interrupt=True)
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
    for signum in (signal.SIGINT, signal.SIGTERM, DRILL_SIGNAL):
        signal.signal(signum, remember_signal)
    if docker("image", "inspect", IMAGE).returncode != 0:
        checked(docker("pull", IMAGE, timeout=180), "pull pinned PostgreSQL image")
    exercise(interrupt=False)
    exercise(interrupt=True)
    check_interruption()
    if output := os.environ.get("GITHUB_OUTPUT"):
        Path(output).write_text("verdict=passed\n", encoding="utf-8")


if __name__ == "__main__":
    try:
        main()
    except FixtureFailure as error:
        sys.exit(f"database-bootstrap-fixture: failed to {error}")
    except Exception:
        sys.exit("database-bootstrap-fixture: failed")
    print("database-bootstrap-fixture: TLS, fencing, grants and teardown passed")
