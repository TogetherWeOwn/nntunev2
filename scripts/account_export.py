"""Pure offline account projection. No collectors, authorization, I/O or app wiring.

Inputs are deliberately NOT database models. Adapters must explicitly select public
identity and account-scoped fields. See docs/account-export-format.md for the v1
contract, coverage semantics and the integration/security work this does not do.
"""

from __future__ import annotations

import csv
import io
import itertools
import json
import re
import types
import unicodedata
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Generic, TypeVar, get_args, get_origin, get_type_hints


class ExportError(ValueError):
    """Invalid or incomplete input; no archive is returned. Contains no source data."""


T = TypeVar("T")


@dataclass(frozen=True)
class Owned(Generic[T]):
    owner_user_id: str | None
    value: T


@dataclass(frozen=True)
class Section(Generic[T]):
    status: str
    rows: tuple[Owned[T], ...]


@dataclass(frozen=True)
class Account:
    id: str
    email: str
    created_at: datetime
    deleting_at: datetime | None


@dataclass(frozen=True)
class SignInMethod:
    cred_id: str
    registered_at: datetime
    sign_count: int


@dataclass(frozen=True)
class Session:
    id: str
    created_at: datetime
    expires_at: datetime
    revoked: bool


@dataclass(frozen=True)
class AppKey:
    label: str
    client_name: str | None
    key_prefix: str | None
    username: str | None
    scopes: tuple[str, ...]
    auth_mode: str | None
    created_at: datetime
    expires_at: datetime | None
    last_used_at: datetime | None
    revoked_at: datetime | None
    created_from_ip: str | None


@dataclass(frozen=True)
class Track:
    title: str
    artist: str
    release: str
    recording_mbid: str | None
    release_mbid: str | None
    artist_mbids: tuple[str, ...]
    duration_seconds: int | None
    available: bool
    catalogue_missing: bool


@dataclass(frozen=True)
class LibraryTrack:
    added_at: datetime
    track: Track


@dataclass(frozen=True)
class Library:
    name: str
    created_at: datetime
    tracks: Section[LibraryTrack]


@dataclass(frozen=True)
class PlaylistEntry:
    position: int
    track: Track


@dataclass(frozen=True)
class Playlist:
    public_id: str
    name: str
    is_public: bool
    created_at: datetime
    entries: Section[PlaylistEntry]
    # Legacy storage is a list of public identity strings, not arbitrary objects.
    # Unknown legacy shapes must fail rather than exporting unchecked JSONB.
    legacy_items: tuple[str, ...]


@dataclass(frozen=True)
class Genre:
    public_id: str
    name: str
    is_public: bool
    created_at: datetime


@dataclass(frozen=True)
class Play:
    track: Track
    recorded_at: datetime
    stream_seconds: int


@dataclass(frozen=True)
class Usage:
    period_start: datetime
    period_end: datetime
    stream_seconds: int
    ingest_jobs: int


@dataclass(frozen=True)
class BillingAccount:
    plan: str
    status: str


@dataclass(frozen=True)
class Subscription:
    plan: str
    status: str
    period_start: datetime | None
    period_end: datetime | None
    cancel_at_period_end: bool


@dataclass(frozen=True)
class Invoice:
    stripe_invoice_id: str
    amount: int
    currency: str
    paid_at: datetime


@dataclass(frozen=True)
class BillingOperation:
    kind: str
    state: str
    created_at: datetime
    updated_at: datetime


@dataclass(frozen=True)
class DeletionState:
    scope: str
    state: str
    started_at: datetime
    erase_after: datetime


@dataclass(frozen=True)
class SecurityActivity:
    action: str
    ok: bool
    at: datetime


@dataclass(frozen=True)
class LegacyProviderVault:
    exists: bool
    updated_at: datetime


# Explicit projection allowlists. Extra attributes on source subclasses are ignored.
# No asdict(), __dict__, debug serialization, or arbitrary JSON passthrough.
FIELDS = {
    Account: "id email created_at deleting_at",
    SignInMethod: "cred_id registered_at sign_count",
    Session: "id created_at expires_at revoked",
    AppKey: "label client_name key_prefix username scopes auth_mode created_at expires_at last_used_at revoked_at created_from_ip",
    Track: "title artist release recording_mbid release_mbid artist_mbids duration_seconds available catalogue_missing",
    LibraryTrack: "added_at track",
    Library: "name created_at tracks",
    PlaylistEntry: "position track",
    Playlist: "public_id name is_public created_at entries legacy_items",
    Genre: "public_id name is_public created_at",
    Play: "track recorded_at stream_seconds",
    Usage: "period_start period_end stream_seconds ingest_jobs",
    BillingAccount: "plan status",
    Subscription: "plan status period_start period_end cancel_at_period_end",
    Invoice: "stripe_invoice_id amount currency paid_at",
    BillingOperation: "kind state created_at updated_at",
    DeletionState: "scope state started_at erase_after",
    SecurityActivity: "action ok at",
    LegacyProviderVault: "exists updated_at",
}

# Every source is mandatory, even when explicitly not collected. Future sources
# have no accepted row schema yet and cannot be claimed as collected.
SECTIONS = {
    "sign_in_methods": SignInMethod,
    "sessions": Session,
    "app_keys": AppKey,
    "libraries": Library,
    "legacy_library": LibraryTrack,
    "playlists": Playlist,
    "genres": Genre,
    "listening_history": Play,
    "usage_totals": Usage,
    "billing_accounts": BillingAccount,
    "subscriptions": Subscription,
    "invoices": Invoice,
    "billing_operations": BillingOperation,
    "deletion_state": DeletionState,
    "security_activity": SecurityActivity,
    "legacy_provider_vault": LegacyProviderVault,
    "subsonic_credentials": None,
    "listenbrainz_links": None,
    "follows": None,
}
MAX_ROWS = 100_000
MAX_TEXT_BYTES = 65_536
MAX_INPUT_TEXT_BYTES = 8 * 1024 * 1024
MAX_ARCHIVE_BYTES = 64 * 1024 * 1024


@dataclass(frozen=True)
class Snapshot:
    account: Account | None
    generated_at: datetime
    sections: dict[str, Section]


def utc_from_epoch(seconds: int) -> datetime:
    """Convert legacy epoch seconds at the adapter/DTO boundary (not milliseconds)."""
    if type(seconds) is not int:
        raise ExportError("invalid epoch seconds")
    try:
        return datetime.fromtimestamp(seconds, timezone.utc)
    except (ValueError, OverflowError, OSError):
        raise ExportError("invalid epoch seconds") from None


def _timestamp(value: datetime) -> str:
    if type(value) is not datetime or value.tzinfo is not timezone.utc:
        raise ExportError("timestamp must use UTC")
    return value.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _text(value: str) -> str:
    if type(value) is not str:
        raise ExportError("expected text")
    try:
        if len(value.encode("utf-8")) > MAX_TEXT_BYTES:
            raise ExportError("text limit exceeded")
    except UnicodeEncodeError:
        raise ExportError("invalid Unicode") from None
    return value


class _Projection:
    def __init__(self, owner: str):
        self.owner = owner
        self.rows = 0
        self.text_bytes = 0

    def count(self, count: int):
        self.rows += count
        if self.rows > MAX_ROWS:
            raise ExportError("row limit exceeded")

    def value(self, value, expected):
        if get_origin(expected) is types.UnionType:
            if value is None and type(None) in get_args(expected):
                return None
            return self.value(value, next(t for t in get_args(expected) if t is not type(None)))
        if get_origin(expected) is tuple:
            if type(value) is not tuple or len(value) > MAX_ROWS:
                raise ExportError("invalid list")
            self.count(len(value))
            return [self.value(v, get_args(expected)[0]) for v in value]
        if get_origin(expected) is Section:
            return self.section(value, get_args(expected)[0])
        if expected is datetime:
            return _timestamp(value)
        if expected is str:
            text = _text(value)
            self.text_bytes += len(text.encode("utf-8"))
            if self.text_bytes > MAX_INPUT_TEXT_BYTES:
                raise ExportError("input text limit exceeded")
            return text
        if expected in (int, bool):
            if type(value) is not expected or (expected is int and not 0 <= value <= 2**63 - 1):
                raise ExportError("invalid number or boolean")
            return value
        if expected not in FIELDS or not isinstance(value, expected):
            raise ExportError("unknown row type")
        hints = get_type_hints(expected)
        row = {key: self.value(getattr(value, key), hints[key]) for key in FIELDS[expected].split()}
        if expected is Account and not row["id"]:
            raise ExportError("missing public account identity")
        if expected in (Playlist, Genre) and not row["public_id"]:
            raise ExportError("missing public identity")
        if expected is Session:
            row["id"] = row["id"][:8]
        if expected is Playlist:
            # Preserve legacy public identity strings exactly, never media locators.
            if any(not item or "://" in item or item.lstrip().startswith("//") for item in row["legacy_items"]):
                raise ExportError("unsupported legacy identity")
        if expected is Track:
            for key in ("recording_mbid", "release_mbid"):
                if row[key] is not None and not _mbid(row[key]):
                    raise ExportError("invalid MusicBrainz identity")
            if any(not _mbid(item) for item in row["artist_mbids"]):
                raise ExportError("invalid MusicBrainz identity")
            if row["catalogue_missing"] and row["available"]:
                raise ExportError("missing catalogue cannot be available")
        return row

    def section(self, section, expected):
        if type(section) is not Section or type(section.rows) is not tuple:
            raise ExportError("missing source coverage")
        if section.status not in ("collected", "not_collected"):
            raise ExportError("unknown source coverage")
        if section.status == "not_collected" and section.rows:
            raise ExportError("uncollected source contains rows")
        if expected is None and section.status != "not_collected":
            raise ExportError("source has no accepted schema")
        self.count(len(section.rows))
        rows = []
        for owned in section.rows:
            if type(owned) is not Owned:
                raise ExportError("missing ownership")
            if owned.owner_user_id is not None and (type(owned.owner_user_id) is not str or not owned.owner_user_id):
                raise ExportError("invalid ownership identity")
            # Shared rows and other principals never enter the projection.
            if owned.owner_user_id == self.owner:
                rows.append(self.value(owned.value, expected))
        rows.sort(key=_canonical)
        if expected is PlaylistEntry:
            rows.sort(key=lambda r: r["position"])
            if len({r["position"] for r in rows}) != len(rows):
                raise ExportError("duplicate playlist position")
        if expected in (Playlist, Genre):
            if len({r["public_id"] for r in rows}) != len(rows):
                raise ExportError("duplicate public identity")
        return {"status": section.status, "rows": rows}


def _mbid(value: str) -> bool:
    return bool(re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", value))


def _canonical(value) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def project(snapshot: Snapshot) -> dict:
    """Allowlist and filter a complete snapshot; this is NOT authorization."""
    if type(snapshot) is not Snapshot or not isinstance(snapshot.account, Account):
        raise ExportError("account absent or erased")
    if type(snapshot.sections) is not dict or snapshot.sections.keys() != SECTIONS.keys():
        raise ExportError("missing or unknown section")
    projection = _Projection(_text(snapshot.account.id))
    return {
        "schema_version": 1,
        "generated_at": _timestamp(snapshot.generated_at),
        "account": projection.value(snapshot.account, Account),
        **{key: projection.section(snapshot.sections[key], expected) for key, expected in SECTIONS.items()},
    }


def _display(value) -> str:
    # Escape controls visibly; do not silently lose the original source text.
    text = str(value) if value is not None else ""
    return "".join(f"\\u{ord(c):04x}" if unicodedata.category(c).startswith("C") and c not in "\r\n" else c for c in text)


class _OutputBudget:
    def __init__(self, limit=None):
        self.remaining = MAX_ARCHIVE_BYTES if limit is None else limit


class _Utf8Buffer:
    def __init__(self, budget):
        self.budget = budget
        self.out = io.BytesIO()

    def write(self, text):
        # Check even before encoding, then charge actual UTF-8 bytes before retaining
        # them. Repeated identifiers in CSV must not amplify into unbounded output.
        if len(text) > self.budget.remaining:
            raise ExportError("archive size limit exceeded")
        encoded = text.encode("utf-8")
        if len(encoded) > self.budget.remaining:
            raise ExportError("archive size limit exceeded")
        self.budget.remaining -= len(encoded)
        self.out.write(encoded)
        return len(text)


def _utf8(chunks, budget):
    buffer = _Utf8Buffer(budget)
    for chunk in chunks:
        buffer.write(chunk)
    return buffer.out.getvalue()


def _csv(rows, budget=None) -> bytes:
    out = _Utf8Buffer(_OutputBudget() if budget is None else budget)
    writer = csv.writer(out, lineterminator="\r\n")
    for row in rows:
        cells = []
        for value in row:
            cell = _display(value)
            if cell.lstrip().startswith(("=", "+", "-", "@")):
                cell = "'" + cell
            cells.append(cell)
        writer.writerow(cells)
    return out.out.getvalue()


TRACK_COLUMNS = ["title", "artist", "release", "recording_mbid", "release_mbid", "artist_mbids", "duration_seconds", "available", "catalogue_missing"]


def _track_cells(track: dict) -> list:
    return [";".join(track[key]) if key == "artist_mbids" else track[key] for key in TRACK_COLUMNS]


def _playlist_name(index: int, name: str) -> str:
    # Ordinal guarantees uniqueness even on case-insensitive/normalizing extractors.
    slug = re.sub(r"[^A-Za-z0-9_-]", "_", name)[:48].strip("_") or "playlist"
    return f"playlists/{index:06d}-{slug}"


def export_archive(snapshot: Snapshot) -> bytes:
    """Return a complete deterministic STORE ZIP, or raise ExportError; never write files."""
    data = project(snapshot)
    budget = _OutputBudget()
    encoder = json.JSONEncoder(ensure_ascii=False, sort_keys=True, indent=2)
    files = {
        "account.json": _utf8(itertools.chain(encoder.iterencode(data), ("\n",)), budget),
    }
    library = [["library_name", "library_created_at", "added_at", *TRACK_COLUMNS]]
    for row in data["libraries"]["rows"]:
        for item in row["tracks"]["rows"]:
            library.append([row["name"], row["created_at"], item["added_at"], *_track_cells(item["track"])])
    for item in data["legacy_library"]["rows"]:
        library.append(["", "", item["added_at"], *_track_cells(item["track"])])
    files["library.csv"] = _csv(library, budget)
    history = [["recorded_at", "stream_seconds", *TRACK_COLUMNS]]
    for row in data["listening_history"]["rows"]:
        history.append([row["recorded_at"], row["stream_seconds"], *_track_cells(row["track"])])
    files["listening_history.csv"] = _csv(history, budget)
    playlist_files = []
    for index, row in enumerate(data["playlists"]["rows"], 1):
        name = _playlist_name(index, row["name"])
        entries = row["entries"]["rows"]
        files[name + ".csv"] = _csv(itertools.chain(
            (["playlist_public_id", "position", "unavailable", *TRACK_COLUMNS],),
            ([row["public_id"], e["position"], not e["track"]["available"], *_track_cells(e["track"])] for e in entries),
        ), budget)
        lines = ["#EXTM3U", "# Metadata only; no media locations."]
        for entry in entries:
            track = entry["track"]
            label = f'{track["artist"]} - {track["title"]}'
            label = "".join(f"\\u{ord(c):04x}" if unicodedata.category(c).startswith("C") or unicodedata.category(c) in ("Zl", "Zp") or c in "\r\n" else c for c in label)
            duration = track["duration_seconds"] if track["duration_seconds"] is not None else -1
            lines.append(f"#EXTINF:{duration},{label}")
        files[name + ".m3u8"] = _utf8((line + "\n" for line in lines), budget)
        playlist_files.append(f'{name}.csv / {name}.m3u8: playlist public_id {json.dumps(row["public_id"], ensure_ascii=True)}')
    coverage = [f'{key}: {data[key]["status"]} ({len(data[key]["rows"])} rows)' for key in SECTIONS]
    readme = [
        "NNTune account export — schema_version 1",
        f'Generated at {data["generated_at"]}',
        "Offline snapshot supplied by caller; this helper is not an authorization or collection check.",
        "account.json: lossless allowed fields, all source coverage, original text and legacy identities.",
        "library.csv: library memberships and retained track identity (blank library fields mean legacy membership).",
        "listening_history.csv: plays, recorded time, stream seconds and retained track identity.",
        "playlists/*.csv: public playlist identity, position, unavailable flag and track identity.",
        "playlists/*.m3u8: EXTINF metadata only, never playable media locations.",
        "Playlist filename mapping:", *playlist_files,
        "CSV: UTF-8, quoted commas/newlines; formula-leading cells have an apostrophe prefix.",
        "Controls are visibly escaped in CSV/M3U8; account.json preserves original text.",
        "Track available=false retains dormant entries; catalogue_missing=true retains the user's stored identity.",
        "Nested library tracks and playlist entries have independent coverage in account.json.",
        "Coverage (collected empty is distinct from not_collected):", *coverage,
        "Excluded: other users and shared catalogue beyond retained identity; audio, download and cover locations;",
        "NC-licensed material; secrets/verifiers and provider-vault contents; operational/transient records;",
        "request/webhook bodies and audit source_ip (not accepted for this format).",
        "Provider-vault metadata includes only existence and update time. Sessions contain only an eight-character id prefix.",
        "Not a live export: no delivery, expiry, rate limit, auth, step-up, erasure synchronization or storage is implemented.",
    ]
    files["README.txt"] = _utf8((line + "\n" for line in readme), budget)
    if sum(map(len, files.values())) > MAX_ARCHIVE_BYTES:
        raise ExportError("archive size limit exceeded")
    output = io.BytesIO()
    try:
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED, allowZip64=False) as archive:
            for name, content in sorted(files.items()):
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = 0o100600 << 16
                archive.writestr(info, content)
    except (ValueError, OverflowError, zipfile.LargeZipFile):
        raise ExportError("archive serialization failed") from None
    return output.getvalue()
