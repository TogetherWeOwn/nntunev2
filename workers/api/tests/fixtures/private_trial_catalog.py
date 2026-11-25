"""Shared synthetic PostgreSQL catalog setup for the private trial fixtures."""

import hashlib

from apply_migration import transaction_body
from staging_apply import load_entries
from staging_trial_role_preflight import DATABASE


def create_catalog(sql, maintenance, owner, executor, password):
    sql(f"""
        CREATE ROLE {owner} LOGIN CREATEROLE NOINHERIT PASSWORD '{password}';
        CREATE ROLE {executor} LOGIN NOINHERIT PASSWORD '{password}';
        GRANT pg_signal_backend TO {owner} WITH INHERIT TRUE;
        CREATE DATABASE {DATABASE} OWNER {owner} TEMPLATE template0;
    """, database=maintenance)


def apply_catalog(sql, root, executor):
    entries = [entry for entry in load_entries(root) if entry["target"] == "app"]
    migrations = []
    for entry in entries:
        source = (root / "migrations" / entry["file"]).read_bytes()
        if hashlib.sha256(source).hexdigest() != entry["sha256"]:
            raise ValueError("synthetic catalog migration does not match the manifest")
        migrations.append(transaction_body(source.decode()))
    ledger = ", ".join(f"('{entry['file']}','{entry['sha256']}')" for entry in entries)
    body = "\n".join(migrations)
    sql(f"""GRANT CONNECT, CREATE ON DATABASE {DATABASE} TO {executor};
        ALTER SCHEMA public OWNER TO {executor};
        SET ROLE {executor};
        SET search_path = public, pg_catalog;
        {body}
        CREATE TABLE public.schema_migrations(file text, sha256 text);
        INSERT INTO public.schema_migrations VALUES {ledger};
    """)
    return entries
