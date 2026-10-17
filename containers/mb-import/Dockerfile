# mb-import: batch-only MusicBrainz dump import + replication applier.
# Interactive traffic never touches this image (ADR 0001/0007).
FROM postgres:18-bookworm AS base

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates python3 \
 && rm -rf /var/lib/apt/lists/*

FROM base AS test
WORKDIR /job
COPY containers/mb-import/etl.sh /job/etl.sh
COPY containers/mb-import/etl.py /job/etl.py
COPY containers/mb-import/tests /job/tests
RUN chmod 0755 /job/etl.sh /job/etl.py /job/tests/test_etl.sh \
 && bash /job/tests/test_etl.sh \
 && touch /tmp/mb-import-tests-passed

FROM rust:1-bookworm AS validator
WORKDIR /src
# Cargo.lock is intentionally not tracked for this workspace. Copy the full
# (dockerignored) workspace so Cargo can resolve all member manifests on a
# clean checkout, then build the single validator package online.
COPY . .
RUN cargo build --release -p mb-import-validate

FROM base AS runtime
WORKDIR /job
COPY containers/mb-import/import.sh /job/import.sh
COPY containers/mb-import/etl.sh /job/etl.sh
COPY containers/mb-import/etl.py /job/etl.py
COPY containers/mb-import/replicate.sh /job/replicate.sh
COPY --from=validator /src/target/release/mb-validate /usr/local/bin/mb-validate
RUN chmod 0755 /job/import.sh /job/etl.sh /job/etl.py /job/replicate.sh
# Keep the ETL test stage in the default build graph without shipping tests.
COPY --from=test /tmp/mb-import-tests-passed /tmp/mb-import-tests-passed
RUN rm /tmp/mb-import-tests-passed

# Default: show usage; the scheduler overrides CMD per run.
CMD ["bash", "/job/import.sh", "--help"]
