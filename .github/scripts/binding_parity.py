#!/usr/bin/env python3
"""Wrangler binding parity check: workers x environments x ops inventory.

Binding skew is the classic staging-green/prod-red cause: a worker declares
a binding in one environment (or one example file) and not the other, or a
wrangler config drifts away from the provisioned resource inventory. This
script makes that drift a CI failure instead of a deploy-time surprise.

Three facts are diffed, for staging AND prod:

  1. The manifest (``binding_manifest.toml`` next to this script) maps each
     worker config to the bindings it is required to declare, by binding
     name and type (R2, KV, Queues, Hyperdrive, Analytics Engine, Durable
     Objects, services, assets, D1). Every ``workers/*/wrangler.toml*`` and
     ``apps/*/wrangler.toml*`` file must be mapped, and every active binding
     found in a config must be claimed by the manifest.
  2. The wrangler configs themselves. Effective bindings per environment
     are the top-level bindings overridden by ``[env.staging]`` /
     ``[env.production]`` bindings of the same name and type. The binding
     (name, type) sets must be identical across staging and prod — any
     difference is reported as staging/prod skew.
  3. The provisioned inventory in ``docs/ops.md``. Where the manifest names
     the expected resource for an environment, that resource must appear in
     the ops inventory for that environment (the Environments matrix for
     resource names; the resource tables for KV/Hyperdrive ids), and a
     non-placeholder resource value in the wrangler config must equal it.

Template configs (``wrangler.toml.example``) may declare a binding in a
commented-out block and use ``<placeholder>`` resource values; both count
as "declared" there, because the template is the deploy contract and real
ids are filled in outside the repo. Commented-out declarations never trigger
the unexpected-binding check (a documented not-yet block is not a binding).
In a real ``wrangler.toml``, a commented-out or placeholder binding is drift.

Pre-existing resource-name drift whose fix is owned elsewhere (see the
reconciliation table in ``docs/ops.md``) is recorded in the manifest as
``[[known_drift]]`` entries. An entry only ever suppresses the exact
(worker, env, binding, actual, expected) mismatch it names; when the value
is fixed the entry goes stale and this check fails until it is removed.
Known-drift entries are rejected for id-typed bindings (KV, Hyperdrive).

This script checks binding *presence, shape and inventory membership* —
not binding values beyond the inventory names above, and not KV keys.
Drift output names the worker, environment and binding; for id-typed
bindings it never prints the actual or expected id, so no credential-like
value reaches a (public) CI log.

Usage:
  python3 .github/scripts/binding_parity.py [--root DIR]
      [--manifest PATH] [--ops PATH]
  Paths are resolved relative to --root (default: repo root, i.e. the
  current directory). Exit 0 = parity (known-drift warnings may print),
  1 = drift found, 2 = a manifest, inventory or config file could not
  be parsed (fail closed).
"""

import argparse
import re
import sys
import tomllib
from pathlib import Path

ENVS = ("staging", "prod")
ENV_ALIASES = {"staging": "staging", "prod": "prod", "production": "prod"}

# Binding type -> wrangler key holding the bound resource.
RESOURCE_KEY = {
    "r2": "bucket_name",
    "kv": "id",
    "queue_producer": "queue",
    "queue_consumer": "queue",
    "hyperdrive": "id",
    "analytics_engine": "dataset",
    "durable_object": "class_name",
    "service": "service",
    "assets": "directory",
    "d1": "database_name",
}

# Types whose resource value is an opaque id. Ids are compared but never
# printed: CI logs on this repo are public.
ID_TYPES = frozenset({"kv", "hyperdrive"})

CONFIG_GLOBS = (
    "workers/*/wrangler.toml",
    "workers/*/wrangler.toml.example",
    "apps/*/wrangler.toml",
    "apps/*/wrangler.toml.example",
)


class ParityError(Exception):
    """A manifest, inventory or config file could not be parsed."""


def _entry(table, item, type_name, binding_key):
    if not isinstance(item, dict):
        raise ParityError(f"{table}: entry is not a table ({type_name})")
    binding = item.get(binding_key)
    if not binding:
        raise ParityError(f"{table}: entry without {binding_key!r} ({type_name})")
    return {
        "type": type_name,
        "binding": binding,
        "resource": item.get(RESOURCE_KEY[type_name]),
        "declared": False,
    }


_KNOWN_TABLES = frozenset(
    {
        "r2_buckets", "kv_namespaces", "hyperdrive",
        "analytics_engine_datasets", "d1_databases", "services",
        "queues", "durable_objects", "assets",
    }
)


def extract_bindings(data):
    """Return {(type, binding): entry} for one wrangler table level."""
    if not isinstance(data, dict):
        raise ParityError("wrangler table level is not a table")
    out = {}

    def add(item):
        key = (item["type"], item["binding"])
        if key in out:
            raise ParityError(f"duplicate binding {item['binding']!r} ({item['type']})")
        out[key] = item

    for item in data.get("r2_buckets", []):
        add(_entry("r2_buckets", item, "r2", "binding"))
    for item in data.get("kv_namespaces", []):
        add(_entry("kv_namespaces", item, "kv", "binding"))
    for item in data.get("hyperdrive", []):
        add(_entry("hyperdrive", item, "hyperdrive", "binding"))
    for item in data.get("analytics_engine_datasets", []):
        add(_entry("analytics_engine_datasets", item, "analytics_engine", "binding"))
    for item in data.get("d1_databases", []):
        add(_entry("d1_databases", item, "d1", "binding"))
    for item in data.get("services", []):
        add(_entry("services", item, "service", "binding"))
    queues = data.get("queues", {})
    if not isinstance(queues, dict):
        raise ParityError("queues: not a table")
    for item in queues.get("producers", []):
        add(_entry("queues.producers", item, "queue_producer", "binding"))
    for item in queues.get("consumers", []):
        if not isinstance(item, dict):
            raise ParityError("queues.consumers: entry is not a table")
        queue = item.get("queue")
        if not queue:
            raise ParityError("queues.consumers: entry without 'queue'")
        add(
            {
                "type": "queue_consumer",
                "binding": f"consumer:{queue}",
                "resource": queue,
                "declared": False,
            }
        )
    durable_objects = data.get("durable_objects", {})
    if not isinstance(durable_objects, dict):
        raise ParityError("durable_objects: not a table")
    for item in durable_objects.get("bindings", []):
        add(_entry("durable_objects.bindings", item, "durable_object", "name"))
    assets = data.get("assets")
    if isinstance(assets, dict) and assets.get("binding"):
        add(_entry("assets", assets, "assets", "binding"))
    # Fail closed on binding-shaped tables this script does not model
    # (a new Cloudflare binding kind): surface them as unknown types so
    # the unexpected-binding check flags them instead of ignoring them.
    for key, value in data.items():
        if key in _KNOWN_TABLES:
            continue
        candidates = value if isinstance(value, list) else [value]
        for item in candidates:
            if isinstance(item, dict) and item.get("binding"):
                add(
                    {
                        "type": f"unknown:{key}",
                        "binding": item["binding"],
                        "resource": None,
                        "declared": False,
                    }
                )
    return out


# Commented-out declarations in *.example templates: a small line scanner.
# Only known table headers open a block; only string key = "value" lines for
# binding/resource keys are read inside one. Prose comments never match.
_COMMENT_TABLES = {
    "r2_buckets": ("r2", "binding"),
    "kv_namespaces": ("kv", "binding"),
    "hyperdrive": ("hyperdrive", "binding"),
    "analytics_engine_datasets": ("analytics_engine", "binding"),
    "d1_databases": ("d1", "binding"),
    "services": ("service", "binding"),
    "queues.producers": ("queue_producer", "binding"),
    "queues.consumers": ("queue_consumer", None),
    "durable_objects.bindings": ("durable_object", "name"),
    "assets": ("assets", "binding"),
}
_COMMENT_HEADER = re.compile(r"^\[\[?([A-Za-z0-9_.]+)\]?\]\s*$")
_COMMENT_KEY = re.compile(
    r"^(binding|name|bucket_name|id|queue|dataset|class_name|service|database_name|directory)"
    r'\s*=\s*"([^"]*)"\s*$'
)


def extract_commented_bindings(text):
    """Return {(type, binding): entry} from commented-out example blocks."""
    out = {}
    current = None  # (type_name, binding_key, fields)

    def flush():
        nonlocal current
        if current is None:
            return
        type_name, binding_key, fields = current
        current = None
        if type_name == "queue_consumer":
            queue = fields.get("queue")
            if not queue:
                return
            binding, resource = f"consumer:{queue}", queue
        else:
            binding = fields.get(binding_key)
            if not binding:
                return
            resource = fields.get(RESOURCE_KEY[type_name])
        out[(type_name, binding)] = {
            "type": type_name,
            "binding": binding,
            "resource": resource,
            "declared": True,
        }

    for raw_line in text.split("\n"):
        line = raw_line.strip()
        if not line.startswith("#"):
            if line:  # real TOML ends any commented block being scanned
                flush()
            continue
        body = line[1:].strip()
        header = _COMMENT_HEADER.match(body)
        if header:
            flush()
            known = _COMMENT_TABLES.get(header.group(1))
            if known:
                current = (known[0], known[1], {})
            continue
        if current is None:
            continue
        key = _COMMENT_KEY.match(body)
        if key:
            current[2][key.group(1)] = key.group(2)
    flush()
    return out


def parse_config(path):
    """Return ({env: effective bindings}, [unknown env names])."""
    try:
        text = path.read_text(encoding="utf-8")
        data = tomllib.loads(text)
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise ParityError(f"{path}: cannot parse wrangler config ({exc})") from exc
    try:
        top = extract_bindings(data)
        if path.name.endswith(".example"):
            for key, item in extract_commented_bindings(text).items():
                top.setdefault(key, item)
        per_env = {}
        unknown = []
        for raw_name, env_data in data.get("env", {}).items():
            normalized = ENV_ALIASES.get(raw_name)
            if normalized is None:
                unknown.append(raw_name)
                continue
            per_env.setdefault(normalized, {}).update(extract_bindings(env_data))
    except ParityError as exc:
        raise ParityError(f"{path}: {exc}") from exc
    except (AttributeError, TypeError) as exc:
        raise ParityError(f"{path}: malformed wrangler table shape ({exc})") from exc
    effective = {env: {**top, **per_env.get(env, {})} for env in ENVS}
    return effective, unknown


def parse_ops_inventory(text, source="docs/ops.md"):
    """Return ({env: resource-name set}, {every backticked token}).

    Resource names come from the Environments matrix columns; the full
    token set backs the id-typed inventory check (KV/Hyperdrive ids are
    listed in their own tables, not the matrix).
    """
    match = re.search(r"^## Environments matrix\s*$", text, re.MULTILINE)
    if not match:
        raise ParityError(f"{source}: no '## Environments matrix' section")
    section = text[match.end() :]
    next_heading = re.search(r"^## ", section, re.MULTILINE)
    if next_heading:
        section = section[: next_heading.start()]
    inventory = {env: set() for env in ENVS}
    for line in section.split("\n"):
        cells = line.split("|")
        if len(cells) < 5:  # | concern | staging | prod |
            continue
        for env, cell in zip(ENVS, cells[2:4]):
            inventory[env].update(re.findall(r"`([^`]+)`", cell))
    if not any(inventory.values()):
        raise ParityError(f"{source}: Environments matrix has no resources")
    return inventory, set(re.findall(r"`([^`]+)`", text))


def load_manifest(path):
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise ParityError(f"{path}: cannot parse manifest ({exc})") from exc
    extra = set(data) - {"worker", "known_drift"}
    if extra:
        raise ParityError(f"{path}: manifest has unknown top-level keys {sorted(extra)}")
    workers = []
    seen_workers = set()
    for worker in data.get("worker", []):
        if not isinstance(worker, dict):
            raise ParityError(f"{path}: [[worker]] is not a table")
        extra = set(worker) - {"name", "config", "binding"}
        if extra:
            raise ParityError(f"{path}: [[worker]] has unknown keys {sorted(extra)}")
        name, config = worker.get("name"), worker.get("config")
        if not name or not config:
            raise ParityError(f"{path}: [[worker]] needs name and config")
        if name in seen_workers:
            raise ParityError(f"{path}: duplicate worker {name!r}")
        seen_workers.add(name)
        bindings = []
        seen_bindings = set()
        for binding in worker.get("binding", []):
            if not isinstance(binding, dict):
                raise ParityError(f"{path}: worker {name}: binding is not a table")
            extra = set(binding) - {"binding", "type", *ENVS}
            if extra:
                raise ParityError(
                    f"{path}: worker {name}: binding has unknown keys {sorted(extra)}"
                )
            type_name, binding_name = binding.get("type"), binding.get("binding")
            if type_name not in RESOURCE_KEY or not binding_name:
                raise ParityError(
                    f"{path}: worker {name}: binding needs a known type and a name"
                )
            key = (type_name, binding_name)
            if key in seen_bindings:
                raise ParityError(f"{path}: worker {name}: duplicate binding {key}")
            seen_bindings.add(key)
            bindings.append(
                {
                    "type": type_name,
                    "binding": binding_name,
                    "expected": {env: binding.get(env) for env in ENVS},
                }
            )
        workers.append({"name": name, "config": config, "bindings": bindings})
    if not workers:
        raise ParityError(f"{path}: manifest maps no workers")
    by_worker = {w["name"]: w for w in workers}
    known_drift = []
    for drift in data.get("known_drift", []):
        if not isinstance(drift, dict):
            raise ParityError(f"{path}: [[known_drift]] is not a table")
        extra = set(drift) - {"worker", "env", "binding", "actual", "expected", "reason"}
        if extra:
            raise ParityError(f"{path}: [[known_drift]] has unknown keys {sorted(extra)}")
        worker = by_worker.get(drift.get("worker", ""))
        if worker is None:
            raise ParityError(f"{path}: known_drift names an unmapped worker")
        entries = [b for b in worker["bindings"] if b["binding"] == drift.get("binding")]
        if not entries:
            raise ParityError(f"{path}: known_drift names an unmapped binding")
        if entries[0]["type"] in ID_TYPES:
            raise ParityError(f"{path}: known_drift is not allowed for id-typed bindings")
        if drift.get("env") not in ENVS or not drift.get("reason"):
            raise ParityError(f"{path}: known_drift needs env and a reason")
        if drift.get("expected") != entries[0]["expected"][drift["env"]]:
            raise ParityError(
                f"{path}: known_drift expected value disagrees with the binding map"
            )
        known_drift.append(drift)
    return workers, known_drift


def is_placeholder(value):
    return value is None or "<" in value or ">" in value


def _fmt(worker, env, binding, type_name, message):
    return f"worker={worker} env={env} binding={binding} ({type_name}): {message}"


def check(root, manifest_path, ops_path):
    """Return (drifts, warnings, worker_count). Raises ParityError."""
    root = Path(root)
    workers, known_drift = load_manifest(manifest_path)
    try:
        ops_text = ops_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise ParityError(f"{ops_path}: cannot read inventory ({exc})") from exc
    inventory, all_tokens = parse_ops_inventory(ops_text, str(ops_path))

    drifts, warnings = [], []
    used_known_drift = set()

    mapped = {w["config"] for w in workers}
    on_disk = set()
    for pattern in CONFIG_GLOBS:
        on_disk.update(p.relative_to(root).as_posix() for p in root.glob(pattern))
    for config in sorted(on_disk - mapped):
        drifts.append(
            _fmt(config, "-", "-", "-", "wrangler config is not mapped in the manifest")
        )

    for worker in workers:
        name, config_rel = worker["name"], worker["config"]
        config_path = root / config_rel
        if not config_path.is_file():
            drifts.append(_fmt(name, "-", "-", "-", f"mapped config {config_rel} is missing"))
            continue
        is_example = config_rel.endswith(".example")
        effective, unknown_envs = parse_config(config_path)
        for raw_env in unknown_envs:
            drifts.append(
                _fmt(name, raw_env, "-", "-", "unrecognized environment section")
            )
        required = {(b["type"], b["binding"]): b for b in worker["bindings"]}

        for binding in worker["bindings"]:
            for env in ENVS:
                expected = binding["expected"][env]
                if expected is None:
                    continue
                if binding["type"] in ID_TYPES:
                    if expected not in all_tokens:
                        drifts.append(
                            _fmt(
                                name, env, binding["binding"], binding["type"],
                                "manifest inventory id is absent from the ops inventory",
                            )
                        )
                elif expected not in inventory[env]:
                    drifts.append(
                        _fmt(
                            name, env, binding["binding"], binding["type"],
                            f"expected resource {expected!r} is absent from the "
                            f"ops inventory for {env}",
                        )
                    )

        for env in ENVS:
            present = effective[env]
            for key, binding in sorted(required.items()):
                type_name, binding_name = key
                actual = present.get(key)
                if actual is None:
                    drifts.append(
                        _fmt(name, env, binding_name, type_name, "missing required binding")
                    )
                    continue
                resource = actual["resource"]
                if is_placeholder(resource):
                    if not is_example:
                        drifts.append(
                            _fmt(
                                name, env, binding_name, type_name,
                                "resource is a placeholder in a real wrangler.toml",
                            )
                        )
                    continue
                expected = binding["expected"][env]
                if expected is None or resource == expected:
                    continue
                drift_key = (name, env, binding_name, resource, expected)
                known = next(
                    (
                        d
                        for d in known_drift
                        if (d["worker"], d["env"], d["binding"], d.get("actual"), d.get("expected"))
                        == drift_key
                    ),
                    None,
                )
                if known is not None:
                    used_known_drift.add(drift_key)
                    warnings.append(
                        "known drift "
                        + _fmt(
                            name, env, binding_name, type_name,
                            f"resource {resource!r} != ops inventory {expected!r} "
                            f"({known['reason']})",
                        )
                    )
                elif type_name in ID_TYPES:
                    drifts.append(
                        _fmt(
                            name, env, binding_name, type_name,
                            "resource id does not match the ops inventory",
                        )
                    )
                else:
                    drifts.append(
                        _fmt(
                            name, env, binding_name, type_name,
                            f"resource {resource!r} != ops inventory {expected!r}",
                        )
                    )
            for key, item in sorted(present.items()):
                if key not in required and not item["declared"]:
                    drifts.append(
                        _fmt(
                            name, env, item["binding"], item["type"],
                            "active binding is not in the manifest",
                        )
                    )

        staging_keys = set(effective["staging"])
        prod_keys = set(effective["prod"])
        for type_name, binding_name in sorted(staging_keys - prod_keys):
            drifts.append(
                _fmt(
                    name, "prod", binding_name, type_name,
                    "present in staging but missing in prod (staging/prod skew)",
                )
            )
        for type_name, binding_name in sorted(prod_keys - staging_keys):
            drifts.append(
                _fmt(
                    name, "staging", binding_name, type_name,
                    "present in prod but missing in staging (staging/prod skew)",
                )
            )

    for drift in known_drift:
        drift_key = (
            drift["worker"], drift["env"], drift["binding"],
            drift.get("actual"), drift.get("expected"),
        )
        if drift_key not in used_known_drift:
            drifts.append(
                _fmt(
                    drift["worker"], drift["env"], drift["binding"], "-",
                    "stale known-drift entry (the drift it names is gone); remove it",
                )
            )

    return drifts, warnings, len(workers)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--root", default=".", help="repo root (default: cwd)")
    parser.add_argument(
        "--manifest", default=".github/scripts/binding_manifest.toml",
        help="manifest path, relative to --root unless absolute",
    )
    parser.add_argument(
        "--ops", default="docs/ops.md",
        help="ops inventory path, relative to --root unless absolute",
    )
    args = parser.parse_args(argv)
    root = Path(args.root)
    manifest = Path(args.manifest)
    ops = Path(args.ops)
    if not manifest.is_absolute():
        manifest = root / manifest
    if not ops.is_absolute():
        ops = root / ops
    try:
        drifts, warnings, worker_count = check(root, manifest, ops)
    except ParityError as exc:
        print(f"::error::binding-parity: {exc}")
        return 2
    for warning in warnings:
        print(f"::warning::binding-parity: {warning}")
    for drift in drifts:
        print(f"::error::binding-parity: {drift}")
    print(
        f"binding-parity: checked {worker_count} workers, "
        f"{len(drifts)} drifts, {len(warnings)} known-drift warnings"
    )
    return 1 if drifts else 0


if __name__ == "__main__":
    sys.exit(main())
