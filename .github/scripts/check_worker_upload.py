#!/usr/bin/env python3
"""Credential-free dry-run of the staging Worker upload path.

Run AFTER the real api/mb/ingest/stream release builds exist
(workers/<name>/build). For each Worker it:

1. extracts the exact "Generate staging wrangler.toml" run script and the
   exact upload command from .github/workflows/deploy.yml,
2. runs the generation script in a throwaway fixture tree with the real
   prebuilt bundle copied in,
3. asserts the generated config has no [build] command (root or env.staging),
4. runs the pinned real Wrangler CLI with the workflow's upload argv plus
   --dry-run --outdir, with poisoned cargo/worker-build on PATH, and asserts
   nothing tried to rebuild,
5. checks query-string redaction in the staging API's serialized upload
   metadata in both mirror scenarios,
6. runs a negative control: a config WITH the custom build command must
   invoke the poisoned worker-build and be rejected.

No secrets, no Cloudflare API calls. Usage:

    GITHUB_SHA=<40 hex> python3 -I .github/scripts/check_worker_upload.py \
        --wrangler path/to/node_modules/wrangler/bin/wrangler.js
"""

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
from email import policy
from email.parser import BytesParser
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".github/workflows/deploy.yml"
MANIFEST = ".github/scripts/binding_manifest.toml"
BUILD_CMD = "worker-build --release -- --locked"
TIMEOUT = 600


class Failure(Exception):
    pass


def check(cond, message):
    if not cond:
        raise Failure(message)


# --- workflow extraction -------------------------------------------------

def job_text(text, name):
    m = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:|\Z)", text, re.M | re.S)
    check(m, f"missing job {name} in {WORKFLOW}")
    return m.group(1)


def step_text(job, step_name):
    for step in re.findall(r"^      - .*?(?=^      - |\Z)", job, re.M | re.S):
        if re.match(rf"      - name: {re.escape(step_name)}\n", step):
            return step
    raise Failure(f"missing step {step_name!r}")


def run_script(step):
    """Return the dedented `run: |` block of a step."""
    lines = step.split("\n")
    start = lines.index("        run: |") + 1
    body = []
    for line in lines[start:]:
        if line.strip() and not line.startswith(" " * 10):
            break
        body.append(line[10:])
    return "\n".join(body).rstrip("\n") + "\n"


def workflow_commands(text, worker, sha):
    job = job_text(text, f"staging-{worker}")
    gen = run_script(step_text(job, "Generate staging wrangler.toml"))
    deploy = step_text(job, f"Deploy {worker} to staging")
    m = re.search(r"^          command: (deploy .*)$", deploy, re.M)
    check(m, f"missing upload command for {worker}")
    command = m.group(1).replace("${{ github.sha }}", sha)
    check("${{" not in command, f"unsubstituted expression in upload command: {command}")
    argv = shlex.split(command)
    check("--no-build" not in argv, f"{worker}: upload still passes unsupported --no-build: {command}")
    check(argv[:3] == ["deploy", "--env", "staging"], f"{worker}: unexpected upload argv {argv}")
    check(f"BUILD_SHA:{sha}" in argv, f"{worker}: upload argv lacks BUILD_SHA for the commit")
    return gen, argv


# --- process helpers -----------------------------------------------------

def run(argv, *, cwd, env, label):
    """Run, echo full stdout/stderr, return CompletedProcess (no raise)."""
    print(f"::group::{label}", flush=True)
    print("$ " + " ".join(shlex.quote(a) for a in argv), flush=True)
    proc = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=TIMEOUT)
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    print(f"[exit {proc.returncode}]\n::endgroup::", flush=True)
    return proc


def make_tools(base, node):
    """poison/ (cargo, worker-build refuse and leave a marker) and tools/."""
    poison, tools = base / "poison", base / "tools"
    poison.mkdir()
    tools.mkdir()
    for name in ("cargo", "worker-build"):
        path = poison / name
        path.write_text(
            '#!/bin/sh\necho "$0 $*" >> "$POISON_MARKER"\n'
            'echo "poisoned $0: a second build is forbidden" >&2\nexit 97\n'
        )
        path.chmod(0o755)
    (tools / "python3").symlink_to(sys.executable)
    (tools / "node").symlink_to(node)
    return f"{poison}:{tools}:/usr/bin:/bin"


def base_env(home, path, marker):
    return {
        "PATH": path,
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_CACHE_HOME": str(home / "cache"),
        "WRANGLER_SEND_METRICS": "false",
        "WRANGLER_LOG_PATH": str(home / "logs"),
        "POISON_MARKER": str(marker),
        "CI": "1",
        "NO_COLOR": "1",
    }


# --- wrangler pin --------------------------------------------------------

def check_wrangler(wrangler, expected, env):
    check(wrangler.is_file() and wrangler.name == "wrangler.js" and wrangler.parent.name == "bin",
          f"--wrangler must be <pkg>/bin/wrangler.js, got {wrangler}")
    pkg = json.loads((wrangler.parent.parent / "package.json").read_text())
    check(pkg.get("name") == "wrangler", f"{wrangler}: package is {pkg.get('name')!r}, not wrangler")
    check(pkg.get("version") == expected,
          f"wrangler package version {pkg.get('version')} != deploy.yml WRANGLER_VERSION {expected}")
    proc = run(["node", str(wrangler), "--version"], cwd=ROOT, env=env, label="wrangler --version")
    check(proc.returncode == 0, "wrangler --version failed")
    check(re.search(rf"(?<![\w.]){re.escape(expected)}(?![\w.])", proc.stdout + proc.stderr),
          f"wrangler --version does not report {expected}")


# --- scenarios -----------------------------------------------------------

def fixture(base, worker, label):
    """Copy manifest and the real prebuilt bundle into a fresh tree."""
    tree = base / label
    wdir = tree / "workers" / worker
    src = ROOT / "workers" / worker / "build"
    check((src / "worker/shim.mjs").is_file(), f"{src}/worker/shim.mjs missing: run the real {worker} release build first")
    # The entry shim imports ../index.js, whose JS/Wasm dependencies live
    # beside worker/. Preserve the whole build output, not just the shim.
    shutil.copytree(src, wdir / "build")
    (tree / ".github/scripts").mkdir(parents=True)
    shutil.copy(ROOT / MANIFEST, tree / MANIFEST)
    if worker == "stream":
        shutil.copy(ROOT / "workers/stream/wrangler.toml", wdir / "wrangler.toml")
        script = ".github/scripts/staging_stream_config.py"
        shutil.copy(ROOT / script, tree / script)
    return tree, wdir


def generate(worker, gen_script, wdir, env, summary_env, label):
    script = wdir.parent.parent / f"generate-{worker}.sh"
    script.write_text(gen_script)
    proc = run(["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", str(script)],
               cwd=wdir, env={**env, **summary_env}, label=f"{label}: generate wrangler.toml")
    check(proc.returncode == 0, f"{label}: config generation failed")
    return tomllib.loads((wdir / "wrangler.toml").read_text())


def assert_no_build(config, label):
    check("build" not in config, f"{label}: generated root config still has [build]")
    check("build" not in config.get("env", {}).get("staging", {}),
          f"{label}: generated env.staging still has [build]")


def dry_run(wrangler, argv, wdir, env, label, *, upload_body=None):
    out = wdir.parent.parent / f"out-{wdir.name}-{label}".replace(" ", "_")
    args = ["node", str(wrangler), *argv, "--dry-run", "--outdir", str(out)]
    if upload_body is not None:
        args.extend(["--outfile", str(upload_body)])
    return out, run(args, cwd=wdir, env=env, label=f"{label}: wrangler {' '.join(argv)} --dry-run")


def assert_upload_redaction(upload_body, label):
    # Wrangler --outfile writes the multipart body without its Content-Type
    # header. Recover the boundary from the first line; don't search the bundle
    # for JSON text, which could also appear inside a JS or Wasm module.
    check(upload_body.is_file(), f"{label}: Wrangler did not write the upload body")
    body = upload_body.read_bytes()
    boundary = re.match(rb"--([A-Za-z0-9_-]{1,70})\r\n", body)
    check(boundary, f"{label}: missing or invalid multipart boundary in Wrangler upload body")
    message = BytesParser(policy=policy.default).parsebytes(
        b'Content-Type: multipart/form-data; boundary="' + boundary[1] + b'"\r\n\r\n' + body
    )
    check(message.is_multipart() and not message.defects, f"{label}: malformed Wrangler upload body")
    parts = [part for part in message.iter_parts()
             if part.get_content_disposition() == "form-data"
             and part.get_param("name", header="Content-Disposition") == "metadata"]
    check(len(parts) == 1, f"{label}: expected exactly one Wrangler upload metadata part")
    try:
        metadata = json.loads(parts[0].get_payload(decode=True))
    except (ValueError, TypeError):
        raise Failure(f"{label}: invalid Wrangler upload metadata JSON") from None
    observability = metadata.get("observability") if isinstance(metadata, dict) else None
    check(isinstance(observability, dict) and observability.get("redact_query_string") is True,
          f"{label}: Wrangler upload metadata must set observability.redact_query_string to true")


def scenario(wrangler, wf_text, sha, manifest, base, path, worker, mirror):
    label = f"{worker}" + ("" if worker != "api" else (" mirror-present" if mirror else " mirror-absent"))
    if worker == "stream":
        label += " private-enabled" if mirror else " private-disabled"
    tag = label.replace(" ", "_")
    gen_script, argv = workflow_commands(wf_text, worker, sha)
    workers = {w["name"]: {b["binding"]: b for b in w["binding"]} for w in manifest["worker"]}
    hd = workers["api"]["HYPERDRIVE"]["staging"]
    summary_env = {"GITHUB_STEP_SUMMARY": str(base / f"summary-{tag}.md")}
    Path(summary_env["GITHUB_STEP_SUMMARY"]).touch()
    if worker == "api":
        summary_env.update(HYPERDRIVE_ID_STAGING=hd, API_RP_ID_STAGING="staging.nntune.com",
                           API_RP_ORIGIN_STAGING="https://staging.nntune.com")
        if mirror:
            summary_env["HYPERDRIVE_ID_MB"] = workers["api"]["MB_DB"]["staging"]
    elif worker == "mb":
        summary_env["HYPERDRIVE_ID_MB"] = workers["mb"]["MB_DB"]["staging"]
    elif worker == "ingest":
        summary_env["HYPERDRIVE_ID_STAGING"] = workers["ingest"]["LIBRARY_DB"]["staging"]
    elif mirror:
        summary_env.update(
            PRIVATE_MUSIC_STAGING_JOURNEY="true", PRIVATE_PLAYBACK_STAGING_ENABLED="true",
            PRIVATE_PLAYBACK_STAGING_S3_CONFIGURATION="11111111-1111-4111-8111-111111111111",
            PRIVATE_PLAYBACK_STAGING_S3_BUCKET="nntunev2-private-fixtures-staging",
            PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT="https://objects.example.test",
            PRIVATE_PLAYBACK_STAGING_S3_REGION="test-region",
            PRIVATE_PLAYBACK_STAGING_ROOT_VERSION="22222222-2222-4222-8222-222222222222",
        )

    home = base / f"home-{tag}"
    home.mkdir()
    marker = base / f"marker-{tag}"
    env = base_env(home, path, marker)
    tree, wdir = fixture(base, worker, f"tree-{tag}")
    config = generate(worker, gen_script, wdir, env, summary_env, label)
    assert_no_build(config, label)
    if worker == "stream":
        stage = config["env"].get("staging", {})
        check(set(config["env"]) == {"staging"} and config["name"] == stage.get("name") == "nntune-stream-staging",
              f"{label}: upload must target only the staging Stream Worker")
        check(all(scope.get("workers_dev") is False and scope.get("preview_urls") is False and "routes" not in scope
                  for scope in (config, stage)), f"{label}: Stream must be internal")
        check(stage["vars"].get("PRIVATE_PLAYBACK_ENABLED") == ("true" if mirror else "false"),
              f"{label}: private playback does not match the requested mode")
    if worker == "api":
        summary = Path(summary_env["GITHUB_STEP_SUMMARY"]).read_text()
        staging = config["env"]["staging"]
        check(staging.get("observability", {}).get("redact_query_string") is True,
              f"{label}: staging API invocation query strings must be redacted")
        bound = [h["binding"] for h in staging["hyperdrive"]]
        if mirror:
            check(bound == ["HYPERDRIVE", "MB_DB"] and "mirror: configured" in summary,
                  f"{label}: expected MB_DB bound, got {bound}")
        else:
            check(bound == ["HYPERDRIVE"] and "mirror: absent" in summary,
                  f"{label}: expected no MB_DB, got {bound}")

    # Keep the serialized body separate from the bundle output, and only in
    # the temporary credential-free API fixture.
    upload_body = tree / "upload.multipart" if worker == "api" else None
    out, proc = dry_run(wrangler, argv, wdir, env, label, upload_body=upload_body)
    check(proc.returncode == 0, f"{label}: wrangler dry-run failed (exit {proc.returncode})")
    if marker.exists():
        raise Failure(f"{label}: poisoned build tool was invoked:\n{marker.read_text()}")
    check("Running custom build" not in proc.stdout + proc.stderr, f"{label}: wrangler ran a custom build")
    check(out.is_dir() and any(out.iterdir()), f"{label}: dry-run wrote nothing to {out}")

    if upload_body is not None:
        assert_upload_redaction(upload_body, label)
        print(f"{label}: Wrangler upload metadata redacts query strings", flush=True)

    if worker == "api" and mirror:
        negative_control(wrangler, argv, wdir, env, marker, label)


def negative_control(wrangler, argv, wdir, env, marker, label):
    """Same upload with the custom build command restored must hit the poison."""
    label = f"{label} negative-control"
    with open(wdir / "wrangler.toml", "a") as f:
        f.write(f'\n[build]\ncommand = "{BUILD_CMD}"\n')
    assert not marker.exists()
    _, proc = dry_run(wrangler, argv, wdir, env, label)
    check(marker.exists() and "worker-build" in marker.read_text(),
          f"{label}: custom build did not invoke the poisoned worker-build "
          "(poison is not exercised, so the no-rebuild proof is void)")
    check(proc.returncode != 0, f"{label}: wrangler accepted a failing custom build")
    marker.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--wrangler", required=True, type=Path, help="path to wrangler/bin/wrangler.js")
    args = parser.parse_args()

    sha = os.environ.get("GITHUB_SHA", "")
    check(re.fullmatch(r"[0-9a-f]{40}", sha), "GITHUB_SHA must be set to a 40-hex commit sha")
    node = shutil.which("node")
    check(node, "node not found on PATH")
    wf_text = (ROOT / WORKFLOW).read_text()
    m = re.search(r"^  WRANGLER_VERSION: (\S+)$", wf_text, re.M)
    check(m, "WRANGLER_VERSION not found in deploy.yml")
    manifest = tomllib.loads((ROOT / MANIFEST).read_text())

    with tempfile.TemporaryDirectory(prefix="worker-upload-") as tmp:
        base = Path(tmp)
        path = make_tools(base, node)
        home = base / "home-version"
        home.mkdir()
        wrangler = args.wrangler.resolve()
        check_wrangler(wrangler, m.group(1), base_env(home, path, base / "marker-version"))
        for worker, mirror in (("api", True), ("api", False), ("mb", False), ("ingest", False),
                               ("stream", False), ("stream", True)):
            scenario(wrangler, wf_text, sha, manifest, base, path, worker, mirror)
    print("check_worker_upload: ok (api with/without mirror, mb, ingest, stream with/without private playback)")


if __name__ == "__main__":
    try:
        main()
    except Failure as err:
        print(f"check_worker_upload: FAIL: {err}", file=sys.stderr)
        sys.exit(1)
