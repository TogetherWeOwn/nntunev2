#!/usr/bin/env python3
"""Self-tests for check_dependabot_coverage.py.

The shipped repo must stay green; deleting any expected entry (repair
cargo/docker, npm, github-actions) must turn the check red, as must an
unlisted new standalone crate in either the `exclude` form or the
own-`[workspace]` form. Structural breaks (`version`/`updates`), a missing
`.gitignore`, stale cargo entries, uncovered tracked lockfiles, and
digest-less external FROMs (stage refs exempt) are input errors or drift.
Run from the repo root:

  python3 .github/scripts/test_check_dependabot_coverage.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_dependabot_coverage import (
    check_coverage,
    normalize_directory,
    parse_dependabot_entries,
    unpinned_from_refs,
)

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "check_dependabot_coverage.py"

DEPENDABOT = """\
# Lockfiles: Cargo.lock and workers/repair/Cargo.lock.
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
  - package-ecosystem: "cargo"
    directory: "/workers/repair"
  - package-ecosystem: "docker"
    directory: "/containers/repair"
  - package-ecosystem: "npm"
    directory: "/e2e"
  - package-ecosystem: "github-actions"
    directory: "/"
"""

ROOT_CARGO = """\
[workspace]
members = []
exclude = ["workers/repair"]
"""


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def make_fixture(extra_updates=""):
    tmp = Path(tempfile.mkdtemp(prefix="dependabot-coverage-"))
    write(tmp / "Cargo.toml", ROOT_CARGO)
    write(tmp / "Cargo.lock", "lock")
    write(tmp / "workers" / "repair" / "Cargo.toml", "[package]\nname = \"x\"\n")
    write(tmp / "workers" / "repair" / "Cargo.lock", "lock")
    write(
        tmp / "containers" / "repair" / "Dockerfile",
        "FROM debian:bookworm-slim@sha256:"
        "7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587\n",
    )
    write(tmp / "e2e" / "package.json", "{}\n")
    (tmp / ".github" / "workflows").mkdir(parents=True)
    write(tmp / ".github" / "dependabot.yml", DEPENDABOT + extra_updates)
    write(tmp / ".gitignore", "Cargo.lock\n!/Cargo.lock\n!/workers/repair/Cargo.lock\n")
    return tmp


def run_check(root):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
    )
    return proc.returncode, proc.stdout, proc.stderr


class DependabotCoverageTests(unittest.TestCase):
    def test_shipped_repo_is_green(self):
        drift, err = check_coverage(ROOT)
        self.assertIsNone(err, err)
        self.assertEqual(drift, [], drift)

    def test_shipped_repo_green_via_subprocess(self):
        code, out, _ = run_check(ROOT)
        self.assertEqual(code, 0, out)
        self.assertIn("OK", out)

    def test_fixture_is_green(self):
        tmp = make_fixture()
        try:
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertEqual(drift, [], drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_repair_cargo_entry_fails(self):
        # The exact omission this card fixes: drop the repair cargo entry.
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('directory: "/workers/repair"', 'directory: "/workers/mb"')
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("/workers/repair" in d for d in drift),
                drift,
            )
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_repair_docker_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('directory: "/containers/repair"', 'directory: "/containers/mb-import"')
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("/containers/repair" in d for d in drift),
                drift,
            )
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_unpinned_from_fails(self):
        # The revert mutant: strip the digest and the check must go red.
        tmp = make_fixture()
        try:
            dockerfile = tmp / "containers" / "repair" / "Dockerfile"
            text = dockerfile.read_text(encoding="utf-8")
            dockerfile.write_text(
                text.split("@sha256:")[0] + "\n", encoding="utf-8"
            )
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("Dockerfile" in d and "FROM" in d for d in drift), drift
            )
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_bom_prefixed_unpinned_from_fails(self):
        # A file-start BOM (editor artifact) must not hide an unpinned
        # FROM end to end: the utf-8 read preserves it, the scan strips it.
        tmp = make_fixture()
        try:
            dockerfile = tmp / "containers" / "repair" / "Dockerfile"
            text = dockerfile.read_text(encoding="utf-8")
            dockerfile.write_text(
                "\ufeff" + text.split("@sha256:")[0] + "\n", encoding="utf-8"
            )
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("Dockerfile" in d and "FROM" in d for d in drift), drift
            )
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_heredoc_from_does_not_register_stage(self):
        # Heredoc bodies are content end to end: a pinned FROM..AS inside
        # one must not exempt a later floating FROM of that name.
        tmp = make_fixture()
        try:
            dockerfile = tmp / "containers" / "repair" / "Dockerfile"
            pinned = dockerfile.read_text(encoding="utf-8")
            dockerfile.write_text(
                "RUN <<EOF\n"
                + pinned.replace("\n", " AS base\n", 1)
                + "EOF\n"
                + "FROM base\n",
                encoding="utf-8",
            )
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("Dockerfile" in d and "'base'" in d for d in drift),
                drift,
            )
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_stage_from_ref_is_allowed(self):
        # Internal stage refs (`FROM base`) carry no digest by design.
        tmp = make_fixture()
        try:
            dockerfile = tmp / "containers" / "repair" / "Dockerfile"
            pinned = dockerfile.read_text(encoding="utf-8")
            dockerfile.write_text(
                pinned.replace("\n", " AS base\n", 1)
                + "RUN echo build\n"
                + "FROM base AS runtime\n",
                encoding="utf-8",
            )
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertEqual(drift, [], drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_unpinned_from_refs_unit(self):
        digest = "0" * 64
        self.assertEqual(
            unpinned_from_refs(f"FROM img:tag@sha256:{digest} AS build\n"), []
        )
        self.assertEqual(
            unpinned_from_refs(
                f"from --platform=linux/amd64 img@sha256:{digest}\n"
            ),
            [(1, f"img@sha256:{digest}")],
        )
        self.assertEqual(
            unpinned_from_refs(f"FROM img@sha256:{digest}\n"),
            [(1, f"img@sha256:{digest}")],
        )
        self.assertEqual(
            unpinned_from_refs("FROM img:tag\n"), [(1, "img:tag")]
        )
        self.assertEqual(
            unpinned_from_refs(f"FROM img:tag@sha256:{'0' * 63}\n"),
            [(1, f"img:tag@sha256:{'0' * 63}")],
        )
        self.assertEqual(
            unpinned_from_refs(
                f"FROM img:tag@sha256:{digest} AS base\nFROM BASE AS rt\n"
            ),
            [],
        )
        # A self-named stage must not exempt its own floating ref: the
        # name exists for later lines only, like Docker itself.
        self.assertEqual(
            unpinned_from_refs("FROM debian AS debian\n"), [(1, "debian")]
        )
        self.assertEqual(
            unpinned_from_refs("FROM postgres AS postgres\n"),
            [(1, "postgres")],
        )
        # Registry ports are not tags: a port without a tag stays drift.
        self.assertEqual(
            unpinned_from_refs(
                f"FROM registry:5000/org/img@sha256:{digest}\n"
            ),
            [(1, f"registry:5000/org/img@sha256:{digest}")],
        )
        self.assertEqual(
            unpinned_from_refs(
                f"FROM registry:5000/org/img:tag@sha256:{digest}\n"
            ),
            [],
        )
        # `scratch` takes no tag and cannot be pinned, so it is exempt.
        self.assertEqual(unpinned_from_refs("FROM scratch\n"), [])
        self.assertEqual(unpinned_from_refs("FROM SCRATCH\n"), [])
        # A file-start BOM (editor artifact) must not hide an unpinned FROM:
        # BuildKit strips it before parsing.
        self.assertEqual(
            unpinned_from_refs("\ufeffFROM debian\n"), [(1, "debian")]
        )
        self.assertEqual(
            unpinned_from_refs(f"\ufeffFROM img:tag@sha256:{digest}\n"), []
        )
        # A pasted NBSP before FROM is still an instruction (BuildKit trims
        # Unicode whitespace), but between FROM and the ref it is not a
        # separator, so that line is not a FROM at all.
        self.assertEqual(
            unpinned_from_refs("\xa0FROM debian\n"), [(1, "debian")]
        )
        self.assertEqual(unpinned_from_refs("FROM\xa0debian\n"), [])
        # Heredoc bodies are content: a FROM..AS inside one must not
        # register a stage that exempts a later floating FROM.
        self.assertEqual(
            unpinned_from_refs(
                "RUN <<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        self.assertEqual(
            unpinned_from_refs(
                "COPY <<EOF /fixture\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        self.assertEqual(
            unpinned_from_refs(
                "ADD <<EOF /fixture\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        # `<<-` strips leading tabs from the terminator; quoted delimiters
        # unquote to the same terminator; `2<<` is an fd redirect opener.
        self.assertEqual(
            unpinned_from_refs(
                "RUN <<-EOF\n"
                f"\tFROM img:tag@sha256:{digest} AS base\n"
                "\tEOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        self.assertEqual(
            unpinned_from_refs(
                'RUN <<"EOF"\n'
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        self.assertEqual(
            unpinned_from_refs(
                "RUN cat 2<<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        # Terminators match exactly: trailing content keeps the heredoc open.
        self.assertEqual(
            unpinned_from_refs(
                "RUN <<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF \n"
                "EOF\n"
                "FROM base\n"
            ),
            [(5, "base")],
        )
        # Multiple openers on one line consume sequential bodies.
        self.assertEqual(
            unpinned_from_refs(
                "RUN <<A echo hi <<B echo yo\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "A\n"
                "echo body\n"
                "B\n"
                "FROM base\n"
            ),
            [(6, "base")],
        )
        # ONBUILD-wrapped RUN/COPY/ADD open heredocs like BuildKit's
        # compound directives; a real stage still exempts across one.
        self.assertEqual(
            unpinned_from_refs(
                "ONBUILD RUN <<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        self.assertEqual(
            unpinned_from_refs(
                f"FROM img:tag@sha256:{digest} AS base\n"
                "RUN <<EOF\n"
                "echo hi\n"
                "EOF\n"
                "FROM base\n"
            ),
            [],
        )
        # `<<` outside RUN/COPY/ADD (or quoted, spaced, or JSON-form) opens
        # nothing: later FROMs are still scanned.
        self.assertEqual(
            unpinned_from_refs('RUN ["echo", "<<EOF"]\nFROM debian\n'),
            [(2, "debian")],
        )
        self.assertEqual(
            unpinned_from_refs("ENV X=<<EOF\nFROM debian\n"),
            [(2, "debian")],
        )
        self.assertEqual(
            unpinned_from_refs("RUN echo '<<EOF'\nFROM debian\n"),
            [(2, "debian")],
        )
        self.assertEqual(
            unpinned_from_refs("RUN << EOF\nFROM debian\n"),
            [(2, "debian")],
        )
        self.assertEqual(
            unpinned_from_refs("# RUN <<EOF\nFROM debian\n"),
            [(2, "debian")],
        )
        # Dockerfile word splitting is Unicode-wide like BuildKit's shell
        # lexer: an NBSP separates an opener word too.
        self.assertEqual(
            unpinned_from_refs(
                "RUN echo\xa0<<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(4, "base")],
        )
        # Continuations join before parsing (no space inserted): an opener
        # glued to the previous word is not an opener, indented it is.
        # (No space before the backslash: `foo \` would keep its space and
        # still open, exactly like BuildKit's continuation replacement.)
        self.assertEqual(
            unpinned_from_refs(
                "RUN echo foo\\\n"
                "<<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [],
        )
        self.assertEqual(
            unpinned_from_refs(
                "RUN echo foo \\\n"
                "  <<EOF\n"
                f"FROM img:tag@sha256:{digest} AS base\n"
                "EOF\n"
                "FROM base\n"
            ),
            [(5, "base")],
        )

    def test_new_standalone_crate_without_entry_fails(self):
        # A new excluded crate with no entry must turn the check red.
        tmp = make_fixture()
        try:
            cargo = (tmp / "Cargo.toml").read_text(encoding="utf-8")
            cargo = cargo.replace('exclude = ["workers/repair"]',
                                  'exclude = ["workers/repair", "workers/new"]')
            (tmp / "Cargo.toml").write_text(cargo, encoding="utf-8")
            write(tmp / "workers" / "new" / "Cargo.toml", "[package]\nname = \"y\"\n")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/workers/new" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_own_workspace_crate_without_entry_fails(self):
        # Own-[workspace] form: builds fine, not in exclude, no lockfile.
        # The exclude list alone would miss it; the repo scan must catch it.
        tmp = make_fixture()
        try:
            write(
                tmp / "workers" / "solo" / "Cargo.toml",
                "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n\n[workspace]\n",
            )
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/workers/solo" in d for d in drift), drift)
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_workspace_member_needs_no_entry(self):
        # Members resolve to the root lockfile: no cargo entry required.
        tmp = make_fixture()
        try:
            cargo = (tmp / "Cargo.toml").read_text(encoding="utf-8")
            cargo = cargo.replace(
                'members = []', 'members = ["workers/stream"]'
            )
            (tmp / "Cargo.toml").write_text(cargo, encoding="utf-8")
            write(tmp / "workers" / "stream" / "Cargo.toml", "[package]\nname = \"s\"\n")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertEqual(drift, [], drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_members_glob_is_input_error(self):
        tmp = make_fixture()
        try:
            cargo = (tmp / "Cargo.toml").read_text(encoding="utf-8")
            cargo = cargo.replace('members = []', 'members = ["crates/*"]')
            (tmp / "Cargo.toml").write_text(cargo, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn("glob", err)
            code, _, err_out = run_check(tmp)
            self.assertEqual(code, 2, err_out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_npm_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('directory: "/e2e"', 'directory: "/web"')
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/e2e" in d for d in drift), drift)
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_github_actions_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('- package-ecosystem: "github-actions"\n    directory: "/"', "")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("github-actions" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_stale_cargo_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text += '  - package-ecosystem: "cargo"\n    directory: "/workers/ghost"\n'
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("stale" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_tracked_lockfile_without_entry_fails(self):
        # Lockfile loop: a tracked lock whose dir has no cargo entry fails
        # even when no Cargo.toml advertises the directory.
        tmp = make_fixture()
        try:
            write(tmp / "workers" / "ghost" / "Cargo.lock", "lock")
            gitignore = (tmp / ".gitignore").read_text(encoding="utf-8")
            gitignore += "!/workers/ghost/Cargo.lock\n"
            (tmp / ".gitignore").write_text(gitignore, encoding="utf-8")
            dependabot = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            dependabot = dependabot.replace(
                "# Lockfiles: Cargo.lock and workers/repair/Cargo.lock.",
                "# Lockfiles: Cargo.lock, workers/repair/Cargo.lock and workers/ghost/Cargo.lock.",
            )
            (tmp / ".github" / "dependabot.yml").write_text(dependabot, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/workers/ghost" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_lockfile_without_negation_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".gitignore").read_text(encoding="utf-8")
            text = text.replace("!/workers/repair/Cargo.lock\n", "")
            (tmp / ".gitignore").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("negat" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_gitignore_is_input_error(self):
        tmp = make_fixture()
        try:
            (tmp / ".gitignore").unlink()
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn(".gitignore", err)
            code, _, err_out = run_check(tmp)
            self.assertEqual(code, 2, err_out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_malformed_version_is_input_error(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace("version: 2", "version: [")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn("version", err)
            code, _, err_out = run_check(tmp)
            self.assertEqual(code, 2, err_out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_updates_is_input_error(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace("updates:\n", "")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn("updates", err)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_root_lock_header_needs_word_match(self):
        # Nested lock names all contain `Cargo.lock`: a header naming only
        # the nested lock must not satisfy the root lock.
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace(
                "# Lockfiles: Cargo.lock and workers/repair/Cargo.lock.",
                "# Lockfiles: workers/repair/Cargo.lock.",
            )
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("Cargo.lock" in d and "header" in d for d in drift), drift
            )
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_stale_header_fails(self):
        # A lockfile the header does not name must fail.
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace("workers/repair/Cargo.lock", "workers/api/Cargo.lock")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("header" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_entry_parsing_ignores_comments(self):
        text = (
            "# directory: \"/ignored\"\n"
            '- package-ecosystem: "cargo"\n'
            '  directory: "/workers/repair" # trailing\n'
        )
        self.assertEqual(
            parse_dependabot_entries(text), {("cargo", "/workers/repair")}
        )

    def test_normalize_directory(self):
        self.assertEqual(normalize_directory("/"), "/")
        self.assertEqual(normalize_directory("workers/repair/"), "/workers/repair")
        self.assertEqual(normalize_directory("/e2e"), "/e2e")


if __name__ == "__main__":
    unittest.main(verbosity=2)
