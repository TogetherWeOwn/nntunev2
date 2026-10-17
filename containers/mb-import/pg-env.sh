# pg-env.sh -- translate DATABASE_URL into libpq PG* environment variables.
#
# DATABASE_URL arrives only via env/secret store and must never reach a
# child process in argv (argv is world-readable, e.g. via `ps`). libpq
# does not read DATABASE_URL, so callers source this file, run
# configure_pg_env_from_database_url once, and then invoke plain `psql`
# with no connection argument: host, port, user, database, password and
# the TLS/timeout options below travel in the PG* environment instead.
# The parser reads the URL from its own environment (never argv) and
# prints only shell-quoted PG* assignments; error messages never echo
# input-derived text (a refused parameter is named only when it is a
# known libpq keyword with no PG* equivalent, fixed below).
#
# Precedence otherwise matches the old `psql "$DATABASE_URL"` form,
# where a URI parameter overrides the corresponding PG* variable and an
# unspecified parameter falls back to the environment: only parameters
# the URI actually specifies are (re)set here. Service files are the
# exception: libpq ranks explicit parameters above a service file and
# a service file above PG* variables, so after translation a service
# could silently replace the URL's host, database or TLS settings. A
# `service` parameter, or an inherited PGSERVICE, is therefore refused
# rather than silently redirected. A URI query parameter with no PG*
# equivalent (and multi-host URIs) is likewise refused rather than
# silently dropped. This restores the "never in argv" contract only; a
# process environment is not a security boundary against same-UID
# processes. Requires python3 on PATH.

configure_pg_env_from_database_url() {
  local pg_assignments
  if ! pg_assignments="$(python3 - <<'PYEOF'
import os
import re
import shlex
import sys
from urllib.parse import unquote, urlsplit


def fail(message):
    sys.stderr.write("pg-env: %s\n" % message)
    sys.exit(1)


# Percent-decoding must preserve non-UTF-8 bytes (e.g. %FF) exactly as
# libpq delivers them, so decode with surrogateescape and emit the
# assignments with the same error handler.
sys.stdout.reconfigure(errors="surrogateescape")


def dec(value):
    return unquote(value, errors="surrogateescape")


SERVICE_REFUSAL = (
    "a libpq service file would override DATABASE_URL settings "
    "after PG* translation"
)

# libpq connection parameter -> PG* environment variable: every pair in
# the libpq docs ("Environment Variables") except `service`, refused
# above for precedence reasons. Query parameters outside this map have
# no env delivery channel and are refused below.
ENV_FOR_PARAM = {
    "host": "PGHOST",
    "hostaddr": "PGHOSTADDR",
    "port": "PGPORT",
    "user": "PGUSER",
    "password": "PGPASSWORD",
    "passfile": "PGPASSFILE",
    "dbname": "PGDATABASE",
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
    "sslcrl": "PGSSLCRL",
    "sslcrldir": "PGSSLCRLDIR",
    "sslsni": "PGSSLSNI",
    "sslcertmode": "PGSSLCERTMODE",
    "sslnegotiation": "PGSSLNEGOTIATION",
    "sslcompression": "PGSSLCOMPRESSION",
    "ssl_min_protocol_version": "PGSSLMINPROTOCOLVERSION",
    "ssl_max_protocol_version": "PGSSLMAXPROTOCOLVERSION",
    "gssencmode": "PGGSSENCMODE",
    "gsslib": "PGGSSLIB",
    "gssdelegation": "PGGSSDELEGATION",
    "krbsrvname": "PGKRBSRVNAME",
    "require_auth": "PGREQUIREAUTH",
    "requirepeer": "PGREQUIREPEER",
    "load_balance_hosts": "PGLOADBALANCEHOSTS",
    "min_protocol_version": "PGMINPROTOCOLVERSION",
    "max_protocol_version": "PGMAXPROTOCOLVERSION",
}

# libpq keywords with no PG* environment equivalent. Only these fixed
# names may appear in a refusal message: any other input-derived "key"
# can be misparsed credential text (an unrecognised URI scheme reaches
# the keyword parser with userinfo still attached; a raw '?' in a
# password turns its tail into a query string), so its name is withheld
# rather than printed to the job log.
KNOWN_UNMAPPABLE = frozenset(
    [
        "fallback_application_name",
        "keepalives",
        "keepalives_count",
        "keepalives_idle",
        "keepalives_interval",
        "replication",
        "sslpassword",
        "tcp_user_timeout",
    ]
)


def unmappable_message(kind, key):
    if key in KNOWN_UNMAPPABLE:
        return "DATABASE_URL %s %r has no PG* environment equivalent" % (kind, key)
    return (
        "DATABASE_URL has an unsupported %s (name withheld) "
        "with no PG* environment equivalent" % kind
    )


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
        if key == "service":
            fail("DATABASE_URL 'service' parameter is refused: " + SERVICE_REFUSAL)
        if key not in ENV_FOR_PARAM:
            fail(unmappable_message("keyword", key))
        params[key] = "".join(chars)


url = os.environ.get("DATABASE_URL", "")

if os.environ.get("PGSERVICE"):
    fail("PGSERVICE is set and must be unset: " + SERVICE_REFUSAL)

# The old `psql "$DATABASE_URL"` accepted every form libpq expands from
# a dbname argument: a postgres URI, a keyword/value conninfo string,
# or a bare database name. Keep all three working.
params = {}
if url.startswith(("postgres://", "postgresql://")):
    # libpq tolerates a raw '#' or '?' in the password, but in URI
    # grammar they cut the authority short, so refuse them with the
    # remedy instead of misparsing. The candidate userinfo is the text
    # before the last '@' of the authority. A '?' there is userinfo
    # unless an earlier '@' precedes it (genuine userinfo, then host,
    # then a query string on a URL with no '/' path whose value
    # contains '@'); an '=' after the '?' proves nothing, as a
    # password can contain one too.
    authority = url.split("://", 1)[1].split("/", 1)[0]
    userinfo_candidate, at_sign, _ = authority.rpartition("@")
    if at_sign and (
        "#" in userinfo_candidate
        or (
            "?" in userinfo_candidate
            and "@" not in userinfo_candidate.split("?", 1)[0]
        )
    ):
        fail(
            "DATABASE_URL userinfo contains a raw '#' or '?': "
            "percent-encode it (%23 / %3F) in the URI"
        )
    try:
        parts = urlsplit(url)
    except ValueError:
        fail("DATABASE_URL is not a well-formed URI")
    if parts.fragment:
        fail(
            "DATABASE_URL contains a raw '#': "
            "percent-encode it (%23) in the URI"
        )
    # urlsplit leaves userinfo percent-encoded; decode exactly once so
    # special-character credentials reach libpq byte for byte. Empty
    # userinfo parts are skipped: libpq ignores them, so PGUSER /
    # PGPASSWORD from the environment still apply.
    if parts.username:
        params["user"] = dec(parts.username)
    if parts.password:
        params["password"] = dec(parts.password)
    if parts.hostname:
        if "," in parts.hostname:
            fail("multi-host DATABASE_URL URI is not supported; name a single host")
        params["host"] = dec(parts.hostname)
    try:
        port = parts.port
    except ValueError:
        fail(
            "DATABASE_URL carries an invalid port (a raw '#' or '?' in "
            "the password must be percent-encoded as %23 / %3F)"
        )
    if port is not None:
        params["port"] = str(port)
    if parts.path not in ("", "/"):
        params["dbname"] = dec(parts.path[1:])
    # Query parameters override the corresponding URI parts (libpq URI
    # semantics). Split manually: form decoding would turn a literal '+'
    # into a space, which libpq does not do.
    for field in parts.query.split("&"):
        if not field:
            continue
        key, _, value = field.partition("=")
        key = dec(key)
        if key == "service":
            fail("DATABASE_URL 'service' parameter is refused: " + SERVICE_REFUSAL)
        if key not in ENV_FOR_PARAM:
            fail(unmappable_message("query parameter", key))
        params[key] = dec(value)
elif re.match(r"[A-Za-z][A-Za-z0-9+.-]*://", url):
    # A URI-shaped input with any other scheme (a driver-specific
    # scheme, a typo) must not fall through to the keyword parser,
    # where the text up to the first '=' still contains userinfo.
    fail(
        "DATABASE_URL URI scheme is not supported; "
        "use postgres:// or postgresql://"
    )
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
