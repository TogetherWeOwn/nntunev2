"""Hosted-only fixture on the api-migrations loopback PostgreSQL service."""

from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
import os
import subprocess
from unittest.mock import patch

import billing_history_preflight as preflight
from billing_history_preflight import DATABASE, SCRIPTS, run


ROLE = "invoice_preflight_fixture"
ADMIN = {
    "PATH": os.defpath, "PGHOSTADDR": "127.0.0.1", "PGPORT": "5432",
    "PGDATABASE": "postgres", "PGUSER": "postgres", "PGPASSWORD": "ci-throwaway",
    "PGPASSFILE": "/dev/null", "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "3",
}
OBSERVER = {**ADMIN, "PGDATABASE": DATABASE, "PGUSER": ROLE}
MIGRATIONS = (
    "0002_auth_vault.sql", "0021_billing_schema.sql", "0026_plan_reseed_no_trial.sql",
    "0029_paid_invoice_history.sql", "0059_paid_invoice_covered_periods.sql",
)


def sql(statement, database=DATABASE):
    result = subprocess.run(
        ["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"],
        input=statement, env={**ADMIN, "PGDATABASE": database},
        capture_output=True, text=True, timeout=15,
    )
    if result.returncode:
        raise RuntimeError("disposable billing fixture SQL failed")
    return result.stdout


def check(status, **counts):
    expected = dict(total=3, exact=0, eligible_hydration=0, collision=0, incomplete=0, fresh=0)
    expected.update(counts)
    assert run(ROLE, 3, OBSERVER) == (status, expected)


def refused(role, connection):
    try:
        run(role, 3, connection)
    except ValueError:
        return
    raise AssertionError("unadmitted comparison returned a report")


def exercise():
    assert run(ROLE, 2, OBSERVER) == (2, None)
    sql("UPDATE billing_preflight.scope SET retained = true WHERE stripe_invoice_id = 'in_fixture_3'")
    check(2, exact=1, eligible_hydration=1, incomplete=1)
    sql("""UPDATE billing_preflight.scope SET retained = false, user_id = 'other_fixture'
           WHERE stripe_invoice_id = 'in_fixture_3';
           UPDATE billing_preflight.projection SET user_id = 'other_fixture'
           WHERE stripe_invoice_id = 'in_fixture_3';""")
    check(1, exact=1, eligible_hydration=1, collision=1)
    sql("DELETE FROM billing_preflight.scope WHERE stripe_invoice_id = 'in_fixture_3'")
    assert run(ROLE, 2, OBSERVER) == (2, None)  # Count matches, but a projection is out of scope.
    sql("""INSERT INTO billing_preflight.scope VALUES ('in_fixture_3', 'user_fixture', 'sub_fixture', false);
           UPDATE billing_preflight.projection SET user_id = 'user_fixture'
           WHERE stripe_invoice_id = 'in_fixture_3';""")
    # The old item-plan report retains a different plan. The old takeover
    # report retains a shorter span even after exact coverage was hydrated.
    sql("""UPDATE public.paid_invoice_history SET plan_id = 'pro'
           WHERE stripe_invoice_id = 'in_fixture_1'""")
    check(1, collision=1, eligible_hydration=1, fresh=1)
    sql("""UPDATE public.paid_invoice_history SET plan_id = 'plus', period_start = period_start + interval '1 day'
           WHERE stripe_invoice_id = 'in_fixture_1'""")
    check(1, collision=1, eligible_hydration=1, fresh=1)
    sql("""UPDATE public.paid_invoice_history
           SET period_start = period_start - interval '1 day', paid_at = paid_at + interval '0.5 seconds'
           WHERE stripe_invoice_id = 'in_fixture_1'""")
    check(1, collision=1, eligible_hydration=1, fresh=1)
    sql("""UPDATE public.paid_invoice_history SET paid_at = paid_at - interval '0.5 seconds'
           WHERE stripe_invoice_id = 'in_fixture_1'""")
    sql("""UPDATE billing_preflight.projection SET start_s = end_s, starts = '{}', ends = '{}'
           WHERE stripe_invoice_id = 'in_fixture_2';
           UPDATE public.paid_invoice_history SET period_start = period_end
           WHERE stripe_invoice_id = 'in_fixture_2';""")
    check(0, exact=2, fresh=1)

    sql("""UPDATE billing_preflight.projection SET evidence_complete = false
           WHERE stripe_invoice_id = 'in_fixture_1';
           DELETE FROM billing_preflight.projection WHERE stripe_invoice_id = 'in_fixture_2';""")
    check(2, incomplete=2, fresh=1)
    sql("""UPDATE billing_preflight.scope SET stripe_subscription_id = 'sub_fixture_absent'
           WHERE stripe_invoice_id = 'in_fixture_3';
           UPDATE billing_preflight.projection SET stripe_subscription_id = 'sub_fixture_absent'
           WHERE stripe_invoice_id = 'in_fixture_3';""")
    check(2, incomplete=3)

    refused("different_observer", OBSERVER)
    refused(ROLE, {**OBSERVER, "PGDATABASE": "postgres"})
    refused("postgres", {**ADMIN, "PGDATABASE": DATABASE})
    sql(f"GRANT UPDATE (amount_cents) ON public.paid_invoice_history TO {ROLE}")
    refused(ROLE, OBSERVER)
    sql(f"""REVOKE UPDATE (amount_cents) ON public.paid_invoice_history FROM {ROLE};
            ALTER TABLE public.paid_invoice_history ENABLE ROW LEVEL SECURITY;""")
    refused(ROLE, OBSERVER)


def invoke(environment):
    with (patch.dict(os.environ, environment, clear=True),
          redirect_stdout(StringIO()) as out, redirect_stderr(StringIO()) as err):
        status = preflight.main()
    return status, out.getvalue(), err.getvalue()


def exercise_entrypoint():
    env = {
        "NNTUNE_INVOICE_PREFLIGHT": "1", "BILLING_PREFLIGHT_HOST": "staging.example.test",
        "BILLING_PREFLIGHT_ROLE": ROLE, "BILLING_PREFLIGHT_PASSWORD": ADMIN["PGPASSWORD"],
        "BILLING_PREFLIGHT_SCOPE_COUNT": "3", "PGHOSTADDR": "ignored", "PGSERVICE": "ignored",
    }
    real_run = subprocess.run

    def loopback(argv, *, env, **options):
        assert env["PGHOST"] == "staging.example.test" and env["PGSSLMODE"] == "verify-full"
        assert env["PGUSER"] == ROLE and env["PGDATABASE"] == DATABASE
        assert "PGHOSTADDR" not in env and "PGSERVICE" not in env
        assert env["PGPASSFILE"] == "/dev/null" and env["PGGSSENCMODE"] == "disable"
        assert env["PGSSLROOTCERT"] == "/etc/ssl/certs/ca-certificates.crt"
        assert env["PGPASSWORD"] == ADMIN["PGPASSWORD"] and env["PGPASSWORD"] not in argv
        return real_run(argv, env=OBSERVER, **options)

    with patch.object(subprocess, "run", side_effect=loopback) as psql:
        for host in ("/var/run/postgresql", "staging.example.test,other.example.test"):
            assert invoke({**env, "BILLING_PREFLIGHT_HOST": host}) == (3, "", "refused\n")
        assert invoke({**env, "BILLING_PREFLIGHT_PASSWORD": ""}) == (3, "", "refused\n")
        psql.assert_not_called()
        status, out, err = invoke(env)
        assert (status, err) == (0, "")
        assert json.loads(out) == dict(total=3, exact=1, eligible_hydration=1,
                                       collision=0, incomplete=0, fresh=1)


def main():
    assert invoke({}) == (3, "", "disabled\n")
    sql(f"CREATE DATABASE {DATABASE} TEMPLATE template0;", "postgres")
    try:
        migration_dir = SCRIPTS.parents[1] / "migrations"
        sql("\n".join((migration_dir / name).read_text() for name in MIGRATIONS))
        sql((SCRIPTS.parent / "fixtures/billing_history_preflight.sql").read_text())
        exercise_entrypoint()
        assert sql("SELECT count(*), count(covered_periods) FROM public.paid_invoice_history") == "2|1\n"
        exercise()
    finally:
        sql(f"DROP DATABASE {DATABASE}; DROP ROLE IF EXISTS {ROLE};", "postgres")
    print("Invoice history preflight fixture passed")


if __name__ == "__main__":
    main()
