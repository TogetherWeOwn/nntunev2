#!/usr/bin/env python3
import runpy
import tempfile
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

runner = runpy.run_path(str(Path(__file__).with_name("subsonic-staging-runner.py")))
StagingModel, matrix, rehearse = (runner[key] for key in ("StagingModel", "MATRIX", "rehearse"))


class RecordingModel(StagingModel):
    delays = {}

    def __init__(self):
        super().__init__()
        self.starts = []

    def start(self, slot):
        number = slot["slot"]
        self.starts.append((number, self.now))
        super().start(slot)
        if number in self.delays:
            self.pending[number] = self.now + self.delays[number]


class RequestContractTests(unittest.TestCase):
    bearer = "synthetic-bearer"
    api_key = "disposable+key/=with&reserved" * 16

    def test_posts_use_staging_and_separate_credentials(self):
        posts = list(runner["prepare_requests"](matrix, self.bearer, self.api_key))
        slots = runner["load_matrix"](matrix)["slots"]
        self.assertEqual([post.slot for post in posts], list(range(1, 49)))
        for post, slot in zip(posts, slots):
            with self.subTest(slot=post.slot):
                headers = dict(post.headers)
                url = urlsplit(post.url)
                self.assertEqual(post.method, "POST")
                self.assertEqual(
                    (url.scheme, url.netloc), ("https", "staging.nntune.com")
                )
                self.assertEqual(
                    (url.path, url.query, url.fragment),
                    (f"/rest/{slot['endpoint']}", "", ""),
                )
                self.assertEqual(
                    headers["Content-Type"], "application/x-www-form-urlencoded"
                )
                self.assertEqual(headers["Accept-Encoding"], "identity")
                self.assertEqual(headers["X-Subsonic-Measurement-Case"], slot["case"])
                auth = None if post.slot == 5 else f"Bearer {self.bearer}"
                self.assertEqual(headers.get("Authorization"), auth)
                self.assertNotIn(self.api_key, repr(post))
                self.assertNotIn(self.bearer, repr(post))
                self.assertLessEqual(len(post.body), 8192)
                params = {key: [value] for key, value in slot["params"].items()}
                if post.slot not in (5, 6):
                    params["apiKey"] = [self.api_key]
                self.assertEqual(
                    parse_qs(
                        post.body.decode("ascii"),
                        keep_blank_values=True,
                        strict_parsing=True,
                    ),
                    params,
                )

    def test_missing_or_unsafe_credentials_and_changed_matrix_refuse(self):
        prepare = runner["prepare_requests"]
        for bearer, key in (
            ("", self.api_key),
            ("a\r\nb", self.api_key),
            ("a" * 1025, self.api_key),
            (self.bearer, "a" * 31),
            (self.bearer, "a" * 513),
            (self.bearer, "a" * 32 + "雪"),
        ):
            with self.assertRaisesRegex(runner["Stop"], "^credential-shape$"):
                prepare(matrix, bearer, key)
        with tempfile.TemporaryDirectory() as tmp:
            tampered = Path(tmp) / "matrix.json"
            tampered.write_text(
                matrix.read_text().replace('"getAlbum"', '"unreviewed"', 1)
            )
            with self.assertRaisesRegex(runner["Stop"], "^unreviewed-matrix$"):
                prepare(tampered, self.bearer, self.api_key)

    def test_api_key_respects_worker_encoded_value_limit(self):
        prepare = runner["prepare_requests"]
        key = "+" * 341 + "a"  # 1024 encoded bytes, not including the field name.
        post = next(prepare(matrix, self.bearer, key))
        self.assertEqual(parse_qs(post.body.decode("ascii"))["apiKey"], [key])
        with self.assertRaisesRegex(runner["Stop"], "^credential-shape$"):
            prepare(matrix, self.bearer, key + "a")

    def test_fixed_rejections_require_exact_application_response(self):
        classify = runner["classify_fixed_rejection"]
        slots = runner["load_matrix"](matrix)["slots"]
        off = (
            b'<subsonic-response status="failed" version="1.16.1"><error code="70" '
            b'message="Endpoint not supported in staging."/></subsonic-response>'
        )
        unavailable = (
            b'{"subsonic-response":{"status":"failed","version":"1.16.1",'
            b'"error":{"code":0,"message":"Service unavailable."}}}'
        )
        oversize = unavailable.replace(
            b"Service unavailable.", b"Request or document too large."
        )
        oversize_xml = off.replace(b"70", b"0").replace(
            b"Endpoint not supported in staging.",
            b"Request or document too large.",
        )
        for number, status, fmt, retry, body in (
            (5, 404, "xml", None, off),
            *((n, 503, "json", "1", unavailable) for n in (9, 12, 15)),
            (18, 413, "json", None, oversize),
            (21, 413, "xml", None, oversize_xml),
            (24, 413, "json", None, oversize),
        ):
            slot = slots[number - 1]
            args = [status, body, f"application/{fmt}", "private, no-store", retry]
            with self.subTest(slot=number):
                self.assertEqual(classify(slot, *args), slot["expected"])
                for i, replacement in enumerate(
                    (302, b"x" * len(body), "text/javascript", "public", "2")
                ):
                    bad = args.copy()
                    bad[i] = replacement
                    with self.assertRaisesRegex(runner["Stop"], "^response-mismatch$"):
                        classify(slot, *bad)
                if retry is not None:
                    with self.assertRaisesRegex(runner["Stop"], "^response-mismatch$"):
                        classify(slot, *args[:-1], None)
        with self.assertRaisesRegex(runner["Stop"], "^body-limit$"):
            classify(
                slots[23],
                413,
                b"x" * 1025,
                "application/json",
                "private, no-store",
                None,
            )
        with self.assertRaisesRegex(runner["Stop"], "^response-mismatch$"):
            classify(slots[36], 503, unavailable, "application/json", "private, no-store", "1")


class RunnerTests(unittest.TestCase):
    def test_reviewed_schedule(self):
        model = RecordingModel()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["cleanup"]), ("complete", "simulated-off-and-drained"))
        self.assertEqual([number for number, _ in model.starts], list(range(1, 49)))
        self.assertTrue(all(b - a >= 1000 for (_, a), (_, b) in zip(model.starts, model.starts[1:])))
        self.assertEqual([row["state"] for row in report["slots"].values()], ["accepted"] * 48)
        self.assertEqual(report["simulated_pair_overlap"],
                         {1: False, 2: False, 3: True, 4: True, 5: True, 6: True})
        self.assertEqual(report["max_simulated_clients"], 2)
        self.assertEqual(report["max_simulated_backends"], 2)
        self.assertLess(model.now, 120000)

    def test_modified_matrix_never_arms(self):
        for raw in (matrix.read_text().replace('"in_flight": 2', '"in_flight": 3'),
                    matrix.read_text().replace('"in_flight": 2', '"in_flight": 99, "in_flight": 2')):
            with tempfile.TemporaryDirectory() as tmp:
                tampered = Path(tmp) / "matrix.json"
                tampered.write_text(raw)
                model = RecordingModel()
                report = rehearse(tampered, model)
                self.assertEqual((report["stop"], report["cleanup"]), ("unreviewed-matrix", "not-armed"))
                self.assertEqual(model.readback(), ("baseline", False))
                self.assertEqual(model.starts, [])

    def test_telemetry_loss_and_pressure_stop_before_next_start(self):
        for starts, observation, reason in ((0, (-1, True, 0), "telemetry-stale-or-unsafe"),
                                            (1, (50, True, 3), "backend-pressure"),
                                            (1, (50, False, 1), "telemetry-stale-or-unsafe")):
            class Unsafe(RecordingModel):
                def telemetry(self):
                    return observation if self.lease and len(self.starts) >= starts else super().telemetry()

            report = rehearse(matrix, Unsafe())
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["max_simulated_backends"], 0 if starts else None)
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_pair_busy_requires_attribution_and_is_bounded(self):
        for fmt, size, attributed, delay, stop in (
            ("json", 112, False, 100, "unattributed-pair-result"),
            ("xml", 120, False, 250, "unattributed-pair-result"),
            ("xml", 120, True, 250, "complete"), ("xml", 120, True, 300, "busy-latency"),
        ):
            class Busy(RecordingModel):
                delays = {38: delay}

                def result(self, slot):
                    result = super().result(slot)
                    return {"oracle": f"fixed_capacity_{fmt}", "http_status": 503,
                            "body_bytes": size} if result and slot["slot"] == 38 else result

                def busy_observed(self, slot):
                    return attributed and slot["slot"] == 38

            report = rehearse(matrix, Busy())
            accepted = stop == "complete"
            self.assertEqual((report["stop"], report["starts"]), (stop, 48 if accepted else 38))
            self.assertEqual(report["slots"][38]["state"], "accepted" if accepted else "rejected")
            self.assertEqual(report["slots"][39]["state"], "accepted" if accepted else "unstarted")

    def test_reader_unfinished_at_five_seconds_stops_and_cleans_up(self):
        model = RecordingModel()
        model.delays = {41: 6000, 42: 6000}
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("request-or-reader-timeout", 42))
        self.assertEqual(model.now - dict(model.starts)[41], 5000)
        self.assertEqual(report["simulated_pair_overlap"], {1: False, 2: False, 3: True})
        self.assertEqual([report["slots"][i]["state"] for i in (41, 42, 43)],
                         ["unfinished", "unfinished", "unstarted"])
        self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_partial_arm_and_cancellation_restore(self):
        class PartialArm(RecordingModel):
            def arm(self):
                super().arm()
                raise RuntimeError("not part of the report")

        class Cancelled(RecordingModel):
            def tick(self):
                raise KeyboardInterrupt

        class Superseded(Cancelled):
            def tick(self):
                self.version, self.lease = "new-default-off", False
                super().tick()

        for model, reason, starts, version in (
            (PartialArm(), "model-failure", 0, "baseline"), (Cancelled(), "cancelled", 1, "baseline"),
            (Superseded(), "cancelled", 1, "new-default-off"),
        ):
            report = rehearse(matrix, model)
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")
            self.assertEqual(model.readback(), (version, False))

    def test_client_cancellation_does_not_prove_backend_drain(self):
        class Lingering(RecordingModel):
            def telemetry(self):
                return self.now, True, int(bool(self.starts))

        model = Lingering()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("clock-or-window", 1))
        self.assertEqual(report["cleanup"], "unverified")
        self.assertEqual(model.readback(), ("baseline", False))


if __name__ == "__main__":
    unittest.main()
