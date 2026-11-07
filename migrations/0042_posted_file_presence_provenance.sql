-- Preserve the difference between an explicit missing article and an unavailable
-- presence check. Existing unconfirmed rows cannot prove either outcome; leave
-- both new fields NULL. Confirmation helpers also grant no STAT provenance.
-- A report records the answered repost count it observed. A later answered POST
-- makes that report historical, never post-repair negative evidence. Nothing here
-- declares terminal loss or grants replacement, promotion or R2 cleanup authority.

SET search_path = public, pg_catalog;

ALTER TABLE posted_file_article_progress
    ADD COLUMN IF NOT EXISTS presence_verdict TEXT,
    ADD COLUMN IF NOT EXISTS presence_repost_attempts INTEGER;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
        WHERE conrelid = 'posted_file_article_progress'::regclass
          AND conname = 'posted_file_presence_provenance') THEN
        ALTER TABLE posted_file_article_progress
            ADD CONSTRAINT posted_file_presence_provenance CHECK (
                (presence_verdict IS NULL AND presence_repost_attempts IS NULL)
                OR (presence_verdict IS NOT NULL AND presence_repost_attempts IS NOT NULL
                    AND presence_verdict IN ('present', 'missing', 'unavailable')
                    AND presence_repost_attempts BETWEEN 0 AND repost_attempts
                    AND (presence_verdict = 'present') = confirmed)
            );
    END IF;
END $$;
