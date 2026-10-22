#!/usr/bin/env bash
# Image self-test: the shipped transcode image encodes deterministically
# and emits valid `opus-64` artifacts. Run in CI (transcode-container job)
# against a freshly built image; also runnable locally with docker.
#
# Usage: bash determinism_self_test.sh [image[:tag]] [port]
# Defaults: nntune-transcode:ci, 18081.
#
# Asserts:
# 1. the image ffmpeg carries libopus (the pinned profile's encoder);
# 2. one fixture encoded under two staging ids yields byte-identical
#    artifacts with identical SHA-256 (spec acceptance: same bytes/hash
#    whichever path produced them);
# 3. the artifact is ffprobe-valid Ogg Opus 64k: codec opus, 48 kHz,
#    stereo, bitrate within the 64k envelope.
#
# The fixture is synthesised with python3 (no host ffmpeg needed): a
# 5 s 44.1 kHz stereo 16-bit WAV sine mix, so the encode also covers
# the resample + remix path. The only host tools are docker, curl,
# python3 and sha256sum.
set -uo pipefail

IMAGE="${1:-nntune-transcode:ci}"
PORT="${2:-18081}"
NAME="transcode-selftest-$$"

fail() { echo "self-test FAIL: $*" >&2; docker rm -f "$NAME" >/dev/null 2>&1; exit 1; }
pass() { echo "self-test PASS: $*"; }

command -v docker >/dev/null 2>&1 || { echo "self-test FAIL: docker is required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "self-test FAIL: curl is required" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "self-test FAIL: python3 is required" >&2; exit 1; }

tmp="$(mktemp -d)" || { echo "self-test FAIL: mktemp failed" >&2; exit 1; }
trap 'rm -rf -- "$tmp"; docker rm -f "$NAME" >/dev/null 2>&1' EXIT

# 1. Encoder capability is part of the shipped contract.
docker run -d --name "$NAME" -p "127.0.0.1:${PORT}:8080" "$IMAGE" >/dev/null \
  || fail "docker run $IMAGE"
for _ in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:${PORT}/health" | grep -q '"ok":true'; then break; fi
  sleep 1
done
curl -fsS "http://127.0.0.1:${PORT}/health" | grep -q '"ok":true' || fail "/health never ready"
docker exec "$NAME" ffmpeg -hide_banner -encoders 2>/dev/null | grep -q 'libopus' \
  || fail "image ffmpeg has no libopus encoder"
pass "image ffmpeg carries libopus"

# 2. Synthesise the fixture (deterministic bytes, resample path covered).
python3 - "$tmp/fixture.wav" <<'PYEOF'
import math, struct, sys
path, rate, seconds, channels = sys.argv[1], 44100, 5, 2
frames = rate * seconds
data = bytearray()
for i in range(frames):
    t = i / rate
    s = int(12000 * (math.sin(2 * math.pi * 440 * t) + 0.5 * math.sin(2 * math.pi * 880 * t)))
    s = max(-32768, min(32767, s))
    data += struct.pack('<h', s) * channels
with open(path, 'wb') as handle:
    handle.write(b'RIFF' + struct.pack('<I', 36 + len(data)) + b'WAVEfmt ')
    handle.write(struct.pack('<IHHIIHH', 16, 1, channels, rate, rate * channels * 2, channels * 2, 16))
    handle.write(b'data' + struct.pack('<I', len(data)) + bytes(data))
PYEOF
[ -f "$tmp/fixture.wav" ] || fail "fixture not written"
input_sha="$(sha256sum "$tmp/fixture.wav" | cut -d' ' -f1)"
input_key="0000-$(python3 -c "import hashlib; print(hashlib.sha256(b'selftest-fixture.wav').hexdigest())")"
staging_a="$(python3 -c 'print("a" * 64)')"
staging_b="$(python3 -c 'print("b" * 64)')"

encode_once() { # $1=staging $2=out_file $3=reply_file
  local staging="$1" out="$2" reply="$3"
  curl -fsS -X PUT --data-binary "@$tmp/fixture.wav" \
    "http://127.0.0.1:${PORT}/jobs/selftest01/t/${staging}/in/${input_key}" \
    | grep -q '"size":' || fail "PUT input $staging"
  curl -fsS -H 'Content-Type: application/json' \
    -d "{\"job_id\":\"selftest01\",\"staging_id\":\"$staging\",\"input_key\":\"$input_key\",\"source_sha256\":\"$input_sha\"}" \
    "http://127.0.0.1:${PORT}/transcode" > "$reply" \
    || fail "POST /transcode $staging"
  grep -q '"status":"transcoded"' "$reply" || fail "not transcoded $staging: $(cat "$reply")"
  curl -fsS "http://127.0.0.1:${PORT}/jobs/selftest01/t/${staging}/out/artifact.ogg" -o "$out" \
    || fail "GET artifact $staging"
}

encode_once "$staging_a" "$tmp/artifact-a.ogg" "$tmp/reply-a.json"
encode_once "$staging_b" "$tmp/artifact-b.ogg" "$tmp/reply-b.json"

sha_a="$(sha256sum "$tmp/artifact-a.ogg" | cut -d' ' -f1)"
sha_b="$(sha256sum "$tmp/artifact-b.ogg" | cut -d' ' -f1)"
[ "$sha_a" = "$sha_b" ] || fail "staging rerun differs: $sha_a vs $sha_b"
reply_sha_a="$(python3 -c "import json; print(json.load(open('$tmp/reply-a.json'))['artifact_sha256'])")"
reply_sha_b="$(python3 -c "import json; print(json.load(open('$tmp/reply-b.json'))['artifact_sha256'])")"
[ "$reply_sha_a" = "$sha_a" ] || fail "reply hash $reply_sha_a != fetched bytes $sha_a"
[ "$reply_sha_b" = "$sha_b" ] || fail "reply hash $reply_sha_b != fetched bytes $sha_b"
pass "two stagings, identical bytes+hash ($sha_a)"

# 3. ffprobe validity: Ogg Opus 64k profile.
probe="$(docker exec "$NAME" ffprobe -v error -show_entries stream=codec_name,sample_rate,channels,bit_rate -of csv=p=0 "/scratch/selftest01/t/${staging_a}/out/artifact.ogg")" \
  || fail "ffprobe failed"
echo "$probe" | grep -q '^opus,48000,2,' || fail "profile mismatch: $probe"
bitrate="$(echo "$probe" | cut -d, -f4)"
[ "$bitrate" -ge 60000 ] && [ "$bitrate" -le 70000 ] \
  || fail "bitrate $bitrate outside the 64k envelope (probe: $probe)"
pass "ffprobe-valid opus-64 ($probe)"

echo "self-test: all assertions passed"
