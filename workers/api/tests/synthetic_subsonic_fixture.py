"""Synthetic, disposable API-key Subsonic fixture for the routing smoke.

GitHub Actions only: it is seeded into the run-scoped database that the smoke
creates and drops, never into an operator, staging or production database.
Every secret is generated per run from `secrets`, is not a real credential,
and is never printed. Nothing here activates, mounts or enables a route: the
fixture is data plus a scenario table, and the smoke asserts /rest stays inert.

Activation verification usage: in a later mount PR run the disposable Actions
harness with --mirror-fixture and replace the inert assertions with checks of
actual status, content type, envelope and catalogue contents. For the draft mount,
a valid key and intact mirror should allow ping and member catalogue reads;
a wrong or revoked key should fail authentication. Repeat ping with MB_DB absent
to exercise catalogue failure after successful auth. Unknown routes, HLS and
unsupported operations have terminal responses before auth; mutation endpoints
must refuse after auth, and stream/download must not return audio. Confirm exact
responses against the activating revision, not this inert baseline.

Compare business-state tables before and after refused operations, but do not
require the credential rows to be unchanged: successful authentication updates
last_used_at and may update attempt state. Assert those effects separately. The
fixture's billing states do not establish an entitlement gate: catalogue
membership is derived from library rows. These scenarios are inputs, not proof
of activation, staging, or production Hyperdrive behaviour.
"""
import base64, hashlib, hmac, json, re, secrets
from collections import namedtuple

SAFE = re.compile(r"[A-Za-z0-9_.@-]+")
PEPPER_VERSION = 1
REL, REC, ART, OTHER_REL, OTHER_REC, OTHER_ART = (
    "00000000-0000-4000-8000-0000000000%02d" % n for n in (1, 2, 3, 4, 5, 6))
# name: (subscription status or None, credential state, user state)
USERS = {"active": ("active", "live", "ok"), "grace": ("past_due", "live", "ok"),
         "terminal": ("canceled", "live", "ok"), "nonmember": (None, "live", "ok"),
         "revoked": ("active", "revoked", "ok"), "deleting": ("active", "live", "deleting")}
# Tables whose row text must not change while the route is inert.
OBSERVED = ("users", "subscriptions", "subsonic_credentials", "library", "playlists", "genres",
            "usage_ledger", "sessions", "auth_challenges")
Fixture = namedtuple("Fixture", "pepper keys")
Scenario = namedtuple("Scenario", "name kind who path mutates inert active")
# who: fixture user whose key is sent, or a literal credential shape.
SCENARIOS = (
    Scenario("ok-ping", "verify", "active", "ping.view", False, "no auth, failed", "ok"),
    Scenario("wrong-key", "denial", "bad", "ping.view", False, "failed", "failed code 44"),
    Scenario("mixed-u-and-key", "denial", "active", "ping.view&u=x", False, "failed", "failed code 43"),
    Scenario("revoked-key", "denial", "revoked", "ping.view", False, "failed", "failed code 44"),
    Scenario("deleting-user", "denial", "deleting", "ping.view", False, "failed", "failed code 44"),
    Scenario("visible-album", "verify", "active", f"getAlbum.view&id={REL}", False, "failed", "member album"),
    Scenario("other-album", "denial", "active", f"getAlbum.view&id={OTHER_REL}", False, "failed", "not found"),
    Scenario("nonmember", "denial", "nonmember", "getArtists.view", False, "failed", "UNVERIFIED: library-derived"),
    Scenario("terminal-sub", "terminal", "terminal", "getArtists.view", False, "failed", "UNVERIFIED: billing gate?"),
    Scenario("grace-sub", "terminal", "grace", "getArtists.view", False, "failed", "UNVERIFIED: billing gate?"),
    Scenario("scrobble", "mutation", "active", f"scrobble.view&id={REC}", True, "failed", "failed code 0"),
    Scenario("star", "mutation", "active", f"star.view&id={REC}", True, "failed", "failed code 0"),
    Scenario("playlist", "mutation", "terminal", "createPlaylist.view&name=ci", True, "failed", "UNVERIFIED"),
    Scenario("stream", "stream", "active", f"stream.view&id={REC}", False, "no audio", "no audio"),
    Scenario("stream-denied", "stream", "nonmember", f"stream.view&id={REC}", False, "no audio", "UNVERIFIED"),
    Scenario("download-terminal", "stream", "terminal", f"download.view&id={REC}", False, "no audio", "UNVERIFIED"),
    Scenario("malformed-key", "failure", "malformed", "ping.view", False, "failed", "failed, no 5xx"),
    Scenario("oversize-key", "failure", "oversize", "ping.view", False, "failed", "failed, no 5xx"),
    Scenario("unknown-op", "terminal", "active", "noSuchOperation.view", False, "text 404", "text 404"),
    Scenario("hls", "terminal", "active", "hls.m3u8", False, "gone", "gone"),
)


def b64(raw):
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def build():
    pepper = secrets.token_bytes(32)
    keys = {name: "ci-synthetic-" + secrets.token_hex(16) for name in USERS}
    keys.update(bad="ci-synthetic-" + secrets.token_hex(16), malformed="not a key!/%00",
                oversize="ci-synthetic-" + "a" * 2048)
    return Fixture(pepper, keys)


def pepper_secret(fx):
    return json.dumps({"current_version": PEPPER_VERSION,
                       "keys": {str(PEPPER_VERSION): b64(fx.pepper)}})


def key_hmac(fx, name):
    return hmac.new(fx.pepper, fx.keys[name].encode(), hashlib.sha256).hexdigest()


def seed_sql(fx, mirror=False):
    """Idempotence is not needed: the database is fresh for every run."""
    out = []
    for name, (sub, cred, state) in USERS.items():
        uid = f"ci-fixture-{name}"
        assert SAFE.fullmatch(uid) and SAFE.fullmatch(key_hmac(fx, name)), "unsafe fixture value"
        dele = "now()" if state == "deleting" else "NULL"
        rev = "now()" if cred == "revoked" else "NULL"
        out.append(f"INSERT INTO users (id, email, created_at, deleting_at) VALUES "
                   f"('{uid}', '{uid}@example.test', 1700000000, {dele});")
        out.append("INSERT INTO subsonic_credentials (user_id, label, subsonic_username, api_key_hmac, "
                   f"pepper_version, revoked_at) VALUES ('{uid}', 'ci synthetic', 'ci_{name}', "
                   f"'{key_hmac(fx, name)}', {PEPPER_VERSION}, {rev});")
        if sub:
            out.append("INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, "
                       "stripe_subscription_item_id, stripe_price_id, plan_id, status) VALUES "
                       f"('{uid}', 'sub_ci_{name}', 'cus_ci_{name}', 'si_ci_{name}', 'price_ci', 'plus', '{sub}');")
    # Shared catalogue (NULL owner), one member-owned row, one other-owner row for isolation checks.
    for key, owner, release, recording in (("shared", "NULL", REL, REC),
                                           ("active", "'ci-fixture-active'", REL, REC),
                                           ("other", "'ci-fixture-nonmember'", OTHER_REL, OTHER_REC)):
        out.append("INSERT INTO library (idempotency_key, job_id, user_id, release_mbid, recording_mbids) VALUES "
                   f"('ci-fixture-{key}', 'ci-job-{key}', {owner}, '{release}', ARRAY['{recording}']);")
    if mirror:  # needs mirror-target migrations; see mirror_files()
        out.append(f"INSERT INTO mb_artist (mbid, name, sort_name) VALUES ('{ART}', 'CI Synthetic Artist', 'Artist, CI');"
                   f"INSERT INTO mb_release (mbid, artist_mbid, title, year) VALUES ('{REL}', '{ART}', 'CI Synthetic Release', 2000);"
                   f"INSERT INTO mb_recording (mbid, release_mbid, title, track_no, duration_ms) VALUES ('{REC}', '{REL}', 'CI Synthetic Track', 1, 1000);"
                   f"INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no) VALUES ('{REL}', '{REC}', 1, 1);"
                   f"INSERT INTO mb_artist (mbid, name, sort_name) VALUES ('{OTHER_ART}', 'Other Artist', 'Other Artist');"
                   f"INSERT INTO mb_release (mbid, artist_mbid, title) VALUES ('{OTHER_REL}', '{OTHER_ART}', 'Other Release');"
                   f"INSERT INTO mb_recording (mbid, release_mbid, title) VALUES ('{OTHER_REC}', '{OTHER_REL}', 'Other Track');"
                   f"INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no) VALUES ('{OTHER_REL}', '{OTHER_REC}', 1, 1);")
    return "BEGIN;\n" + "\n".join(out) + "\nCOMMIT;\n"


def mirror_files():
    # Same ordered set CI's mirror job applies (manifest target = "mirror").
    return ("0001_mb_mirror.sql", "0014_mb_import_resume_dlq.sql", "0016_mb_track.sql",
            "0017_mb_replication_packet_ledger.sql", "0030_mb_mirror_w6b.sql")


def snapshot_sql():
    parts = [f"coalesce((SELECT md5(string_agg(t::text, '|' ORDER BY t::text)) FROM {t} t), '-')" for t in OBSERVED]
    return "SELECT " + " || '/' || ".join(parts)


def request_path(fx, s):
    key = fx.keys[s.who]
    quoted = re.sub(r"[^A-Za-z0-9_.~-]", lambda m: "%%%02X" % ord(m.group()), key)
    op, _, extra = s.path.partition("&")
    return f"/rest/{op}?f=json&apiKey={quoted}" + (f"&{extra}" if extra else "")
