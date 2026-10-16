# mb-import: batch-only MusicBrainz dump import + replication applier.
# Interactive traffic never touches this image (ADR 0001/0007).
#
# Build from the REPOSITORY ROOT:
#   docker build -f containers/mb-import/Dockerfile .
FROM rust:1-bookworm AS validator
WORKDIR /src
# Cargo.lock is intentionally not tracked for this workspace. Copy the full
# (dockerignored) workspace so Cargo can resolve all member manifests on a
# clean checkout, then build the single validator package online.
COPY . .
RUN cargo build --release -p mb-import-validate

FROM postgres:18-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates python3 \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /job
COPY containers/mb-import/import.sh /job/import.sh
COPY containers/mb-import/replicate.sh /job/replicate.sh
COPY containers/mb-import/test-import.sh /job/test-import.sh
COPY --from=validator /src/target/release/mb-validate /usr/local/bin/mb-validate
RUN chmod 0755 /job/import.sh /job/replicate.sh /job/test-import.sh

# Default: show usage; the scheduler overrides CMD per run.
CMD ["bash", "/job/import.sh", "--help"]
