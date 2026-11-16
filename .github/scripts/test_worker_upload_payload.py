#!/usr/bin/env python3
"""Tests for the dormant Worker upload payload validator."""

import hashlib
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_upload_payload as validator


class PayloadTests(unittest.TestCase):
    def setUp(self):
        self.wasm = b"\x00asm\x01\x00\x00\x00"
        self.modules = {"entry.js": "application/javascript+module", "module.wasm": "application/wasm"}
        self.receipt = {"module.wasm": {"length": len(self.wasm), "sha256": hashlib.sha256(self.wasm).hexdigest()}}
        self.parts = [
            b'Content-Disposition: form-data; name="metadata"\r\n\r\n{"main_module":"entry.js"}',
            b'Content-Disposition: form-data; name="entry.js"; filename="entry.js"\r\nContent-Type: application/javascript+module\r\n\r\nexport default {}',
            b'Content-Disposition: form-data; name="module.wasm"; filename="module.wasm"\r\nContent-Type: application/wasm\r\n\r\n' + self.wasm,
        ]

    def body(self, parts=None):
        return b"--test\r\n" + b"\r\n--test\r\n".join(parts or self.parts) + b"\r\n--test--\r\n"

    def validate(self, body=None):
        return validator.validate_upload(body or self.body(), "multipart/form-data; boundary=test", self.modules, self.receipt)

    def test_valid_payload_reports_hashes(self):
        result = self.validate()
        self.assertEqual(result["body_sha256"], hashlib.sha256(self.body()).hexdigest())
        self.assertEqual(result["parts"]["module.wasm"], self.receipt["module.wasm"])

    def test_rejects_substitution_and_missing_extra_duplicate_parts(self):
        for body in [self.body().replace(self.wasm, b"\x00asm\x02\x00\x00\x00"),
                     self.body(self.parts[:-1]), self.body(self.parts + [self.parts[-1]]),
                     self.body(self.parts + [self.parts[-1].replace(b'module.wasm', b'extra.wasm')])]:
            with self.subTest(body=body), self.assertRaises(validator.InvalidPayload):
                self.validate(body)

    def test_rejects_ambiguous_and_malformed_parts(self):
        for body in [self.body().replace(b'name="module.wasm"', b'name="module.wasm"; name="other"'),
                     self.body().replace(b'filename="module.wasm"', b'filename="entry.js"'),
                     self.body().replace(b'filename="module.wasm"', b'filename="replacement.wasm"'),
                     self.body().replace(b'name="entry.js"; filename="entry.js"\r\n', b'name="entry.js"; filename="entry.js"\n'),
                     self.body().replace(b'Content-Type: application/wasm', b'Content-Type: application/wasm\r\nContent-Type: text/plain'),
                     self.body().replace(b'{"main_module":"entry.js"}', b'{"main_module":"entry.js","main_module":"other"}'),
                     self.body().replace(b'filename="module.wasm"', b"filename*=utf-8''module.wasm"),
                     self.body().replace(b'name="module.wasm"', b'name*0="module."; name*1="wasm"'),
                     self.body().replace(b'\r\n--test\r\n', b'\r\n--test\r\n--test\r\n', 1),
                     self.body().replace(b'Content-Type: application/wasm', b'Content-Type: multipart/mixed; boundary=nested'),
                     self.body()[:-5], self.body() + b"extra", b"prefix" + self.body()]:
            with self.subTest(body=body), self.assertRaises(validator.InvalidPayload):
                self.validate(body)

    def test_rejects_wrong_digest_or_length_even_when_wasm_is_valid(self):
        for receipt in [{"length": len(self.wasm) + 1, "sha256": self.receipt["module.wasm"]["sha256"]},
                        {"length": len(self.wasm), "sha256": "0" * 64}]:
            with self.subTest(receipt=receipt), self.assertRaises(validator.InvalidPayload):
                validator.validate_upload(self.body(), "multipart/form-data; boundary=test", self.modules,
                                          {"module.wasm": receipt})

    def test_rejects_wasm_disguised_as_javascript(self):
        body = self.body().replace(b'export default {}', self.wasm)
        with self.assertRaises(validator.InvalidPayload):
            self.validate(body)

    def test_rejects_invalid_metadata_and_headers(self):
        for body in [self.body().replace(b'"entry.js"}', b'[]}'),
                     self.body().replace(b'"main_module"', b'"body_part"'),
                     self.body().replace(b'Content-Type: application/wasm', b'Content-Type: application/octet-stream'),
                     self.body().replace(b'Content-Type: application/wasm', b'Content-Transfer-Encoding: base64\r\nContent-Type: application/wasm')]:
            with self.subTest(body=body), self.assertRaises(validator.InvalidPayload):
                self.validate(body)

    def test_rejects_invalid_type_and_bounded_inputs(self):
        for content_type in ["multipart/mixed; boundary=test", "multipart/form-data",
                             "multipart/form-data; boundary=test; boundary=other"]:
            with self.subTest(content_type=content_type), self.assertRaises(validator.InvalidPayload):
                validator.validate_upload(self.body(), content_type, self.modules, self.receipt)
        with patch.object(validator, "MAX_BODY", len(self.body()) - 1):
            with self.assertRaises(validator.InvalidPayload):
                self.validate()
        with patch.object(validator, "MAX_METADATA", 3):
            with self.assertRaises(validator.InvalidPayload):
                self.validate()

    def test_rejects_untrusted_receipt_shapes(self):
        for wasm in [{}, {"module.wasm": {"sha256": "bad", "length": 8}},
                     {"other": self.receipt["module.wasm"]}]:
            with self.subTest(wasm=wasm), self.assertRaises(validator.InvalidPayload):
                validator.validate_upload(self.body(), "multipart/form-data; boundary=test", self.modules, wasm)


if __name__ == "__main__":
    unittest.main()
