#!/usr/bin/env python3
"""Read one bounded receipt in memory. Never extract an artifact to disk."""

import io
import json
import math
import stat
import sys
import zipfile

MAX_ARCHIVE = 64 * 1024
MAX_RECEIPT = 4096


def unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError("duplicate receipt field")
    return fields


def finite_number(token):
    value = float(token)
    if not math.isfinite(value):
        raise ValueError("non-finite receipt number")
    return value


def read_receipt(data, *, sealed=False):
    if not data or len(data) > MAX_ARCHIVE:
        raise ValueError("archive size outside bounds")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        if len(entries) != 1:
            raise ValueError("expected exactly one receipt")
        entry = entries[0]
        mode = entry.external_attr >> 16
        name = "production-rollback-intent.sealed" if sealed else "promotion.json"
        if (
            entry.filename != name
            or entry.orig_filename != name
            or entry.is_dir()
            or (stat.S_IFMT(mode) not in (0, stat.S_IFREG))
            or entry.flag_bits & 1
            or entry.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
            or not 0 < entry.file_size <= MAX_RECEIPT
            or not 0 < entry.compress_size <= MAX_ARCHIVE
        ):
            raise ValueError("unsafe or oversized receipt member")
        # Bounded even if local and central headers disagree; read checks CRC.
        with archive.open(entry) as member:
            data = member.read(MAX_RECEIPT + 1)
        if len(data) != entry.file_size or len(data) > MAX_RECEIPT:
            raise ValueError("receipt length mismatch")
        if not sealed:
            record = json.loads(data.decode("utf-8"), object_pairs_hook=unique_fields,
                                parse_constant=finite_number, parse_float=finite_number)
            if not isinstance(record, dict):
                raise ValueError("expected a receipt object")
        return data


if __name__ == "__main__":
    try:
        if sys.argv[1:] not in ([], ["--sealed-rollback-intent"]):
            raise ValueError("unknown archive format")
        sys.stdout.buffer.write(read_receipt(sys.stdin.buffer.read(MAX_ARCHIVE + 1), sealed=bool(sys.argv[1:])))
    except (ValueError, OSError, RuntimeError, EOFError, zipfile.BadZipFile):
        sys.stderr.write("invalid promotion receipt archive\n")
        sys.exit(1)
