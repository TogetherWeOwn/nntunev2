# mb-import: batch-only MusicBrainz dump import + replication applier.
# Interactive traffic never touches this image (ADR 0001/0007).
FROM postgres:18-bookworm AS base

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates python3 \
 && rm -rf /var/lib/apt/lists/*

FROM base AS test
WORKDIR /job
COPY etl.sh /job/etl.sh
COPY etl.py /job/etl.py
COPY tests /job/tests
RUN chmod 0755 /job/etl.sh /job/etl.py /job/tests/test_etl.sh \
 && bash /job/tests/test_etl.sh \
 && touch /tmp/mb-import-tests-passed

FROM base AS runtime
WORKDIR /job
COPY import.sh /job/import.sh
COPY etl.sh /job/etl.sh
COPY etl.py /job/etl.py
COPY replicate.sh /job/replicate.sh
RUN chmod 0755 /job/import.sh /job/etl.sh /job/etl.py /job/replicate.sh
# Keep the test stage in the default build graph without shipping tests/fixtures.
COPY --from=test /tmp/mb-import-tests-passed /tmp/mb-import-tests-passed
RUN rm /tmp/mb-import-tests-passed

# Default: show usage; the scheduler overrides CMD per run.
CMD ["bash", "/job/import.sh", "--help"]
