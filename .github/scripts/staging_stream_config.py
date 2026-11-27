"""Render the internal Stream Worker configuration for staging only."""

from datetime import date
import json
import os
import re
import sys
import tomllib
from pathlib import Path

FIXTURE_BUCKET = "nntunev2-private-fixtures-staging"
UUID_V4 = r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}"
HTTPS_ORIGIN = r"https://[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+"
PRIVATE_SETTINGS = {
    "S3_CONFIGURATION": UUID_V4,
    "S3_BUCKET": FIXTURE_BUCKET,
    "S3_ENDPOINT": HTTPS_ORIGIN,
    "S3_REGION": r"[a-z0-9][a-z0-9-]{0,62}",
    "ROOT_VERSION": UUID_V4,
}
API_SETTINGS = {"ENVIRONMENT": UUID_V4, "LIBRARY": UUID_V4, **{
    name: PRIVATE_SETTINGS[name] for name in ("S3_CONFIGURATION", "S3_BUCKET", "ROOT_VERSION")
}}
TRIAL_DATABASES = ("UPLOAD", "AUTH", "PUBLISH", "ENROLL")
ROOT = Path(__file__).resolve().parents[2]
BINDING_MANIFEST = ROOT / ".github/scripts/binding_manifest.toml"


def private_vars(env, settings):
    enabled = env.get("PRIVATE_PLAYBACK_STAGING_ENABLED", "")
    if enabled not in ("", "false", "true"):
        raise ValueError("PRIVATE_PLAYBACK_STAGING_ENABLED has an invalid value")
    journey = env.get("PRIVATE_MUSIC_STAGING_JOURNEY", "").lower()
    if journey not in ("", "false", "true"):
        raise ValueError("PRIVATE_MUSIC_STAGING_JOURNEY has an invalid value")
    if (enabled == "true") != (journey == "true"):
        raise ValueError("Arm private playback and the private journey together")
    if enabled != "true":
        return []
    lines = ['PRIVATE_PLAYBACK_ENABLED = "true"']
    for name, pattern in settings.items():
        key = f"PRIVATE_PLAYBACK_STAGING_{name}"
        value = env.get(key, "")
        if len(value) > 253 or not re.fullmatch(pattern, value):
            raise ValueError(f"{key} must name the dedicated staging setting")
        lines.append(f"PRIVATE_PLAYBACK_{name} = {json.dumps(value)}")
    return lines


def api_vars(env):
    lines = private_vars(env, API_SETTINGS)
    if lines and (env.get("STREAM_SERVICE_STAGING") != "nntune-stream-staging"
                  or env.get("STREAM_STAGING_DEPLOY_ENABLED") != "true"):
        raise ValueError("Private API playback requires the staging Stream service and deploy lane")
    if trial_upload(env):
        if not lines or not env.get("HYPERDRIVE_ID_MB"):
            raise ValueError("Private trial upload requires staged playback and the mirror binding")
        lines.append('PRIVATE_TRIAL_UPLOAD_ENABLED = "true"')
        for name in ("ENDPOINT", "REGION"):
            value = env.get(f"PRIVATE_PLAYBACK_STAGING_S3_{name}", "")
            if len(value) > 253 or not re.fullmatch(PRIVATE_SETTINGS[f"S3_{name}"], value):
                raise ValueError(f"PRIVATE_PLAYBACK_STAGING_S3_{name} is invalid")
            lines.append(f"PRIVATE_TRIAL_S3_{name} = {json.dumps(value)}")
    return lines


def trial_upload(env):
    value = env.get("PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED", "")
    if value not in ("", "false", "true"):
        raise ValueError("PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED has an invalid value")
    return value == "true"


def trial_db_bindings(env):
    if not trial_upload(env):
        return []
    manifest = tomllib.loads(BINDING_MANIFEST.read_text())
    existing = {
        binding.get(stage) for worker in manifest["worker"] for binding in worker.get("binding", [])
        if binding.get("type") == "hyperdrive" for stage in ("prod", "staging")
    }
    bindings = []
    for role in TRIAL_DATABASES:
        name = f"PRIVATE_TRIAL_{role}_DB"
        value = env.get(f"{name}_STAGING_ID", "")
        if not re.fullmatch(r"[0-9a-f]{32}", value) or value in existing:
            raise ValueError(f"{name}_STAGING_ID needs a distinct scoped binding")
        existing.add(value)
        bindings.extend(("", "[[env.staging.hyperdrive]]", f'binding = "{name}"', f'id = "{value}"'))
    return bindings


def trial_preflight(env):
    armed = bool(api_vars(env))
    trial_db_bindings(env)
    source = tomllib.loads((ROOT / "workers/stream/wrangler.toml").read_text())
    render(source, env)
    if armed:
        if (env.get("API_RP_ID_STAGING"), env.get("API_RP_ORIGIN_STAGING")) != (
            "staging.nntune.com", "https://staging.nntune.com",
        ):
            raise ValueError("Private trial requires the staging API RP ID and origin")
        manifest = tomllib.loads(BINDING_MANIFEST.read_text())
        api = next(worker for worker in manifest["worker"] if worker["name"] == "api")
        database = next(binding for binding in api["binding"] if binding["binding"] == "HYPERDRIVE")
        if env.get("HYPERDRIVE_ID_STAGING") != database["staging"]:
            raise ValueError("Private trial requires the pinned staging API database binding")
    return armed


def render(source, env):
    stage = source["env"]["staging"]
    bindings = {
        "name": "nntune-stream-staging",
        "durable_objects": {"bindings": [{"name": "NNTP_POOL", "class_name": "NntpPool"}]},
        "r2_buckets": [{"binding": "AUDIO_CACHE", "bucket_name": "nntunev2-audio-staging"}],
        "analytics_engine_datasets": [{"binding": "METER", "dataset": "nntune_meter"}],
        "services": [{"binding": "API", "service": "nntune-api-staging"}],
    }
    if any(stage.get(key) != value for key, value in bindings.items()):
        raise ValueError("Stream staging bindings differ from the checked-in contract")
    if source["main"] != "build/worker/shim.mjs" or source["migrations"] != [{
        "tag": "v1", "new_sqlite_classes": ["NntpPool"],
    }]:
        raise ValueError("Stream entrypoint or Durable Object migration changed")
    compatibility_date = source.get("compatibility_date")
    if not isinstance(compatibility_date, str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", compatibility_date):
        raise ValueError("Stream compatibility date must use YYYY-MM-DD")
    compatibility_date = date.fromisoformat(compatibility_date).isoformat()
    playback = private_vars(env, PRIVATE_SETTINGS)
    lines = [
        'name = "nntune-stream-staging"',
        f'main = {json.dumps(source["main"])}',
        f'compatibility_date = "{compatibility_date}"',
        "workers_dev = false", "preview_urls = false", "",
        '[[migrations]]', 'tag = "v1"', 'new_sqlite_classes = ["NntpPool"]', "",
        '[env.staging]', 'name = "nntune-stream-staging"',
        "workers_dev = false", "preview_urls = false", "",
        'compatibility_flags = ["enable_request_signal"]',
        '[env.staging.vars]',
        'ENVIRONMENT = "staging"',
        *(playback or ['PRIVATE_PLAYBACK_ENABLED = "false"']),
    ]
    if playback:
        observation = env.get("PRIVATE_PLAYBACK_STAGING_READ_OBSERVATION", "")
        if observation:
            if not re.fullmatch(r"[0-9a-f]{32}:[0-9a-f]{64}", observation):
                raise ValueError("Invalid staged private read observation selector")
            lines.append(f"PRIVATE_PLAYBACK_READ_OBSERVATION = {json.dumps(observation)}")
        lines.extend(("", "[env.staging.limits]", "subrequests = 132000"))
    for group, entries in bindings.items():
        if group == "name":
            continue
        path = "durable_objects.bindings" if group == "durable_objects" else group
        if group == "durable_objects":
            entries = entries["bindings"]
        for fields in entries:
            lines.extend(("", f"[[env.staging.{path}]]"))
            lines.extend(f"{key} = {json.dumps(value)}" for key, value in fields.items())
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    match sys.argv[1:]:
        case []:
            config = Path("wrangler.toml")
            config.write_text(render(tomllib.loads(config.read_text()), os.environ))
        case ["--api-vars"]:
            print("\n".join(api_vars(os.environ)))
        case ["--trial-db-bindings"]:
            print("\n".join(trial_db_bindings(os.environ)))
        case ["--trial-preflight"]:
            print(f"browser_playback={int(trial_preflight(os.environ))}")
            print(f"private_trial_upload={int(trial_upload(os.environ))}")
        case _:
            raise SystemExit(2)
