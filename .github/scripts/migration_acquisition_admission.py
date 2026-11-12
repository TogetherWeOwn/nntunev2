#!/usr/bin/env python3
"""Inert identity comparison for synthetic acquisition qualification.

This does not acquire or authorize a witness. A future caller must establish
that the declaration comes from the trusted base and the observation from a
validated event/API snapshot; matching caller-supplied values proves neither.
Transport, resource and offline isolation remain separate, unqualified gates.
"""
from dataclasses import dataclass

from migration_history import COMMIT_RE, HistoryError
from migration_snapshot import REPO_RE, decode


SYNTHETIC_DECLARATION_PATH = "declaration.json"
MAX_DECLARATION_BYTES = 4096


@dataclass(frozen=True)
class Declaration:
    repository_id: int
    repository_name: str
    base: str
    head: str
    source_ref: str


def valid_source_ref(ref):
    if not isinstance(ref, str) or not ref.startswith("refs/"):
        return False
    if (ref.endswith(".") or ".." in ref or "@{" in ref
            or any(ord(c) <= 32 or ord(c) == 127 or 0xD800 <= ord(c) <= 0xDFFF
                   or c in "~^:?*[\\" for c in ref)):
        return False
    return all(part and not part.startswith(".") and not part.endswith(".lock")
               for part in ref.split("/"))


def validate_declaration(declaration):
    if not isinstance(declaration, Declaration):
        raise HistoryError("invalid acquisition declaration")
    if (type(declaration.repository_id) is not int or declaration.repository_id <= 0
            or not isinstance(declaration.repository_name, str)
            or not REPO_RE.fullmatch(declaration.repository_name)
            or not isinstance(declaration.base, str) or not COMMIT_RE.fullmatch(declaration.base)
            or not isinstance(declaration.head, str) or not COMMIT_RE.fullmatch(declaration.head)
            or not valid_source_ref(declaration.source_ref)):
        raise HistoryError("invalid acquisition declaration")


def check_declaration(base_declaration, proposed_declaration):
    """Compare declarations; the caller must authenticate the base separately."""
    validate_declaration(base_declaration)
    validate_declaration(proposed_declaration)
    if proposed_declaration != base_declaration:
        raise HistoryError("acquisition declaration differs from trusted base")


def check_synthetic_base_declaration(git, fixture_base, proposed_declaration):
    """Compare against a fixed fixture path at a pinned commit, not a worktree.

    The caller owns the Git runner and fixture base. This is not origin
    authentication, confinement, or a production declaration interface.
    """
    if not isinstance(fixture_base, str) or not COMMIT_RE.fullmatch(fixture_base):
        raise HistoryError("invalid synthetic fixture base")
    if git("cat-file", "-t", fixture_base, limit=128).strip() != b"commit":
        raise HistoryError("invalid synthetic fixture base")
    tree = git("ls-tree", "-z", fixture_base, "--", SYNTHETIC_DECLARATION_PATH,
               limit=1024)
    parts = tree.split(b"\t", 1)
    if (len(parts) != 2 or parts[1] != SYNTHETIC_DECLARATION_PATH.encode() + b"\0"
            or len(parts[0].split()) != 3
            or parts[0].split()[:2] != [b"100644", b"blob"]):
        raise HistoryError("missing or invalid synthetic base declaration")
    raw = git("cat-file", "blob", f"{fixture_base}:{SYNTHETIC_DECLARATION_PATH}",
              limit=MAX_DECLARATION_BYTES)
    try:
        data = decode(raw.decode("utf-8"))
        declaration = Declaration(**data)
    except (UnicodeError, TypeError, ValueError) as exc:
        raise HistoryError("invalid synthetic base declaration") from exc
    check_declaration(declaration, proposed_declaration)
    return declaration


def check_admission(declaration, observed_repository, observed_ref, observed_head):
    """Compare a trusted declaration to an independently obtained observation."""
    validate_declaration(declaration)
    if (not isinstance(observed_repository, dict)
            or type(observed_repository.get("id")) is not int
            or observed_repository.get("id") != declaration.repository_id
            or observed_repository.get("full_name") != declaration.repository_name):
        raise HistoryError("acquisition repository identity mismatch")
    if observed_ref != declaration.source_ref or observed_head != declaration.head:
        raise HistoryError("acquisition ref or head mismatch")
