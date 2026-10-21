#!/usr/bin/env python3
"""Staging/prod KV config parity check against the versioned schema.

Loads the staging and prod key sets and validates both against
``config/schema.json``. Exits 1 on drift (an invalid schema, a missing
required key, a type mismatch, an undeclared key, or a staging/prod
key-set asymmetry) and 0 when both sides validate with identical key
sets. Per-environment VALUES may differ (hostnames, TTLs); only keys and
types must match, so changing a value never fails this check but adding
a key without updating the schema always does.

Exit codes: 0 = parity holds; 1 = drift found (including an invalid
schema); 2 = unusable input (missing file, bad JSON, or a duplicate
JSON member name in any input: the check itself never ran).

Usage:
  python3 .github/scripts/config_parity_check.py \
    --schema config/schema.json \
    --staging config/staging.json \
    --prod config/prod.json
"""

import argparse
import json
import sys

U64_MAX = 2**64 - 1

TYPES = ("u64", "bool", "string")

# Cap on member-name characters shown in a duplicate diagnostic; the
# name is escaped before display and member values are never shown.
MEMBER_NAME_DISPLAY_MAX = 32


class DuplicateMemberError(ValueError):
    """A JSON object repeated a member name (compared after decoding)."""

    def __init__(self, member_name):
        # Keep the exception text free of the name itself; load_json
        # renders it escaped and length-bounded via display_member_name.
        super().__init__("duplicate JSON member name")
        self.member_name = member_name


def object_without_duplicates(pairs):
    """object_pairs_hook: build a dict, rejecting repeated member names.

    The decoder calls this for every JSON object at any depth (including
    objects nested in arrays), with member names already decoded, so an
    escape-encoded repeat (e.g. ``\\u005f`` for ``_``) counts as a
    duplicate too. Ordinary json.load would silently keep the last value.
    """
    obj = {}
    for name, value in pairs:
        if name in obj:
            raise DuplicateMemberError(name)
        obj[name] = value
    return obj


def display_member_name(name):
    """Render a member name for a diagnostic: escaped and bounded."""
    shown = json.dumps(name[:MEMBER_NAME_DISPLAY_MAX], ensure_ascii=True)
    if len(name) > MEMBER_NAME_DISPLAY_MAX:
        shown += "..."
    return shown


def fail_input(message):
    """Report an unusable input (exit 2): the check itself never ran."""
    print(f"config-parity: input error: {message}", file=sys.stderr)
    return 2


def load_json(path, label):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return json.load(fh, object_pairs_hook=object_without_duplicates), None
    except DuplicateMemberError as exc:
        return None, (
            f"{label} file contains a duplicate JSON member name "
            f"{display_member_name(exc.member_name)}: {path}"
        )
    except FileNotFoundError:
        return None, f"{label} file not found: {path}"
    except json.JSONDecodeError as exc:
        return None, f"{label} file is not valid JSON: {path} ({exc})"
    except OSError as exc:
        return None, f"{label} file unreadable: {path} ({exc})"


def check_value(key, declared_type, value):
    """Return an error string when value does not match declared_type."""
    if declared_type == "u64":
        if isinstance(value, bool) or not isinstance(value, int):
            return f"{key}: expected u64, got {type(value).__name__}"
        if not 0 <= value <= U64_MAX:
            return f"{key}: u64 out of range"
        return None
    if declared_type == "bool":
        if not isinstance(value, bool):
            return f"{key}: expected bool, got {type(value).__name__}"
        return None
    if declared_type == "string":
        if not isinstance(value, str):
            return f"{key}: expected string, got {type(value).__name__}"
        if not value.strip():
            return f"{key}: required string is empty"
        return None
    return f"{key}: unknown declared type {declared_type!r}"


def validate_schema(schema):
    """Return a list of schema-shape errors (empty when the schema is sound)."""
    errors = []
    if not isinstance(schema, dict):
        return ["schema root must be a JSON object"]
    if schema.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    keys = schema.get("keys")
    if not isinstance(keys, dict) or not keys:
        errors.append("keys must be a non-empty object")
        return errors
    for name, spec in keys.items():
        where = f"schema key {name}"
        if not isinstance(spec, dict):
            errors.append(f"{where}: spec must be an object")
            continue
        declared = spec.get("type")
        if declared not in TYPES:
            errors.append(f"{where}: type must be one of {TYPES}")
            continue
        default = spec.get("default")
        if default is not None:
            err = check_value(f"{where} default", declared, default)
            if err:
                errors.append(err)
        if not isinstance(spec.get("required"), bool):
            errors.append(f"{where}: required must be a bool")
        if not isinstance(spec.get("env_overridable"), bool):
            errors.append(f"{where}: env_overridable must be a bool")
        env_var = spec.get("env_var")
        if spec.get("env_overridable"):
            if not isinstance(env_var, str) or not env_var.strip():
                errors.append(f"{where}: env_overridable needs a named env_var")
        elif env_var is not None:
            errors.append(f"{where}: env_var must be null unless env_overridable")
    return errors


def validate_env(label, data, keys):
    """Validate one environment fixture against the schema key table."""
    errors = []
    if not isinstance(data, dict):
        return [f"{label}: fixture root must be a JSON object"]
    for name in data:
        if name not in keys:
            errors.append(f"{label}: undeclared key {name} (add it to the schema first)")
    for name, spec in keys.items():
        if name not in data:
            if spec.get("required"):
                errors.append(f"{label}: missing required key {name}")
            continue
        err = check_value(f"{label}: {name}", spec["type"], data[name])
        if err:
            errors.append(err)
    return errors


def check_parity(schema, staging, prod):
    """Full check: schema shape, both envs, then key-set parity."""
    errors = validate_schema(schema)
    if errors:
        return errors
    keys = schema["keys"]
    errors = validate_env("staging", staging, keys)
    errors += validate_env("prod", prod, keys)
    if errors:
        return errors
    staging_keys = set(staging)
    prod_keys = set(prod)
    for name in sorted(staging_keys - prod_keys):
        errors.append(f"parity: key {name} present in staging but missing in prod")
    for name in sorted(prod_keys - staging_keys):
        errors.append(f"parity: key {name} present in prod but missing in staging")
    for name in sorted(staging_keys & prod_keys):
        s_type = type(staging[name]).__name__
        p_type = type(prod[name]).__name__
        # bool is a subclass of int: report JSON kinds, not Python classes.
        s_kind = "bool" if isinstance(staging[name], bool) else s_type
        p_kind = "bool" if isinstance(prod[name], bool) else p_type
        if s_kind != p_kind:
            errors.append(f"parity: key {name} type differs (staging {s_kind} vs prod {p_kind})")
    return errors


def main(argv):
    parser = argparse.ArgumentParser(description="KV config staging/prod parity check.")
    parser.add_argument("--schema", required=True)
    parser.add_argument("--staging", required=True)
    parser.add_argument("--prod", required=True)
    args = parser.parse_args(argv)

    schema, err = load_json(args.schema, "schema")
    if err:
        return fail_input(err)
    staging, err = load_json(args.staging, "staging")
    if err:
        return fail_input(err)
    prod, err = load_json(args.prod, "prod")
    if err:
        return fail_input(err)

    errors = check_parity(schema, staging, prod)
    if errors:
        for err in errors:
            print(f"config-parity: drift: {err}")
        print(f"config-parity: DRIFT ({len(errors)} problem(s))")
        return 1
    count = len(schema["keys"])
    print(f"config-parity: OK ({count} keys, staging and prod match on keys and types)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
