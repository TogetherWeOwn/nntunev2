#!/usr/bin/env python3
"""Retain, verify and render the complete web payload without rebuilding it."""

import argparse
import io
import json
import os
from pathlib import Path
import stat
import tarfile
import tempfile
import tomllib

from web_release_common import (
    MAX_BYTES, MAX_FILES, Refusal, cli_result, digest, read_json, release_version,
    require, safe_path, source_identity, validate_manifest, validate_plan,
)


def unlinked(path):
    path = Path(path).absolute()
    for part in (path, *path.parents):
        require(not part.is_symlink(), "linked-path")
    return path


def new_output(path):
    path = unlinked(path)
    require(not path.exists() and path.parent.is_dir(), "output-exists-or-parent-missing")
    return path


def tree(root):
    root = unlinked(root)
    require(root.is_dir(), "missing-web-root")
    result = {}
    total = 0
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in [*dirs, *files]:
            item = Path(directory) / name
            metadata = item.lstat()
            require(not stat.S_ISLNK(metadata.st_mode), "linked-member")
            if stat.S_ISDIR(metadata.st_mode):
                continue
            require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1, "nonregular-or-linked-member")
            name = safe_path(item.relative_to(root).as_posix())
            total += metadata.st_size
            require(total <= MAX_BYTES and len(result) < MAX_FILES, "payload-budget")
            result[name] = item.read_bytes()
    return result


def config(path):
    with open(path, "rb") as handle:
        value = tomllib.load(handle)
    require(set(value) <= {"name", "main", "compatibility_date", "compatibility_flags", "assets", "env", "previews"}, "unapproved-source-config")
    require(value.get("main") in (".svelte-kit/cloudflare/_worker.js", ".svelte-kit/cloudflare/_worker.js/index.js"), "source-entrypoint")
    require(value.get("assets") == {"binding": "ASSETS", "directory": ".svelte-kit/cloudflare"}, "source-assets")
    require(isinstance(value.get("compatibility_date"), str), "source-compatibility")
    require(isinstance(value.get("compatibility_flags", []), list), "source-compatibility")
    for name, environment in value.get("env", {}).items():
        allowed = {"name", "routes"} if name == "staging" else {"name"}
        require(name in ("staging", "production") and set(environment) <= allowed, "unapproved-source-environment")
        require(isinstance(environment.get("name"), str), "source-worker-name")
        for route in environment.get("routes", []):
            require(set(route) <= {"pattern", "zone_name"} and all(isinstance(v, str) for v in route.values()), "source-routes")
    return value


def compatibility(value):
    return {"date": value["compatibility_date"], "flags": value.get("compatibility_flags", [])}


def package(args):
    output = new_output(args.output)
    source = source_identity(args.repository, args.sha, args.run_id, args.attempt)
    plan = read_json(args.plan)
    validate_plan(plan, source)
    settings = config(args.config)
    payload = tree(args.root)
    if args.bundle_root:
        # Kit's loader imports a sibling cloudflare-tmp server outside the assets
        # directory. Bundle that graph ONCE in the producer, then retain every
        # emitted module/map next to index.js, isolated from static asset JS.
        bundle = tree(args.bundle_root)
        require("_worker.js" in bundle, "producer-entrypoint")
        payload = {name: data for name, data in payload.items() if name != "_worker.js" and not name.startswith("_worker.js/")}
        emitted = {}
        for name, data in sorted(bundle.items()):
            # The emitted main is renamed to index.js. A bundle that also ships
            # its own index.js would land on the same destination, and a plain
            # dict update would silently replace the real main with it.
            target = "_worker.js/" + ("index.js" if name == "_worker.js" else name)
            require(target not in emitted, "bundle-destination-collision")
            emitted[target] = data
        payload.update(emitted)
    require("_worker.js" not in payload or b"cloudflare-tmp" not in payload["_worker.js"], "producer-bundle-required")
    manifest = {
        "schema": "nntune-web-release-v1", "components": ["web"], "source": source,
        "compatibility": compatibility(settings),
        "build": {"publicApiBaseUrl": args.public_api_base_url or None}, "plan": plan,
        "files": [{"path": name, "size": len(data), "sha256": digest(data)} for name, data in sorted(payload.items())],
    }
    validate_manifest(manifest)
    members = {"manifest.json": json.dumps(manifest, ensure_ascii=False).encode(), **{"web/" + name: data for name, data in payload.items()}}
    with tempfile.NamedTemporaryFile(dir=output.parent, delete=False) as temporary:
        tmp = Path(temporary.name)
    try:
        with tarfile.open(tmp, "w:gz", format=tarfile.PAX_FORMAT) as archive:
            for name, data in sorted(members.items()):
                member = tarfile.TarInfo(name)
                member.size = len(data)
                member.mode = 0o644
                archive.addfile(member, io.BytesIO(data))
        # Exclusive destination: never overwrite another run's artifact.
        os.link(tmp, output)
    finally:
        tmp.unlink()
    return {"manifest": manifest}


def check_payload(manifest, payload, **expected):
    inventory = validate_manifest(manifest, **expected)
    require(set(inventory) == set(payload), "inventory-members")
    for name, data in payload.items():
        require(len(data) == inventory[name]["size"] and digest(data) == inventory[name]["sha256"], "inventory-content")


def archive_contents(path):
    files = {}
    seen = set()
    total = 0
    try:
        with tarfile.open(path, "r:gz") as archive:
            for member in archive:
                name = member.name.rstrip("/") if member.isdir() else member.name
                safe_path(name)
                require(name not in seen, "archive-duplicate")
                seen.add(name)
                require(len(seen) <= MAX_FILES * 2, "archive-count")
                if member.isdir():
                    require(name == "web" or name.startswith("web/"), "archive-directory")
                    continue
                require(member.isfile() and not member.issym() and not member.islnk(), "archive-type")
                require(name == "manifest.json" or name.startswith("web/"), "archive-root")
                total += member.size
                require(0 <= member.size <= MAX_BYTES and total <= MAX_BYTES + 8 * 1024 * 1024, "archive-budget")
                require(name != "manifest.json" or member.size <= 8 * 1024 * 1024, "manifest-budget")
                handle = archive.extractfile(member)
                require(handle is not None, "archive-member")
                files[name] = handle.read(member.size + 1)
                require(len(files[name]) == member.size, "archive-truncated")
    except tarfile.TarError as error:
        raise Refusal("archive-invalid") from error
    require("manifest.json" in files, "manifest-missing")
    # The same strict duplicate-key reader is used for local and archived JSON.
    manifest = json.loads(files.pop("manifest.json"), object_pairs_hook=unique_json)
    return manifest, {name.removeprefix("web/"): data for name, data in files.items()}


def unique_json(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "json-duplicate")
        result[key] = value
    return result


def verify(args):
    destination = unlinked(args.destination)
    require(not destination.exists() or (destination.is_dir() and not any(destination.iterdir())), "destination-not-empty")
    require(destination.parent.is_dir(), "destination-parent")
    source = source_identity(args.repository, args.sha, args.run_id, args.attempt)
    manifest, payload = archive_contents(args.archive)
    check_payload(manifest, payload, source=source, plan_id=args.plan_id, portable=args.require_portable)
    # Validate the entire archive before writing any member. Never tar.extract().
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        root = Path(temporary)
        for name, data in payload.items():
            target = root / "web" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        (root / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False))
        if destination.exists():
            destination.rmdir()  # Empty, checked above; no existing data is removed.
        os.rename(root, destination)
    return {"manifest": manifest}


def render(args):
    output = new_output(args.output)
    root = unlinked(args.verified_root)
    require(set(p.name for p in root.iterdir()) == {"manifest.json", "web"}, "verified-root-shape")
    manifest = read_json(root / "manifest.json")
    check_payload(manifest, tree(root / "web"), portable=args.environment == "production")
    settings = config(args.source_config)
    require(compatibility(settings) == manifest["compatibility"], "compatibility-mismatch")
    release_version(args.release_version, canary=args.environment == "staging")
    environment = settings.get("env", {}).get(args.environment, {})
    require(isinstance(environment.get("name"), str), "environment-name")
    worker = root / "web" / "_worker.js"
    if worker.is_dir():
        worker /= "index.js"
    require(worker.is_file(), "worker-entrypoint")
    quote = lambda value: json.dumps(value, ensure_ascii=False)
    lines = [
        f'name = {quote(settings["name"])}', f'main = {quote(str(worker))}',
        f'compatibility_date = {quote(manifest["compatibility"]["date"])}',
        f'compatibility_flags = {quote(manifest["compatibility"]["flags"])}',
        'find_additional_modules = true', f'base_dir = {quote(str(worker.parent))}',
        '[assets]', 'binding = "ASSETS"', f'directory = {quote(str(root / "web"))}',
        '[[rules]]', 'type = "ESModule"', 'globs = ["**/*.js", "**/*.mjs"]', 'fallthrough = true',
        '[[rules]]', 'type = "CompiledWasm"', 'globs = ["**/*.wasm"]', 'fallthrough = true',
        f'[env.{args.environment}]', f'name = {quote(environment["name"])}',
        f'[env.{args.environment}.vars]', f'DEPLOYED_RELEASE_VERSION = {quote(args.release_version)}',
    ]
    for route in environment.get("routes", []):
        lines.append(f'[[env.{args.environment}.routes]]')
        lines.extend(f'{key} = {quote(value)}' for key, value in route.items())
    with output.open("x", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
    return {"config": str(output)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    pack = commands.add_parser("package")
    pack.add_argument("--root", required=True)
    pack.add_argument("--bundle-root", help="Complete pinned Wrangler producer output, bundled before retention")
    pack.add_argument("--config", required=True)
    pack.add_argument("--plan", required=True)
    pack.add_argument("--public-api-base-url", required=True)
    pack.add_argument("--output", required=True)
    check = commands.add_parser("verify")
    check.add_argument("--archive", required=True)
    check.add_argument("--plan-id", required=True)
    check.add_argument("--destination", required=True)
    check.add_argument("--require-portable", action="store_true")
    for command in (pack, check):
        command.add_argument("--repository", required=True)
        command.add_argument("--sha", required=True)
        command.add_argument("--run-id", type=int, required=True)
        command.add_argument("--attempt", type=int, required=True)
    rendering = commands.add_parser("render-config")
    rendering.add_argument("--verified-root", required=True)
    rendering.add_argument("--source-config", required=True)
    rendering.add_argument("--environment", choices=("staging", "production"), required=True)
    rendering.add_argument("--release-version", required=True)
    rendering.add_argument("--output", required=True)
    args = parser.parse_args()
    cli_result(lambda: {"package": package, "verify": verify, "render-config": render}[args.command](args))


if __name__ == "__main__":
    main()
