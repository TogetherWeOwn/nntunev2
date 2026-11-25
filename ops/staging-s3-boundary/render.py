#!/usr/bin/env python3
"""Render one credential's Caddy policy from its access ID and exact keys."""

import argparse
from pathlib import Path
import re

BUCKET = "nntunev2-private-fixtures-staging"
UUID = r"[0-9a-f]{12}4[0-9a-f]{3}[89ab][0-9a-f]{15}"
KEY = re.compile(rf"(?:media/{UUID}/{UUID}|media-digests/{UUID}/{UUID}/{UUID}/00000000)")


def render(access_id, keys):
    if not re.fullmatch(r"[A-Za-z0-9]{1,128}", access_id):
        raise ValueError("invalid access key ID")
    if not 1 <= len(keys) <= 2 or len(set(keys)) != len(keys):
        raise ValueError("provide one or two distinct fixture keys")
    if any(len(key) > 128 or not KEY.fullmatch(key) for key in keys):
        raise ValueError("invalid fixture key")
    # These alphabets have no Caddyfile, CEL or regexp metacharacters.
    paths = "|".join(f"/{BUCKET}/{key}" for key in sorted(keys))
    template = Path(__file__).with_name("Caddyfile.in").read_text()
    return template.replace("@PATHS@", paths).replace("@ACCESS@", access_id)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("access_id")
    parser.add_argument("key", nargs="+")
    args = parser.parse_args()
    try:
        print(render(args.access_id, args.key), end="")
    except ValueError as error:
        parser.error(str(error))
