#!/usr/bin/env python3
"""Dormant multipart validator; it does not perform or authorize uploads.

The caller must obtain the module inventory and WASM receipt from a trusted
build/command boundary, not from the request being validated. This helper is
not yet connected to deployment and does not establish upload provenance.
"""

import hashlib
import json
import re
from email import policy
from email.parser import BytesParser

MAX_BODY = 64 * 1024 * 1024
MAX_PARTS = 128
MAX_METADATA = 1024 * 1024
MIMES = {"application/javascript+module", "application/javascript",
         "application/wasm", "application/octet-stream", "text/plain",
         "application/source-map", "text/x-python", "text/x-python-requirement"}
WASM_MAGIC = b"\x00asm"


class InvalidPayload(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise InvalidPayload(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate metadata key")
        result[key] = value
    return result


def validate_upload(body, content_type, modules, wasm):
    """Validate bounded raw bytes against trusted module types and WASM receipt.

    modules maps exact part names to MIME types. wasm maps exact WASM part names
    to {'sha256': lowercase hex, 'length': integer}. No module name is inferred
    from a path or extension. Returns observed hashes, never raw part content.
    """
    require(type(body) is bytes and len(body) <= MAX_BODY, "invalid body size")
    require(isinstance(content_type, str), "invalid content type")
    match = re.fullmatch(r'multipart/form-data;\s*boundary=(?:"([A-Za-z0-9\x27()+_,./:=?-]{1,70})"|([A-Za-z0-9\x27()+_,./:=?-]{1,70}))', content_type, re.I)
    require(match is not None, "invalid multipart content type")
    boundary = (match.group(1) or match.group(2)).encode("ascii")
    require(body.startswith(b"--" + boundary + b"\r\n") and
            body.endswith(b"--" + boundary + b"--\r\n"), "invalid framing")
    require(isinstance(modules, dict) and 0 < len(modules) < MAX_PARTS and
            isinstance(wasm, dict) and wasm, "invalid trusted inventory")
    require("metadata" not in modules and set(wasm) <= set(modules), "invalid trusted inventory")
    for name, mime in modules.items():
        require(isinstance(name, str) and name and mime in MIMES, "invalid trusted module")
    for name, receipt in wasm.items():
        require(modules[name] == "application/wasm" and isinstance(receipt, dict) and
                set(receipt) == {"sha256", "length"} and
                isinstance(receipt["sha256"], str) and
                re.fullmatch(r"[0-9a-f]{64}", receipt["sha256"]) and
                type(receipt["length"]) is int and 0 <= receipt["length"] <= MAX_BODY,
                "invalid trusted WASM receipt")
    require({n for n, m in modules.items() if m == "application/wasm"} == set(wasm),
            "unreceipted trusted WASM")
    # Bound the flat wire structure before the recursive MIME parser sees it.
    chunks = body[len(boundary) + 4:-(len(boundary) + 8)].split(b"\r\n--" + boundary + b"\r\n")
    require(len(chunks) == len(modules) + 1 and len(chunks) <= MAX_PARTS,
            "ambiguous multipart framing")
    for chunk in chunks:
        header, separator, _ = chunk.partition(b"\r\n\r\n")
        require(separator and len(header) <= 8192, "invalid header framing")
        for line in header.split(b"\r\n"):
            require(b"\n" not in line and b"\r" not in line and b":" in line,
                    "invalid header framing")
            key, value = line.split(b":", 1)
            require(key.lower() in (b"content-disposition", b"content-type"), "unexpected part header")
            if key.lower() == b"content-type":
                require(value.strip() in {mime.encode("ascii") for mime in MIMES}, "unexpected module type")
    try:
        message = BytesParser(policy=policy.default).parsebytes(
            b"Content-Type: " + content_type.encode("ascii") + b"\r\nMIME-Version: 1.0\r\n\r\n" + body)
    except (ValueError, UnicodeError) as exc:
        raise InvalidPayload("malformed multipart") from exc
    require(message.is_multipart() and not message.defects and message.preamble is None and
            message.epilogue in (None, ""), "malformed multipart")
    parts = list(message.iter_parts())
    require(len(parts) == len(modules) + 1 and len(parts) <= MAX_PARTS, "unexpected part count")
    observed, metadata, filenames = {}, None, set()
    for part in parts:
        require(not part.defects and not part.is_multipart(), "malformed part")
        headers = list(part.raw_items())
        require(all(k.lower() in {"content-disposition", "content-type"} for k, _ in headers), "unexpected part header")
        require(len(part.get_all("Content-Disposition", [])) == 1 and
                len(part.get_all("Content-Type", [])) <= 1, "duplicate part header")
        disposition = part["Content-Disposition"]
        raw_disposition = next((v for k, v in headers if k.lower() == "content-disposition"), "")
        require(re.fullmatch(r'form-data; name="[\x21\x23-\x5b\x5d-\x7e]+"(?:; filename="[\x21\x23-\x5b\x5d-\x7e]+")?', raw_disposition) is not None and
                disposition is not None and not disposition.defects and
                disposition.content_disposition == "form-data", "invalid disposition")
        params = part.get_params(header="content-disposition", unquote=True)
        require(params is not None and len(params) in (2, 3) and
                len({k.lower() for k, _ in params}) == len(params), "ambiguous disposition")
        values = dict(params[1:])
        require(set(values) in ({"name"}, {"name", "filename"}), "unexpected disposition")
        name = values.get("name")
        require(isinstance(name, str) and name not in observed, "duplicate part name")
        payload = part.get_payload(decode=True)
        require(isinstance(payload, bytes), "invalid part body")
        if name == "metadata":
            require(set(values) == {"name"} and not part.get_all("Content-Type", []) and
                    len(payload) <= MAX_METADATA, "invalid metadata part")
            try:
                metadata = json.loads(payload.decode("utf-8"), object_pairs_hook=unique_object,
                                      parse_constant=lambda _: require(False, "invalid JSON constant"))
            except (UnicodeError, ValueError, RecursionError) as exc:
                raise InvalidPayload("invalid metadata JSON") from exc
        else:
            require(name in modules and set(values) == {"name", "filename"}, "unexpected module")
            filename = values["filename"]
            require(isinstance(filename, str) and filename == name and filename not in filenames,
                    "unexpected or duplicate filename")
            filenames.add(filename)
            content = part["Content-Type"]
            raw_type = next((v for k, v in headers if k.lower() == "content-type"), "")
            require(content is not None and not content.defects and not content.params and
                    raw_type == modules[name] and part.get_content_type() == modules[name],
                    "unexpected module type")
            is_wasm = payload.startswith(WASM_MAGIC) or name.lower().endswith(".wasm") or filename.lower().endswith(".wasm") or modules[name] == "application/wasm"
            require(is_wasm == (name in wasm), "unexpected WASM")
            if name in wasm:
                require(payload.startswith(WASM_MAGIC) and len(payload) == wasm[name]["length"] and
                        hashlib.sha256(payload).hexdigest() == wasm[name]["sha256"], "WASM mismatch")
        observed[name] = {"length": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}
    require(set(observed) == set(modules) | {"metadata"} and isinstance(metadata, dict), "missing part")
    main = metadata.get("main_module")
    require(isinstance(main, str) and main in modules and
            modules[main] == "application/javascript+module" and
            "body_part" not in metadata, "invalid main module")
    return {"body_length": len(body), "body_sha256": hashlib.sha256(body).hexdigest(), "parts": observed}
