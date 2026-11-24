"""Render the internal Stream Worker configuration for staging only."""

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


def private_vars(env, settings):
    enabled = env.get("PRIVATE_PLAYBACK_STAGING_ENABLED", "")
    if enabled not in ("", "false", "true"):
        raise ValueError("PRIVATE_PLAYBACK_STAGING_ENABLED has an invalid value")
    if (enabled == "true") != (env.get("PRIVATE_MUSIC_STAGING_JOURNEY", "").lower() == "true"):
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
    return lines


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
    playback = private_vars(env, PRIVATE_SETTINGS)
    lines = [
        'name = "nntune-stream-staging"',
        f'main = {json.dumps(source["main"])}',
        f'compatibility_date = {json.dumps(source["compatibility_date"])}',
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
        case ["--trial-preflight"]:
            api_vars(os.environ)
            source = tomllib.loads(Path("workers/stream/wrangler.toml").read_text())
            render(source, os.environ)
        case _:
            raise SystemExit(2)
