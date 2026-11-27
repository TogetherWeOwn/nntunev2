"""Verify bounded extract provenance and file receipts, not row semantics.

Callers must validate row shapes and references before using these bytes.
"""

import errno
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

MAX_BYTES = 32 * 1024**2
MAX_ROWS = 50_000
FILES = ("artist_genres.jsonl", "genre_parents.jsonl", "genres.jsonl")
SHA = re.compile(r"[0-9a-f]{64}\Z")


def _shape(value, keys):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError("invalid record shape")


def _pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _invalid_constant(_):
    raise ValueError("non-finite JSON")


def _plain(info, file_type):
    attributes = getattr(info, "st_file_attributes", 0)
    return file_type(info.st_mode) and not (attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT)


def _read(directory, name, limit):
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_BINARY", 0)
    expected = None
    try:
        if isinstance(directory, Path):
            path = directory / name
            expected = path.lstat()
            if not _plain(expected, stat.S_ISREG):
                raise ValueError("regular file required")
            fd = os.open(path, flags)
        else:
            fd = os.open(name, flags | os.O_NOFOLLOW, dir_fd=directory)
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            raise ValueError("regular file required") from None
        raise
    try:
        opened = os.fstat(fd)
        if (not _plain(opened, stat.S_ISREG)
                or (expected is not None and not os.path.samestat(expected, opened))):
            raise ValueError("regular file required")
        with os.fdopen(fd, "rb", closefd=False) as stream:
            raw = stream.read(limit + 1)
    finally:
        os.close(fd)
    if len(raw) > limit:
        raise ValueError("input byte budget exceeded")
    return raw


def _sha(value):
    return isinstance(value, str) and SHA.fullmatch(value) is not None


def verify_receipts(directory):
    """Return manifest, its digest and raw output bytes after receipt checks only."""
    directory = Path(directory)
    if os.open not in os.supports_dir_fd or not hasattr(os, "O_NOFOLLOW"):
        # These callers use trusted local paths. Check the opened files against
        # their entries and refuse if the receipt directory changed during verification.
        expected = directory.lstat()
        if not _plain(expected, stat.S_ISDIR):
            raise ValueError("extract directory required")
        result = _verify_receipts(directory)
        if not os.path.samestat(expected, directory.lstat()):
            raise ValueError("extract directory changed")
        return result
    try:
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except OSError as exc:
        if exc.errno in (errno.ELOOP, errno.ENOTDIR):
            raise ValueError("extract directory required") from None
        raise
    try:
        return _verify_receipts(fd)
    finally:
        os.close(fd)


def _verify_receipts(directory):
    raw_manifest = _read(directory, "manifest.json", 1024**2)
    try:
        manifest = json.loads(raw_manifest.decode("utf-8"), object_pairs_hook=_pairs,
                              parse_constant=_invalid_constant)
    except (UnicodeError, RecursionError):
        raise ValueError("invalid manifest JSON") from None
    _shape(manifest, ("schema", "extractor_version", "extractor_sha256", "source", "input",
                      "language", "limits", "counts", "outputs"))
    if manifest["schema"] != "nntune.genre-extract.v1" or manifest["extractor_version"] != "1":
        raise ValueError("unsupported extract version")
    _shape(manifest["source"], ("name", "license", "dump_version"))
    source = manifest["source"]
    if (source["name"] != "wikidata" or source["license"] != "CC0-1.0"
            or not isinstance(source["dump_version"], str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", source["dump_version"])):
        raise ValueError("unsupported source declaration")
    _shape(manifest["input"], ("sha256", "bytes", "format"))
    identity = manifest["input"]
    if (not _sha(manifest["extractor_sha256"]) or not _sha(identity["sha256"])
            or type(identity["bytes"]) is not int or identity["bytes"] < 0
            or identity["format"] not in ("json-array", "jsonl")
            or not isinstance(manifest["language"], str)
            or not re.fullmatch(r"[a-z][a-z0-9-]{0,34}", manifest["language"])):
        raise ValueError("invalid extract input identity")
    _shape(manifest["counts"], ("entities", "invalid_claims"))
    _shape(manifest["limits"], ("max_line_bytes", "max_input_bytes", "max_entities", "max_db_bytes",
                                "max_output_bytes", "max_depth"))
    counts, limits = manifest["counts"], manifest["limits"]
    if any(type(v) is not int or v < 0 for section in (counts, limits) for v in section.values()):
        raise ValueError("invalid extract counts or limits")
    min_input_bytes = 2 if identity["format"] == "json-array" else 0
    line_limit = limits["max_line_bytes"]
    if (not max(1, min_input_bytes) <= line_limit < sys.maxsize
            or not 0 < limits["max_input_bytes"] < sys.maxsize
            or not 65536 <= limits["max_db_bytes"] <= 1024**4
            or limits["max_depth"] > 10_000
            or limits["max_entities"] == 0
            or not min_input_bytes <= identity["bytes"] <= limits["max_input_bytes"]
            or counts["entities"] > min(limits["max_entities"], identity["bytes"])
            or identity["bytes"] > (counts["entities"] + min_input_bytes) * line_limit
            or (counts["entities"] == 0 and counts["invalid_claims"] != 0)):
        raise ValueError("inconsistent extract counts or limits")
    _shape(manifest["outputs"], FILES)
    outputs = {}
    budget = min(MAX_BYTES, limits["max_output_bytes"]) - len(raw_manifest)
    for name in FILES:
        receipt = manifest["outputs"][name]
        _shape(receipt, ("sha256", "bytes", "rows"))
        if (type(receipt["bytes"]) is not int or receipt["bytes"] < 0
                or type(receipt["rows"]) is not int or not 0 <= receipt["rows"] <= MAX_ROWS
                or (counts["entities"] == 0 and receipt["rows"] != 0)
                or not _sha(receipt["sha256"])):
            raise ValueError("invalid output receipt")
        if receipt["bytes"] > budget:
            raise ValueError("extract output byte budget exceeded")
        raw = _read(directory, name, receipt["bytes"])
        budget -= len(raw)
        if (len(raw) != receipt["bytes"] or hashlib.sha256(raw).hexdigest() != receipt["sha256"]
                or (raw and not raw.endswith(b"\n")) or raw.count(b"\n") != receipt["rows"]):
            raise ValueError("extract receipt or framing mismatch")
        outputs[name] = raw
    return manifest, hashlib.sha256(raw_manifest).hexdigest(), outputs
