"""Read-only admission and evidence checks for the protected staging journey."""

import http.client
from io import BytesIO
import json
import os
import re
import sys


STAGING = "staging.nntune.com"
STRIPE = "api.stripe.com"
MAX_BYTES = 64 * 1024
HTTP_TOKEN = rb"[!#$%&'*+.^_`|~0-9A-Za-z-]+"
HTTP_QUOTED = rb'"(?:[\t !#-\[\]-~\x80-\xff]|\\[\t \x21-\x7e\x80-\xff])*"'
CHUNK_SIZE = re.compile(rb"([0-9a-fA-F]+)(?:[ \t]*;[ \t]*" + HTTP_TOKEN
                        + rb"(?:[ \t]*=[ \t]*(?:" + HTTP_TOKEN + rb"|" + HTTP_QUOTED + rb"))?)*\r\n")
TRAILER_FIELD = re.compile(HTTP_TOKEN + rb":[\t \x21-\x7e\x80-\xff]*\r\n")
ENROLLMENT_ID = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
PREREQUISITES = (
    "staging_isolated", "invoice_history", "effective_privileges",
    "stream_deployed", "stripe_configuration", "synthetic_scope", "teardown_ready",
)
JOURNEY = (
    "admitted", "invited_signup", "payment_applied", "playback_and_meter",
    "playback_cap", "renewal", "cancellation_or_lapse", "teardown_verified",
)


class Refused(Exception):
    """Carries only a runner-defined result code."""


def matches(value, pattern):
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def require(condition, code):
    if not condition:
        raise Refused(code)


def unique_object(pairs):
    obj = dict(pairs)
    require(len(obj) == len(pairs), "invalid_json")
    return obj


def reject_constant(_):
    raise Refused("invalid_json")


def read_json(source):
    raw = source.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES, "input_too_large")
    return json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object,
                      parse_constant=reject_constant)


def read_chunked_json(stream):
    # Require the final CRLF, which http.client treats as optional at EOF.
    body, framing_budget = bytearray(), MAX_BYTES

    def framing_line():
        nonlocal framing_budget
        line = stream.readline(min(framing_budget + 1, 8192))
        framing_budget -= len(line)
        require(framing_budget >= 0 and line.endswith(b"\r\n"), "invalid_response")
        return line

    while True:
        size_line = CHUNK_SIZE.fullmatch(framing_line())
        require(size_line, "invalid_response")
        size = int(size_line[1], 16)
        if size == 0:
            while (trailer := framing_line()) != b"\r\n":
                require(TRAILER_FIELD.fullmatch(trailer), "invalid_response")
            return read_json(BytesIO(body))
        require(len(body) + size <= MAX_BYTES, "input_too_large")
        chunk = stream.read(size)
        require(len(chunk) == size and stream.read(2) == b"\r\n", "invalid_response")
        body.extend(chunk)


def get_json(host, path, headers):
    # Direct TLS, without redirects, proxy settings or caller-supplied hosts.
    connection = http.client.HTTPSConnection(host, timeout=8)
    try:
        connection.request("GET", path, headers={"Accept": "application/json", **headers})
        with connection.getresponse() as response:
            require(response.status not in (401, 403) and not 300 <= response.status < 400,
                    "access_denied")
            require(response.status == 200, "upstream_refused")
            media_type = response.getheader("Content-Type", "").split(";", 1)[0]
            require(media_type.strip().lower() == "application/json", "invalid_response")
            length = response.getheader("Content-Length")
            transfer = response.getheader("Transfer-Encoding")
            if transfer is not None:
                require(response.version == 11 and transfer.strip(" \t").lower() == "chunked"
                        and length is None, "invalid_response")
                payload = read_chunked_json(response.fp)
            else:
                if length is not None:
                    require(matches(length.strip(" \t"), r"[0-9]+"), "invalid_response")
                payload = read_json(response)
            require(response.length in (None, 0) and isinstance(payload, dict), "invalid_response")
            return payload
    finally:
        connection.close()


def validate_evidence(evidence, sha, phase):
    required = {"app", "environment", "origin", "build", "stripe_account", "checks"}
    if phase == "finish":
        required.update(("checkout_session", "enrollment_id", "playback_receipt"))
    require(isinstance(evidence, dict) and evidence.keys() == required, "missing_evidence")
    require((evidence["app"], evidence["environment"], evidence["origin"])
            == ("nntune-v2", "staging", f"https://{STAGING}"), "wrong_target")
    require(evidence["build"] == sha, "wrong_build")
    checks = evidence["checks"]
    names = PREREQUISITES + (JOURNEY if phase == "finish" else ())
    require(isinstance(checks, dict) and checks.keys() == set(names), "missing_evidence")
    require(all(checks[name] == sha for name in names), "wrong_build")
    require(matches(evidence["stripe_account"], r"acct_[A-Za-z0-9]{1,128}"), "stripe_account")
    if phase == "finish":
        require(matches(evidence["checkout_session"], r"cs_test_[A-Za-z0-9]{1,128}"),
                "stripe_mode")
        validate_playback_receipt(evidence, sha)


def validate_playback_receipt(evidence, sha):
    enrollment_id = evidence["enrollment_id"]
    require(matches(enrollment_id, ENROLLMENT_ID), "invalid_enrollment")
    receipt = evidence["playback_receipt"]
    fields = {"build", "checkout_session", "enrollment_id", "played_at", "metered_at"}
    require(isinstance(receipt, dict) and receipt.keys() == fields, "invalid_playback_receipt")
    played_at, metered_at = receipt["played_at"], receipt["metered_at"]
    require(type(played_at) is int and type(metered_at) is int and 0 < played_at <= metered_at,
            "invalid_playback_receipt")
    require(receipt["build"] == sha, "wrong_build")
    require(receipt["checkout_session"] == evidence["checkout_session"], "playback_checkout_mismatch")
    require(matches(receipt["enrollment_id"], ENROLLMENT_ID), "invalid_playback_receipt")
    require(receipt["enrollment_id"] == enrollment_id, "playback_enrollment_mismatch")


def verify_staging_build(sha, access):
    require(get_json(STAGING, "/api/build", access) == {"sha": sha}, "wrong_build")
    api = get_json(STAGING, "/healthz", access)
    require(api.keys() == {"ok", "build_sha"} and api["ok"] is True and
            api["build_sha"] == sha, "wrong_build")


def verify_staging(evidence, sha, phase, env):
    key = env.get("PAID_INVITE_STRIPE_READ_KEY")
    require(matches(key, r"(?:rk|sk)_test_[A-Za-z0-9]{1,256}"), "stripe_mode")
    access = {
        "CF-Access-Client-Id": env.get("CF_ACCESS_CLIENT_ID"),
        "CF-Access-Client-Secret": env.get("CF_ACCESS_CLIENT_SECRET"),
    }
    require(all(matches(value, r"[!-~]{1,512}") for value in access.values()), "access_denied")
    verify_staging_build(sha, access)

    stripe = {"Authorization": f"Bearer {key}", "Stripe-Version": "2024-12-18.acacia"}
    balance = get_json(STRIPE, "/v1/balance", stripe)
    require(balance.get("object") == "balance" and balance.get("livemode") is False,
            "stripe_mode")
    account = get_json(STRIPE, "/v1/account", stripe)
    require(account.get("object") == "account" and account.get("id") == evidence["stripe_account"],
            "stripe_account")
    if phase == "finish":
        session_id = evidence["checkout_session"]
        session = get_json(STRIPE, f"/v1/checkout/sessions/{session_id}", stripe)
        require(session.get("livemode") is False, "stripe_mode")
        amount = session.get("amount_total")
        require(
            session.get("object") == "checkout.session" and session.get("id") == session_id
            and session.get("mode") == "subscription" and session.get("status") == "complete"
            and session.get("payment_status") == "paid" and type(amount) is int and amount > 0,
            "payment_unverified",
        )
        subscription_id = session.get("subscription")
        require(matches(subscription_id, r"sub_[A-Za-z0-9]{1,128}"), "payment_unverified")
        subscription = get_json(STRIPE, f"/v1/subscriptions/{subscription_id}", stripe)
        require(subscription.get("livemode") is False, "stripe_mode")
        require(subscription.get("object") == "subscription"
                and subscription.get("id") == subscription_id
                and subscription.get("status") == "canceled", "teardown_unverified")
        verify_staging_build(sha, access)


def main(argv, env, source):
    try:
        require(env.get("NNTUNE_PAID_INVITE_ACCEPTANCE") == "1", "disabled")
        require(argv in (["admit"], ["finish"]), "invalid_phase")
        phase = argv[0]
        sha = env.get("EXPECTED_BUILD_SHA")
        require(matches(sha, r"[0-9a-f]{40}"), "wrong_build")
        evidence = read_json(source)
        validate_evidence(evidence, sha, phase)
        verify_staging(evidence, sha, phase, env)
    except Refused as error:
        print(json.dumps({"result": str(error)}))
        return 1
    except (OSError, ValueError, RecursionError, http.client.HTTPException):
        print(json.dumps({"result": "unreadable_evidence"}))
        return 1
    result = "admitted" if phase == "admit" else "accepted"
    print(json.dumps({"result": result, "scope": "operator-assisted-staging", "build": sha}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], os.environ, sys.stdin.buffer))
