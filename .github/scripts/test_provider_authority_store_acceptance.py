"""NON-AUTHORITATIVE / NOT INSTALLED; hermetic static + synthetic Rust acceptance.

Run: python3 -B .github/scripts/test_provider_authority_store_acceptance.py
Builder supplies tests/support/provider_authority_store_adapter.rs; frozen files
are never edited. Missing adapter uses a compile-valid DENY-ALL witness, not a
fake passing implementation. Cargo goes ONLY through the bounded slot pool.
Static SQL inspection never executes SQL, even in an in-memory database.
"""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
CORE = ROOT / "crates/nntune-core"
FIXTURE = CORE / "tests/fixtures"
ADAPTER = CORE / "tests/support/provider_authority_store_adapter.rs"
MODEL = "crates/nntune-core/src/provider_authority_store.rs"
DESIGN = "docs/design/provider-authority-store/"
SQL = DESIGN + "schema.sql"
CONTRACT = DESIGN + "transaction-contract.json"
# Reviewed runner/loading inventory at the pinned baseline. Hashes below freeze
# executable discovery boundaries, not PostgreSQL behavior or future security.
RUNNERS = {
    ".github/scripts/migrations_gate.py": "b1a1eef3bd641f43cef9f905cee795da1f293f8dc9f272d7cf2fdd6e7ac59882",
    ".github/scripts/test_migrations_gate.py": "06ac222b3a4727dc4ffd97a8b42f240a062c621c7875d8c3fc43fd954688f360",
    ".github/scripts/test_migrations_gate_manifest_shape.py": "69326839fbe1fc92d263a13460e0b4d8dd2d894bd53d1e2a97c0b89d8e38aaaa",
    ".github/scripts/test_migrations_gate_regressions.py": "aad959eefb501efad7bb852c15bc8e3127717ad4dbcf1aa926648c794efcfadd",
    ".github/scripts/test_migrations_gate_root_traversal.py": "1b2d49ee288a3a9fc6b42b96db5a818c44d055aa4e8836531f4c10796ed3c3ae",
    ".github/scripts/test_migrations_gate_root_validity.py": "a7363114c754d5da9aa9cc85f4f841f6610471b0a715db83af9aaa4896259704",
    ".github/scripts/test_migrations_gate_unreadable_sql.py": "a8782d1aded4826f5b8ed755d3f4f68306b75fff1cc9854601ee9f501a081083",
    ".github/workflows/ci.yml": "6036432aca8a25c7fd99b397928ef8080d41d11d248cef016545c0bf8f87dc6b",
    ".github/workflows/deploy-production.yml": "c892ea2d2c52386e415e6ab3d4d7fee8381e27df85d5061b6010a40d32407bdf",
    ".github/workflows/deploy.yml": "81e5d56430249a56860df42c674b0572b1613584f843e5871ac88afe13d1fb38",
    ".github/workflows/pr-size.yml": "95446dc3679888fafb60feb01d69c1aa55bf810fa373260e439acc7c2d238287",
    "Cargo.toml": "f3badb570eac217fa43269863e4beda44001858753de7159aca4b729b50f7583",
    "containers/mb-import/tests/input_admission/command_double.py": "21b4d27067f594567e9d8e3e84e515c4732a9a967af273b014569bb667bfd13e",
    "containers/mb-import/tests/test_argv_hygiene.sh": "12d6156b11f25d5a0846e8fb653b796c78d651f12d340444fc7903479a708743",
    "crates/nntune-core/Cargo.toml": "fa98d08e579fb1376d78c671eef32d70d107f2bf247991df373f9973aba68c5a",
    "migrations/manifest.toml": "a3859d336b87fb0081d8c3ce1e57560e88937c9d22ad7fc2cbe4363b82645b61",
    "workers/api/src/billing.rs": "9ccef93a1519df6d8c2885d3df1c9a681dd2fac4309c02c42eedb92fc7dbc777",
    "workers/api/src/handlers.rs": "8fceeca8a869034f039ba7b1be23cbc3c7e1d52136b5a51c5b22c079ec9b3fc1",
    "workers/api/src/ledger.rs": "f3cae82f30e22a8525ad13a0156c493ba6283a96f7dd0020d4d8361ba41fa45e",
    "workers/api/src/library.rs": "eecd6d871b8c5c2e623e9435b2a10eca7b516244bc5f7b73d0e74622ce2c9f2f",
    "workers/api/tests/account_deletion_acceptance.rs": "3dc134c26787a0cc699889eab4143ac0367482224d6c2e59fb73e3ddeab9af7d",
    "workers/api/tests/kek_rotation_acceptance.rs": "d56e8c8627e0e8161446d6103c926016841ba54b1f4b1d7c8885038243924764",
    "workers/api/tests/kek_rotation_migration_order_acceptance.rs": "5d0f2b1c5fc9d982b402b19ed0f471ea175ccee4c1454200af09ebf57d959c05",
    "workers/api/tests/ledger_tests.rs": "f8aeb812c19d652de1c2abafb194c50077f328ee0dcbabb46d89699d9092eff2",
    "workers/api/tests/pr30_findings_acceptance.rs": "22491bdb1dfd38f0cfc4a7b9835e2f1430e11b5a2b940e3e0e40c67ac53fda88",
    "workers/api/tests/run_migration_tests.sh": "528db1a0021e79e5ed516d8d89e8fb1270b8d25971d6c48a204e17295be7a336",
    "workers/ingest/src/lib.rs": "f47f923632771771c8d7df149313b1de58ff8dd2fe8604ba0a754d27ac216623",
    "workers/ingest/src/library.rs": "bac2de9af591bd9984c6fc738bf4d0d8b17a713b1b63bdc1cbbe9bc52efbfdc8",
    "workers/ingest/src/transcode.rs": "23bebe3c8fbb46e171c0d7bf689eba3b1e29cbf023ff906e977ee013a2d38ce7",
    "workers/ingest/tests/admission_commit_sqlstate_acceptance.rs": "b26b2ab417c00f6036a1459c404d62015ba1f463b4e989ab3ae1788b8728f9ca",
    "workers/mb/src/lib.rs": "16192c5cd3433bbf25d9a4c74e569a77ed470c4fc6f8d02cdbb2b1e8a7d83d38",
    "workers/stream/src/lib.rs": "003d895b68760175a656f0c293d5422e327eb92249ed80082cbdcc073c85db8e",
    "workers/stream/src/meter.rs": "b33ef981388692a3706c2495a1451d274a99a22ba0bafbc9e3734b7c9c3b715f",
    "workers/stream/src/runtime.rs": "65b8292e1b1b8b7fad1fe82923d5ec0ff0b95000656fba567d90b50f6dcebe19",
}
MIGRATIONS = "5d77fdc15855b4b81443c35e5b19d651a9abae490e2e4fba6a7d1ef6f8fbcc4a"
WRITERS = (
    "library_remove playback_session_revoke session_logout session_expire user_disable "
    "user_delete_tombstone security_epoch_reset playback_entitlement_remove import_entitlement_remove "
    "permission_remove source_withdraw source_replace target_source_delete target_source_replace "
    "track_selection_change material_replace material_revoke provider_update provider_delete "
    "owner_disable policy_change plan_decrease issuer_key_revoke caller_key_revoke import_cancel "
    "import_expire job_generation_change probe_attempt_replace probe_dispatch_invalidate "
    "probe_plan_change probe_target_change admitting_session_expire"
).split()
LOCKS = set((
    "owner_policy session user_security deletion_tombstone library current_source material "
    "permit lease origin_mapping import_attempt source_selection_plan pending_envelope receipt "
    "publication_operation target_pointer own_library key current_scope progress dispatch_claim "
    "accepted_projection nonce_jti audit_outbox"
).split())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def files(root):
    result = subprocess.run(["git", "ls-files", "-co", "--exclude-standard", "-z"],
                            cwd=root, check=True, capture_output=True).stdout
    return sorted(set(p.decode() for p in result.split(b"\0") if p))


def migration_digest(root, paths):
    h = hashlib.sha256()
    for p in paths:
        if p.startswith("migrations/"):
            h.update(p.encode() + b"\0" + (root / p).read_bytes() + b"\0")
    return h.hexdigest()


def containment_errors(root):
    paths = files(root)
    errors = [p for p, sha in RUNNERS.items()
              if not (root / p).is_file() or digest(root / p) != sha]
    if migration_digest(root, paths) != MIGRATIONS:
        errors.append("migration discovery tree changed")
    allowed = {MODEL, SQL, CONTRACT, DESIGN + "README.md",
               ADAPTER.relative_to(ROOT).as_posix(), Path(__file__).relative_to(ROOT).as_posix(),
               "crates/nntune-core/tests/provider_authority_store_acceptance.rs",
               "crates/nntune-core/tests/fixtures/provider_authority_store_contract.rs",
               "crates/nntune-core/tests/fixtures/provider_authority_store_cases.rs"}
    for p in paths:
        path = root / p
        if path.is_symlink():
            if p.startswith(DESIGN) or p in allowed:
                errors.append("symlink: " + p)
            continue
        if p in allowed or p in RUNNERS:
            continue
        if p.endswith((".rs", ".py", ".sh", ".js", ".mjs", ".ts", ".yml", ".yaml", ".toml", ".json")):
            text = path.read_text(errors="replace")
            code = re.sub(r"//[^\n]*|/\*.*?\*/", "", text, flags=re.S)
            if p.startswith(("workers/stream/src/", "workers/ingest/src/")) and re.search(r"\b(?:kek|vault_(?:db|kek|connection|url))\b", code, re.I):
                errors.append("stream/ingest vault boundary: " + p)
            if "provider_authority_store" in text or "provider-authority-store" in text:
                errors.append("new foundation caller/loader/re-export: " + p)
            # A new broad discovery path could load the SQL without naming it.
            if re.search(r"(?is)(?:glob|walk|find|migrat|include_str!|include_bytes!).{0,160}(?:\.sql|sql\b)", text):
                # Existing non-authority loaders have an explicit reviewed hash.
                errors.append("unreviewed SQL discovery/loading: " + p)
    for p in [MODEL, ADAPTER.relative_to(ROOT).as_posix()]:
        path = root / p
        if path.exists():
            text = path.read_text()
            if re.search(r"\b(sqlx|postgres|sqlite|diesel|hyperdrive|worker|tokio|unsafe|extern)\b|"
                         r"std\s*::\s*(net|fs|process)|include_(str|bytes)!", text, re.I):
                errors.append("non-inert dependency: " + p)
    return errors


class StaticAcceptance(unittest.TestCase):
    def test_runtime_and_sql_remain_detached_from_all_discovery_paths(self):
        self.assertEqual(containment_errors(ROOT), [])

    def test_sql_is_definition_only_outside_migration_tree(self):
        path = ROOT / SQL
        self.assertTrue(path.is_file(), "missing inert SQL definition (expected RED before foundation)")
        text = path.read_text()
        self.assertIn("NON-AUTHORITATIVE", text)
        self.assertIn("NOT INSTALLED", text)
        sql = re.sub(r"--[^\n]*|/\*.*?\*/", "", text, flags=re.S).lower()
        self.assertNotRegex(sql, r"\b(insert|update|delete|drop|truncate|grant|copy|do|execute)\b")
        self.assertNotRegex(sql, r"\b(password|username|credentials?|vault_envelope|kek|content_key|capability|play_grant|stream_proof|raw_proof|private_key)\b")
        # Closed durable facts, not permission to install the tables. Names below
        # are local DDL fixture names, not accepted wire fields or API operations.
        columns = {
            "play_origin": "v kind env play_intent_id authorization_id playback_request_id owner pool_owner track_id session_id session_epoch user_epoch library_epoch track_epoch material_epoch grant_digest grant_expires_at metadata_contract admission_id publication_id manifest_id manifest_revision manifest_digest source material_ref selection issued_at hard_deadline plan_digest",
            "probe_origin": "v kind env job_id attempt_id delivery_id owner pool_owner permission_epoch user_epoch admission_id job_generation source_selection_id selection_version selection_plan_digest audio_ordinal track_id manifest_id manifest_revision input_object_id input_revision input_digest accepted_projection_id accepted_projection_digest consumer operation issued_at hard_deadline plan_digest",
            "provider_plan": "env parent_id kind actions plan_digest",
            "content_step": "env parent_id step_id action_ordinal state",
            "connection_step": "env scope_id step_id pair operation state",
            "provider_jti": "env issuer jti claim_digest issuer_key cnf owner_epoch permission_epoch provider_version policy_version context_digest scope_id scope_digest plan_digest issued_at expires_at state reservation_id reservation_until consuming_request_id lease_until terminal_outcome audit_event_id",
            "proof_nonce": "env caller kid kv proof_jti",
            "provider_reservation": "env reservation_id jti resolver_role cnf reserved_at reservation_until state",
            "provider_lease": "env lease_id jti scope_id scope_digest context_digest plan_digest cnf issuer_key lease_until state",
            "authority_fence": "env owner generation state",
            "authority_tombstone": "env parent_id closed_at retain_until replacement_used",
            "audit_outbox": "event_id environment role purpose contract_version outcome reason timestamp",
        }
        for table, fields in columns.items():
            match = re.search(r"create\s+table\s+(?:if\s+not\s+exists\s+)?" + table + r"\s*\((.*?)\)\s*;", sql, re.S)
            self.assertIsNotNone(match, "missing inert table " + table)
            for field in fields.split():
                self.assertRegex(match[1], r"\b" + field + r"\b", table + "." + field)
            if table in ("play_origin", "probe_origin"):
                for field in ["owner", "pool_owner", "track_id"] + (["session_id"] if table == "play_origin" else ["job_id"]):
                    self.assertRegex(match[1], r"\b" + field + r"\s+text\b", "native identity must remain TEXT")
        for key in ["env,authorization_id", "env,play_intent_id", "env,playback_request_id",
                    "env,issuer,jti", "env,caller,kid,kv,proof_jti", "env,parent_id,step_id"]:
            compact = re.sub(r"\s+", "", sql)
            self.assertRegex(compact, r"(?:unique|primarykey)\(" + key + r"\)")
        self.assertNotRegex(sql, r"(?:unique|primary\s+key)\s*\(\s*(?:authorization_id|play_intent_id|playback_request_id|jti|proof_jti)\s*\)")
        self.assertRegex(sql, r"authorization_id\s*(?:<>|!=)\s*play_intent_id")
        self.assertRegex(sql, r"authorization_id\s*(?:<>|!=)\s*playback_request_id")
        self.assertRegex(sql, r"play_intent_id\s*(?:<>|!=)\s*playback_request_id")
        self.assertRegex(sql, r"86400|24\s+hours")

    def test_every_writer_contract_is_unintegrated_with_synchronous_fence(self):
        import json
        path = ROOT / CONTRACT
        self.assertTrue(path.is_file(), "missing transaction/writer contract (expected RED)")
        data = json.loads(path.read_text())
        self.assertEqual(data["status"], "NON-AUTHORITATIVE / NOT INSTALLED")
        self.assertEqual(set(data["shared_lock_keys"]), LOCKS)
        self.assertFalse(data["outbox_is_synchronous_fence"])
        self.assertEqual(set(data["snapshot_fields"]), set((
            "environment owner actual_owner session user_security deletion_tombstone library "
            "source_tuple material_tuple publication_tuple consumed_permit_ownership origin_plan_mapping "
            "material_lease entitlement provider owner_epoch permission_epoch policy key_acceptance "
            "scope progress dispatch_claim admission job_generation selected_plan attempt "
            "absolute_deadlines monotonic_deadline audit_ack"
        ).split()))
        transactions = "permit_consume play_release material_lease_check provider_reserve provider_finalize scope_progress import_enroll import_material_delivery import_report import_publish probe_dispatch nonce_admission key_activation".split()
        self.assertEqual(set(data["transactions"]), set(transactions))
        for name, row in data["transactions"].items():
            self.assertFalse(row["integrated"], name)
            self.assertEqual(set(row["lock_keys"]), LOCKS, name)
            self.assertEqual(row["snapshot"], "shared_snapshot", name)
            self.assertTrue(row["linearization_point"], name)
            self.assertTrue(row["post_unwrap_and_post_audit_rereads"], name)
        self.assertEqual(set(data["writers"]), set(WRITERS))
        for writer, row in data["writers"].items():
            self.assertFalse(row["integrated"], writer)
            self.assertEqual(set(row["lock_keys"]), LOCKS, writer)
            for phase in ["current_snapshot", "post_unwrap_reread", "post_audit_reread",
                          "linearization_point", "atomic_fence_and_outbox"]:
                self.assertTrue(row[phase], writer + "." + phase)
        self.assertEqual(data["reader_only"], ["import.discovery"])


def rust_acceptance():
    # Generate only the integration-driver glue; no source or SQL is generated.
    name = "provider_authority_store_acceptance_" + str(os.getpid())
    path = CORE / "tests" / (name + ".rs")
    contract = FIXTURE / "provider_authority_store_contract.rs"
    cases = FIXTURE / "provider_authority_store_cases.rs"
    denial = '''
mod adapter {
    use super::contract::*;
    #[derive(Debug)] pub struct Missing;
    pub fn new(_: &str) -> Missing { Missing }
    impl Model for Missing {
        fn freeze(&mut self,_:Seed)->Result<(),Denial>{Err(Denial::Unsupported)}
        fn record_nonce(&mut self,_:Nonce,_:Time)->Result<(),Denial>{Err(Denial::Unsupported)}
        fn reserve(&mut self,_:&Id,_:&Id,_:Call,_:Nonce)->Result<Reservation,Denial>{Err(Denial::Unsupported)}
        fn first_finalize(&mut self,_:Finalize)->Result<Delivery,Denial>{Err(Denial::Unsupported)}
        fn invalidate(&mut self,_:&Id,_:&'static str,_:Time)->Result<(),Denial>{Err(Denial::Unsupported)}
        fn inspect(&self,_:&Id)->Option<View>{None}
        fn unsupported(&mut self,_:&str)->Result<(),Denial>{Err(Denial::Unsupported)}
    }
}
'''
    binding = ('#[path = "' + str(ADAPTER) + '"] mod adapter;') if ADAPTER.is_file() else denial
    source = ('#![allow(dead_code)]\n#[path = "' + str(contract) + '"] mod contract;\n'
              + binding + '\n#[path = "' + str(cases) + '"] mod cases;\n')
    try:
        with path.open("x") as out:
            out.write(source)
        result = subprocess.run([sys.executable, str(ROOT / "scripts/cargo_cache.py"), "run", "--",
                                 "test", "-p", "nntune-core", "--no-default-features", "--test", name], cwd=ROOT)
        return result.returncode
    finally:
        if path.exists() and path.read_text() == source:
            path.unlink()


if __name__ == "__main__":
    if "--static-only" in sys.argv:
        unittest.main(argv=[sys.argv[0]], verbosity=2)
    else:
        static = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(StaticAcceptance))
        rust = rust_acceptance()
        sys.exit(75 if rust == 75 else (0 if static.wasSuccessful() and rust == 0 else 1))
