#!/usr/bin/env python3
"""Validate a private, exclusively owned Worker oracle preparation tree.

Trusted preparation must select secret-free contents: names and metadata alone
cannot prove that file bytes contain no secrets. This is not an isolation or
immutability mechanism; exclusive ownership must continue until sealing.
"""
import os
import stat
from pathlib import Path

ROOTS = frozenset({"source", "build", "config", "dependencies", "harness", "observer"})
MAX_ENTRIES = 100_000
MAX_BYTES = 512 * 1024 * 1024
FORBIDDEN_NAMES = frozenset({"home", "id_rsa", "id_ed25519"})
FORBIDDEN_PARTS = ("secret", "credential", "password", "token")
FORBIDDEN_SUFFIXES = (".pem", ".key", ".p12", ".pfx", ".env")


def validate_inputs(root, output):
    """Reject unapproved paths, metadata and sizes; do not copy or execute files.

    `root` is a purpose-built staging tree, not the checkout. `output` is the
    separate future destination. Both must remain exclusively controlled by
    trusted preparation throughout validation and subsequent sealing.
    """
    root = Path(root).absolute()
    output = Path(output).absolute()
    if root.resolve() != root or output.resolve() != output:
        raise ValueError("symlink or non-canonical input/output path")
    if root == output or root in output.parents or output in root.parents:
        raise ValueError("input and output must be disjoint")

    count = total = 0
    pending = [(root, True)]
    while pending:
        path, is_root = pending.pop()
        info = path.lstat()
        count += 1
        if count > MAX_ENTRIES:
            raise ValueError("entry limit exceeded")
        if not is_root:
            name = path.name.lower()
            if (name.startswith(".") or name in FORBIDDEN_NAMES
                    or any(part in name for part in FORBIDDEN_PARTS)
                    or name.endswith(FORBIDDEN_SUFFIXES)):
                raise ValueError("forbidden name")
        if stat.S_ISLNK(info.st_mode) or info.st_mode & 0o7022:
            raise ValueError("unsafe type or permissions")
        if os.listxattr(path, follow_symlinks=False):
            raise ValueError("extended metadata is not permitted")
        if stat.S_ISDIR(info.st_mode):
            with os.scandir(path) as entries:
                for entry in entries:
                    if is_root and entry.name not in ROOTS:
                        raise ValueError("unapproved input tree")
                    pending.append((Path(entry.path), False))
                    if len(pending) + count > MAX_ENTRIES:
                        raise ValueError("entry limit exceeded")
        elif stat.S_ISREG(info.st_mode) and not is_root and info.st_nlink == 1:
            if path.parent == root:
                raise ValueError("input roots must be directories")
            total += info.st_size
            if total > MAX_BYTES:
                raise ValueError("byte limit exceeded")
        else:
            raise ValueError("unsupported input type")
    return count, total
