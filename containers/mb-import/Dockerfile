# mb-import: batch-only MusicBrainz dump import + replication applier.
# Interactive traffic never touches this image (ADR 0001/0007).
FROM postgres:18-bookworm@sha256:5c4783a8cc3f0074912164f94dfa1be4e043438f0f707cf968d7af9a0d31a2c8 AS base

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates python3 \
 && rm -rf /var/lib/apt/lists/*

FROM base AS test
WORKDIR /job
COPY containers/mb-import/etl.sh /job/etl.sh
COPY containers/mb-import/etl.py /job/etl.py
COPY containers/mb-import/tests /job/tests
RUN chmod 0755 /job/etl.sh /job/etl.py /job/tests/test_etl.sh /job/tests/test_etl_reference_graphs.sh \
 && bash /job/tests/test_etl.sh \
 && bash /job/tests/test_etl_reference_graphs.sh \
 && touch /tmp/mb-import-tests-passed

FROM rust:1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS validator
WORKDIR /src
# The root Cargo.lock is tracked, proven fresh in CI (`cargo metadata
# --locked`) and audited by cargo-deny. Copy the full (dockerignored)
# workspace so Cargo can resolve all member manifests on a clean checkout,
# then build the single validator package --locked so the shipped binary
# only ever links the audited dependency set.
COPY . .
RUN cargo build --locked --release -p mb-import-validate

FROM base AS runtime
WORKDIR /job
COPY containers/mb-import/import.sh /job/import.sh
COPY containers/mb-import/etl.sh /job/etl.sh
COPY containers/mb-import/etl.py /job/etl.py
COPY containers/mb-import/replicate.sh /job/replicate.sh
COPY containers/mb-import/pg-env.sh /job/pg-env.sh
COPY --from=validator /src/target/release/mb-validate /usr/local/bin/mb-validate
RUN chmod 0755 /job/import.sh /job/etl.sh /job/etl.py /job/replicate.sh /job/pg-env.sh \
    && chown -R postgres:postgres /job
# Keep the ETL test stage in the default build graph without shipping tests.
COPY --from=test /tmp/mb-import-tests-passed /tmp/mb-import-tests-passed
RUN rm /tmp/mb-import-tests-passed

# Run the job as the image's existing non-root postgres user. /job is owned
# by it (the CI integration suite writes fixtures under /job); script
# scratch goes to world-writable /tmp via mktemp, or the mounted dump dir.
USER postgres

# Default: show usage; the scheduler overrides CMD per run.
CMD ["bash", "/job/import.sh", "--help"]
