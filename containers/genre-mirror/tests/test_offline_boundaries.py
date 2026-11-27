"""Regressions for the offline mirror's admission and ownership boundaries."""

import os
from pathlib import Path
import sys

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cohort
import synthetic_resolver
import test_extract_receipts
from extract_receipts import verify_receipts
from refresh_synthetic import synthetic_refresh_sql
from rollup import resolve
from test_cohort import fixture as cohort_fixture
from test_refresh_synthetic import config as refresh_config
from test_rollup import fixture, membership


@pytest.fixture
def receipt():
    case = test_extract_receipts.ReceiptTests()
    case.setUp()
    try:
        yield case
    finally:
        case.doCleanups()


@pytest.mark.parametrize(("section", "key", "value"), [
    ("limits", "max_line_bytes", 0),
    ("limits", "max_line_bytes", 1),
    ("limits", "max_line_bytes", 2),
    ("limits", "max_db_bytes", 4096),
    ("limits", "max_depth", 10_001),
    ("limits", "max_input_bytes", 1),
    ("limits", "max_entities", 1),
    ("limits", "max_output_bytes", 1),
    ("input", "bytes", 0),
    ("counts", "entities", 0),
])
def test_extract_receipt_rejects_impossible_declared_limits(receipt, section, key, value):
    receipt.manifest[section][key] = value
    receipt.write_manifest()
    with pytest.raises(ValueError, match="limits|budget|receipt"):
        verify_receipts(receipt.directory)


def test_extract_receipt_rejects_invalid_utf8(receipt):
    receipt.path.write_bytes(b"\xff")
    with pytest.raises(ValueError, match="manifest"):
        verify_receipts(receipt.directory)


def test_verify_receipts_without_directory_descriptors(receipt, monkeypatch):
    monkeypatch.setattr(os, "supports_dir_fd", set())
    monkeypatch.delattr(os, "O_NOFOLLOW", raising=False)
    assert verify_receipts(receipt.directory)[0] == receipt.manifest


@pytest.mark.skipif(not hasattr(os, "mkfifo"), reason="requires a POSIX FIFO")
@pytest.mark.parametrize("fallback", [False, True])
def test_extract_receipt_rejects_fifo_and_directory_symlink(receipt, tmp_path, monkeypatch, fallback):
    if fallback:
        monkeypatch.setattr(os, "supports_dir_fd", set())
    alias = tmp_path / "alias"
    alias.symlink_to(receipt.directory, target_is_directory=True)
    with pytest.raises(ValueError, match="directory"):
        verify_receipts(alias)
    receipt.path.unlink()
    os.mkfifo(receipt.path)
    with pytest.raises(ValueError, match="regular file"):
        verify_receipts(receipt.directory)


def test_cohort_checks_the_actual_bytes():
    class ForgedCohort(bytes):
        def decode(self, *args, **kwargs):
            return cohort_fixture().decode()

    with pytest.raises(ValueError):
        cohort.admit_cohort(ForgedCohort(b"{}"))


def test_rollup_rejects_conflicting_genre_presence():
    genres, parents, config = fixture()
    with pytest.raises(ValueError, match="genres"):
        resolve(genres + [{"genre_qid": "Q1", "present": False}], parents, [], config)


@pytest.mark.parametrize("edge", [("Q99", "Q1"), ("Q3", "Q99"), ("Q8", "Q1")])
def test_rollup_rejects_unbacked_parent_edges(edge):
    genres, parents, config = fixture()
    genres.append({"genre_qid": "Q8", "present": False})
    child, parent = edge
    parents.append({"genre_qid": child, "parent_qid": parent})
    with pytest.raises(ValueError, match="parents"):
        resolve(genres, parents, [], config)


def test_rollup_does_not_match_an_absent_anchor():
    genres, parents, config = fixture()
    config["roots"][0]["anchors"].append("Q8")
    genres.append({"genre_qid": "Q8", "present": False})
    parents.append({"genre_qid": "Q3", "parent_qid": "Q8"})
    result = resolve(genres, parents, [membership("a", "Q8", "wikidata")], config)
    assert not result["artist_roots"]
    assert {"genre_qid": "Q8", "reason": "unknown_genre"} in result["diagnostics"]
    assert {"genre_qid": "Q3", "root_id": "broad"} not in result["genre_rollup"]


def test_refresh_cannot_persist_an_override_for_a_missing_genre():
    config = refresh_config()
    config["overrides"].append({"genre_qid": "Q104", "action": "exclude", "reason": "fixture"})
    with pytest.raises(ValueError, match="override"):
        synthetic_refresh_sql(cohort_fixture(), [], config)


def test_refresh_rejects_membership_values_that_can_change_after_validation():
    class ForgedArtist(str):
        def lower(self):
            return "33333333-3333-4333-8333-333333333333"

    artist = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    row = membership(ForgedArtist(artist), "Q101", "synthetic-fixture")
    with pytest.raises(ValueError, match="artist MBID"):
        synthetic_refresh_sql(cohort_fixture(artist_mbids=[artist], playable_artist_mbids=[]),
                              [row], refresh_config())


def test_synthetic_graph_rows_are_bounded(monkeypatch):
    genres, parents, config = fixture()
    monkeypatch.setattr(synthetic_resolver, "MAX_EXTRACT_ROWS", len(genres) - 1)
    with pytest.raises(ValueError, match="over-budget synthetic graph rows"):
        synthetic_resolver.resolve_synthetic(cohort_fixture(), genres, parents, [], config)
