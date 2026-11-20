"""Run the separately built owned-transport WASM Worker on hosted Actions only."""
import hashlib
import hmac
import importlib.util
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from functools import partial
from pathlib import Path

spec = importlib.util.spec_from_file_location("relay", Path(__file__).with_name("pg_commit_relay.py"))
relay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(relay)
routing = relay.load("worker_routing_postgres_smoke")
BILLING_DB = relay.BILLING_DB
close_observer = relay.load("pg_close_observer")


def parse_pids(status, body, marker, count):
    match = re.fullmatch(re.escape(marker) + r"(:[1-9][0-9]*)" * count, body)
    routing.check(status == 200 and match, f"unexpected test response: {status} {body[:300]!r}")
    pids = tuple(int(pid[1:]) for pid in match.groups())
    routing.check(len(set(pids)) == count, "expected distinct database sessions")
    return pids


def wait_for_sessions(settings, pids, *, clock=time.monotonic, sleep=time.sleep, observer=routing.psql):
    # Like the native contention checks, this observer is not a participant.
    ids = ",".join(str(int(pid)) for pid in pids)
    deadline = clock() + 10
    while True:
        budget = deadline - clock()
        routing.check(budget > 0, "owned database sessions did not close")
        remaining = observer(settings, settings.get("admin", "nntune_ci"), tuples=True, timeout=budget, sql=(
            f"SELECT (SELECT count(*) FROM pg_stat_activity WHERE pid IN ({ids})) + "
            f"(SELECT count(*) FROM pg_locks WHERE pid IN ({ids}))"))
        budget = deadline - clock()
        routing.check(budget > 0, "owned database sessions did not close")
        if remaining == "0":
            return
        sleep(min(0.2, budget))


def check_observation_deadline():
    def exercise(finish, should_pass):
        ticks = iter((0, 0, finish))
        def observer(*args, timeout, **kwargs):
            routing.check(timeout == 10, "observer timeout did not use remaining budget")
            return "0"
        try:
            wait_for_sessions({}, (1, 2), clock=lambda: next(ticks), observer=observer)
        except routing.Failure as err:
            if should_pass or str(err) != "owned database sessions did not close":
                raise
        else:
            routing.check(should_pass, "late zero result was accepted")
    exercise(9, True)
    exercise(12, False)
    print("PASS: observation deadline rejects a late zero result", flush=True)


@contextmanager
def billing_database(settings):
    admin = partial(routing.psql, settings, settings["admin"], timeout=10)
    observe = partial(routing.psql, settings, BILLING_DB, timeout=10)
    admin(f"CREATE DATABASE {BILLING_DB}")
    try:
        for name in routing.app_migrations():
            observe(stdin=(routing.ROOT / "migrations" / name).read_text())
        yield observe
    finally:
        admin(f"DROP DATABASE {BILLING_DB}")


def check_write_deadline(base, settings, observe):
    status, _, body = routing.http("POST", base + "/owned-write-deadline-proof")
    pids = parse_pids(status, body, "owned-write-deadline-ok", 1)
    wait_for_sessions(settings, pids)
    count = observe("SELECT count(*) FROM users WHERE id = 'deadline_probe'", tuples=True)
    routing.check(count == "0", f"deadline write survived: {count!r}")
    print("PASS: standalone Worker deadline rolled back a prior write", flush=True)


def check_billing_ack(base, settings, observe):
    with relay.commit_relay() as ack:
        status, _, body = routing.http("POST", base + "/billing-ack-proof")
        pids = parse_pids(status, body, "billing-ack-ok", 1)
        ack.result(timeout=2)
    wait_for_sessions(settings, pids)
    actual = observe("SELECT id FROM users ORDER BY id", tuples=True)
    routing.check(actual.splitlines() == ["withheld"], f"unexpected durable rows: {actual!r}")
    print("PASS: withheld COMMIT; durable row and closed backend", flush=True)


def check_reconcile_ack(base):
    status, _, body = routing.http("POST", base + "/billing-ack-fixture")
    routing.check(status == 200 and body == "billing-ack-fixture-ok", body)
    with relay.commit_relay() as ack:
        status, _, body = routing.http("POST", base + "/billing-reconcile-ack-proof")
        routing.check(status == 200 and body == "billing-reconcile-ack-ok", body)
        routing.check(ack.result(timeout=2) == b"W", "relay did not withhold COMMIT")
    print("PASS: test-feature billing prototype lost COMMIT acknowledgement, retry and replay", flush=True)


def check_webhook_ack(base, observe):
    status, _, body = routing.http("POST", base + "/billing-ack-fixture")
    routing.check(status == 200 and body == "billing-ack-fixture-ok", body)
    observe("""
        INSERT INTO public.billing_accounts (user_id, stripe_customer_id)
        VALUES ('worker_fault_user', 'cus_worker_fault');
        INSERT INTO public.plan_prices (stripe_price_id, plan_id, currency, price_cents, billing_interval, livemode)
        VALUES ('price_worker_fault', 'plus', 'usd', 800, 'month', false);
        INSERT INTO public.billing_operations (id, user_id, kind, state, request_key, request_payload)
        VALUES ('a0000000-0000-4000-8000-000000000001', 'worker_fault_user', 'checkout', 'pending',
                'webhook_ack_fixture', '{"kind":"checkout","price_id":"price_worker_fault","session_id":"cs_worker_fault"}');
    """)

    def snapshot():
        return json.loads(
            observe(
                """
            SELECT jsonb_build_array(
                COALESCE((SELECT jsonb_agg(s ORDER BY stripe_subscription_id) FROM public.subscriptions s), '[]'),
                COALESCE((SELECT jsonb_agg(h ORDER BY stripe_invoice_id) FROM public.paid_invoice_history h), '[]'),
                COALESCE((SELECT jsonb_agg(o ORDER BY id) FROM public.billing_operations o), '[]'),
                COALESCE((SELECT jsonb_agg(e ORDER BY stripe_event_id) FROM public.billing_webhook_events e), '[]'))
        """,
                tuples=True,
            )
        )

    body = json.dumps(
        {
            "id": "evt_worker_fault",
            "type": "checkout.session.completed",
            "livemode": False,
            "data": {"object": {"id": "cs_worker_fault"}},
        }
    ).encode()
    stamp = "1786032000"
    digest = hmac.digest(
        b"webhook-ack-fixture", stamp.encode() + b"." + body, "sha256"
    ).hex()
    headers = {
        "Stripe-Signature": f"t={stamp},v1={digest}",
        "Content-Type": "application/json",
    }
    before = snapshot()
    routing.check(
        before[:2] == [[], []] and not before[3],
        "webhook fixture already has billing state",
    )
    with relay.commit_relay() as ack:
        status, _, error = routing.http(
            "POST", base + "/billing-webhook-ack-proof", body, headers
        )
        routing.check(
            status == 500 and error == '{"error":"internal error"}',
            f"first delivery: {status} {error}",
        )
        routing.check(
            ack.result(timeout=2) == b"W", "webhook relay did not withhold COMMIT"
        )

    subs, history, operations, claims = snapshot()
    routing.check(
        operations == before[2] and not claims,
        "failed delivery changed checkout or retained claim",
    )
    for row in subs + history:
        routing.check(
            row["user_id"] == "worker_fault_user"
            and row["stripe_subscription_id"] == "sub_worker_fault"
            and row["plan_id"] == "plus",
            "committed billing rows have incorrect attribution",
        )
    routing.check(
        [(s["status"], s["paid_invoice_id"]) for s in subs]
        == [("active", "in_worker_second")],
        "lost acknowledgement did not leave the expected committed subscription",
    )
    routing.check(
        [(h["stripe_invoice_id"], h["amount_cents"], h["currency"]) for h in history]
        == [("in_worker_first", 800, "usd"), ("in_worker_second", 800, "eur")],
        "lost acknowledgement did not leave both attributed history rows",
    )

    settled = None
    for phase in ("redelivery", "replay"):
        status, _, reply = routing.http(
            "POST", base + "/billing-webhook-retry-proof", body, headers
        )
        routing.check(
            status == 200 and reply == '{"ok":true}', f"{phase}: {status} {reply}"
        )
        after = snapshot()
        routing.check(
            after[:2] == [subs, history], f"{phase} changed committed billing state"
        )
        routing.check(len(after[2]) == 1, f"{phase} added or removed a checkout")
        op = after[2][0]
        routing.check(
            op["id"] == before[2][0]["id"]
            and op["state"] == "succeeded"
            and op["request_payload"] == before[2][0]["request_payload"],
            f"{phase} did not settle the original checkout",
        )
        routing.check(
            [claim["stripe_event_id"] for claim in after[3]] == ["evt_worker_fault"],
            f"{phase} did not retain the delivery claim",
        )
        if settled is not None:
            routing.check(after == settled, "replay changed settled checkout or claim")
        settled = after
    print(
        "PASS: signed webhook recovered and settled after a withheld billing COMMIT",
        flush=True,
    )


def main():
    routing.check(os.environ.get("GITHUB_ACTIONS") == "true" and
                  os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted", "hosted Actions only")
    check_observation_deadline()
    bundle = Path(os.environ["OWNED_TEST_BUNDLE"]).resolve()
    wasm = bundle / "index_bg.wasm"
    shim = bundle / "worker/shim.mjs"
    routing.check(wasm.is_file() and shim.is_file(), "test Worker bundle missing")
    wrangler = routing.find_wrangler(None)
    wasm_bytes = wasm.read_bytes()
    routing.check(b"owned-drive-ok:5" in wasm_bytes, "test entry marker missing from WASM")
    print(f"test wasm sha256: {hashlib.sha256(wasm_bytes).hexdigest()}", flush=True)
    subprocess.run(["node", str(wrangler), "--version"], check=True, timeout=30)
    workerd = list((routing.ROOT / "apps/web/node_modules/.pnpm").glob("workerd@*/node_modules/workerd/bin/workerd"))
    routing.check(len(workerd) == 1, f"expected one workerd executable, got {len(workerd)}")
    subprocess.run([str(workerd[0]), "--version"], check=True, timeout=30)
    settings = {"host": "127.0.0.1", "port": "5432", "user": "postgres",
                "password": "ci-throwaway", "admin": "nntune_ci"}
    print("PostgreSQL:", routing.psql(settings, "nntune_ci", "SHOW server_version", tuples=True), flush=True)
    with tempfile.TemporaryDirectory(prefix="owned-worker-smoke-") as directory:
        tmp = Path(directory)
        (tmp / "wrangler.toml").write_text(f'name = "owned-socket-test"\nmain = "{shim}"\ncompatibility_date = "2026-10-01"\n')
        (tmp / "home").mkdir()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(tmp / "home"),
               "WRANGLER_SEND_METRICS": "false", "CI": "1", "NO_COLOR": "1"}
        with (tmp / "wrangler.log").open("w+") as log:
            proc = subprocess.Popen([shutil.which("node"), str(wrangler), "dev", "--config", str(tmp / "wrangler.toml"),
                                     "--local", "--ip", "127.0.0.1", "--port", str(port),
                                     "--persist-to", str(tmp / "state"), "--show-interactive-dev-session=false"],
                                    cwd=tmp, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                start = time.monotonic()
                while True:
                    routing.check(proc.poll() is None, "wrangler exited before test response")
                    routing.check(time.monotonic() - start < 180, "test Worker timed out")
                    try:
                        status, _, body = routing.http("GET", f"http://127.0.0.1:{port}/")
                    except OSError:
                        time.sleep(1)
                        continue
                    pids = parse_pids(status, body, "owned-drive-ok:5", 2)
                    wait_for_sessions(settings, pids)
                    print("PASS: real Worker PostgreSQL sessions close after success and work deadline", flush=True)
                    print("PASS: synthetic socket ownership and close, empty guard drop, deadline precedence, co-poll/drop, Worker timer", flush=True)
                    print(f"startup and execution elapsed: {time.monotonic() - start:.3f}s", flush=True)
                    break
                with billing_database(settings) as observe:
                    check_write_deadline(f"http://127.0.0.1:{port}", settings, observe)
                    check_billing_ack(f"http://127.0.0.1:{port}", settings, observe)
                admin = partial(routing.psql, settings, settings["admin"], timeout=10)
                admin(f"CREATE DATABASE {BILLING_DB} TEMPLATE template0")
                try:
                    url = f"http://127.0.0.1:{port}/billing-deadline-proof"
                    status, _, body = routing.http("POST", url)
                    routing.check(status == 200 and body == "billing-deadline-ok", body)
                    print("PASS: billing deadline after writes rolled back", flush=True)
                    print(
                        "PASS: PostgreSQL watchdog rolled back abandoned billing writes",
                        flush=True,
                    )
                finally:
                    admin(f"DROP DATABASE {BILLING_DB}")
                admin(f"CREATE DATABASE {BILLING_DB} TEMPLATE template0")
                try:
                    check_reconcile_ack(f"http://127.0.0.1:{port}")
                finally:
                    admin(f"DROP DATABASE {BILLING_DB}")
                admin(f"CREATE DATABASE {BILLING_DB} TEMPLATE template0")
                try:
                    observe = partial(routing.psql, settings, BILLING_DB, timeout=10)
                    check_webhook_ack(f"http://127.0.0.1:{port}", observe)
                finally:
                    admin(f"DROP DATABASE {BILLING_DB}")
                admin(f"CREATE DATABASE {BILLING_DB} TEMPLATE template0")
                try:
                    url = f"http://127.0.0.1:{port}/billing-cancel-proof"
                    with close_observer.close_observer() as (closed, relay_result):
                        with ThreadPoolExecutor(max_workers=1) as pool:
                            started = time.monotonic()
                            request = pool.submit(routing.http, "POST", url)
                            saw_eof = closed.wait(8)
                            status, _, body = request.result(timeout=30)
                            elapsed = time.monotonic() - started
                            routing.check(
                                elapsed < 3,
                                f"cancellation observation exceeded host bound: {elapsed:.3f}s",
                            )
                        routing.check(
                            status == 200 and body == "billing-cancel-ok", body
                        )
                        routing.check(
                            saw_eof,
                            "client EOF was not observed",
                        )
                        relay_result.result(timeout=2)
                    print(
                        "PASS: in-flight billing prototype cancellation rolled back",
                        flush=True,
                    )
                finally:
                    admin(f"DROP DATABASE {BILLING_DB}")
            finally:
                routing.process_group.shutdown_process_group(proc)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (routing.Failure, relay.ProtocolError, relay.barrier.BarrierError,
            OSError, subprocess.SubprocessError, KeyError) as err:
        print(f"FAIL: {err}", file=sys.stderr)
        sys.exit(1)
