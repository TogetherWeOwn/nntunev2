-- Content checks are separate from presence/POST evidence. Only the fenced
-- adapter records a verifier's complete authenticated-read result against the
-- immutable generation and sealed plan. No legacy timestamp/flag is backfilled
-- as content proof. Unknown/unavailable/auth/integrity failures are retained as
-- nonaffirmative observations, not terminal loss or replacement permission.
--
-- These records alone grant no retention, reuse, promotion, publication or R2
-- deletion authority. Independent-backbone provenance comes from canonical
-- pool configuration, never account aliases or the inequality of two strings.
-- Custody, envelopes/AAD, the unique hash constraint and references stay intact.

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS posted_file_content_checks (
    generation_id TEXT NOT NULL REFERENCES posted_files(id) ON DELETE RESTRICT,
    round BIGINT NOT NULL CHECK (round >= 1),
    side SMALLINT NOT NULL CHECK (side IN (0, 1)),
    backbone TEXT NOT NULL CHECK (backbone ~ '^[a-z0-9-]{1,64}$'),
    state TEXT NOT NULL CHECK (state IN
        ('intact', 'unavailable', 'authentication_failed', 'integrity_failed')),
    checked_articles INTEGER NOT NULL CHECK (checked_articles BETWEEN 0 AND 1000000),
    checked_at BIGINT NOT NULL CHECK (checked_at BETWEEN 0 AND 253402300799),
    PRIMARY KEY (generation_id, round, side),
    UNIQUE (generation_id, round, backbone),
    CHECK ((state = 'intact' AND checked_articles > 0)
        OR (state <> 'intact' AND checked_articles = 0))
);
