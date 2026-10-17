# pg-env.sh -- translate DATABASE_URL into libpq PG* environment variables.
#
# DATABASE_URL arrives only via env/secret store and must never reach a
# child process in argv (argv is world-readable, e.g. via `ps`). libpq
# does not read DATABASE_URL, so callers source this file, run
# configure_pg_env_from_database_url once, and then invoke plain `psql`
# with no connection argument: host, port, user, database, password and
# the TLS/timeout options below travel in the PG* environment instead.
# The parser reads the URL from its own environment (never argv) and
# prints only shell-quoted PG* assignments; error messages name an
# offending parameter key, never a value.
#
# Precedence matches the old `psql "$DATABASE_URL"` form, where a URI
# parameter overrides the corresponding PG* variable and an unspecified
# parameter falls back to the environment: only parameters the URI
# actually specifies are (re)set here. A URI query parameter with no PG*
# equivalent (and multi-host URIs) is refused rather than silently
# dropped. This restores the "never in argv" contract only; a process
# environment is not a security boundary against same-UID processes.
# Requires python3 on PATH.

configure_pg_env_from_database_url() {
  local pg_assignments
  if ! pg_assignments="$(python3 - <<'PYEOF'
import os
import shlex
import sys
from urllib.parse import unquote, urlsplit


def fail(message):
    sys.stderr.write("pg-env: %s\n" % message)
    sys.exit(1)


# libpq connection parameter -> PG* environment variable (libpq docs,
# "Environment Variables"). Query parameters outside this map have no
# env delivery channel and are refused below.
ENV_FOR_PARAM = {
    "host": "PGHOST",
    "hostaddr": "PGHOSTADDR",
    "port": "PGPORT",
    "user": "PGUSER",
    "password": "PGPASSWORD",
    "passfile": "PGPASSFILE",
    "dbname": "PGDATABASE",
    "service": "PGSERVICE",
    "options": "PGOPTIONS",
    "application_name": "PGAPPNAME",
    "client_encoding": "PGCLIENTENCODING",
    "connect_timeout": "PGCONNECT_TIMEOUT",
    "sslmode": "PGSSLMODE",
    "requiressl": "PGREQUIRESSL",
    "sslrootcert": "PGSSLROOTCERT",
    "sslcert": "PGSSLCERT",
    "sslkey": "PGSSLKEY",
    "channel_binding": "PGCHANNELBINDING",
    "target_session_attrs": "PGTARGETSESSIONATTRS",
}

def parse_keyword_conninfo(text):
    """libpq keyword/value conninfo -> {parameter: value}.

    Pairs are separated by whitespace; a value may be single-quoted,
    and a backslash escapes the next character in either form.
    """
    params = {}
    i, n = 0, len(text)
    while True:
        while i < n and text[i].isspace():
            i += 1
        if i >= n:
            return params
        start = i
        while i < n and not text[i].isspace() and text[i] != "=":
            i += 1
        key = text[start:i]
        while i < n and text[i].isspace():
            i += 1
        if i >= n or text[i] != "=":
            fail("DATABASE_URL keyword conninfo is missing '=' after a keyword")
        i += 1
        while i < n and text[i].isspace():
            i += 1
        chars = []
        if i < n and text[i] == "'":
            i += 1
            closed = False
            while i < n:
                if text[i] == "\\" and i + 1 < n:
                    chars.append(text[i + 1])
                    i += 2
                elif text[i] == "'":
                    closed = True
                    i += 1
                    break
                else:
                    chars.append(text[i])
                    i += 1
            if not closed:
                fail("DATABASE_URL keyword conninfo has an unterminated quoted value")
        else:
            while i < n and not text[i].isspace():
                if text[i] == "\\" and i + 1 < n:
                    chars.append(text[i + 1])
                    i += 2
                else:
                    chars.append(text[i])
                    i += 1
        if key not in ENV_FOR_PARAM:
            fail(
                "DATABASE_URL keyword %r has no PG* environment equivalent" % key
            )
        params[key] = "".join(chars)


url = os.environ.get("DATABASE_URL", "")

# The old `psql "$DATABASE_URL"` accepted every form libpq expands from
# a dbname argument: a postgres URI, a keyword/value conninfo string,
# or a bare database name. Keep all three working.
params = {}
if url.startswith(("postgres://", "postgresql://")):
    try:
        parts = urlsplit(url)
    except ValueError:
        fail("DATABASE_URL is not a well-formed URI")
    # urlsplit leaves userinfo percent-encoded; decode exactly once so
    # special-character credentials reach libpq byte for byte.
    if parts.username is not None:
        params["user"] = unquote(parts.username)
    if parts.password is not None:
        params["password"] = unquote(parts.password)
    if parts.hostname is not None:
        if "," in parts.hostname:
            fail("multi-host DATABASE_URL URI is not supported; name a single host")
        params["host"] = unquote(parts.hostname)
    try:
        port = parts.port
    except ValueError:
        fail("DATABASE_URL carries an invalid port")
    if port is not None:
        params["port"] = str(port)
    if parts.path not in ("", "/"):
        params["dbname"] = unquote(parts.path[1:])
    # Query parameters override the corresponding URI parts (libpq URI
    # semantics). Split manually: form decoding would turn a literal '+'
    # into a space, which libpq does not do.
    for field in parts.query.split("&"):
        if not field:
            continue
        key, _, value = field.partition("=")
        key = unquote(key)
        if key not in ENV_FOR_PARAM:
            fail(
                "DATABASE_URL query parameter %r has no PG* environment equivalent"
                % key
            )
        params[key] = unquote(value)
elif "=" in url:
    params = parse_keyword_conninfo(url)
elif url:
    # No '=' and no URI prefix: libpq treated the old argument as a
    # literal database name, whitespace and all.
    params["dbname"] = url
else:
    fail("DATABASE_URL is empty")

for key, value in params.items():
    print("export %s=%s" % (ENV_FOR_PARAM[key], shlex.quote(value)))
PYEOF
)"; then
    return 1
  fi
  eval "$pg_assignments"
  # Nothing after this point needs the URL; drop it so a later edit
  # cannot pass it to a child by accident.
  unset DATABASE_URL
}
