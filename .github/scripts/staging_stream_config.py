"""Render the internal Stream Worker configuration for staging only."""

import json
import os
import re
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
    enabled = env.get("PRIVATE_PLAYBACK_STAGING_ENABLED", "")
    if enabled not in ("", "false", "true"):
        raise ValueError("PRIVATE_PLAYBACK_STAGING_ENABLED has an invalid value")
    if (enabled == "true") != (env.get("PRIVATE_MUSIC_STAGING_JOURNEY", "").lower() == "true"):
        raise ValueError("Arm encrypted Stream and the private journey together")
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
        f'PRIVATE_PLAYBACK_ENABLED = {json.dumps("true" if enabled == "true" else "false")}',
    ]
    if enabled == "true":
        for name, pattern in PRIVATE_SETTINGS.items():
            key = f"PRIVATE_PLAYBACK_STAGING_{name}"
            value = env.get(key, "")
            if len(value) > 253 or not re.fullmatch(pattern, value):
                raise ValueError(f"{key} must name the dedicated staging setting")
            lines.append(f"PRIVATE_PLAYBACK_{name} = {json.dumps(value)}")
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
    config = Path("wrangler.toml")
    config.write_text(render(tomllib.loads(config.read_text()), os.environ))
