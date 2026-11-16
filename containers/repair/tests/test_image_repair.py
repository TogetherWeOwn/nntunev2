"""Exercise the shipped repair server and its real par2 binary over HTTP."""

import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import urllib.request
import uuid

CONTAINER = sys.argv[1]
BASE = "http://127.0.0.1:18080"


def docker(*args):
    subprocess.run(["docker", *args], check=True, timeout=60)


def request(method, path, body=None, timeout=10):
    req = urllib.request.Request(BASE + path, data=body, method=method)
    with urllib.request.urlopen(req, timeout=timeout) as response:
        return response.status, response.read()


def key(name):
    return hashlib.sha256(name.encode()).hexdigest()


def main():
    original = bytes(range(256)) * 128
    job = "job_" + uuid.uuid4().hex
    staging = "staging-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory() as tmp:
        fixture = pathlib.Path(tmp)
        (fixture / "sample.bin").write_bytes(original)
        docker("exec", CONTAINER, "mkdir", "/scratch/fixture")
        docker(
            "cp",
            str(fixture / "sample.bin"),
            f"{CONTAINER}:/scratch/fixture/sample.bin",
        )
        docker(
            "exec",
            "-w",
            "/scratch/fixture",
            CONTAINER,
            "/usr/local/bin/par2",
            "create",
            "-s1024",
            "-r50",
            "-n1",
            "sample.par2",
            "sample.bin",
        )
        docker("cp", f"{CONTAINER}:/scratch/fixture/.", tmp)
        docker("exec", CONTAINER, "rm", "-rf", "/scratch/fixture")

        recovery = sorted(fixture.glob("*.par2"))
        if len(recovery) < 2:
            raise RuntimeError("par2 did not generate an index and recovery volume")
        damaged = bytearray(original)
        damaged[0] ^= 0xFF
        if damaged == original:
            raise AssertionError("input was not damaged")
        files = [("sample.bin", bytes(damaged), "data")]
        files.extend((p.name, p.read_bytes(), "par2") for p in recovery)
        manifest = []
        prefix = f"/jobs/{job}/r1"
        for name, data, role in files:
            object_key = key(name)
            status, _ = request("PUT", f"{prefix}/in/{object_key}", data)
            if status != 201:
                raise AssertionError(f"unexpected staging status {status}")
            manifest.append(
                {"file_name": name, "key": object_key, "size": len(data), "role": role}
            )

        payload = json.dumps(
            {
                "job_id": job,
                "round": 1,
                "staging_id": staging,
                "timeout_secs": 60,
                "files": manifest,
            }
        ).encode()
        status, body = request("POST", "/repair", payload, timeout=75)
        result = json.loads(body)
        expected = {
            "file_name": "sample.bin",
            "key": key("sample.bin"),
            "size": len(original),
        }
        if (
            status != 200
            or result.get("status") != "repaired"
            or result.get("files") != [expected]
        ):
            raise AssertionError(f"unexpected repair result: {status} {result}")
        status, output = request("GET", f"{prefix}/out/{key('sample.bin')}")
        if status != 200 or output != original:
            raise AssertionError("repaired output does not match the original bytes")
        print(
            f"Real image repair returned {len(output)} original bytes (exact match)",
            flush=True,
        )


if __name__ == "__main__":
    main()
