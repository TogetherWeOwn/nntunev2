"""Hosted-only fixture on the api-migrations loopback PostgreSQL service."""

import os
import subprocess
import sys

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
    # Generated test-mode monthly periods and paid-at starts use the same
    # projection shape as the paid invite journey.
    check(0, exact=1, eligible_hydration=1, fresh=1)
    assert run(ROLE, 2, OBSERVER) == (2, None)

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

    sql("""UPDATE billing_preflight.projection SET evidence_complete = false
           WHERE stripe_invoice_id = 'in_fixture_1';
           DELETE FROM billing_preflight.projection WHERE stripe_invoice_id = 'in_fixture_2';""")
    check(2, incomplete=2, fresh=1)
    sql("""UPDATE billing_preflight.projection SET stripe_subscription_id = 'sub_fixture_other'
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


def main():
    result = subprocess.run(
        [sys.executable, str(SCRIPTS / "billing_history_preflight.py")],
        env={}, capture_output=True, text=True, timeout=5,
    )
    assert (result.returncode, result.stdout, result.stderr) == (3, "", "disabled\n")
    sql(f"CREATE DATABASE {DATABASE} TEMPLATE template0;", "postgres")
    try:
        migration_dir = SCRIPTS.parents[1] / "migrations"
        sql("\n".join((migration_dir / name).read_text() for name in MIGRATIONS))
        sql((SCRIPTS / "billing_history_preflight_fixture.sql").read_text())
        before = archive_counts()
        check(0, exact=1, eligible_hydration=1, fresh=1)
        assert archive_counts() == before == "2|1\n"
        exercise()
    finally:
        sql(f"DROP DATABASE {DATABASE}; DROP ROLE IF EXISTS {ROLE};", "postgres")
    print("Invoice history preflight fixture passed")


def archive_counts():
    return sql("SELECT count(*), count(covered_periods) FROM public.paid_invoice_history")


if __name__ == "__main__":
    main()
