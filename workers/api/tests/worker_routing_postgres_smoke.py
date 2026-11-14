#!/usr/bin/env python3
"""API Worker fetch-routing smoke against the CI PostgreSQL service.

Runs the prebuilt bundle (workers/api/build/worker/shim.mjs) under the locked
`wrangler dev --local` with a local Hyperdrive binding to a run-scoped database
created in (and dropped from) the service, migrated from migrations/manifest.toml.
Proves Worker -> PostgreSQL via POST /v1/auth/passkey/login/start (reads the
inserted auth_challenges row back), plus non-/rest and /rest routing controls.
/rest is asserted against the inert main baseline; the two observed tables
must not gain rows. This is NOT Subsonic activation proof. Missing prerequisites fail.

Env: NNTUNE_SMOKE_PG_PASSWORD (required), _HOST (loopback, 127.0.0.1), _PORT (5432),
_USER (postgres), _ADMIN_DB (nntune_ci). Needs node and a host psql client.

    python3 -I workers/api/tests/worker_routing_postgres_smoke.py [--wrangler PATH]
"""
import argparse, base64, importlib.util, json, os, re, secrets, shutil, socket, subprocess
import sys, tempfile, time, tomllib, urllib.error, urllib.parse, urllib.request
from pathlib import Path

spec = importlib.util.spec_from_file_location("process_group", Path(__file__).resolve().with_name("process_group.py"))
process_group = importlib.util.module_from_spec(spec)
spec.loader.exec_module(process_group)

ROOT = Path(__file__).resolve().parents[3]
BUNDLE = ROOT / "workers/api/build/worker/shim.mjs"
MANIFEST = ROOT / "migrations/manifest.toml"
WRANGLER_VERSION = "4.153.0"
RP_ID = "example.test"
ORIGIN = f"https://{RP_ID}"
IDENT = re.compile(r"[A-Za-z0-9_]+")
NOT_FOUND = "worker-json"


class Failure(Exception):
    pass


def check(cond, message):
    if not cond:
        raise Failure(message)


def b64(raw):
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def pg_settings(env):
    check("NNTUNE_SMOKE_PG_PASSWORD" in env, "NNTUNE_SMOKE_PG_PASSWORD is required")
    s = {"host": env.get("NNTUNE_SMOKE_PG_HOST", "127.0.0.1"),
         "port": env.get("NNTUNE_SMOKE_PG_PORT", "5432"),
         "user": env.get("NNTUNE_SMOKE_PG_USER", "postgres"),
         "password": env["NNTUNE_SMOKE_PG_PASSWORD"],
         "admin": env.get("NNTUNE_SMOKE_PG_ADMIN_DB", "nntune_ci")}
    check(s["host"] in ("127.0.0.1", "localhost"), "database host must be loopback")
    check(s["port"].isdigit() and 0 < int(s["port"]) < 65536, "bad port")
    check(IDENT.fullmatch(s["user"]) and IDENT.fullmatch(s["admin"]), "unsafe database identifier")
    return s


def conn_string(s, db):
    pw = urllib.parse.quote(s["password"], safe="")
    return f"postgresql://{s['user']}:{pw}@{s['host']}:{s['port']}/{db}?sslmode=disable"


def wrangler_toml(conn, sha):
    check(re.fullmatch(r"[0-9a-f]{40}", sha), "build sha must be 40 lowercase hex")
    check('"' not in conn and "\\" not in conn and '"' not in str(BUNDLE), "unsupported characters")
    return f'''name = "nntune-api-routing-smoke"
main = "{BUNDLE}"
compatibility_date = "2026-10-01"

[vars]
RP_ID = "{RP_ID}"
RP_ORIGIN = "{ORIGIN}"
BUILD_SHA = "{sha}"
# Local stand-in only: the real Hyperdrive cache gate is not exercised here.
HYPERDRIVE_CACHING_DISABLED = "1"

[[hyperdrive]]
binding = "HYPERDRIVE"
id = "00000000000000000000000000000000"
localConnectionString = "{conn}"
'''


def dev_vars():
    return (f"SESSION_SECRET_BASE64={b64(secrets.token_bytes(32))}\n"
            f"VAULT_KEK_BASE64={b64(secrets.token_bytes(32))}\nVAULT_KEK_VERSION=1\n")


def app_migrations():
    with open(MANIFEST, "rb") as fh:
        files = [m["file"] for m in tomllib.load(fh)["migration"] if m["target"] == "app"]
    check(files, "no app-target migrations in manifest")
    for f in files:
        check(Path(f).name == f, "invalid migration filename")
    return files


def classify(status, ctype, body):
    if status == 404 and ctype.split(";")[0].strip().lower() == "text/plain" and body.strip() == "Not Found":
        return "subsonic-text-404"
    try:
        doc = json.loads(body)
    except ValueError:
        return "other"
    if isinstance(doc, dict):
        if status == 200 and isinstance(doc.get("subsonic-response"), dict):
            return "subsonic-envelope"
        if status == 404 and doc.get("code") == "not_found":
            return NOT_FOUND
    return "other"


def check_ping(status, ctype, body):
    check(classify(status, ctype, body) == "subsonic-envelope",
          f"/rest ping: expected HTTP 200 envelope: {status} {body[:100]!r}")
    check(json.loads(body)["subsonic-response"].get("status") == "failed",
          "unauthenticated /rest/ping must fail")


def psql(s, db, sql=None, stdin=None, tuples=False, **variables):
    args = [shutil.which("psql") or "psql", "-h", s["host"], "-p", s["port"], "-U", s["user"],
            "-X", "-q", "-v", "ON_ERROR_STOP=1", "-d", db]
    args += ["-A", "-t"] if tuples else []
    for k, v in variables.items():
        args += ["-v", f"{k}={v}"]
    args += ["-c", sql] if sql is not None else []
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "PGPASSWORD": s["password"],
           "PGOPTIONS": "-c search_path=public,pg_catalog", "PGCONNECT_TIMEOUT": "10"}
    p = subprocess.run(args, input=stdin, capture_output=True, text=True, env=env, timeout=300)
    check(p.returncode == 0, f"psql failed: {p.stderr.strip()[:300]}")
    return p.stdout.strip()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())


def http(method, url, body=None, headers=None):
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with OPENER.open(req, timeout=30) as r:
            return r.status, r.headers.get("content-type", ""), r.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("content-type", ""), e.read().decode(errors="replace")


def find_wrangler(explicit):
    found = [Path(explicit)] if explicit else sorted(
        (ROOT / "apps/web/node_modules/.pnpm").glob("wrangler@*/node_modules/wrangler/bin/wrangler.js"))
    check(len(found) == 1 and found[0].is_file(), f"expected exactly one wrangler.js, found {len(found)}")
    pkg = found[0].resolve().parent.parent / "package.json"
    check(json.loads(pkg.read_text()).get("version") == WRANGLER_VERSION, f"wrangler must be {WRANGLER_VERSION}")
    return found[0].resolve()


def prerequisites(args):
    check(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted",
          "run only on a GitHub-hosted Actions runner with its disposable PostgreSQL service")
    for name in ("DATABASE_URL", "NNTUNE_TEST_APP_DATABASE_URL", "NNTUNE_TEST_MIRROR_DATABASE_URL"):
        check(name not in os.environ, f"{name} is set: external database targets are not accepted")
    check(shutil.which("node") and shutil.which("psql"), "node and psql must be on PATH")
    s = pg_settings(os.environ)
    wrangler = find_wrangler(args.wrangler or os.environ.get("NNTUNE_WRANGLER"))
    check(BUNDLE.is_file() and (BUNDLE.parent.parent / "index_bg.wasm").is_file(),
          "missing prebuilt Worker bundle (worker-build --release -- --locked in workers/api)")
    sha = os.environ.get("GITHUB_SHA", "")
    return s, wrangler, sha if re.fullmatch(r"[0-9a-f]{40}", sha) else "0123456789abcdef0123456789abcdef01234567"


def smoke(base, s, db):
    q = lambda sql, **v: psql(s, db, stdin=sql, tuples=True, **v)
    status, _, body = http("GET", base + "/healthz")
    check(status == 200 and json.loads(body).get("ok") is True, f"healthz: {status} {body[:160]}")
    for path in ("/api/no-such-route-canary", "/restx", "/restx/ping.view", "/rest-v2/ping.view",
                 "/REST/ping.view", "/v1/rest/ping.view"):
        st, ct, b = http("GET", base + path)
        check(classify(st, ct, b) == NOT_FOUND, f"non-rest control {path}: {st} {ct} {b[:100]!r}")
    check(q("SELECT count(*) FROM auth_challenges") == "0", "fresh database must have no challenges")
    st, _, b = http("POST", base + "/v1/auth/passkey/login/start",
                    json.dumps({"email": f"smoke-{secrets.token_hex(6)}@{RP_ID}"}).encode(),
                    {"content-type": "application/json", "origin": ORIGIN})
    check(st == 200, f"login/start: {st} {b[:160]}")
    cid = json.loads(b).get("challenge")
    check(isinstance(cid, str) and re.fullmatch(r"[A-Za-z0-9_-]{8,256}", cid), "no usable challenge id")
    row = q("SELECT purpose || '|' || rp_id || '|' || coalesce(user_id, 'null') "
            "FROM auth_challenges WHERE id = :'cid'", cid=cid)
    check(row == f"login|{RP_ID}|null", f"challenge row not in PostgreSQL: {row!r}")
    check(int(q("SELECT count(*) FROM auth_rate_limit_slots")) >= 1, "no auth_rate_limit_slots row written")
    print("ok: Worker -> PostgreSQL via auth_challenges + auth_rate_limit_slots", flush=True)
    snap = q("SELECT (SELECT count(*) FROM auth_challenges) || '/' || (SELECT count(*) FROM auth_rate_limit_slots)")
    for path, want in (("/rest/bogus-operation", "subsonic-text-404"), ("/rest/", "subsonic-text-404"),
                       ("/rest", "subsonic-text-404"),
                       ("/rest/getOpenSubsonicExtensions?f=json", "subsonic-envelope"),
                       ("/rest/ping.view?f=json&apiKey=synthetic-invalid", "subsonic-envelope")):
        st, ct, b = http("GET", base + path)
        check(classify(st, ct, b) == want, f"/rest control {path}: want {want}: {st} {ct} {b[:100]!r}")
        if path.startswith("/rest/ping.view?"):
            check_ping(st, ct, b)
    after = q("SELECT (SELECT count(*) FROM auth_challenges) || '/' || (SELECT count(*) FROM auth_rate_limit_slots)")
    check(after == snap, "current main /rest changed observed table counts")
    print("ok: /rest claimed by the Subsonic adapter; observed table counts unchanged", flush=True)


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--wrangler")
    ap.add_argument("--postgres-service", action="store_true", help="accepted; service mode is the only mode")
    try:
        s, wrangler, sha = prerequisites(ap.parse_args(argv))
    except Failure as err:
        print(f"FAIL: {err}", file=sys.stderr)
        return 1
    db = f"nntune_smoke_{secrets.token_hex(6)}"
    tmp = Path(tempfile.mkdtemp(prefix="nntune-routing-smoke-"))
    proc = None
    created = False
    failed = False
    redact = lambda t: t.replace(conn_string(s, db), "<db-url>").replace(s["password"], "<redacted>")
    try:
        psql(s, s["admin"], "SELECT 1")
        psql(s, s["admin"], f"CREATE DATABASE {db}")
        created = True
        for name in app_migrations():
            print(f"migrate app: {name}", flush=True)
            psql(s, db, stdin=(ROOT / "migrations" / name).read_text())
        (tmp / "wrangler.toml").write_text(wrangler_toml(conn_string(s, db), sha))
        (tmp / ".dev.vars").write_text(dev_vars())
        (tmp / "home").mkdir()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(tmp / "home"),
               "WRANGLER_SEND_METRICS": "false", "CI": "1", "NO_COLOR": "1"}
        log = open(tmp / "wrangler.log", "wb")
        proc = subprocess.Popen(
            [shutil.which("node"), str(wrangler), "dev", "--config", str(tmp / "wrangler.toml"), "--local",
             "--ip", "127.0.0.1", "--port", str(port), "--persist-to", str(tmp / "state"),
             "--show-interactive-dev-session=false"],
            cwd=str(tmp), env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        base = f"http://127.0.0.1:{port}"
        tail = lambda: redact("\n".join((tmp / "wrangler.log").read_text(errors="replace").splitlines()[-40:]))
        deadline = time.monotonic() + 180
        while True:
            check(proc.poll() is None, "wrangler exited before readiness:\n" + tail())
            check(time.monotonic() < deadline, "worker not ready in time:\n" + tail())
            try:
                st, _, b = http("GET", base + "/healthz")
            except OSError:
                time.sleep(1)
                continue
            check(st == 200, f"/healthz answered {st} {b[:160]!r}\n" + tail())
            break
        smoke(base, s, db)
    except Failure as err:
        print(f"FAIL: {redact(str(err))}", file=sys.stderr)
        failed = True
    finally:
        if proc:
            try:
                process_group.shutdown_process_group(proc)
            except (OSError, ValueError) as err:
                print(f"FAIL: process cleanup: {redact(str(err))}", file=sys.stderr)
                failed = True
        if created:
            try:
                psql(s, s["admin"], f"DROP DATABASE IF EXISTS {db} WITH (FORCE)")
            except Failure as err:
                print(f"FAIL: cleanup could not drop {db}: {redact(str(err))}", file=sys.stderr)
                failed = True
        try:
            shutil.rmtree(tmp)
        except OSError as err:
            print(f"FAIL: temporary directory cleanup: {redact(str(err))}", file=sys.stderr)
            failed = True
    if failed:
        return 1
    print("worker routing postgres smoke: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
