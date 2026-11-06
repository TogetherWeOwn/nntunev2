#!/usr/bin/env bash
# Run build.sh under the Cloudflare Container standard-4 envelope and record
# gate 3 (build time, peak disk, peak memory).
#
# Usage: measure.sh <image> <tools-dir> <scratch-file> <mount> <out.json>
# Env: MB_DUMP.
#
# - Disk: <mount> is a fresh ext4 filesystem of exactly 20,000,000,000 bytes
#   (the 20 GB Container disk cap) on a loop device; going over the cap
#   fails the build with ENOSPC rather than being estimated. Usage is
#   sampled every 5 s.
# - Memory/CPU: `docker run --cpus 4 --memory 12g --memory-swap 12g`; peak
#   is the container cgroup's memory.peak (cgroup v2), sampled every 5 s,
#   and an OOM kill is reported. memory.peak includes page cache, so an
#   I/O-heavy build pins it at the limit; peak anonymous memory (`anon` in
#   memory.stat) is sampled alongside as the working-set figure.
# CPU speed is the GitHub runner's, not Cloudflare's; the report says so.
set -euo pipefail

image=$1 tools=$2 scratch=$3 mount=$4 out=$5
: "${MB_DUMP:?MB_DUMP is required}"

sudo fallocate -l 20000000000 "$scratch"
sudo mkfs.ext4 -q -F -m 0 "$scratch"
sudo mkdir -p "$mount"
sudo mount -o loop "$scratch" "$mount"
sudo chmod 0777 "$mount"
disk_base=$(df -B1 --output=used "$mount" | tail -1 | tr -d ' ')
disk_size=$(df -B1 --output=size "$mount" | tail -1 | tr -d ' ')

cid=$(docker run -d --cpus 4 --memory 12g --memory-swap 12g \
  -e MB_DUMP="$MB_DUMP" \
  -v "$GITHUB_WORKSPACE:/src:ro" -v "$tools:/tools:ro" -v "$mount:/work" \
  "$image" bash /src/workers/mb/spike/build.sh)
cgroup=/sys/fs/cgroup/system.slice/docker-${cid}.scope
start=$(date -u +%s)
peak_disk=0 peak_mem=0 peak_anon=0
while [ "$(docker inspect -f '{{.State.Running}}' "$cid")" = "true" ]; do
  used=$(df -B1 --output=used "$mount" | tail -1 | tr -d ' ')
  [ "$used" -gt "$peak_disk" ] && peak_disk=$used
  if [ -r "$cgroup/memory.peak" ]; then
    mem=$(cat "$cgroup/memory.peak")
    [ "$mem" -gt "$peak_mem" ] && peak_mem=$mem
  fi
  if [ -r "$cgroup/memory.stat" ]; then
    anon=$(awk '$1 == "anon" {print $2}' "$cgroup/memory.stat")
    [ "${anon:-0}" -gt "$peak_anon" ] && peak_anon=$anon
  fi
  sleep 5
done
end=$(date -u +%s)
exit_code=$(docker inspect -f '{{.State.ExitCode}}' "$cid")
oom=$(docker inspect -f '{{.State.OOMKilled}}' "$cid")
docker logs "$cid" > "$(dirname "$out")/build.log" 2>&1 || true
docker rm "$cid" > /dev/null

M_START=$start M_END=$end M_PEAK_DISK=$peak_disk M_DISK_BASE=$disk_base \
M_DISK_SIZE=$disk_size M_PEAK_MEM=$peak_mem M_PEAK_ANON=$peak_anon M_EXIT=$exit_code M_OOM=$oom \
python3 - "$out" "$mount/phases.tsv" <<'PY'
import json, os, sys
out, phases_path = sys.argv[1], sys.argv[2]
phases = []
if os.path.exists(phases_path):
    rows = [line.split("\t") for line in open(phases_path).read().split("\n") if line]
    phases = [{"phase": name, "at": int(ts)} for name, ts in rows]
env = os.environ
start, end = int(env["M_START"]), int(env["M_END"])
peak_disk = int(env["M_PEAK_DISK"]) - int(env["M_DISK_BASE"])
result = {
    "dump": env["MB_DUMP"],
    "exit_code": int(env["M_EXIT"]),
    "oom_killed": env["M_OOM"] == "true",
    "wall_seconds": end - start,
    "peak_disk_bytes": peak_disk,
    "disk_cap_bytes": int(env["M_DISK_SIZE"]),
    "peak_memory_bytes": int(env["M_PEAK_MEM"]),
    "peak_anon_memory_bytes": int(env["M_PEAK_ANON"]),
    "memory_cap_bytes": 12 * 1024**3,
    "phases": phases,
}
result["gate_3_pass"] = (result["exit_code"] == 0 and result["wall_seconds"] <= 7200
                         and not result["oom_killed"])
json.dump(result, open(out, "w"), indent=2)
print(json.dumps(result, indent=2))
PY
