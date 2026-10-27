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
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CORE = ROOT / "crates/nntune-core"
FIXTURE = CORE / "tests/fixtures"
ADAPTER = CORE / "tests/support/provider_authority_store_adapter.rs"
MODEL = "crates/nntune-core/src/provider_authority_store.rs"
DESIGN = "docs/design/provider-authority-store/"
SQL = DESIGN + "schema.sql"
CONTRACT = DESIGN + "transaction-contract.json"
# Containment is semantic, not a hash pin: unrelated edits to CI, runners or
# workers must not redden this suite. Inventory reviewed at freeze: SQL lives in
# migrations/ (migrations_gate.py, run_migration_tests.sh, ci.yml importer
# list), workers/api/tests/sql/ and workers/mb/seed/; API tests include_str!
# named migrations. None is an unanchored glob that reaches docs/.
PROVIDER_STUB = "workers/stream/src/runtime.rs"
STUB = re.compile(r"async fn provider\(&self, _user_id: &str\) -> core::result::Result<PoolProvider, ProbeError> \{\s*"
                  r"Err\(ProbeError::ProviderUnavailable\)\s*\}")
NAMED = re.compile(r"provider[_-]authority[_-]store|docs/design/provider-authority")
# Discovery that could load the inert SQL without naming it.
DISCOVERY = re.compile(
    r"\*\*/\*\.sql|rglob\(\s*[\"'][^\"']*\.sql|"
    r"\bfind\s+(?:\.|/|docs|\"?\$\{?(?:ROOT|REPO\w*|GITHUB_WORKSPACE)\}?\"?)(?:\s|/)[^\n]*\.sql|"
    r"docs/[^\s\"'`]*\.sql|docs/\*|migrate!\s*\(\s*\"[^\"]*docs", re.I)
VAULT = re.compile(
    r"\bVAULT_KEK\w*|\bKEK_(?:BASE64|CURRENT|V\d+)\b|\bvault_(?:db|kek|connection|url)\b|"
    r"nntune_api\s*::\s*vault|hyperdrive\s*\(\s*\"(?:HYPERDRIVE|\w*VAULT\w*)\"|"
    r"binding\s*=\s*\"(?:HYPERDRIVE|\w*VAULT\w*)\"", re.I)
TEXT = (".rs", ".py", ".sh", ".js", ".mjs", ".ts", ".yml", ".yaml", ".toml", ".json", ".sql", ".example")
# Closed durable facts, not permission to install the tables. Names below are
# local DDL fixture names, not accepted wire fields or API operations.
COLUMNS = {
    "play_origin": "v kind env play_intent_id authorization_id playback_request_id owner pool_owner track_id session_id session_epoch user_epoch library_epoch track_epoch material_epoch grant_digest grant_expires_at metadata_contract admission_id publication_id manifest_id manifest_revision manifest_digest source material_ref selection issued_at hard_deadline plan_digest",
    "probe_origin": "v kind env job_id attempt_id delivery_id owner pool_owner permission_epoch user_epoch admission_id job_generation source_selection_id selection_version selection_plan_digest audio_ordinal track_id manifest_id manifest_revision input_object_id input_revision input_digest accepted_projection_id accepted_projection_digest consumer operation issued_at hard_deadline plan_digest",
    "provider_plan": "env parent_id kind actions plan_digest",
    "content_step": "env parent_id step_id action_ordinal state",
    "connection_step": "env scope_id step_id pair operation state",
    "provider_jti": "env issuer jti claim_digest issuer_key cnf owner_epoch permission_epoch provider_version policy_version context_digest scope_id scope_digest plan_digest issued_at expires_at state reservation_id reservation_until consuming_request_id lease_until terminal_outcome audit_event_id",
    "proof_nonce": "env caller kid kv proof_jti",
    "provider_reservation": "env issuer reservation_id jti resolver_role cnf reserved_at reservation_until state",
    "provider_lease": "env issuer lease_id jti scope_id scope_digest context_digest plan_digest cnf issuer_key lease_until state",
    "authority_fence": "env owner generation state",
    "authority_tombstone": "env parent_id closed_at retain_until replacement_used",
    "audit_outbox": "event_id environment role purpose contract_version outcome reason timestamp",
}
INERT_TABLES = tuple(COLUMNS)
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


def files(root):
    result = subprocess.run(["git", "ls-files", "-co", "--exclude-standard", "-z"],
                            cwd=root, check=True, capture_output=True).stdout
    return sorted(set(p.decode() for p in result.split(b"\0") if p))


def strip_comments(path, text):
    if path.endswith((".rs", ".js", ".mjs", ".ts")):
        return re.sub(r"//[^\n]*|/\*.*?\*/", "", text, flags=re.S)
    if path.endswith((".toml", ".example", ".sh", ".py", ".yml", ".yaml")):
        return re.sub(r"(?m)^\s*#[^\n]*|\s#\s[^\n]*", "", text)
    return text


def containment_errors(root, overlay=None):
    """Overlay maps a repo path to replacement text, for self-test mutants."""
    overlay = overlay or {}
    paths = sorted(set(files(root)) | set(overlay))

    def read(p):
        if p in overlay:
            return overlay[p]
        path = root / p
        return path.read_text(errors="replace") if path.is_file() and not path.is_symlink() else None

    errors = []
    stub = read(PROVIDER_STUB) or ""
    if not STUB.search(stub) or len(re.findall(r"\bfn provider\b", stub)) != 1:
        errors.append("runtime provider() is no longer the unavailable stub")
    allowed = {MODEL, SQL, CONTRACT, DESIGN + "README.md",
               ADAPTER.relative_to(ROOT).as_posix(), Path(__file__).relative_to(ROOT).as_posix(),
               "crates/nntune-core/tests/provider_authority_store_acceptance.rs",
               "crates/nntune-core/tests/fixtures/provider_authority_store_contract.rs",
               "crates/nntune-core/tests/fixtures/provider_authority_store_cases.rs"}
    tables = "|".join(INERT_TABLES)
    for p in paths:
        if p not in overlay and (root / p).is_symlink():
            if p.startswith(DESIGN) or p in allowed:
                errors.append("symlink: " + p)
            continue
        if p.startswith(DESIGN) and p not in allowed:
            errors.append("unexpected design file: " + p)
        if p in allowed or not p.endswith(TEXT):
            continue
        text = read(p)
        if text is None:
            continue
        code = strip_comments(p, text)
        if NAMED.search(text):
            errors.append("new foundation caller/loader/re-export: " + p)
        if DISCOVERY.search(code):
            errors.append("unanchored SQL discovery/loading: " + p)
        if p.endswith(".sql") and (re.search(r"(?i)create\s+table\s+(?:if\s+not\s+exists\s+)?(?:" + tables + r")\b", text)
                                   or "NOT INSTALLED" in text):
            errors.append("inert authority SQL outside its design path: " + p)
        if (re.match(r"workers/(?:stream|ingest)/(?:src/|wrangler\.toml)", p)) and VAULT.search(code):
            errors.append("stream/ingest vault boundary: " + p)
    for p in [MODEL, ADAPTER.relative_to(ROOT).as_posix()]:
        text = read(p)
        if text is not None:
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
        for table, fields in COLUMNS.items():
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

    def test_reservations_and_leases_retain_the_full_ledger_identity(self):
        # P1 §8, incorporated by P2 §1 and S §1; storage only, no new wire ID.
        sql = re.sub(r"--[^\n]*|/\*.*?\*/", "", (ROOT / SQL).read_text(), flags=re.S).lower()
        namespace = {"env", "issuer", "jti"}
        for table in ("provider_reservation", "provider_lease"):
            with self.subTest(table=table):
                match = re.search(r"create\s+table\s+" + table + r"\s*\((.*?)\)\s*;", sql, re.S)
                self.assertIsNotNone(match, "missing " + table)
                body = match[1]
                for field in namespace:
                    self.assertRegex(body, r"\b" + field + r"\s+text\s+not\s+null\b", table + "." + field)
                keys = re.findall(r"(?:primary\s+key|unique)\s*\(([^)]+)\)", body)
                keys += re.findall(r"create\s+unique\s+index\s+\w+\s+on\s+" + table + r"\s*\(([^)]+)\)", sql)
                keys += re.findall(r"(?m)^\s*(\w+)\s+[^,\n]*?\b(?:primary\s+key|unique)\b(?!\s*\()", body)
                normalized = [{f.strip().replace("lease_id", "jti") for f in key.split(",")} for key in keys]
                self.assertIn(namespace, normalized, table + " must reject duplicate full ledger identities")
                for key in normalized:
                    if key <= namespace:
                        self.assertEqual(key, namespace, table + " has a narrower uniqueness key")
                bindings = re.findall(r"foreign\s+key\s*\(([^)]+)\)\s*references\s+provider_jti\s*\(([^)]+)\)", body)
                self.assertTrue(any(
                    set(zip((f.strip() for f in local.split(",")), (f.strip() for f in remote.split(","))))
                    == {(f, f) for f in namespace}
                    for local, remote in bindings
                ), table + " must bind its retained issuer/JTI to the exact ledger row")

    def test_lease_storage_cannot_silently_replace_the_bare_jti_identity(self):
        sql = re.sub(r"--[^\n]*|/\*.*?\*/", "", (ROOT / SQL).read_text(), flags=re.S).lower()
        match = re.search(r"create\s+table\s+provider_lease\s*\((.*?)\)\s*;", sql, re.S)
        self.assertIsNotNone(match, "missing provider_lease")
        self.assertRegex(match[1], r"\blease_id\s+text\s+not\s+null\b")
        self.assertRegex(match[1], r"check\s*\(\s*(?:lease_id\s*=\s*jti|jti\s*=\s*lease_id)\s*\)")

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


def mutants(root):
    """Non-executed text mutants; each must be rejected by containment."""
    def plus(p, extra):
        path = root / p
        original = path.read_text() if path.is_file() else ""
        return {p: original + "\n" + extra + "\n"}
    stub = (root / PROVIDER_STUB).read_text()
    return {
        "changed provider stub": ({PROVIDER_STUB: stub.replace(
            "Err(ProbeError::ProviderUnavailable)\n    }\n}", "Ok(PoolProvider::default())\n    }\n}", 1)}, "provider()"),
        "production re-export": (plus("crates/nntune-core/src/lib.rs", "pub mod provider_authority_store;"), "re-export"),
        "production call": (plus("workers/api/src/handlers.rs", "fn f() { nntune_core::provider_authority_store::reserve(); }"), "re-export"),
        "stream KEK": (plus("workers/stream/src/lib.rs", 'fn k(e: &Env) { e.secret("VAULT_KEK_CURRENT"); }'), "vault"),
        "ingest vault": (plus("workers/ingest/src/lib.rs", 'fn v(e: &Env) { e.hyperdrive("HYPERDRIVE"); }'), "vault"),
        "stream vault binding": (plus("workers/stream/wrangler.toml", '[[hyperdrive]]\nbinding = "VAULT_DB"'), "vault"),
        "inert SQL under migrations": ({"migrations/9999_x.sql": "create table provider_jti (env text);"}, "inert authority SQL"),
        "workflow executes design SQL": (plus(".github/workflows/ci.yml", "      - run: psql -f docs/design/provider-authority-store/schema.sql"), "re-export"),
        "include_str into DB call": (plus("workers/api/src/ledger.rs", 'fn m(c: &C) { c.execute_batch(include_str!("../../../docs/design/x/schema.sql")); }'), "SQL discovery"),
        "migrate macro": (plus("workers/api/src/lib.rs", 'static M: Migrator = sqlx::migrate!("../../docs/design");'), "SQL discovery"),
        "broad local SQL glob": (plus("workers/api/tests/run_migration_tests.sh", "for f in $(find . -name '*.sql'); do psql -f \"$f\"; done"), "SQL discovery"),
        "recursive SQL glob": (plus(".github/scripts/migrations_gate.py", 'ALL = list(Path(".").glob("**/*.sql"))'), "SQL discovery"),
        "extra design file": ({DESIGN + "install.sh": "echo"}, "unexpected design file"),
        "adapter dependency": ({ADAPTER.relative_to(ROOT).as_posix(): "use sqlx::PgPool;"}, "non-inert"),
    }


class ContainmentMutants(unittest.TestCase):
    def test_missing_optional_mutant_targets_are_still_checked_as_overlays(self):
        with tempfile.TemporaryDirectory(prefix="inert-mutant-targets-") as directory:
            root = Path(directory)
            stub = root / PROVIDER_STUB
            stub.parent.mkdir(parents=True)
            stub.write_text((ROOT / PROVIDER_STUB).read_text())
            overlays = mutants(root)
            for name, (overlay, expect) in overlays.items():
                with self.subTest(name=name):
                    errors = containment_errors(ROOT, overlay)
                    self.assertTrue(any(expect in e for e in errors), (name, errors))

    def test_control_and_every_mutant(self):
        self.assertEqual(containment_errors(ROOT), [])
        for name, (overlay, expect) in mutants(ROOT).items():
            with self.subTest(name):
                errors = containment_errors(ROOT, overlay)
                self.assertTrue(any(expect in e for e in errors), (name, errors))


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
        suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(c)
                                   for c in (StaticAcceptance, ContainmentMutants))
        static = unittest.TextTestRunner(verbosity=2).run(suite)
        rust = rust_acceptance()
        sys.exit(75 if rust == 75 else (0 if static.wasSuccessful() and rust == 0 else 1))
