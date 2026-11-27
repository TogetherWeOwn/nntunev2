"""Offline publisher-admission fixture for the dormant Phase 2 slice.

This test models only the immutable inventory and manifest boundary from the
signed proposal. It never contacts a provider, writes a pointer, or invokes a
generator. The real publication path remains absent and default-off.
"""

from __future__ import annotations

import hashlib
import json
import unittest
from pathlib import Path


EXPECTED = Path(__file__).parents[3] / "containers" / "mb-import" / "tests" / "expected"
PUBLISHED = {
    "mbdump-artist.tsv",
    "mbdump-release_group.tsv",
    "mbdump-release.tsv",
    "mbdump-recording.tsv",
    "mbdump-track.tsv",
    "mbdump-release_credit.tsv",
    "mbdump-artist_alias.tsv",
    "mbdump-recording_alias.tsv",
    "mbdump-release_alias.tsv",
    "mbdump-release_group_alias.tsv",
    "mbdump-work_alias.tsv",
    "mbdump-label_alias.tsv",
    "mbdump-artist_recording.tsv",
    "mbdump-artist_release.tsv",
    "mbdump-artist_work.tsv",
    "mbdump-recording_recording.tsv",
    "mbdump-recording_work.tsv",
    "mbdump-work.tsv",
    "mbdump-label.tsv",
    "mbdump-release_label.tsv",
}
RETIRED = {
    "mbdump-url.tsv",
    "mbdump-artist_url.tsv",
    "mbdump-recording_url.tsv",
    "mbdump-recording_music_video.tsv",
    "mbdump-recording_isrc.tsv",
}


def inventory() -> dict[str, bytes]:
    objects = {name: (EXPECTED / name).read_bytes() for name in PUBLISHED}
    rows = []
    for line in objects["mbdump-release.tsv"].splitlines(keepends=True):
        if not line.endswith(b"\n") or b"\r" in line:
            raise ValueError("release fixture must be LF terminated")
        fields = line[:-1].split(b"\t")
        if len(fields) != 8:
            raise ValueError("release fixture must have the source eight-field shape")
        rows.append(b"\t".join(fields[:5] + fields[6:]) + b"\n")
    objects["mbdump-release.tsv"] = b"".join(rows)
    return objects


def canonical_manifest(objects: dict[str, bytes]) -> tuple[bytes, str]:
    if set(objects) != PUBLISHED:
        raise ValueError("manifest inventory is outside the closed publication set")
    entries = []
    for name in sorted(objects):
        body = objects[name]
        if not body or not body.endswith(b"\n") or b"\r" in body:
            raise ValueError(f"invalid TSV envelope: {name}")
        if name == "mbdump-release.tsv":
            for row in body.splitlines(keepends=True):
                fields = row[:-1].split(b"\t")
                if len(fields) != 7 or b"coverartarchive.org" in row:
                    raise ValueError("release projection contains an unstripped artwork field")
        entries.append(
            {
                "key": name,
                "bytes": len(body),
                "rows": body.count(b"\n"),
                "sha256": hashlib.sha256(body).hexdigest(),
            }
        )
    manifest = {
        "profile": "phase2-fixture/v1",
        "tables": entries,
    }
    raw = json.dumps(manifest, ensure_ascii=True, separators=(",", ":"), sort_keys=True).encode()
    return raw, hashlib.sha256(raw).hexdigest()


def admit_bundle(
    objects: dict[str, bytes], expected_manifest: bytes, expected_digest: str
) -> None:
    manifest, digest = canonical_manifest(objects)
    if manifest != expected_manifest or digest != expected_digest:
        raise ValueError("bundle does not match the pinned manifest")


class Phase2PublisherContract(unittest.TestCase):
    def test_inventory_is_deterministic_and_excludes_retired_outputs(self) -> None:
        objects = inventory()
        raw, digest = canonical_manifest(objects)
        self.assertEqual(digest, "58440d8676623f4831639f2af61cc860a6bf41bce664b1fa2e807dfd7d3fe5ab")
        admit_bundle(objects, raw, digest)
        reversed_objects = dict(reversed(list(objects.items())))
        self.assertEqual(canonical_manifest(reversed_objects), (raw, digest))
        self.assertEqual(set(objects), PUBLISHED)
        self.assertTrue(RETIRED.isdisjoint(objects))
        self.assertNotIn(b"coverartarchive.org", objects["mbdump-release.tsv"])
        release_fields = objects["mbdump-release.tsv"].splitlines()[0].split(b"\t")
        self.assertEqual(len(release_fields), 7)
        self.assertEqual(release_fields[1], b"11111111-1111-4111-8111-111111111111")
        self.assertEqual(release_fields[5], b"66666666-6666-4666-8666-666666666666")

    def test_missing_extra_and_mutated_objects_refuse_admission(self) -> None:
        objects = inventory()
        raw, digest = canonical_manifest(objects)
        missing = dict(objects)
        missing.pop("mbdump-work.tsv")
        with self.assertRaisesRegex(ValueError, "closed publication set"):
            admit_bundle(missing, raw, digest)

        extra = dict(objects)
        extra["mbdump-url.tsv"] = (EXPECTED / "mbdump-url.tsv").read_bytes()
        with self.assertRaisesRegex(ValueError, "closed publication set"):
            admit_bundle(extra, raw, digest)

        mutated = dict(objects)
        mutated["mbdump-work.tsv"] = b"0" + objects["mbdump-work.tsv"][1:]
        with self.assertRaisesRegex(ValueError, "pinned manifest"):
            admit_bundle(mutated, raw, digest)

        truncated = dict(objects)
        truncated["mbdump-work.tsv"] = truncated["mbdump-work.tsv"][:-1]
        with self.assertRaisesRegex(ValueError, "TSV envelope"):
            admit_bundle(truncated, raw, digest)

        artwork = dict(objects)
        artwork["mbdump-release.tsv"] = (EXPECTED / "mbdump-release.tsv").read_bytes()
        with self.assertRaisesRegex(ValueError, "unstripped artwork field"):
            admit_bundle(artwork, raw, digest)


if __name__ == "__main__":
    unittest.main()
