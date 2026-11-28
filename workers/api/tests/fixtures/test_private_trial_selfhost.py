"""Qualify the Stage A stock PostgreSQL target on disposable Docker projects."""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[4]
OPS = ROOT / "ops/staging-trial-pg"
sys.path[:0] = [str(ROOT / ".github/scripts"), str(OPS)]
from create_tls import create_tls
from private_trial_catalog import apply_catalog, create_catalog
from staging_api_identity import Unavailable
from staging_apply import StagingApplyError
from staging_trial_ambient_acl import SELFHOST_CATALOG, compose_selfhost
from staging_trial_identity import verify
from staging_trial_isolated import FIXTURE_HOSTS, HOSTS, KIND, NAMES, ROLES, SERVING, selfhost_target_config, target_config
from staging_stream_config import api_vars, trial_db_bindings
from staging_trial_role_preflight import BASELINE, BINDINGS, DATABASE
from staging_trial_selfhost_grants import install_sql
from staging_trial_selfhost import (
    CONTROL, check_catalog, check_session, fingerprint, tls_connection,
)

BOOTSTRAP = "nntune_trial_bootstrap"
SERVICE = "nntune-trial-pg"
PASSWORD = "ci-throwaway"
PSQL = ["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"]
HBA = "pg_hba.conf"
EXTRA_DB = "trial_fixture_other"


class Disposable:
    def __init__(self, path, certs, wrong_ca, pull, loopback):
        self.path = path
        self.loopback = loopback
        self.pull = pull
        self.env = {"PATH": os.environ["PATH"], "HOME": os.environ["HOME"],
                    "DOCKER_HOST": "unix:///var/run/docker.sock", "TRIAL_PG_DIR": str(path),
                    "TRIAL_PG_UID": str(os.getuid()), "TRIAL_PG_GID": str(os.getgid())}
        project = f"nntune-stage-a-{secrets.token_hex(8)}"
        self.command = ["docker", "compose", "--env-file", "/dev/null", "-f", str(OPS / "compose.yml"), "-p", project]
        for name in ("config", "data", "tls"):
            (path / name).mkdir(mode=0o700, parents=True)
        for name in (HBA, "postgresql.conf"):
            shutil.copyfile(OPS / name, path / "config" / name)
        for name in ("server.crt", "server.key"):
            shutil.copy(certs / "server" / name, path / "tls" / name)
        self.ca = path / "tls" / "ca.crt"
        self.wrong_ca = path / "tls" / "wrong-ca.crt"
        shutil.copyfile(certs / "ca/ca.crt", self.ca)
        shutil.copyfile(wrong_ca, self.wrong_ca)
        (path / "init-password").write_text(secrets.token_hex(16), encoding="ascii")
        (path / "init-password").chmod(0o600)
        (path / "passwd").write_text(f"{BOOTSTRAP}:x:{os.getuid()}:{os.getgid()}::/tmp:/bin/false\n", encoding="ascii")

    @contextmanager
    def running(self):
        config = json.loads(subprocess.check_output(self.command + ["config", "--format", "json"],
                                                    env=self.env, timeout=10))
        service, = config["services"].values()
        network, = config["networks"].values()
        if not (network["internal"] and not service.get("ports") and len(service["networks"]) == 1
                and service["cap_drop"] == ["ALL"] and service["read_only"] and service["cpus"] == 2
                and int(service["mem_limit"]) == 2 * 1024**3 and service["user"] == f"{os.getuid()}:{os.getgid()}"):
            raise AssertionError("unsafe disposable PostgreSQL configuration")
        try:
            self.up()
            yield self
        finally:
            subprocess.run(self.command + ["down", "--volumes", "--timeout", "5"],
                           check=True, timeout=30, env=self.env)
            result = subprocess.check_output(self.command + ["ps", "--all", "-q"], env=self.env, timeout=10)
            if result.strip():
                raise RuntimeError("disposable PostgreSQL containers remain")

    def up(self, *options):
        subprocess.run(self.command + ["up", "--wait", "--wait-timeout", "40", "--pull", self.pull, *options],
                       check=True, timeout=150, env=self.env)

    def run(self, argv, *, env, **kwargs):
        child = {key: value for key, value in env.items() if key.startswith("PG")}
        args = [item.replace(str(self.path / "tls"), "/trial/tls") for item in argv]
        if "PGSSLROOTCERT" in child:
            child["PGSSLROOTCERT"] = child["PGSSLROOTCERT"].replace(str(self.path / "tls"), "/trial/tls")
        if self.loopback and child.get("PGHOST") in FIXTURE_HOSTS.values():
            child["PGHOSTADDR"] = "127.0.0.1"
        if self.loopback and args[0] == "openssl":
            destination = args.index("-connect") + 1
            args[destination] = "127.0.0.1:5432"
        options = [item for key in child for item in ("-e", key)]
        return subprocess.run(self.command + ["exec", "-T", *options, SERVICE, *args],
                              env=self.env | child, **kwargs)

    def psql(self, env, sql):
        return self.run(PSQL, env=env, input=sql, capture_output=True, text=True, timeout=30)

    def query(self, env, sql, stage):
        result = self.psql(env, sql)
        if result.returncode:
            raise StagingApplyError(stage)
        return result.stdout

    def admin(self, sql, *, database=DATABASE):
        result = self.psql({"PGUSER": BOOTSTRAP, "PGDATABASE": database, "PGHOST": "/var/run/postgresql"}, sql)
        if result.returncode:
            raise AssertionError(result.stderr)
        return result.stdout.strip()

    def setup(self):
        version = self.admin("SHOW server_version_num;", database="postgres")
        if version != "180006":
            raise AssertionError("expected PostgreSQL 18.6")
        owner, executor = ROLES["owner"], ROLES["executor"]
        create_catalog(self.admin, "postgres", owner, executor, PASSWORD)
        self.admin(f"""
            CREATE DATABASE {EXTRA_DB} OWNER {owner} TEMPLATE template0;
            ALTER DATABASE postgres OWNER TO {owner};
            ALTER DATABASE template1 OWNER TO {owner};
            ALTER DATABASE template0 OWNER TO {owner};
        """, database="postgres")
        entries = apply_catalog(self.admin, ROOT, executor)
        self.admin(f"GRANT SELECT (file, sha256) ON public.schema_migrations TO {owner};"
                   "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;"
                   f"GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner};")
        if self.admin("SELECT file || ':' || sha256 FROM public.schema_migrations ORDER BY file;").splitlines() != [
                f"{entry['file']}:{entry['sha256']}" for entry in entries]:
            raise AssertionError("unexpected catalog ledger")
        return next(entry["sha256"] for entry in entries if entry["file"] == BASELINE)

    def pins(self):
        control = subprocess.check_output(self.command + ["exec", "-T", SERVICE, "pg_controldata"],
                                          env=self.env, text=True, timeout=10)
        instance = int(re.search(r"^Database system identifier:\s+([0-9]+)$", control, re.MULTILINE)[1])
        db_oid = int(self.admin("SELECT oid FROM pg_database WHERE datname = current_database();"))
        owners = {key: self.role_oid(ROLES[key]) for key in ("owner", "executor")}
        return {"kind": KIND, "system_identifier": instance, "database_oid": db_oid,
                "ca_sha256": fingerprint(self.ca.read_text(encoding="ascii")),
                "server_sha256": fingerprint((self.path / "tls/server.crt").read_text(encoding="ascii")),
                "routes": {key: [host, 5432] for key, host in FIXTURE_HOSTS.items()},
                "role_oids": owners | dict.fromkeys(SERVING)}

    def role_oid(self, login):
        return int(self.admin(f"SELECT oid FROM pg_roles WHERE rolname = '{login}';"))

    def connect(self, pins, route, role, *, mode="verify-full", ca=None):
        host, port = pins["routes"][route]
        url = f"postgresql://{ROLES[role]}:{PASSWORD}@{host}:{port}/{DATABASE}?sslmode={mode}"
        return tls_connection(url, pins, route, role, ca or self.ca, run=self.run)


class Qualification(unittest.TestCase):
    def check_hba(self, cluster):
        self.assertEqual(cluster.admin("""
            SELECT type || ':' || array_to_string(database, ',') || ':' || array_to_string(user_name, ',')
                || ':' || coalesce(address, '') || ':' || auth_method
            FROM pg_hba_file_rules ORDER BY rule_number;
        """).splitlines(), [
            f"local:all:{BOOTSTRAP}::peer",
            f"hostssl:{DATABASE}:{','.join(ROLES.values())}:all:scram-sha-256",
            "host:all:all:all:reject",
        ])
        self.assertEqual(cluster.admin("SELECT count(*) FROM pg_hba_file_rules WHERE error IS NOT NULL OR options IS NOT NULL;"), "0")
        self.assertEqual(cluster.admin("SHOW max_connections;"), "40")

    def exercise_catalog(self, cluster, pins, baseline_digest):
        owner = cluster.connect(pins, "direct", "owner")
        executor = cluster.connect(pins, "direct", "executor")
        check_catalog(pins, owner, executor, query=cluster.query)
        for changed in ({"database_oid": pins["database_oid"] + 1},
                        {"role_oids": pins["role_oids"] | {"executor": pins["role_oids"]["executor"] + 100}}):
            with self.assertRaises(StagingApplyError):
                check_catalog(pins | changed, owner, executor, query=cluster.query)
        cluster.admin(f"UPDATE public.schema_migrations SET sha256 = repeat('0',64) WHERE file = '{BASELINE}';")
        with self.assertRaisesRegex(StagingApplyError, "migration baseline"):
            check_catalog(pins, owner, executor, query=cluster.query)
        cluster.admin(f"UPDATE public.schema_migrations SET sha256 = '{baseline_digest}' WHERE file = '{BASELINE}';")
        before = cluster.admin(SELFHOST_CATALOG)
        cluster.admin("CREATE SCHEMA pscale_extensions; CREATE EXTENSION btree_gin WITH SCHEMA pscale_extensions;")
        with self.assertRaisesRegex(StagingApplyError, "extensions differ"):
            check_catalog(pins, owner, executor, query=cluster.query)
        self.assertEqual(cluster.admin("SELECT extname FROM pg_extension ORDER BY extname;"), "btree_gin\nplpgsql")
        self.assertEqual(cluster.admin(SELFHOST_CATALOG), before)
        cluster.admin("DROP EXTENSION btree_gin; DROP SCHEMA pscale_extensions;")
        check_catalog(pins, owner, executor, query=cluster.query)
        return owner

    def exercise_tls(self, cluster, pins, owner):
        for mode in ("disable", "require", "verify-ca"):
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                cluster.connect(pins, "direct", "owner", mode=mode)
        with self.assertRaisesRegex(StagingApplyError, "certificate identity"):
            wrong = pins | {"ca_sha256": fingerprint(cluster.wrong_ca.read_text(encoding="ascii"))}
            cluster.connect(wrong, "direct", "owner", ca=cluster.wrong_ca)
        with self.assertRaisesRegex(StagingApplyError, "certificate identity"):
            cluster.connect(pins | {"server_sha256": "0" * 64}, "direct", "owner")
        self.assertNotEqual(cluster.psql(owner | {"PGSSLROOTCERT": str(cluster.wrong_ca)}, "SELECT 1;").returncode, 0)
        for invalid in ({"PGSSLMODE": "disable", "PGCHANNELBINDING": "disable"},
                        {"PGHOST": "incorrect.trial.invalid", "PGHOSTADDR": "127.0.0.1"}):
            self.assertNotEqual(cluster.psql(owner | invalid, "SELECT 1;").returncode, 0)

    def exercise_acl(self, cluster, pins, owner):
        self.assertEqual(cluster.admin("SELECT datistemplate AND NOT datallowconn "
                                       "FROM pg_database WHERE datname = 'template0';"), "t")
        before = json.loads(cluster.admin(SELFHOST_CATALOG))
        postgres_id = next(entry[0] for entry in before["databases"] if entry[1] == "postgres")
        template_id = next(entry[0] for entry in before["databases"] if entry[1] == "template0")
        template_connect = [template_id, 0, pins["role_oids"]["owner"], "CONNECT", False]
        self.assertIn(template_connect, before["acl"])
        plan = {"kind": KIND, "system_identifier": pins["system_identifier"], "database_oid": pins["database_oid"],
                "baseline": before, "consumers": [[postgres_id, pins["role_oids"]["executor"], ROLES["executor"], "CONNECT"]]}
        raw = json.dumps(plan)
        digest = hashlib.sha256(raw.encode()).hexdigest()
        with self.assertRaises(ValueError):
            compose_selfhost("restrict-ambient", raw, digest, pins | {"system_identifier": 1})
        with self.assertRaises(ValueError):
            compose_selfhost("restrict-ambient", raw, "0" * 64, pins)
        statements = {action: compose_selfhost(action, raw, digest, pins) for action in
                      ("check-ambient", "restrict-ambient", "restore-ambient")}
        cluster.query(owner, statements["check-ambient"], "check ambient")
        cluster.admin(f"GRANT CREATE ON DATABASE {DATABASE} TO PUBLIC;")
        drifted = json.loads(cluster.admin(SELFHOST_CATALOG))
        with self.assertRaises(StagingApplyError):
            cluster.query(owner, statements["restrict-ambient"], "restrict ambient")
        self.assertEqual(json.loads(cluster.admin(SELFHOST_CATALOG)), drifted)
        cluster.admin(f"REVOKE CREATE ON DATABASE {DATABASE} FROM PUBLIC;")
        for _ in range(2):
            cluster.query(owner, statements["restrict-ambient"], "restrict ambient")
        self.assertNotIn(template_connect, json.loads(cluster.admin(SELFHOST_CATALOG))["acl"])
        self.assertEqual(cluster.admin(f"SELECT has_database_privilege('{ROLES['executor']}', 'postgres', 'CONNECT');"), "t")
        for key in SERVING:
            role = ROLES[key]
            cluster.admin(f"CREATE ROLE {role} LOGIN NOINHERIT PASSWORD '{PASSWORD}'; GRANT CONNECT ON DATABASE {DATABASE} TO {role};")
            pins["role_oids"][key] = cluster.role_oid(role)
            env = cluster.connect(pins, "routed", key)
            check_session(pins, key, cluster.connect(pins, "direct", key), query=cluster.query)
            check_session(pins, key, env, query=cluster.query)
            self.assertEqual(cluster.query(env, "SELECT has_function_privilege('pg_catalog.pg_control_system()', 'EXECUTE');",
                                           "runtime control file privilege"), "f\n")
            for database in ("postgres", "template0", "template1", EXTRA_DB):
                self.assertNotEqual(cluster.psql(env | {"PGDATABASE": database}, "SELECT 1;").returncode, 0)
                cluster.admin(f"GRANT CONNECT ON DATABASE {database} TO {role};")
                with self.assertRaises(StagingApplyError):
                    check_session(pins, key, env, query=cluster.query)
                cluster.admin(f"REVOKE CONNECT ON DATABASE {database} FROM {role};")
            check_session(pins, key, env, query=cluster.query)
        with self.assertRaises(StagingApplyError):
            cluster.query(owner, statements["restore-ambient"], "restore ambient with roles present")
        executor = cluster.connect(pins, "direct", "executor")
        recipe = install_sql(ROOT, pins)
        for _ in range(2):
            cluster.query(executor, recipe, "install stock caller ACLs")
        for key, statement in {
            "auth": "SELECT key_hmac FROM public.native_rest_credentials;",
            "enroll": "SELECT id FROM public.ingest_jobs WHERE false FOR SHARE; "
                      "SELECT public.reserve_storage_digest(NULL::uuid, 3600);",
            "publish": "PREPARE guarded AS " + (ROOT / "workers/api/src/handlers/private_trial/lock.sql").read_text()
                       + "; EXECUTE guarded(NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);",
            "upload": "SELECT wrapped_record FROM public.storage_object_envelope_intents;",
        }.items():
            child = cluster.connect(pins, "routed", key)
            cluster.query(child, "BEGIN READ WRITE;\n" + statement + "\nROLLBACK;", f"{key} positive caller grants")
        query = (ROOT / "workers/api/src/handlers/private_trial/identity.sql").read_text()
        rows = [[binding, json.loads(cluster.query(cluster.connect(pins, "routed", key),
                                                  "BEGIN READ ONLY;\n" + query + "\nROLLBACK;", "identity query"))]
                for binding, key in zip(BINDINGS, SERVING, strict=True)]
        deploy = {"identity": "matched", "build_sha": "a" * 40, "version_id": "version", "deployment_id": "deployment"}
        self.assertEqual(verify("all", pins, deploy, {**deploy, "rows": rows})["bindings"],
                         dict.fromkeys(BINDINGS, "matched"))

        environment = "a0000000-0000-4000-8000-000000000001"
        library = "b0000000-0000-4000-8000-000000000001"
        obj = "c0000000-0000-4000-8000-000000000001"
        root = "f0000000-0000-4000-8000-000000000001"
        cluster.admin(f"""
            INSERT INTO public.storage_environments VALUES ('{environment}');
            INSERT INTO public.storage_library_namespaces VALUES ('{library}', '{environment}', NULL);
        """)
        inserted = cluster.query(cluster.connect(pins, "routed", "enroll"), f"""
            BEGIN READ WRITE;
            INSERT INTO public.storage_object_intents VALUES
                ('{obj}', 'd0000000-0000-4000-8000-000000000001', '{environment}', '{library}',
                 'e0000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab', 48), 'hex'));
            INSERT INTO public.storage_object_envelope_intents
            SELECT object_id, '{root}', decode('4e4e544d454430310002', 'hex') || uuid_send('{root}')
                 || uuid_send(environment_id) || uuid_send(library_id) || uuid_send(object_id) || uuid_send(generation_id)
                 || uuid_send(provider_configuration_version_id) || int8send(plaintext_bytes) || int8send(envelope_bytes)
                 || header_tag || decode(repeat('00', 60), 'hex')
              FROM public.storage_object_intents WHERE object_id = '{obj}' RETURNING 1;
            ROLLBACK;
        """, "enrollment executes the wrapped-envelope trigger")
        self.assertEqual(inserted.strip(), "1")
        for key, denied in {
            "auth": "INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at) SELECT NULL, NULL, NULL WHERE false",
            "enroll": "UPDATE public.ingest_jobs SET id = id WHERE false",
            "publish": "UPDATE public.ingest_jobs SET id = id WHERE false",
            "upload": "SELECT key_hmac FROM public.native_rest_credentials",
        }.items():
            child = cluster.connect(pins, "routed", key)
            result = cluster.psql(child, "BEGIN READ WRITE;\n" + denied + ";")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("may only lock this relation" if key in ("enroll", "publish") else "permission denied",
                          result.stderr)
        cluster.admin(f"GRANT CONNECT ON DATABASE template0 TO {ROLES['auth']};")
        with self.assertRaises(StagingApplyError):
            cluster.query(executor, recipe, "reject closed-template CONNECT drift")
        cluster.admin(f"REVOKE CONNECT ON DATABASE template0 FROM {ROLES['auth']};")
        for key in SERVING:
            role = ROLES[key]
            cluster.admin(f"DROP OWNED BY {role}; DROP ROLE {role};")
        for _ in range(2):
            cluster.query(owner, statements["restore-ambient"], "restore ambient")
        self.assertEqual(json.loads(cluster.admin(SELFHOST_CATALOG)), before)

    def exercise_restart_and_channel_binding(self, cluster, pins, owner):
        # An empty mounted file is sufficient after initialization. Give this
        # disposable alternate TLS server password auth to isolate channel binding.
        (cluster.path / "init-password").write_bytes(b"")
        hba = cluster.path / "config" / HBA
        hba.write_text(hba.read_text(encoding="ascii").replace("scram-sha-256", "password"), encoding="ascii")
        cluster.up("--force-recreate")
        self.assertEqual(int(cluster.admin(CONTROL + ";")), pins["system_identifier"])
        unbound = owner | {"PGCHANNELBINDING": "disable"}
        self.assertEqual(cluster.query(unbound, "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid();",
                                       "verified TLS control without channel binding"), "t\n")
        denied = cluster.psql(owner, "SELECT 1;")
        self.assertNotEqual(denied.returncode, 0)
        self.assertIn("channel binding required", denied.stderr)

    def test_catalog_and_continuity(self):
        with tempfile.TemporaryDirectory(prefix="nntune-stage-a-") as directory:
            root = Path(directory)
            certs, wrong_certs = root / "identity", root / "wrong-identity"
            create_tls(certs, fixture=True)
            create_tls(wrong_certs, fixture=True)
            args = certs, wrong_certs / "ca/ca.crt", self.pull, self.loopback
            with (Disposable(root / "primary", *args).running() as primary,
                  Disposable(root / "alternate", *args).running() as alternate):
                digest = primary.setup()
                alternate.setup()
                pins = selfhost_target_config(json.dumps(primary.pins()), fixture=True)
                alternate_pins = alternate.pins()
                self.assertEqual(pins | {"system_identifier": alternate_pins["system_identifier"]}, alternate_pins)
                self.assertNotEqual(pins["system_identifier"], alternate_pins["system_identifier"])
                self.assertEqual(int(primary.admin(CONTROL + ";")), pins["system_identifier"])
                self.assertEqual(int(alternate.admin(CONTROL + ";")), alternate_pins["system_identifier"])
                self.check_hba(primary)
                hba = primary.path / "config" / HBA
                original = hba.read_text(encoding="ascii")
                try:
                    hba.write_text(original.replace("peer", "trust"), encoding="ascii")
                    with self.assertRaises(AssertionError):
                        self.check_hba(primary)
                finally:
                    hba.write_text(original, encoding="ascii")
                self.check_hba(primary)
                owner = self.exercise_catalog(primary, pins, digest)
                self.exercise_tls(primary, pins, owner)
                check_session(pins, "owner", primary.connect(pins, "routed", "owner"), query=primary.query)
                check_session(pins, "executor", primary.connect(pins, "routed", "executor"), query=primary.query)
                alternate_owner = alternate.connect(pins, "routed", "owner")
                with self.assertRaisesRegex(StagingApplyError, "session continuity"):
                    check_session(pins, "owner", alternate_owner, query=alternate.query)
                check_session(alternate_pins, "owner", alternate_owner, query=alternate.query)
                self.exercise_acl(primary, pins, owner)
                self.exercise_restart_and_channel_binding(alternate, alternate_pins, alternate_owner)

    def test_tunnel_overlay_keeps_postgres_internal(self):
        with tempfile.TemporaryDirectory() as directory:
            config = json.loads(subprocess.check_output([
                "docker", "compose", "--env-file", "/dev/null", "-f", str(OPS / "compose.yml"),
                "-f", str(OPS / "compose.tunnel.yml"), "config", "--format", "json",
            ], env={"PATH": os.environ["PATH"], "HOME": os.environ["HOME"], "TRIAL_PG_DIR": directory,
                    "TRIAL_TUNNEL_ID": "12345678-0000-4000-8000-123456789abc"}, timeout=10))
        pg, tunnel = (config["services"][name] for name in (SERVICE, "nntune-trial-tunnel"))
        self.assertEqual(set(pg["networks"]), {"trial"})
        self.assertEqual(set(tunnel["networks"]), {"trial", "egress"})
        self.assertEqual(config["networks"]["trial"]["name"], "nntune-trial-pg-net")
        self.assertTrue(config["networks"]["trial"]["internal"])
        self.assertEqual(config["networks"]["egress"]["driver"], "bridge")
        self.assertFalse(config["networks"]["egress"].get("internal"))
        self.assertFalse(pg.get("ports") or tunnel.get("ports"))
        self.assertRegex(tunnel["image"], r"^cloudflare/cloudflared@sha256:[0-9a-f]{64}$")
        ingress = (OPS / "tunnel.yml").read_text().split("ingress:\n", 1)[1]
        self.assertEqual(ingress.splitlines(), [
            line for host in HOSTS.values() for line in (
                f"  - hostname: {host}", "    service: tcp://nntune-trial-pg:5432",
            )
        ] + ["  - service: http_status:404"])

    def test_disarmed_identity_rejects_drift(self):
        pins = {"database_oid": 20000, "role_oids": {key: 20001 + n for n, key in enumerate(ROLES)}}
        deploy = {"identity": "matched", "build_sha": "a" * 40, "version_id": "version", "deployment_id": "deployment"}
        rows = [[binding, [DATABASE, pins["database_oid"], ROLES[key], pins["role_oids"][key],
                          ROLES[key], pins["role_oids"][key]]]
                for binding, key in zip(BINDINGS, SERVING, strict=True)]
        reply = {**deploy, "rows": rows}
        self.assertEqual(verify("all", pins, deploy, reply)["bindings"], dict.fromkeys(BINDINGS, "matched"))
        self.assertEqual(verify("upload", pins, deploy, reply | {"rows": rows[:1]})["bindings"], {BINDINGS[0]: "matched"})
        for changed in ({"version_id": "other"}, {"rows": rows[:3]}, {"rows": rows[:3] + [rows[0]]}):
            with self.assertRaises(Unavailable):
                verify("all", pins, deploy, reply | changed)
        env = {"PRIVATE_TRIAL_IDENTITY_MODE": "all", **{
            binding + "_STAGING_ID": f"{index:032x}" for index, binding in enumerate(BINDINGS, 1)
        }}
        self.assertEqual(api_vars(env), ['PRIVATE_TRIAL_IDENTITY_MODE = "all"'])
        self.assertEqual(trial_db_bindings(env).count("[[env.staging.hyperdrive]]"), 4)
        for arm in ("PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED", "PRIVATE_MUSIC_STAGING_JOURNEY",
                    "PRIVATE_PLAYBACK_STAGING_ENABLED"):
            with self.assertRaises(ValueError):
                api_vars(env | {arm: "true"})

    def test_target_kind_pins_are_disjoint(self):
        cfg = {"database_oid": 16494, "executor_oid": 20001, "direct": "direct.fixture.psdb.cloud",
               "direct_port": 5432, "routed": "routed.fixture.psdb.cloud", "routed_port": 6432,
               "owner": "postgres.fixture", "executor": "executor.fixture", "reader": NAMES[0],
               "writer": NAMES[1]}
        self.assertEqual(target_config(json.dumps(cfg), None, False), cfg)
        stock = {"kind": KIND, "database_oid": 20000, "system_identifier": 30000, "ca_sha256": "a" * 64,
                 "server_sha256": "b" * 64, "routes": {key: [host, 5432] for key, host in HOSTS.items()},
                 "role_oids": {key: 20001 + n for n, key in enumerate(ROLES)}}
        self.assertEqual(selfhost_target_config(json.dumps(stock)), stock)
        with self.assertRaises(ValueError):
            selfhost_target_config(json.dumps(cfg))
        with self.assertRaises(ValueError):
            target_config(json.dumps(stock), None, False)
        with self.assertRaises(ValueError):
            selfhost_target_config((OPS / "pins.example.json").read_text(encoding="ascii"))


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--pull", choices=("never", "missing"), default="never")
    cli.add_argument("--loopback", action="store_true", help="use 127.0.0.1 in each disposable namespace")
    opts, unittest_argv = cli.parse_known_args()
    Qualification.pull, Qualification.loopback = opts.pull, opts.loopback
    unittest.main(argv=[sys.argv[0], *unittest_argv], verbosity=2)
