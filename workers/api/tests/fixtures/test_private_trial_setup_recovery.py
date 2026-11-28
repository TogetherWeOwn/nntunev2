"""Rehearse the database portion of a Stage C handoff using Stage A's fixture."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_private_trial_selfhost import Disposable, PASSWORD, ROOT
from create_tls import create_tls
from private_trial_catalog import apply_catalog, create_catalog
from staging_apply import StagingApplyError
from staging_trial_ambient_acl import SELFHOST_CATALOG
from staging_trial_isolated import ROLES, SERVING, selfhost_target_config
from staging_trial_selfhost import check_catalog, check_session


class SetupRecovery(unittest.TestCase):
    def test_resume_unsent_apply_on_original_database(self):
        owner, executor = ROLES["owner"], ROLES["executor"]
        ledger = "SELECT file || ':' || sha256 FROM public.schema_migrations ORDER BY file;"
        runtime_names = ", ".join(f"'{ROLES[role]}'" for role in SERVING)
        runtime_roles = f"SELECT count(*) FROM pg_roles WHERE rolname IN ({runtime_names});"
        ledger_absent = "SELECT to_regclass('public.schema_migrations') IS NULL;"
        public_tables = "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename;"
        with tempfile.TemporaryDirectory(prefix="nntune-trial-recovery-") as directory:
            root = Path(directory)
            certs = root / "identity"
            create_tls(certs, fixture=True)
            disposable = Disposable(root / "pg", certs, certs / "ca/ca.crt", "never", True)
            with disposable.running() as cluster:
                project = cluster.command[-1]
                readbacks = []
                for resource, options in (("container", ["--all"]), ("network", [])):
                    listing = ["docker", resource, "ls", "-q", "--no-trunc", *options,
                               "--filter", f"label=com.docker.compose.project={project}"]
                    ids = subprocess.check_output(listing, env=cluster.env, timeout=10).splitlines()
                    self.assertEqual(len(ids), 1)
                    readbacks.append((listing, ids))
                    self.addCleanup(self.assert_resources_absent, listing, cluster.env)

                create_catalog(cluster.admin, "postgres", owner, executor, PASSWORD)
                cluster.admin("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;"
                              f"GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner};")
                pins = selfhost_target_config(json.dumps(cluster.pins()), fixture=True)
                for role in SERVING:
                    self.assertIsNone(pins["role_oids"][role])
                before_apply = cluster.admin(SELFHOST_CATALOG)
                before_tables = cluster.admin(public_tables)
                (cluster.path / "init-password").write_bytes(b"")
                subprocess.run(cluster.command + ["restart", "--timeout", "5"],
                               env=cluster.env, check=True, timeout=30)
                cluster.up("--no-recreate")

                for listing, ids in readbacks:
                    self.assertEqual(subprocess.check_output(listing, env=cluster.env, timeout=10).splitlines(), ids)
                self.assertEqual(cluster.pins(), pins)
                direct_owner = cluster.connect(pins, "direct", "owner")
                executor_session = cluster.connect(pins, "direct", "executor")
                check_session(pins, "owner", direct_owner, query=cluster.query)
                check_session(pins, "owner", cluster.connect(pins, "routed", "owner"), query=cluster.query)
                self.assertEqual(cluster.admin(ledger_absent), "t")
                self.assertEqual(cluster.admin(public_tables), before_tables)
                with self.assertRaisesRegex(StagingApplyError, "migration baseline"):
                    check_catalog(pins, direct_owner, executor_session, query=cluster.query)
                self.assertEqual(cluster.admin(SELFHOST_CATALOG), before_apply)
                self.assertEqual(cluster.admin(runtime_roles), "0")

                # An interrupted apply can commit DDL before it writes the ledger.
                cluster.admin("CREATE TABLE public.interrupted_apply_fixture (id int);")
                self.assertEqual(cluster.admin(ledger_absent), "t")
                self.assertNotEqual(cluster.admin(public_tables), before_tables)
                with self.assertRaisesRegex(StagingApplyError, "migration baseline"):
                    check_catalog(pins, direct_owner, executor_session, query=cluster.query)
                self.assertEqual(cluster.admin(public_tables), "interrupted_apply_fixture")
                # Reset the synthetic control for the separate unsent-apply case.
                cluster.admin("DROP TABLE public.interrupted_apply_fixture;")
                self.assertEqual(cluster.admin(public_tables), before_tables)

                entries = apply_catalog(cluster.admin, ROOT, executor)
                cluster.admin(f"GRANT SELECT (file, sha256) ON public.schema_migrations TO {owner};")
                expected_ledger = [f"{entry['file']}:{entry['sha256']}" for entry in entries]
                after_apply = cluster.admin(SELFHOST_CATALOG)
                check_catalog(pins, direct_owner, executor_session, query=cluster.query)
                self.assertEqual(cluster.pins(), pins)
                self.assertEqual(cluster.admin(ledger).splitlines(), expected_ledger)
                self.assertEqual(cluster.admin(SELFHOST_CATALOG), after_apply)
                self.assertEqual(cluster.admin(runtime_roles), "0")
        self.assertFalse(root.exists() or root.is_symlink())

    def assert_resources_absent(self, listing, env):
        self.assertEqual(subprocess.check_output(listing, env=env, timeout=10).strip(), b"")


if __name__ == "__main__":
    unittest.main(verbosity=2)
