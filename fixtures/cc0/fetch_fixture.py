#!/usr/bin/env python3
"""Fetch a CC0 fixture audio file from R2 into a local cache, by key.

The FLAC files for the fixtures under `fixtures/cc0/` are too large for
git (~133 MB for the first album), so only each album's `manifest.json`
is committed. The audio lives in the R2 staging bucket under the same
prefix as the manifest's repo path: manifest
`fixtures/cc0/<album>/manifest.json` + track `file` `flac/01 - x.flac`
is R2 key `fixtures/cc0/<album>/flac/01 - x.flac`. See
docs/fixture-audio-licences.md for the licence rule every fixture here
must satisfy.

Usage:
  python3 fixtures/cc0/fetch_fixture.py --position 1
  python3 fixtures/cc0/fetch_fixture.py "flac/01 - 1 (jazz).flac"
  python3 fixtures/cc0/fetch_fixture.py --manifest fixtures/cc0/<album>/manifest.json --position 2

On success the cached file's absolute path is the only stdout line.
Tests should call this through the Rust (`nntune-test-fixtures`) or TS
(`e2e/helpers/cc0-fixture.ts`) wrappers rather than directly.

Exit codes (the wrappers rely on these):
  0   file is in the cache and verified (`bytes` + `sha256` match the
      manifest); path printed on stdout.
  77  SKIP: no R2 read access is configured in this environment. The
      reason is on stderr. Tests must skip, not fail.
  1   failure: bad arguments, manifest problems, network error, or a
      downloaded/cached file that does not match the manifest. A
      mismatch is never papered over by re-downloading silently more
      than once per run.

Configuration (environment):
  NNTUNE_FIXTURE_BASE_URL           Unsigned HTTPS base URL the key is
                                    appended to (e.g. a public bucket
                                    URL). Tried first when set.
  NNTUNE_FIXTURE_R2_ACCESS_KEY_ID   S3-compatible credentials for R2.
  NNTUNE_FIXTURE_R2_SECRET_ACCESS_KEY
  NNTUNE_FIXTURE_R2_ENDPOINT        e.g. https://<account>.r2.cloudflarestorage.com
  NNTUNE_FIXTURE_R2_ACCOUNT_ID      Alternative to _ENDPOINT: the
                                    endpoint is derived from it.
  NNTUNE_FIXTURE_R2_BUCKET          Default: nntunev2-audio-staging.
  NNTUNE_FIXTURE_CACHE_DIR          Default: <repo>/.fixture-cache
                                    (gitignored; never commit audio).

Standard library only, so any test environment with python3 can run it.
"""

import argparse
import hashlib
import hmac
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

EXIT_OK = 0
EXIT_FAIL = 1
EXIT_SKIP = 77

DEFAULT_MANIFEST = "fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json"
DEFAULT_BUCKET = "nntunev2-audio-staging"

ENV_BASE_URL = "NNTUNE_FIXTURE_BASE_URL"
ENV_ACCESS_KEY = "NNTUNE_FIXTURE_R2_ACCESS_KEY_ID"
ENV_SECRET_KEY = "NNTUNE_FIXTURE_R2_SECRET_ACCESS_KEY"
ENV_ENDPOINT = "NNTUNE_FIXTURE_R2_ENDPOINT"
ENV_ACCOUNT_ID = "NNTUNE_FIXTURE_R2_ACCOUNT_ID"
ENV_BUCKET = "NNTUNE_FIXTURE_R2_BUCKET"
ENV_CACHE_DIR = "NNTUNE_FIXTURE_CACHE_DIR"

CONFIG_ENV_VARS = (
    ENV_BASE_URL,
    ENV_ACCESS_KEY,
    ENV_SECRET_KEY,
    ENV_ENDPOINT,
    ENV_ACCOUNT_ID,
    ENV_BUCKET,
    ENV_CACHE_DIR,
)


class FetchError(Exception):
    """A real failure (exit 1): never a skip."""


class SkipFetch(Exception):
    """No transport configured (exit 77): tests skip with this reason."""


def repo_root() -> Path:
    # fixtures/cc0/fetch_fixture.py -> repo root is two levels up.
    return Path(__file__).resolve().parents[2]


def load_manifest(manifest_arg: str | None) -> tuple[dict, str]:
    """Return (manifest dict, r2 key prefix for its files)."""
    root = repo_root()
    rel = manifest_arg or DEFAULT_MANIFEST
    path = Path(rel)
    if not path.is_absolute():
        path = root / rel
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise FetchError(f"cannot read manifest {path}: {exc}") from exc
    if not isinstance(data, dict) or not isinstance(data.get("tracks"), list):
        raise FetchError(f"manifest {path} has no tracks list")
    try:
        prefix = path.parent.relative_to(root).as_posix()
    except ValueError as exc:
        raise FetchError(
            f"manifest {path} is outside the repo; its R2 prefix is undefined"
        ) from exc
    return data, prefix


def select_track(manifest: dict, file_arg: str | None, position: int | None) -> dict:
    tracks = manifest["tracks"]
    if position is not None:
        for track in tracks:
            if track.get("position") == position:
                return track
        raise FetchError(f"manifest has no track at position {position}")
    if file_arg is not None:
        for track in tracks:
            if track.get("file") == file_arg:
                return track
        raise FetchError(
            f"manifest does not list {file_arg!r}; refusing to fetch an "
            "unlisted key (only manifest files may be fetched)"
        )
    raise FetchError("give a manifest file path or --position")


def _is_safe_track_file(file) -> bool:
    """A track `file` must stay a relative sub-path of the cache key dir.

    Absolute paths, backslashes and empty/`.`/`..` segments would let a
    manifest write outside the fixture cache; refuse them outright.
    """
    if not isinstance(file, str) or not file or "\\" in file:
        return False
    if file.startswith("/"):
        return False
    return all(part not in ("", ".", "..") for part in file.split("/"))


_LOOPBACK_HOSTS = {"127.0.0.1", "localhost", "::1"}


def _require_secure_url(url: str, env_var: str) -> None:
    """Transports are HTTPS; plaintext HTTP is allowed only on loopback
    (local test servers). Anything else is a misconfiguration, not a skip."""
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme == "https":
        return
    if parsed.scheme == "http" and (parsed.hostname or "") in _LOOPBACK_HOSTS:
        return
    raise FetchError(
        f"{env_var} must be an https URL (http is allowed only for "
        f"loopback test servers), got scheme {parsed.scheme!r}"
    )


class _NoCrossHostAuthRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Strip signed headers when a redirect leaves the original host.

    urllib's default handler copies custom headers onto the redirect
    request, which would forward the SigV4 `Authorization` header (and
    the `x-amz-*` headers it covers) to a foreign host. Same-host
    redirects keep them; cross-host redirects go out unsigned.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        if new is None:
            return None
        if urllib.parse.urlparse(newurl).netloc != urllib.parse.urlparse(
            req.full_url
        ).netloc:
            for header in list(new.headers):
                if header.lower() == "authorization" or header.lower().startswith(
                    "x-amz-"
                ):
                    del new.headers[header]
            # add_header() on the original may have used unredirected_hdrs.
            for header in list(new.unredirected_hdrs):
                if header.lower() == "authorization" or header.lower().startswith(
                    "x-amz-"
                ):
                    del new.unredirected_hdrs[header]
        return new


def verify(path: Path, track: dict) -> bool:
    """True iff `path` exists and matches the manifest bytes + sha256."""
    try:
        if path.stat().st_size != track["bytes"]:
            return False
    except OSError:
        return False
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest() == track["sha256"]


def _signing_key(secret: str, date_stamp: str, region: str, service: str) -> bytes:
    def _hmac(key: bytes, msg: str) -> bytes:
        return hmac.new(key, msg.encode("utf-8"), hashlib.sha256).digest()

    k_date = _hmac(("AWS4" + secret).encode("utf-8"), date_stamp)
    k_region = _hmac(k_date, region)
    k_service = _hmac(k_region, service)
    return _hmac(k_service, "aws4_request")


def s3_get_url(
    endpoint: str, bucket: str, key: str, access_key: str, secret_key: str
) -> urllib.request.Request:
    """Build a SigV4-signed GET request (R2 is S3-compatible).

    R2 endpoints (`*.r2.cloudflarestorage.com`) use virtual-hosted style
    (`<bucket>.<account-endpoint>/<key>`), the addressing the AWS SDKs
    use with R2 by default; path-style requests against the account
    endpoint 404 in CI. Any other endpoint (e.g. a loopback test server)
    keeps path-style (`<endpoint>/<bucket>/<key>`).
    """
    region, service = "auto", "s3"
    parsed = urllib.parse.urlparse(endpoint)
    host = parsed.netloc
    if (parsed.hostname or "").endswith(".r2.cloudflarestorage.com"):
        host = f"{bucket}.{host}"
        base = f"{parsed.scheme}://{host}"
        canonical_uri = "/" + urllib.parse.quote(key, safe="/~")
    else:
        base = endpoint.rstrip("/")
        canonical_uri = "/" + urllib.parse.quote(f"{bucket}/{key}", safe="/~")
    now = datetime.now(timezone.utc)
    amz_date = now.strftime("%Y%m%dT%H%M%SZ")
    date_stamp = now.strftime("%Y%m%d")
    payload_hash = hashlib.sha256(b"").hexdigest()
    canonical_headers = f"host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
    signed_headers = "host;x-amz-content-sha256;x-amz-date"
    canonical_request = "\n".join(
        ["GET", canonical_uri, "", canonical_headers, signed_headers, payload_hash]
    )
    scope = f"{date_stamp}/{region}/{service}/aws4_request"
    string_to_sign = "\n".join(
        [
            "AWS4-HMAC-SHA256",
            amz_date,
            scope,
            hashlib.sha256(canonical_request.encode("utf-8")).hexdigest(),
        ]
    )
    signature = hmac.new(
        _signing_key(secret_key, date_stamp, region, service),
        string_to_sign.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    request = urllib.request.Request(
        base + canonical_uri, method="GET"
    )
    request.add_header("x-amz-content-sha256", payload_hash)
    request.add_header("x-amz-date", amz_date)
    request.add_header(
        "Authorization",
        f"AWS4-HMAC-SHA256 Credential={access_key}/{scope}, "
        f"SignedHeaders={signed_headers}, Signature={signature}",
    )
    return request


def build_request(key: str, env: dict) -> urllib.request.Request:
    base_url = env.get(ENV_BASE_URL, "").strip()
    if base_url:
        _require_secure_url(base_url, ENV_BASE_URL)
        url = base_url.rstrip("/") + "/" + urllib.parse.quote(key, safe="/~")
        return urllib.request.Request(url, method="GET")
    access_key = env.get(ENV_ACCESS_KEY, "").strip()
    secret_key = env.get(ENV_SECRET_KEY, "").strip()
    endpoint = env.get(ENV_ENDPOINT, "").strip()
    account_id = env.get(ENV_ACCOUNT_ID, "").strip()
    if not endpoint and account_id:
        endpoint = f"https://{account_id}.r2.cloudflarestorage.com"
    if access_key and secret_key and endpoint:
        _require_secure_url(endpoint, ENV_ENDPOINT)
        bucket = env.get(ENV_BUCKET, "").strip() or DEFAULT_BUCKET
        return s3_get_url(endpoint, bucket, key, access_key, secret_key)
    raise SkipFetch(
        "CC0 fixture fetch skipped: no R2 read access configured "
        f"(set {ENV_BASE_URL}, or {ENV_ACCESS_KEY} + {ENV_SECRET_KEY} + "
        f"{ENV_ENDPOINT}/{ENV_ACCOUNT_ID}); see docs/fixture-audio-licences.md"
    )


def _http_error_detail(exc: urllib.error.HTTPError) -> str:
    """Extract only the S3 `<Code>` from an HTTP error body.

    A bare "HTTP Error 404" cannot distinguish a missing key from a
    missing bucket; the S3 error code can. The free-text `<Message>`
    is deliberately not surfaced: remote message text does not belong
    in CI logs.
    """
    try:
        body = exc.read(65536).decode("utf-8", errors="replace")
    except Exception:
        return ""
    code = re.search(r"<Code>([^<]+)</Code>", body)
    if code:
        return f" ({code.group(1)})"
    return ""


def download(request: urllib.request.Request, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(f"{dest.name}.part-{os.getpid()}")
    opener = urllib.request.build_opener(_NoCrossHostAuthRedirectHandler())
    try:
        with opener.open(request, timeout=300) as response:
            with tmp.open("wb") as handle:
                for chunk in iter(lambda: response.read(1024 * 1024), b""):
                    handle.write(chunk)
        os.replace(tmp, dest)
    except urllib.error.HTTPError as exc:
        tmp.unlink(missing_ok=True)
        raise FetchError(
            f"download failed: {exc}{_http_error_detail(exc)}"
        ) from exc
    except (urllib.error.URLError, OSError) as exc:
        tmp.unlink(missing_ok=True)
        raise FetchError(f"download failed: {exc}") from exc


def fetch(
    file_arg: str | None,
    position: int | None,
    manifest_arg: str | None,
    env: dict,
) -> Path:
    manifest, prefix = load_manifest(manifest_arg)
    track = select_track(manifest, file_arg, position)
    for field in ("file", "bytes", "sha256"):
        if field not in track:
            raise FetchError(f"manifest track is missing {field!r}")
    if not _is_safe_track_file(track["file"]):
        raise FetchError(
            f"manifest track file {track['file']!r} is not a safe relative "
            "path; refusing to fetch outside the fixture cache"
        )
    key = f"{prefix}/{track['file']}"
    cache_dir = env.get(ENV_CACHE_DIR, "").strip()
    # Always absolute: callers run from other directories and the printed
    # path is the contract (see the module docstring).
    cache_root = (
        Path(cache_dir).resolve() if cache_dir else repo_root() / ".fixture-cache"
    )
    dest = cache_root / key
    if not dest.resolve().is_relative_to(cache_root.resolve()):
        raise FetchError(f"fixture key {key!r} escapes the cache directory")
    if verify(dest, track):
        return dest
    request = build_request(key, env)
    download(request, dest)
    if not verify(dest, track):
        dest.unlink(missing_ok=True)
        raise FetchError(
            f"downloaded {key} does not match the manifest "
            f"(bytes/sha256); deleted the cached copy"
        )
    return dest


def main(argv: list[str] | None = None, env: dict | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "file",
        nargs="?",
        help="track file exactly as listed in the manifest, e.g. 'flac/01 - 1 (jazz).flac'",
    )
    parser.add_argument("--position", type=int, help="track position in the manifest")
    parser.add_argument("--manifest", help=f"manifest path (default: {DEFAULT_MANIFEST})")
    args = parser.parse_args(argv)
    try:
        path = fetch(args.file, args.position, args.manifest, dict(os.environ) if env is None else env)
    except SkipFetch as exc:
        print(str(exc), file=sys.stderr)
        return EXIT_SKIP
    except FetchError as exc:
        print(f"fetch_fixture: {exc}", file=sys.stderr)
        return EXIT_FAIL
    print(path)
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())
