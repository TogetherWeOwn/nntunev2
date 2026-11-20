#!/usr/bin/env python3
"""Synthetic admission comparisons and transport-policy controls, not qualification."""
import http.server
import json
import os
import pathlib
import subprocess
import tempfile
import threading
import unittest

import migration_acquisition_admission as admission
import migration_history as history
from test_migration_snapshot import GitCase, sha


class UnconfinedObjectStoreControl(GitCase):
    def test_existing_git_reader_can_consume_external_alternate(self):
        # This is a negative-control demonstration, not an offline sandbox.
        # An empty store must not be mistaken for a self-contained witness.
        with tempfile.TemporaryDirectory() as tmp:
            store = pathlib.Path(tmp) / "store.git"
            subprocess.run(["git", "init", "-q", "--bare", str(store)], check=True)
            alternates = store / "objects" / "info" / "alternates"
            alternates.write_text(str(self.root / ".git" / "objects") + "\n")
            reader = history.GitHistory(store, self.base, self.head)
            self.assertEqual(reader.git("cat-file", "-t", self.head).strip(), b"commit")
            alternates.unlink()
            with self.assertRaisesRegex(history.HistoryError, "missing or unreadable"):
                history.GitHistory(store, self.base, self.head)


class SyntheticAcquisitionAdmission(unittest.TestCase):
    def test_proposal_cannot_broaden_base_declaration(self):
        base = admission.Declaration(1, "o/r", sha(1), sha(2), "refs/heads/fence")
        admission.check_declaration(base, base)
        for proposal in (
                admission.Declaration(2, "o/r", sha(1), sha(2), "refs/heads/fence"),
                admission.Declaration(1, "o/other", sha(1), sha(2), "refs/heads/fence"),
                admission.Declaration(1, "o/r", sha(3), sha(2), "refs/heads/fence"),
                admission.Declaration(1, "o/r", sha(1), sha(3), "refs/heads/fence"),
                admission.Declaration(1, "o/r", sha(1), sha(2), "refs/heads/other"),
                admission.Declaration(1, "o/r", "", sha(2), "refs/heads/fence")):
            with self.subTest(proposal=proposal), self.assertRaises(history.HistoryError):
                admission.check_declaration(base, proposal)
        with self.assertRaises(history.HistoryError):
            admission.check_declaration(None, base)

    def test_ref_syntax_rejected_even_with_matching_observation(self):
        repository = {"id": 1, "full_name": "o/r"}
        for ref in ("refs/heads/a b", "refs/heads/a..b", "refs/heads/.hidden",
                    "refs/heads/a.lock", "refs/heads/a.lock/b", "refs//heads/a",
                    "refs/heads/a/", "refs/heads/a.", "refs/heads/a@{b",
                    "refs/heads/a~b", "refs/heads/a^b", "refs/heads/a:b",
                    "refs/heads/a?b", "refs/heads/a*b", "refs/heads/a[b",
                    "refs/heads/a\\b", "refs/heads/a\x7fb", "refs/heads/a\nb",
                    "refs/heads/a\ud800b"):
            declaration = admission.Declaration(1, "o/r", sha(1), sha(2), ref)
            with self.subTest(ref=ref), self.assertRaises(history.HistoryError):
                admission.check_admission(declaration, repository, ref, sha(2))
        for ref in ("refs/heads/fence", "refs/tags/v1.2", "refs/heads/a.locked/b"):
            with self.subTest(ref=ref):
                admission.check_admission(admission.Declaration(1, "o/r", sha(1), sha(2), ref),
                                          repository, ref, sha(2))

    def test_declared_identity_and_exact_ref(self):
        declaration = admission.Declaration(1, "o/r", sha(1), sha(2), "refs/heads/fence")
        repository = {"id": 1, "full_name": "o/r"}
        admission.check_admission(declaration, repository, "refs/heads/fence", sha(2))
        for repo, ref, head in (
                ({"id": 2, "full_name": "o/r"}, "refs/heads/fence", sha(2)),
                ({"id": 1, "full_name": "o/renamed"}, "refs/heads/fence", sha(2)),
                ({"id": True, "full_name": "o/r"}, "refs/heads/fence", sha(2)),
                (repository, "refs/heads/other", sha(2)),
                (repository, "refs/heads/fence", sha(3)),
                (repository, "refs/heads/fence", None)):
            with self.subTest(repo=repo, ref=ref, head=head), \
                    self.assertRaises(history.HistoryError):
                admission.check_admission(declaration, repo, ref, head)

    def test_invalid_or_head_only_declarations_fail(self):
        for declaration in (
                None,
                admission.Declaration(True, "o/r", sha(1), sha(2), "refs/heads/fence"),
                admission.Declaration(1, "o/r", "", sha(2), "refs/heads/fence"),
                admission.Declaration(1, "o/r", sha(1), sha(2), "topic"),
                admission.Declaration(1, "o/r", sha(1), sha(2), "refs/")):
            with self.subTest(declaration=declaration), self.assertRaises(history.HistoryError):
                admission.check_admission(declaration, {"id": 1, "full_name": "o/r"},
                                          "refs/heads/fence", sha(2))


class SyntheticTransportPolicy(unittest.TestCase):
    def test_git_rejects_redirects_with_explicit_policy(self):
        # Loopback HTTP tests Git's redirect setting, not the production HTTPS
        # endpoint, certificate policy, repository identity, or resource envelope.
        requests = []
        redirect_status = 301

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append(self.path)
                if self.path.startswith("/original.git/"):
                    self.send_response(redirect_status)
                    self.send_header("Location", "/renamed.git/info/refs?service=git-upload-pack")
                else:
                    self.send_response(404)
                self.end_headers()

            def log_message(self, *args):
                pass

        with http.server.HTTPServer(("127.0.0.1", 0), Handler) as server, \
                tempfile.TemporaryDirectory() as home:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                url = f"http://127.0.0.1:{server.server_port}/original.git"
                env = {"PATH": os.environ["PATH"], "HOME": home,
                       "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0",
                       "NO_PROXY": "127.0.0.1", "no_proxy": "127.0.0.1"}
                for redirect_status in (301, 302, 307, 308):
                    for policy, followed in (("false", False), ("true", True)):
                        requests.clear()
                        result = subprocess.run(
                            ["git", "-c", f"http.followRedirects={policy}", "ls-remote", url],
                            cwd=home, env=env, capture_output=True, timeout=10)
                        with self.subTest(status=redirect_status, policy=policy):
                            self.assertNotEqual(result.returncode, 0)
                            self.assertTrue(requests and requests[0].startswith("/original.git/"))
                            self.assertEqual(any(p.startswith("/renamed.git/") for p in requests),
                                             followed)
            finally:
                server.shutdown()
                thread.join(timeout=10)
                self.assertFalse(thread.is_alive())


class SyntheticBaseSelection(GitCase):
    def test_selected_declaration_with_loopback_git_observation(self):
        # A local HTTP fixture only: no authenticated origin or HTTPS guarantee.
        ref = "refs/heads/fence"
        proposal = admission.Declaration(1, "o/r", self.base, self.head, ref)
        (self.root / "declaration.json").write_text(json.dumps(vars(proposal)))
        pinned = self.commit("fixture declaration")
        git = history.GitHistory(self.root, self.base, self.head).git
        selected = admission.check_synthetic_base_declaration(git, pinned, proposal)
        self.git("update-ref", ref, self.head)
        self.git("update-server-info")
        requests = []
        redirect = False
        root = str(self.root)

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=root, **kwargs)

            def do_GET(self):
                requests.append(self.path)
                if redirect:
                    self.send_response(302)
                    self.send_header("Location", "/elsewhere/info/refs?service=git-upload-pack")
                    self.end_headers()
                else:
                    super().do_GET()

            def log_message(self, *args):
                pass

        with http.server.HTTPServer(("127.0.0.1", 0), Handler) as server, \
                tempfile.TemporaryDirectory() as home:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                url = f"http://127.0.0.1:{server.server_port}/.git"
                env = {"PATH": os.environ["PATH"], "HOME": home,
                       "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0",
                       "NO_PROXY": "127.0.0.1", "no_proxy": "127.0.0.1"}

                def observe():
                    result = subprocess.run(
                        ["git", "-c", "http.followRedirects=false", "ls-remote", "--refs",
                         url, ref], cwd=home, env=env, capture_output=True, timeout=10)
                    if result.returncode or not result.stdout:
                        raise history.HistoryError("synthetic observation failed")
                    head, observed_ref = result.stdout.decode("ascii").strip().split("\t")
                    return observed_ref, head

                repository = {"id": 1, "full_name": "o/r"}  # synthetic, not authenticated
                admission.check_admission(selected, repository, *observe())
                self.git("update-ref", ref, self.moved)
                self.git("update-server-info")
                with self.assertRaisesRegex(history.HistoryError, "ref or head mismatch"):
                    admission.check_admission(selected, repository, *observe())
                redirect = True
                requests.clear()
                with self.assertRaisesRegex(history.HistoryError, "observation failed"):
                    admission.check_admission(selected, repository, *observe())
                self.assertTrue(requests)
                self.assertFalse(any(path.startswith("/elsewhere/") for path in requests))
            finally:
                server.shutdown()
                thread.join(timeout=10)
                self.assertFalse(thread.is_alive())

    def test_git_ref_observation_must_match_pinned_head(self):
        # Local refs exercise the comparison only; they do not authenticate origin.
        ref = "refs/heads/fence"
        other_ref = "refs/heads/other-fence"
        self.git("update-ref", ref, self.head)
        self.git("update-ref", other_ref, self.head)
        declaration = admission.Declaration(1, "o/r", self.base, self.head, ref)
        repository = {"id": 1, "full_name": "o/r"}

        def observe(name):
            result = self.git("ls-remote", "--refs", str(self.root), name)
            if not result:
                return name, None
            head, observed_ref = result.split("\t")
            return observed_ref, head

        admission.check_admission(declaration, repository, *observe(ref))
        with self.assertRaisesRegex(history.HistoryError, "ref or head mismatch"):
            admission.check_admission(declaration, repository, *observe(other_ref))
        self.git("update-ref", ref, self.moved)
        with self.assertRaisesRegex(history.HistoryError, "ref or head mismatch"):
            admission.check_admission(declaration, repository, *observe(ref))
        self.git("update-ref", "-d", ref)
        with self.assertRaisesRegex(history.HistoryError, "ref or head mismatch"):
            admission.check_admission(declaration, repository, *observe(ref))

    def test_declaration_is_selected_from_pinned_fixture_base(self):
        path = self.root / "declaration.json"
        original = {"repository_id": 1, "repository_name": "o/r", "base": sha(1),
                    "head": sha(2), "source_ref": "refs/heads/fence"}
        path.write_text(json.dumps(original))
        pinned_base = self.commit("synthetic declaration on base")
        changed = dict(original, repository_id=2, head=sha(3))
        path.write_text(json.dumps(changed))
        proposed_head = self.commit("synthetic head changes declaration")

        git = history.GitHistory(self.root, self.base, self.head).git
        proposal = admission.Declaration(**original)
        selected = admission.check_synthetic_base_declaration(git, pinned_base, proposal)
        self.assertEqual(selected, proposal)
        tree_id = self.git("rev-parse", f"{pinned_base}^{{tree}}")
        with self.assertRaisesRegex(history.HistoryError, "invalid synthetic fixture base"):
            admission.check_synthetic_base_declaration(git, tree_id, proposal)
        with self.assertRaises(history.HistoryError):
            admission.check_synthetic_base_declaration(git, pinned_base,
                                                       admission.Declaration(**changed))
        # Head contents and an untracked worktree file cannot provide a missing base declaration.
        with self.assertRaises(history.HistoryError):
            admission.check_synthetic_base_declaration(git, self.moved, proposal)
        with self.assertRaises(history.HistoryError):
            admission.check_synthetic_base_declaration(git, "HEAD", proposal)
        with self.assertRaises(history.HistoryError):
            admission.check_synthetic_base_declaration(git, proposed_head, proposal)

        for bad in ('{"repository_id":1,"repository_id":2}', '[]', '{', 'x' * 4097):
            path.write_text(bad)
            bad_base = self.commit("malformed synthetic declaration")
            with self.subTest(bad=bad[:30]), self.assertRaises(history.HistoryError):
                admission.check_synthetic_base_declaration(git, bad_base, proposal)
        path.unlink()
        path.symlink_to("main.txt")
        symlink_base = self.commit("synthetic declaration is a symlink")
        with self.assertRaises(history.HistoryError):
            admission.check_synthetic_base_declaration(git, symlink_base, proposal)


if __name__ == "__main__":
    unittest.main()
