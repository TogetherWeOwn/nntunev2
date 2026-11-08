#!/usr/bin/env python3
"""Offline adversarial regressions for scripts/account_export.py.

Synthetic data only: no database, network, services, files or secrets.
"""

import csv
import dataclasses
import io
import json
import re
import unittest
import zipfile
from datetime import datetime, timedelta, timezone, tzinfo
from unittest.mock import patch

import account_export as ae
from account_export import (
    Account, AppKey, BillingAccount, BillingOperation, DeletionState, ExportError, Genre,
    Invoice, Library, LibraryTrack, LegacyProviderVault, Owned, Play, Playlist, PlaylistEntry,
    SecurityActivity, Section, Session, SignInMethod, Snapshot, Subscription, Track, Usage,
)

UTC = timezone.utc
T0 = datetime(2024, 1, 2, 3, 4, 5, 6, tzinfo=UTC)
T1 = datetime(2025, 6, 7, 8, 9, 10, tzinfo=UTC)
OWNER = "acct-1"
OTHER = "acct-2"
SENTINEL = "SENTINEL-SECRET-7f3a"
MBID_A = "0123abcd-0123-4abc-8def-0123456789ab"
MBID_B = "FEDCBA98-7654-3210-ABCD-EF0123456789"
FUTURE = ("subsonic_credentials", "listenbrainz_links", "follows")


def track(**kw):
    base = dict(
        title="Title", artist="Artist", release="Release", recording_mbid=MBID_A,
        release_mbid=MBID_B, artist_mbids=(MBID_A, MBID_B), duration_seconds=180,
        available=True, catalogue_missing=False,
    )
    base.update(kw)
    return Track(**base)


def own(value, owner=OWNER):
    return Owned(owner, value)


def sec(*rows, status="collected"):
    return Section(status, tuple(rows))


def account(**kw):
    base = dict(id=OWNER, email="user@example.invalid", created_at=T0, deleting_at=None)
    base.update(kw)
    return Account(**base)


def empty_sections():
    sections = {key: Section("collected", ()) for key in ae.SECTIONS}
    for key in FUTURE:
        sections[key] = Section("not_collected", ())
    return sections


def empty_snapshot(**sections):
    all_sections = empty_sections()
    all_sections.update(sections)
    return Snapshot(account(), T1, all_sections)


def keep(rows):
    return tuple(rows)


def full_snapshot(order=keep):
    """One-plus rows for every stored section, plus foreign and shared rows."""
    o = order
    lib_tracks = sec(
        own(LibraryTrack(T0, track(title="L1"))),
        own(LibraryTrack(T1, track(title="L2", available=False))),
        own(LibraryTrack(T1, track(title="FOREIGN-L"), ), OTHER),
        Owned(None, LibraryTrack(T1, track(title="SHARED-L"))),
    )
    lib_tracks = Section("collected", o(lib_tracks.rows))
    pl_entries = Section("collected", o((
        own(PlaylistEntry(3, track(title="P3"))),
        own(PlaylistEntry(1, track(title="P1"))),
        own(PlaylistEntry(2, track(title="P2", available=False))),
        own(PlaylistEntry(1, track(title="FOREIGN-P")), OTHER),
    )))
    sections = empty_sections()
    sections.update({
        "sign_in_methods": Section("collected", o((
            own(SignInMethod("cred-b", T1, 7)), own(SignInMethod("cred-a", T0, 0)),
            own(SignInMethod("cred-x", T0, 1), OTHER),
        ))),
        "sessions": Section("collected", o((
            own(Session("abcdefgh-TAILONE", T0, T1, False)),
            own(Session("zyxwvuts-TAILTWO", T0, T1, True)),
            own(Session("foreign0-TAIL", T0, T1, False), OTHER),
        ))),
        "app_keys": Section("collected", o((
            own(AppKey("k1", "client", "nnt_ab", "name", ("read", "write"), "bearer", T0, T1, T1, None, "203.0.113.5")),
            own(AppKey("k2", None, None, None, (), None, T0, None, None, T1, None)),
        ))),
        "libraries": Section("collected", o((
            own(Library("Main", T0, lib_tracks)),
            own(Library("Empty", T1, Section("not_collected", ()))),
            own(Library("Foreign", T1, Section("collected", ())), OTHER),
        ))),
        "legacy_library": Section("collected", o((
            own(LibraryTrack(T0, track(title="Legacy", release_mbid=None, catalogue_missing=True, available=False))),
        ))),
        "playlists": Section("collected", o((
            own(Playlist("pl-2", "Second", True, T1, Section("not_collected", ()), ("legacy-1", "legacy_2"))),
            own(Playlist("pl-1", "First", False, T0, pl_entries, ())),
            own(Playlist("pl-1", "Dup-but-foreign", False, T0, Section("collected", ()), ()), OTHER),
        ))),
        "genres": Section("collected", o((own(Genre("g-1", "Rock", True, T0)), own(Genre("g-2", "Jazz", False, T1))))),
        "listening_history": Section("collected", o((
            own(Play(track(title="H1"), T0, 30)), own(Play(track(title="H2"), T1, 60)),
        ))),
        "usage_totals": Section("collected", o((own(Usage(T0, T1, 100, 2000)),))),
        "billing_accounts": Section("collected", o((own(BillingAccount("pro", "active")),))),
        "subscriptions": Section("collected", o((own(Subscription("pro", "active", T0, T1, False)),))),
        "invoices": Section("collected", o((own(Invoice("in_1", 500, "usd", T0)), own(Invoice("in_0", 0, "usd", T1))))),
        "billing_operations": Section("collected", o((own(BillingOperation("refund", "done", T0, T1)),))),
        "deletion_state": Section("collected", o((own(DeletionState("account", "pending", T0, T1)),))),
        "security_activity": Section("collected", o((own(SecurityActivity("login", True, T0)), own(SecurityActivity("login", False, T1))))),
        "legacy_provider_vault": Section("collected", o((own(LegacyProviderVault(True, T1)),))),
    })
    return Snapshot(account(), T1, sections)


def with_section(name, section, base=None):
    base = base or full_snapshot()
    sections = dict(base.sections)
    sections[name] = section
    return dataclasses.replace(base, sections=sections)


def zip_of(data):
    archive = zipfile.ZipFile(io.BytesIO(data))
    return archive


def files_of(data):
    with zip_of(data) as archive:
        return {name: archive.read(name) for name in archive.namelist()}


def csv_rows(content):
    return list(csv.reader(io.StringIO(content.decode("utf-8"), newline="")))


def leaky(obj, **extra):
    sub = type("Leaky" + type(obj).__name__, (type(obj),), {})
    new = sub(**{f.name: getattr(obj, f.name) for f in dataclasses.fields(obj)})
    for key, value in extra.items():
        object.__setattr__(new, key, value)
    return new


def history_titles(titles, artists=None, **track_kw):
    plays = []
    for i, title in enumerate(titles):
        artist = artists[i] if artists else "Artist"
        plays.append(own(Play(track(title=title, artist=artist, **track_kw), T0 + timedelta(seconds=i), i)))
    return empty_snapshot(listening_history=Section("collected", tuple(plays)))


class ProjectionTests(unittest.TestCase):
    def test_full_fixture_covers_every_section(self):
        snapshot = full_snapshot()
        self.assertEqual(set(snapshot.sections), set(ae.SECTIONS))
        data = ae.project(snapshot)
        self.assertEqual(data["schema_version"], 1)
        self.assertEqual(data["generated_at"], "2025-06-07T08:09:10.000000Z")
        self.assertEqual(data["account"]["created_at"], "2024-01-02T03:04:05.000006Z")
        self.assertEqual(set(data), {"schema_version", "generated_at", "account", *ae.SECTIONS})
        for key in ae.SECTIONS:
            self.assertEqual(set(data[key]), {"status", "rows"}, key)
            if key in FUTURE:
                self.assertEqual(data[key], {"status": "not_collected", "rows": []})
            else:
                self.assertEqual(data[key]["status"], "collected")
                self.assertTrue(data[key]["rows"], key)

    def test_row_keys_are_exactly_the_allowlist(self):
        data = ae.project(full_snapshot())
        self.assertEqual(set(data["account"]), set(ae.FIELDS[Account].split()))
        for key, cls in ae.SECTIONS.items():
            if cls is None:
                continue
            for row in data[key]["rows"]:
                self.assertEqual(set(row), set(ae.FIELDS[cls].split()), key)
        play = data["listening_history"]["rows"][0]
        self.assertEqual(set(play["track"]), set(ae.FIELDS[Track].split()))

    def test_every_dto_is_allowlisted(self):
        for cls in ae.FIELDS:
            self.assertEqual(set(ae.FIELDS[cls].split()), {f.name for f in dataclasses.fields(cls)}, cls)
        for cls in ae.SECTIONS.values():
            if cls is not None:
                self.assertIn(cls, ae.FIELDS)

    def test_nested_library_and_playlist_coverage(self):
        data = ae.project(full_snapshot())
        libs = {r["name"]: r for r in data["libraries"]["rows"]}
        self.assertEqual(set(libs), {"Main", "Empty"})
        self.assertEqual(libs["Empty"]["tracks"], {"status": "not_collected", "rows": []})
        self.assertEqual({r["track"]["title"] for r in libs["Main"]["tracks"]["rows"]}, {"L1", "L2"})
        pls = {r["public_id"]: r for r in data["playlists"]["rows"]}
        self.assertEqual(pls["pl-2"]["entries"], {"status": "not_collected", "rows": []})
        self.assertEqual(pls["pl-2"]["legacy_items"], ["legacy-1", "legacy_2"])
        self.assertEqual([r["position"] for r in pls["pl-1"]["entries"]["rows"]], [1, 2, 3])

    def test_nested_collected_empty_differs_from_not_collected(self):
        lib = Library("L", T0, Section("collected", ()))
        data = ae.project(with_section("libraries", sec(own(lib))))
        self.assertEqual(data["libraries"]["rows"][0]["tracks"], {"status": "collected", "rows": []})

    def test_foreign_and_shared_rows_are_filtered_everywhere(self):
        data = ae.project(full_snapshot())
        blob = json.dumps(data)
        for needle in ("FOREIGN", "SHARED", "cred-x", "foreign0", "Dup-but-foreign"):
            self.assertNotIn(needle, blob)
        self.assertEqual(len(data["sign_in_methods"]["rows"]), 2)
        self.assertEqual(len(data["sessions"]["rows"]), 2)

    def test_shared_none_owner_never_matches(self):
        snap = with_section("genres", sec(Owned(None, Genre("g", "n", True, T0))))
        self.assertEqual(ae.project(snap)["genres"]["rows"], [])

    def test_foreign_duplicate_identity_is_not_a_collision(self):
        snap = with_section("genres", sec(own(Genre("g", "mine", True, T0)), own(Genre("g", "theirs", True, T0), OTHER)))
        rows = ae.project(snap)["genres"]["rows"]
        self.assertEqual([r["name"] for r in rows], ["mine"])

    def test_dormant_and_missing_identity_tracks_are_retained(self):
        plays = (
            own(Play(track(title="", artist="", release="", recording_mbid=None, release_mbid=None,
                           artist_mbids=(), duration_seconds=None, available=False, catalogue_missing=True), T0, 0)),
            own(Play(track(available=False), T1, 1)),
        )
        data = ae.project(empty_snapshot(listening_history=Section("collected", plays)))
        rows = data["listening_history"]["rows"]
        self.assertEqual(len(rows), 2)
        missing = [r for r in rows if r["track"]["catalogue_missing"]][0]["track"]
        self.assertEqual((missing["available"], missing["recording_mbid"], missing["artist_mbids"], missing["duration_seconds"]),
                         (False, None, [], None))

    def test_missing_identity_account_fails(self):
        for acct in (None, account(id=""), "acct-1", object()):
            snap = dataclasses.replace(full_snapshot(), account=acct)
            with self.assertRaises(ExportError):
                ae.project(snap)
            with self.assertRaises(ExportError):
                ae.export_archive(snap)

    def test_dormant_account_with_deleting_at_is_exported(self):
        snap = dataclasses.replace(full_snapshot(), account=account(deleting_at=T1))
        self.assertEqual(ae.project(snap)["account"]["deleting_at"], "2025-06-07T08:09:10.000000Z")
        self.assertIsNone(ae.project(full_snapshot())["account"]["deleting_at"])

    def test_session_id_is_prefix_only(self):
        data = ae.project(full_snapshot())
        self.assertEqual({r["id"] for r in data["sessions"]["rows"]}, {"abcdefgh", "zyxwvuts"})
        short = ae.project(with_section("sessions", sec(own(Session("ab", T0, T1, False)))))
        self.assertEqual(short["sessions"]["rows"][0]["id"], "ab")
        blob = b"".join(files_of(ae.export_archive(full_snapshot())).values())
        self.assertNotIn(b"TAILONE", blob)
        self.assertNotIn(b"TAILTWO", blob)

    def test_project_output_is_json_serializable_and_matches_account_json(self):
        snap = full_snapshot()
        data = ae.project(snap)
        self.assertEqual(json.loads(files_of(ae.export_archive(snap))["account.json"]), data)

    def test_original_text_preserved_in_json(self):
        title = "a\x00b\n=c\u202e日本"
        snap = history_titles([title])
        data = json.loads(files_of(ae.export_archive(snap))["account.json"])
        self.assertEqual(data["listening_history"]["rows"][0]["track"]["title"], title)


class LeakTests(unittest.TestCase):
    def all_bytes(self, snap):
        return b"\n".join(files_of(ae.export_archive(snap)).values())

    def test_subclass_source_objects_do_not_leak_extras(self):
        extras = dict(secret=SENTINEL, debug={"k": SENTINEL}, password_hash=SENTINEL, source_ip=SENTINEL, raw=SENTINEL)
        for key in ("hmac", "public_key", "token", "payload", "nonce", "ciphertext", "pepper",
                    "invite_code_hash", "dek", "request_payload", "webhook_data", "nc_tags",
                    "audio_url", "nzb_url", "cover_url", "internal_row_id"):
            extras[key] = SENTINEL
        snap = full_snapshot()
        sections = dict(snap.sections)
        sections["sessions"] = sec(own(leaky(Session("abcdefgh-" + SENTINEL, T0, T1, False), **extras)))
        sections["sign_in_methods"] = sec(own(leaky(SignInMethod("c", T0, 1), **extras)))
        sections["app_keys"] = sec(own(leaky(AppKey("k", None, None, None, (), None, T0, None, None, None, None), **extras)))
        sections["genres"] = sec(own(leaky(Genre("g", "n", True, T0), **extras)))
        sections["billing_accounts"] = sec(own(leaky(BillingAccount("p", "s"), **extras)))
        sections["invoices"] = sec(own(leaky(Invoice("i", 1, "usd", T0), **extras)))
        sections["legacy_provider_vault"] = sec(own(leaky(LegacyProviderVault(True, T0), **extras)))
        sections["usage_totals"] = sec(own(leaky(Usage(T0, T1, 1, 2), **extras)))
        sections["subscriptions"] = sec(own(leaky(Subscription("p", "s", T0, T1, False), **extras)))
        sections["billing_operations"] = sec(own(leaky(BillingOperation("k", "s", T0, T1), **extras)))
        sections["deletion_state"] = sec(own(leaky(DeletionState("a", "s", T0, T1), **extras)))
        ltrack = leaky(track(), **extras)
        sections["listening_history"] = sec(own(leaky(Play(ltrack, T0, 1), **extras)))
        sections["legacy_library"] = sec(own(leaky(LibraryTrack(T0, ltrack), **extras)))
        entries = Section("collected", (own(leaky(PlaylistEntry(1, ltrack), **extras)),))
        sections["playlists"] = sec(own(leaky(Playlist("pl", "n", True, T0, entries, ()), **extras)))
        sections["libraries"] = sec(own(leaky(Library("n", T0, Section("collected", (own(leaky(LibraryTrack(T0, ltrack), **extras)),))), **extras)))
        snap = dataclasses.replace(snap, sections=sections, account=leaky(account(), **extras))
        data = ae.project(snap)
        self.assertNotIn(SENTINEL, json.dumps(data))
        self.assertNotIn(SENTINEL.encode(), self.all_bytes(snap))
        self.assertEqual(data["sessions"]["rows"][0]["id"], "abcdefgh")
        self.assertEqual(set(data["account"]), {"id", "email", "created_at", "deleting_at"})

    def test_audit_source_ip_absent(self):
        row = leaky(SecurityActivity("login", True, T0), source_ip="203.0.113.99", ip="203.0.113.99")
        snap = with_section("security_activity", sec(own(row)))
        data = ae.project(snap)
        self.assertEqual(set(data["security_activity"]["rows"][0]), {"action", "ok", "at"})
        self.assertNotIn(b"203.0.113.99", self.all_bytes(snap))
        self.assertNotIn(b"source_ip", self.all_bytes(snap).replace(b"audit source_ip", b""))

    def test_missing_extra_attribute_access_is_not_required(self):
        # Properties on subclasses that would raise must never be touched.
        class Boom(Genre):
            @property
            def secret(self):
                raise AssertionError("must not be read")

        row = Boom("g", "n", True, T0)
        self.assertEqual(ae.project(with_section("genres", sec(own(row))))["genres"]["rows"][0]["public_id"], "g")


class ArchiveTests(unittest.TestCase):
    def test_archive_layout_and_fixed_metadata(self):
        data = ae.export_archive(full_snapshot())
        self.assertIsInstance(data, bytes)
        with zip_of(data) as archive:
            self.assertIsNone(archive.testzip())
            names = archive.namelist()
            self.assertEqual(names, sorted(names))
            self.assertEqual(len(names), len(set(names)))
            self.assertTrue({"README.txt", "account.json", "library.csv", "listening_history.csv"} <= set(names))
            self.assertEqual(len([n for n in names if n.endswith(".csv") and n.startswith("playlists/")]), 2)
            self.assertEqual(len([n for n in names if n.endswith(".m3u8")]), 2)
            for info in archive.infolist():
                self.assertEqual(info.date_time, (1980, 1, 1, 0, 0, 0))
                self.assertEqual(info.compress_type, zipfile.ZIP_STORED)
                self.assertEqual(info.create_system, 3)
                self.assertEqual(info.external_attr, 0o100600 << 16)
                self.assertFalse(info.filename.startswith("/"))
                self.assertNotIn("..", info.filename.split("/"))
                self.assertEqual(info.comment, b"")
                self.assertEqual(info.extra, b"")
            self.assertEqual(archive.comment, b"")

    def test_deterministic_bytes(self):
        a = ae.export_archive(full_snapshot())
        b = ae.export_archive(full_snapshot())
        self.assertEqual(a, b)

    def test_input_ordering_is_canonicalized(self):
        forward = ae.export_archive(full_snapshot())
        backward = ae.export_archive(full_snapshot(order=lambda rows: tuple(reversed(rows))))
        self.assertEqual(forward, backward)
        self.assertEqual(ae.project(full_snapshot()), ae.project(full_snapshot(order=lambda r: tuple(reversed(r)))))

    def test_playlist_entries_ordered_by_position(self):
        files = files_of(ae.export_archive(full_snapshot()))
        for name, content in files.items():
            if name.endswith(".csv") and name.startswith("playlists/"):
                rows = csv_rows(content)
                positions = [int(r[1]) for r in rows[1:]]
                self.assertEqual(positions, sorted(positions))
                if positions:
                    self.assertEqual(positions, [1, 2, 3])
                    self.assertEqual([r[3] for r in rows[1:]], ["P1", "P2", "P3"])
                    self.assertEqual([r[2] for r in rows[1:]], ["False", "True", "False"])

    def test_generated_at_changes_bytes_only_through_timestamp(self):
        a = full_snapshot()
        b = dataclasses.replace(a, generated_at=T0)
        self.assertNotEqual(ae.export_archive(a), ae.export_archive(b))

    def test_library_csv_includes_legacy_with_blank_library_fields(self):
        rows = csv_rows(files_of(ae.export_archive(full_snapshot()))["library.csv"])
        header = rows[0]
        self.assertEqual(header[:3], ["library_name", "library_created_at", "added_at"])
        self.assertEqual(header[3:], ae.TRACK_COLUMNS)
        legacy = [r for r in rows[1:] if r[header.index("title")] == "Legacy"]
        self.assertEqual(len(legacy), 1)
        self.assertEqual(legacy[0][:2], ["", ""])
        self.assertEqual(legacy[0][header.index("catalogue_missing")], "True")
        self.assertEqual(legacy[0][header.index("release_mbid")], "")
        titles = {r[header.index("title")] for r in rows[1:]}
        self.assertEqual(titles, {"L1", "L2", "Legacy"})
        self.assertTrue(all(len(r) == len(header) for r in rows))

    def test_history_csv_artist_mbids_joined(self):
        rows = csv_rows(files_of(ae.export_archive(full_snapshot()))["listening_history.csv"])
        header = rows[0]
        self.assertEqual(rows[1][header.index("artist_mbids")], f"{MBID_A};{MBID_B}")
        self.assertEqual(len(rows), 3)

    def test_readme_coverage_lines(self):
        readme = files_of(ae.export_archive(full_snapshot()))["README.txt"].decode("utf-8")
        self.assertIn("sessions: collected (2 rows)", readme)
        self.assertIn("subsonic_credentials: not_collected (0 rows)", readme)
        self.assertIn("follows: not_collected (0 rows)", readme)
        for key in ae.SECTIONS:
            self.assertRegex(readme, rf"(?m)^{key}: (collected|not_collected) \(\d+ rows\)$")
        self.assertNotIn("FOREIGN", readme)

    def test_empty_account_archive_is_valid(self):
        files = files_of(ae.export_archive(empty_snapshot()))
        self.assertEqual(set(files), {"README.txt", "account.json", "library.csv", "listening_history.csv"})
        self.assertEqual(len(csv_rows(files["library.csv"])), 1)


class CsvTests(unittest.TestCase):
    def history_cells(self, titles, artists=None):
        rows = csv_rows(files_of(ae.export_archive(history_titles(titles, artists)))["listening_history.csv"])
        return rows[0], rows[1:]

    def test_formula_and_control_escaping(self):
        cases = {
            "=1+1": "'=1+1",
            "+1": "'+1",
            "-1": "'-1",
            "@SUM(A1)": "'@SUM(A1)",
            "-": "'-",
            "  =x": "'  =x",
            "\n=x": "'\n=x",
            "\u00a0=x": "'\u00a0=x",
            "\t=x": "\\u0009=x",
            "\x00=x": "\\u0000=x",
            "\u200b=x": "\\u200b=x",
            "\x1b[31m": "\\u001b[31m",
            "\x85note": "\\u0085note",
            "\ue000": "\\ue000",
            "\u0378": "\\u0378",
            "\x07bell": "\\u0007bell",
            "safe=1": "safe=1",
            "'=x": "'=x",
            'a,b"c': 'a,b"c',
            "multi\nline\r\nend,\"q\"": "multi\nline\r\nend,\"q\"",
            "日本語 émoji 😀 ünï": "日本語 émoji 😀 ünï",
            "": "",
        }
        header, rows = self.history_cells(list(cases))
        idx = header.index("title")
        self.assertEqual({r[idx] for r in rows}, set(cases.values()))
        self.assertTrue(all(len(r) == len(header) for r in rows))

    def test_formula_prefix_applies_to_every_text_column(self):
        header, rows = self.history_cells(["t"], ["=cmd|' /C calc'!A0"])
        self.assertEqual(rows[0][header.index("artist")], "'=cmd|' /C calc'!A0")

    def test_csv_uses_crlf_and_utf8(self):
        raw = files_of(ae.export_archive(history_titles(["x\ny", "é"])))["listening_history.csv"]
        raw.decode("utf-8")
        self.assertTrue(raw.endswith(b"\r\n"))
        self.assertNotIn(b"\x00", raw)
        self.assertFalse(raw.startswith(b"\xef\xbb\xbf"))

    def test_none_and_bool_cells(self):
        snap = empty_snapshot(listening_history=sec(own(Play(track(duration_seconds=None, recording_mbid=None, available=False), T0, 5))))
        rows = csv_rows(files_of(ae.export_archive(snap))["listening_history.csv"])
        h = rows[0]
        self.assertEqual(rows[1][h.index("duration_seconds")], "")
        self.assertEqual(rows[1][h.index("recording_mbid")], "")
        self.assertEqual(rows[1][h.index("available")], "False")

    def test_round_trip_every_text_field_row_count(self):
        titles = [f"t{i}" for i in range(50)]
        _, rows = self.history_cells(titles)
        self.assertEqual(len(rows), 50)


class PlaylistFileTests(unittest.TestCase):
    NAMES = [
        "", "../../etc/passwd", "a/b\\c", "日本語", "CON", "same", "same", "SAME", "x" * 100,
        "..", ".hidden", "name\x00nul", "_____", "a\nb", "C:\\win", "--dash--", "Éa", "spa ce.m3u8",
    ]

    def snapshot(self):
        rows = []
        for i, name in enumerate(self.NAMES):
            entries = sec(own(PlaylistEntry(1, track(title=f"T{i}"))))
            rows.append(own(Playlist(f"pub-{i}", name, True, T0, entries, ())))
        return with_section("playlists", Section("collected", tuple(rows)))

    def test_filenames_are_safe_unique_and_mapped(self):
        files = files_of(ae.export_archive(self.snapshot()))
        pattern = re.compile(r"playlists/\d{6}-[A-Za-z0-9_-]{1,48}\.(csv|m3u8)")
        playlist_files = [n for n in files if n.startswith("playlists/")]
        self.assertEqual(len(playlist_files), 2 * len(self.NAMES))
        for name in playlist_files:
            self.assertRegex(name, pattern)
            self.assertNotIn("..", name.split("/"))
        self.assertEqual(len({n.lower() for n in files}), len(files))
        readme = files["README.txt"].decode("utf-8")
        mapping = dict((m.group(3), (m.group(1), m.group(2))) for m in re.finditer(
            r'(?m)^(playlists/\S+\.csv) / (playlists/\S+\.m3u8): playlist public_id "([^"]*)"$', readme))
        self.assertEqual(set(mapping), {f"pub-{i}" for i in range(len(self.NAMES))})
        for public_id, (csv_name, m3u_name) in mapping.items():
            self.assertIn(csv_name, files)
            self.assertIn(m3u_name, files)
            self.assertEqual(csv_name[:-4], m3u_name[:-5])
            rows = csv_rows(files[csv_name])
            self.assertEqual(rows[1][0], public_id)
            self.assertEqual(rows[1][3], "T" + public_id[4:])

    def test_empty_and_pathlike_names_fall_back(self):
        files = files_of(ae.export_archive(self.snapshot()))
        names = sorted(n for n in files if n.endswith(".m3u8"))
        slugs = [n.split("-", 1)[1][:-5] for n in names]
        self.assertIn("playlist", slugs)
        self.assertTrue(all(len(s) <= 48 for s in slugs))
        self.assertTrue(all("/" not in s and "\\" not in s for s in slugs))

    def test_readme_mapping_escapes_odd_public_ids(self):
        pid = "id-\u65e5\n\"x"
        snap = with_section("playlists", sec(own(Playlist(pid, "n", True, T0, sec(), ()))))
        readme = files_of(ae.export_archive(snap))["README.txt"].decode("utf-8")
        line = [l for l in readme.split("\n") if "playlist public_id" in l]
        self.assertEqual(len(line), 1)
        self.assertEqual(json.loads(line[0].split("public_id ", 1)[1]), pid)

    def test_playlist_without_entries_has_header_only(self):
        snap = with_section("playlists", sec(own(Playlist("p", "n", True, T0, Section("not_collected", ()), ("a",)))))
        files = files_of(ae.export_archive(snap))
        self.assertEqual(len(csv_rows(files["playlists/000001-n.csv"])), 1)
        self.assertEqual(files["playlists/000001-n.m3u8"].decode(), "#EXTM3U\n# Metadata only; no media locations.\n")


class M3u8Tests(unittest.TestCase):
    def playlist_text(self, tracks):
        entries = Section("collected", tuple(own(PlaylistEntry(i, t)) for i, t in enumerate(tracks, 1)))
        snap = with_section("playlists", sec(own(Playlist("p", "n", True, T0, entries, ()))))
        return files_of(ae.export_archive(snap))["playlists/000001-n.m3u8"].decode("utf-8")

    def test_every_nonblank_line_is_a_comment_and_injection_is_neutralized(self):
        evil = [
            track(artist="A\n#EXTINF:1,evil\nhttp://example.invalid/x.mp3", title="t\r\n#EXTM3U"),
            track(artist="\nfile:///etc/passwd", title="\rhttp://x"),
            track(artist="a\x00\x1b\x85", title="\u200b\u202e\ue000"),
            track(artist="ok", title="fine", duration_seconds=None),
            track(artist="https://example.invalid/a.mp3", title="/local/path.mp3"),
        ]
        text = self.playlist_text(evil)
        self.assertNotIn("\r", text)
        lines = text.split("\n")
        self.assertEqual(lines[0], "#EXTM3U")
        self.assertEqual(lines[-1], "")
        for line in lines:
            self.assertTrue(line == "" or line.startswith("#"), line)
        self.assertEqual(sum(1 for l in lines if l.startswith("#EXTINF:")), len(evil))
        self.assertEqual(sum(1 for l in lines if l.startswith("#EXTM3U")), 1)
        self.assertEqual(len([l for l in lines if l]), 2 + len(evil))
        for c in text:
            if c != "\n":
                self.assertFalse(ord(c) < 32 or 0x7F <= ord(c) < 0xA0, repr(c))

    def test_extinf_duration_and_label(self):
        text = self.playlist_text([track(artist="A", title="T", duration_seconds=7), track(artist="B", title="U", duration_seconds=None)])
        self.assertIn("#EXTINF:7,A - T\n", text)
        self.assertIn("#EXTINF:-1,B - U\n", text)

    def test_unicode_label_preserved(self):
        text = self.playlist_text([track(artist="Björk", title="日本語")])
        self.assertIn("#EXTINF:180,Björk - 日本語\n", text)

    def test_text_file_contains_no_media_locators_beyond_labels(self):
        text = self.playlist_text([track()])
        self.assertEqual([l for l in text.split("\n") if l and not l.startswith("#")], [])

    def test_unicode_line_separators_do_not_break_comment_lines(self):
        # Zl/Zp separators must not create non-comment lines for Unicode parsers.
        text = self.playlist_text([track(artist="a\u2028http://example.invalid/x.mp3", title="b\u2029c")])
        for line in text.splitlines():
            self.assertTrue(line == "" or line.startswith("#"), line)


class InvalidInputTests(unittest.TestCase):
    def assertRejected(self, snapshot):
        with self.assertRaises(ExportError):
            ae.project(snapshot)
        with self.assertRaises(ExportError):
            ae.export_archive(snapshot)

    def one(self, name, row, base=None):
        return with_section(name, sec(own(row)), base)

    def test_error_message_does_not_contain_source_data(self):
        snap = self.one("sessions", Session(SENTINEL, "not-a-date", T1, False))
        with self.assertRaises(ExportError) as cm:
            ae.export_archive(snap)
        self.assertNotIn(SENTINEL, str(cm.exception))

    def test_is_value_error(self):
        self.assertTrue(issubclass(ExportError, ValueError))

    def test_snapshot_shape(self):
        base = full_snapshot()
        for bad in (None, object(), {"account": account()}):
            with self.assertRaises(ExportError):
                ae.project(bad)
        sections = dict(base.sections)
        missing = dict(sections)
        del missing["follows"]
        self.assertRejected(dataclasses.replace(base, sections=missing))
        self.assertRejected(dataclasses.replace(base, sections={**sections, "extra": sec()}))
        self.assertRejected(dataclasses.replace(base, sections={}))
        self.assertRejected(dataclasses.replace(base, sections=list(sections)))
        self.assertRejected(dataclasses.replace(base, sections=None))
        renamed = {("x" if k == "follows" else k): v for k, v in sections.items()}
        self.assertRejected(dataclasses.replace(base, sections=renamed))

    def test_snapshot_subclass_rejected(self):
        class Sub(Snapshot):
            pass

        base = full_snapshot()
        with self.assertRaises(ExportError):
            ae.project(Sub(base.account, base.generated_at, base.sections))

    def test_timestamps(self):
        base = full_snapshot()
        class Sub(datetime):
            pass

        class ZeroOffset(tzinfo):
            def utcoffset(self, dt):
                return timedelta(0)

            def dst(self, dt):
                return None

            def tzname(self, dt):
                return "Z"

        for bad in (
            datetime(2024, 1, 1), datetime(2024, 1, 1, tzinfo=timezone(timedelta(hours=1))),
            datetime(2024, 1, 1, tzinfo=ZeroOffset()),
            Sub(2024, 1, 1, tzinfo=UTC), "2024-01-01T00:00:00Z", 1700000000, None, 1700000000.0,
        ):
            self.assertRejected(dataclasses.replace(base, generated_at=bad))
            self.assertRejected(self.one("sessions", Session("s", bad, T1, False)))
        self.assertRejected(self.one("sessions", Session("s", T0, None, False)))
        self.assertRejected(self.one("invoices", Invoice("i", 1, "usd", None)))

    def test_epoch_helper(self):
        self.assertEqual(ae.utc_from_epoch(0), datetime(1970, 1, 1, tzinfo=UTC))
        self.assertEqual(ae.utc_from_epoch(1_700_000_000), datetime(2023, 11, 14, 22, 13, 20, tzinfo=UTC))
        self.assertIs(ae.utc_from_epoch(5).tzinfo, UTC)
        for bad in (True, False, 1.5, 1.0, "1", None, b"1", 1_700_000_000_000, 10**20, -10**20, 2**63):
            with self.assertRaises(ExportError):
                ae.utc_from_epoch(bad)

    def test_epoch_helper_result_is_exportable(self):
        snap = self.one("sessions", Session("s", ae.utc_from_epoch(0), ae.utc_from_epoch(86400), False))
        self.assertEqual(ae.project(snap)["sessions"]["rows"][0]["created_at"], "1970-01-01T00:00:00.000000Z")

    def test_primitives(self):
        bads = [
            ("sign_in_methods", lambda v: SignInMethod("c", T0, v), [True, -1, "1", 1.0, 2**63, None]),
            ("sessions", lambda v: Session("s", T0, T1, v), [1, 0, "true", None]),
            ("usage_totals", lambda v: Usage(T0, T1, 1, v), [-5, 1.5, True, "9"]),
            ("invoices", lambda v: Invoice("i", v, "usd", T0), [-1, 2**64, 1.0, True]),
            ("security_activity", lambda v: SecurityActivity("a", v, T0), [1, "ok", None]),
            ("playlists", lambda v: Playlist("p", "n", v, T0, sec(), ()), [0, 1, None]),
        ]
        for name, make, values in bads:
            for value in values:
                with self.subTest(section=name, value=value):
                    self.assertRejected(self.one(name, make(value)))

    def test_boundary_ints_accepted(self):
        data = ae.project(self.one("usage_totals", Usage(T0, T1, 0, 2**63 - 1)))
        self.assertEqual(data["usage_totals"]["rows"][0]["ingest_jobs"], 2**63 - 1)

    def test_text_types(self):
        class S(str):
            pass

        for bad in (None, 5, b"x", S("x"), ["x"], "\ud800", "ok\udfff"):
            with self.subTest(bad=bad):
                self.assertRejected(self.one("genres", Genre("g", bad, True, T0)))
                self.assertRejected(self.one("invoices", Invoice(bad, 1, "usd", T0)))
                self.assertRejected(dataclasses.replace(full_snapshot(), account=account(email=bad)))

    def test_empty_public_identities(self):
        self.assertRejected(self.one("genres", Genre("", "n", True, T0)))
        self.assertRejected(self.one("playlists", Playlist("", "n", True, T0, sec(), ())))

    def test_track_validation(self):
        bads = [
            track(recording_mbid="not-a-uuid"), track(release_mbid="0123abcd"), track(recording_mbid=""),
            track(artist_mbids=("zzzz",)), track(artist_mbids=[MBID_A]), track(artist_mbids=(MBID_A, None)),
            track(recording_mbid=MBID_A + "\n"), track(recording_mbid=" " + MBID_A),
            track(recording_mbid="0123abcd-0123-4abc-8def-0123456789a\u0663"),
            track(available=True, catalogue_missing=True), track(available=1), track(catalogue_missing=0),
            track(duration_seconds=-1), track(duration_seconds=1.5), track(duration_seconds=True),
            track(title=None), track(artist=5), track(release=b"r"),
        ]
        for bad in bads:
            with self.subTest(bad=bad):
                self.assertRejected(self.one("listening_history", Play(bad, T0, 1)))
        self.assertRejected(self.one("listening_history", Play(None, T0, 1)))
        self.assertRejected(self.one("listening_history", Play(track(), T0, -1)))
        self.assertRejected(self.one("listening_history", Play({"title": "x"}, T0, 1)))

    def test_scopes_and_tuples_must_be_tuples(self):
        key = lambda scopes: AppKey("k", None, None, None, scopes, None, T0, None, None, None, None)
        for bad in (["read"], "read", None, ("read", 1), {"read"}, ("read", None)):
            self.assertRejected(self.one("app_keys", key(bad)))
        self.assertRejected(self.one("playlists", Playlist("p", "n", True, T0, sec(), ["a"])))

    def test_optional_fields_reject_wrong_types(self):
        self.assertRejected(self.one("app_keys", AppKey("k", 5, None, None, (), None, T0, None, None, None, None)))
        self.assertRejected(self.one("app_keys", AppKey("k", None, None, None, (), None, T0, "never", None, None, None)))
        self.assertRejected(self.one("app_keys", AppKey("k", None, None, None, (), None, T0, None, None, None, 7)))
        self.assertRejected(self.one("app_keys", AppKey(None, None, None, None, (), None, T0, None, None, None, None)))

    def test_section_container_shape(self):
        for bad in (
            None, (), [], {}, "collected", (own(Genre("g", "n", True, T0)),),
            Section("collected", [own(Genre("g", "n", True, T0))]),
            Section("collected", None), Section("partial", ()),
            Section("", ()), Section(None, ()), Section("COLLECTED", ()), Section("collected ", ()),
            Section("complete", ()),
        ):
            with self.subTest(bad=bad):
                self.assertRejected(with_section("genres", bad))

    def test_section_and_owned_subclasses_rejected(self):
        class SubSection(Section):
            pass

        class SubOwned(Owned):
            pass

        self.assertRejected(with_section("genres", SubSection("collected", ())))
        self.assertRejected(with_section("genres", Section("collected", (SubOwned(OWNER, Genre("g", "n", True, T0)),))))

    def test_malformed_ownership_identities_fail(self):
        for owner in (0, True, "", object()):
            self.assertRejected(with_section("genres", sec(Owned(owner, Genre("g", "n", True, T0)))))

    def test_rows_without_ownership_wrapper_fail(self):
        for raw in (Genre("g", "n", True, T0), None, (OWNER, Genre("g", "n", True, T0)), {"owner": OWNER}):
            self.assertRejected(with_section("genres", Section("collected", (raw,))))

    def test_wrong_row_type_in_section(self):
        wrong = [
            ("sessions", SignInMethod("c", T0, 1)), ("sessions", None), ("sessions", "x"), ("sessions", {}),
            ("genres", Playlist("p", "n", True, T0, sec(), ())), ("invoices", Genre("g", "n", True, T0)),
            ("playlists", Genre("g", "n", True, T0)),
        ]
        for name, row in wrong:
            with self.subTest(name=name, row=row):
                self.assertRejected(self.one(name, row))

    def test_wrong_nested_row_type(self):
        lib = Library("L", T0, sec(own(PlaylistEntry(1, track()))))
        self.assertRejected(self.one("libraries", lib))
        entries = sec(own(LibraryTrack(T0, track())))
        self.assertRejected(self.one("playlists", Playlist("p", "n", True, T0, entries, ())))
        self.assertRejected(self.one("libraries", Library("L", T0, ())))
        self.assertRejected(self.one("libraries", Library("L", T0, None)))
        self.assertRejected(self.one("libraries", Library("L", T0, Section("bogus", ()))))
        self.assertRejected(self.one("libraries", Library("L", T0, Section("not_collected", (own(LibraryTrack(T0, track())),)))))
        self.assertRejected(self.one("playlists", Playlist("p", "n", True, T0, [], ())))

    def test_unknown_future_sections(self):
        for name in FUTURE:
            self.assertRejected(with_section(name, Section("collected", ())))
            self.assertRejected(with_section(name, sec(own(object()))))
            self.assertRejected(with_section(name, Section("not_collected", (own(object()),))))
            self.assertRejected(with_section(name, Section("unknown", ())))
            self.assertRejected(with_section(name, None))
        for name, cls in ae.SECTIONS.items():
            self.assertIs(cls is None, name in FUTURE)

    def test_not_collected_with_rows_fails_for_every_section(self):
        for name in ae.SECTIONS:
            with self.subTest(name=name):
                self.assertRejected(with_section(name, Section("not_collected", (own(Genre("g", "n", True, T0)),))))

    def test_not_collected_known_sections_accepted_empty(self):
        sections = {k: Section("not_collected", ()) for k in ae.SECTIONS}
        snap = Snapshot(account(), T1, sections)
        data = ae.project(snap)
        self.assertTrue(all(data[k] == {"status": "not_collected", "rows": []} for k in ae.SECTIONS))
        readme = files_of(ae.export_archive(snap))["README.txt"].decode()
        self.assertIn("sessions: not_collected (0 rows)", readme)

    def test_duplicate_playlist_ids(self):
        a = Playlist("dup", "A", True, T0, sec(), ())
        b = Playlist("dup", "B", False, T1, sec(), ())
        self.assertRejected(with_section("playlists", sec(own(a), own(b))))
        self.assertRejected(with_section("playlists", sec(own(a), own(a))))

    def test_duplicate_genre_ids(self):
        self.assertRejected(with_section("genres", sec(own(Genre("g", "A", True, T0)), own(Genre("g", "B", True, T0)))))

    def test_duplicate_and_bad_positions(self):
        def pl(*positions):
            entries = sec(*[own(PlaylistEntry(p, track(title=f"t{i}"))) for i, p in enumerate(positions)])
            return Playlist("p", "n", True, T0, entries, ())

        self.assertRejected(self.one("playlists", pl(1, 1)))
        self.assertRejected(self.one("playlists", pl(1, 2, 3, 2)))
        self.assertRejected(self.one("playlists", pl(-1)))
        self.assertRejected(self.one("playlists", pl(True)))
        self.assertRejected(self.one("playlists", pl("1")))
        self.assertRejected(self.one("playlists", pl(None)))
        ae.export_archive(self.one("playlists", pl(0, 5, 100)))
        # Identical positions in different playlists are fine.
        both = sec(own(dataclasses.replace(pl(1), public_id="a")), own(dataclasses.replace(pl(1), public_id="b")))
        ae.export_archive(with_section("playlists", both))

    def test_legacy_items_opaque_strings_never_locators(self):
        def pl(items):
            return Playlist("p", "n", True, T0, sec(), items)

        # Stored legacy library items are opaque strings; any text that is not a locator survives.
        good = ("pl_01", "a-b", "日本語", "A" * 256, "a b", "../x", "a/b", "a.mp3", "file:x", "12345", "x/y//z",
                "mbid:0123abcd-0123-4abc-8def-0123456789ab", "Ünï ☃ 😀", "tab\there", "nl\nin", "nul\x00x", "\x1b[0m",
                "a//b", "  lead", "javascript:alert(1)", "{\"id\":1}")
        data = ae.project(self.one("playlists", pl(good)))
        self.assertEqual(data["playlists"]["rows"][0]["legacy_items"], list(good))
        ae.export_archive(self.one("playlists", pl(())))
        bads = [
            ("http://example.invalid/a.mp3",), ("ok", "https://example.invalid/x"), ("",), ("ok", ""),
            ("file:///etc/passwd",), ("s3://bucket/key",), ("//host/share",), ("  //host/share",),
            ("\t//host",), ("a", "x://y"),
            ({"id": 1},), (1,), (None,), (["a"],), (b"a",), ["a"], "abc", None, ("ok", 5),
        ]
        for bad in bads:
            with self.subTest(bad=bad):
                self.assertRejected(self.one("playlists", pl(bad)))

    def test_legacy_items_str_subclass_and_surrogates_rejected(self):
        class S(str):
            pass

        for bad in (S("a"), "\ud800"):
            self.assertRejected(self.one("playlists", Playlist("p", "n", True, T0, sec(), (bad,))))

    def test_invalid_row_anywhere_fails_whole_archive(self):
        base = full_snapshot()
        poisoned = with_section("genres", sec(*base.sections["genres"].rows, own(Genre("", "n", True, T0))), base)
        self.assertRejected(poisoned)
        nested = Library("N", T0, sec(own(LibraryTrack(T0, track(recording_mbid="bad")))))
        self.assertRejected(with_section("libraries", sec(*base.sections["libraries"].rows, own(nested)), base))

    def test_owned_garbage_row_rejected(self):
        self.assertRejected(self.one("genres", object()))

    def test_subscription_optional_period_dates(self):
        sub = Subscription("pro", "active", None, None, True)
        row = ae.project(self.one("subscriptions", sub))["subscriptions"]["rows"][0]
        self.assertEqual((row["period_start"], row["period_end"]), (None, None))
        row = ae.project(self.one("subscriptions", Subscription("p", "s", T0, None, False)))["subscriptions"]["rows"][0]
        self.assertEqual((row["period_start"], row["period_end"]), ("2024-01-02T03:04:05.000006Z", None))
        ae.export_archive(self.one("subscriptions", sub))
        for bad in ("2024-01-01", 0, datetime(2024, 1, 1)):
            self.assertRejected(self.one("subscriptions", Subscription("p", "s", bad, None, False)))
            self.assertRejected(self.one("subscriptions", Subscription("p", "s", None, bad, False)))
        self.assertRejected(self.one("subscriptions", Subscription("p", "s", None, None, None)))

    def test_other_nullable_fields_remain_required_where_not_optional(self):
        self.assertRejected(self.one("usage_totals", Usage(None, T1, 1, 2)))
        self.assertRejected(self.one("usage_totals", Usage(T0, T1, 1, None)))
        self.assertRejected(self.one("billing_operations", BillingOperation("k", "s", None, T1)))

    def test_usage_exports_ingest_jobs_not_bytes(self):
        row = ae.project(self.one("usage_totals", Usage(T0, T1, 4, 9)))["usage_totals"]["rows"][0]
        self.assertEqual(set(row), {"period_start", "period_end", "stream_seconds", "ingest_jobs"})
        self.assertEqual(row["ingest_jobs"], 9)
        self.assertEqual([f.name for f in dataclasses.fields(Usage)], ["period_start", "period_end", "stream_seconds", "ingest_jobs"])


class LimitTests(unittest.TestCase):
    def test_row_limit_counts_all_rows_including_filtered(self):
        sessions = lambda n, owner=OWNER: tuple(own(Session(f"s{i}", T0, T1, False), owner) for i in range(n))
        with patch.object(ae, "MAX_ROWS", 3):
            ae.export_archive(empty_snapshot(sessions=Section("collected", sessions(3))))
            with self.assertRaises(ExportError):
                ae.export_archive(empty_snapshot(sessions=Section("collected", sessions(4))))
            with self.assertRaises(ExportError):
                ae.project(empty_snapshot(sessions=Section("collected", sessions(3) + sessions(1, OTHER))))
            with self.assertRaises(ExportError):
                ae.project(empty_snapshot(sessions=Section("collected", sessions(2)), genres=Section("collected", (own(Genre("g", "n", True, T0)), own(Genre("h", "n", True, T0))))))

    def test_row_limit_counts_nested_rows(self):
        lib = Library("L", T0, sec(*[own(LibraryTrack(T0, track(title=f"t{i}", artist_mbids=()))) for i in range(3)]))
        with patch.object(ae, "MAX_ROWS", 4):
            ae.project(empty_snapshot(libraries=sec(own(lib))))
        with patch.object(ae, "MAX_ROWS", 3):
            with self.assertRaises(ExportError):
                ae.project(empty_snapshot(libraries=sec(own(lib))))

    def test_row_limit_counts_tuple_primitives(self):
        mbids = tuple(f"0123abcd-0123-4abc-8def-0123456789a{i}" for i in range(3))
        snap = empty_snapshot(listening_history=sec(own(Play(track(artist_mbids=mbids), T0, 1))))
        with patch.object(ae, "MAX_ROWS", 4):
            ae.project(snap)
        with patch.object(ae, "MAX_ROWS", 3):
            with self.assertRaises(ExportError):
                ae.project(snap)
        legacy = empty_snapshot(playlists=sec(own(Playlist("p", "n", True, T0, sec(), ("a", "b", "c")))))
        with patch.object(ae, "MAX_ROWS", 4):
            ae.project(legacy)
        with patch.object(ae, "MAX_ROWS", 3):
            with self.assertRaises(ExportError):
                ae.project(legacy)
        key = AppKey("k", None, None, None, ("a", "b"), None, T0, None, None, None, None)
        scopes = empty_snapshot(app_keys=sec(own(key)))
        with patch.object(ae, "MAX_ROWS", 3):
            ae.project(scopes)
        with patch.object(ae, "MAX_ROWS", 2):
            with self.assertRaises(ExportError):
                ae.project(scopes)

    def test_single_tuple_longer_than_limit_fails(self):
        with patch.object(ae, "MAX_ROWS", 2):
            with self.assertRaises(ExportError):
                ae.project(empty_snapshot(playlists=sec(own(Playlist("p", "n", True, T0, sec(), ("a", "b", "c"))))))

    def test_text_limit_is_bytes_not_characters(self):
        plain = dict(recording_mbid=None, release_mbid=None, artist_mbids=())
        with patch.object(ae, "MAX_TEXT_BYTES", 24):
            ae.export_archive(history_titles(["a" * 24], **plain))
            ae.export_archive(history_titles(["é" * 12], **plain))
            ae.export_archive(history_titles(["😀" * 6], **plain))
            for bad in ("a" * 25, "é" * 13, "😀" * 7):
                with self.subTest(bad=bad):
                    with self.assertRaises(ExportError):
                        ae.export_archive(history_titles([bad], **plain))
            with self.assertRaises(ExportError):
                ae.project(dataclasses.replace(empty_snapshot(), account=account(email="e" * 25)))
            with self.assertRaises(ExportError):
                ae.project(dataclasses.replace(empty_snapshot(), account=account(id="i" * 25)))

    def test_aggregate_input_text_limit(self):
        plain = dict(recording_mbid=None, release_mbid=None, artist_mbids=())
        # Per play: 50-byte title + "Artist" + "Release"; account id/email add 26 bytes.
        two = history_titles(["a" * 50, "b" * 50], **plain)
        three = history_titles(["a" * 50, "b" * 50, "c" * 50], **plain)
        with patch.object(ae, "MAX_INPUT_TEXT_BYTES", 200):
            ae.project(two)
            with self.assertRaises(ExportError):
                ae.project(three)
            with self.assertRaises(ExportError):
                ae.export_archive(three)
        # Each string is under MAX_TEXT_BYTES, so only the aggregate cap can reject this.
        self.assertLess(50, ae.MAX_TEXT_BYTES)
        ae.export_archive(three)

    def test_aggregate_input_text_limit_ignores_filtered_rows(self):
        plain = dict(recording_mbid=None, release_mbid=None, artist_mbids=())
        big = Owned(OTHER, Play(track(title="z" * 5000, **plain), T0, 1))
        snap = history_titles(["a" * 50], **plain)
        rows = snap.sections["listening_history"].rows + (big,)
        snap = with_section("listening_history", Section("collected", rows), snap)
        with patch.object(ae, "MAX_INPUT_TEXT_BYTES", 200):
            data = ae.project(snap)
        self.assertEqual(len(data["listening_history"]["rows"]), 1)

    def test_csv_amplification_stops_before_consuming_all_rows(self):
        consumed = []

        def rows():
            for index in range(1000):
                consumed.append(index)
                yield ["p" * 64, index]

        with patch.object(ae, "MAX_ARCHIVE_BYTES", 256):
            with self.assertRaises(ExportError):
                ae._csv(rows())
        self.assertLess(len(consumed), 5)

    def test_csv_files_share_one_output_budget(self):
        budget = ae._OutputBudget(100)
        ae._csv([["x" * 60]], budget)
        with self.assertRaises(ExportError):
            ae._csv([["y" * 60]], budget)
        self.assertEqual(budget.remaining, 38)

    def test_archive_size_limit_boundary(self):
        snap = full_snapshot()
        total = sum(len(v) for v in files_of(ae.export_archive(snap)).values())
        with patch.object(ae, "MAX_ARCHIVE_BYTES", total):
            ae.export_archive(snap)
        with patch.object(ae, "MAX_ARCHIVE_BYTES", total - 1):
            with self.assertRaises(ExportError):
                ae.export_archive(snap)
        with patch.object(ae, "MAX_ARCHIVE_BYTES", 100):
            with self.assertRaises(ExportError):
                ae.export_archive(empty_snapshot())

    def test_limits_have_sane_defaults(self):
        self.assertEqual(ae.MAX_ROWS, 100_000)
        self.assertEqual(ae.MAX_TEXT_BYTES, 65_536)
        self.assertEqual(ae.MAX_INPUT_TEXT_BYTES, 8 * 1024 * 1024)
        self.assertEqual(ae.MAX_ARCHIVE_BYTES, 64 * 1024 * 1024)


if __name__ == "__main__":
    unittest.main()
