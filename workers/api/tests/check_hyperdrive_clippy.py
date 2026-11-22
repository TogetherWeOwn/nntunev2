"""Hosted check of the release-target API and the lint's negative control."""

import json
from pathlib import Path
import subprocess


API = Path("workers/api")
LIB = API / "src/lib.rs"
FIXTURE = API / "tests/fixtures/hyperdrive_clippy.rs"
CLIPPY = [
    "cargo", "clippy", "--locked", "--manifest-path", str(API / "Cargo.toml"),
    "--target", "wasm32-unknown-unknown", "--lib",
]
DENY = ["--", "-D", "clippy::disallowed_methods"]


def main() -> None:
    subprocess.run([*CLIPPY, *DENY], check=True)
    print("Release-target API passed the Hyperdrive Clippy check", flush=True)

    original = LIB.read_bytes()
    try:
        with LIB.open("ab") as source:
            source.write(b'\n#[path = "../tests/fixtures/hyperdrive_clippy.rs"]\nmod hyperdrive_clippy;\n')
        rejected = subprocess.run(
            [*CLIPPY, "--message-format=json", *DENY],
            stdout=subprocess.PIPE, text=True, check=False,
        )
    finally:
        LIB.write_bytes(original)

    expected = {
        number for number, line in enumerate(FIXTURE.read_text().splitlines(), 1)
        if line.endswith("// rejected")
    }
    actual = set()
    unexpected_error = False
    for line in rejected.stdout.splitlines():
        event = json.loads(line)
        if event["reason"] != "compiler-message" or event["message"]["level"] != "error":
            continue
        message = event["message"]
        print(message["rendered"], end="", flush=True)
        code = message["code"] or {}
        spans = [span for span in message["spans"] if span["is_primary"]]
        if (code.get("code") == "clippy::disallowed_methods" and len(spans) == 1
                and Path(spans[0]["file_name"]).name == FIXTURE.name):
            actual.add(spans[0]["line_start"])
        elif code or spans:
            unexpected_error = True

    if rejected.returncode != 101 or not expected or actual != expected or unexpected_error:
        raise SystemExit(f"Expected only the Clippy errors on lines {sorted(expected)}, got {sorted(actual)}")
    print("Clippy rejected the named Client, Transaction and GenericClient calls")


if __name__ == "__main__":
    main()
