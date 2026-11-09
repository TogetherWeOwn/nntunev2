-- Exercise the exact production insert, passed in by run_migration_tests.sh.
-- Roll back all synthetic data. No live/staging database is a test target.
BEGIN;
PREPARE listen_write(text, text, text, text, smallint, bigint) AS
:listen_history_insert_sql;
INSERT INTO public.users (id, email, created_at) VALUES
    ('listen-a', 'listen-a@example.test', 1000),
    ('listen-b', 'listen-b@example.test', 1000);

EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'complete', 1, 1000);
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM public.listen_events WHERE user_id = 'listen-a') THEN
        RAISE EXCEPTION 'terminal without a start was written';
    END IF;
END $$;
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'start', 0, 1000);
-- A retry must preserve the original timestamp, even after an unknown commit.
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'start', 0, 2000);
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 'wrong', 'start', 0, 2000);
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 'wrong', 'complete', 1, 2000);
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'complete', 1, 2001);
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'complete', 1, 3000);
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155671', 't', 'skip', 1, 3000);
-- Another account may legitimately generate the same client UUID.
EXECUTE listen_write('listen-b', '40c325c0-2cee-489d-a8b6-fbe937155671', 'other', 'start', 0, 1000);
DO $$ BEGIN
    IF (SELECT count(*) FROM public.listen_events WHERE user_id = 'listen-a') <> 2
       OR (SELECT recorded_at FROM public.listen_events WHERE user_id = 'listen-a' AND phase = 0) <> 1000
       OR (SELECT event FROM public.listen_events WHERE user_id = 'listen-a' AND phase = 1) <> 'complete'
       OR (SELECT recorded_at FROM public.listen_events WHERE user_id = 'listen-a' AND phase = 1) <> 2001 THEN
        RAISE EXCEPTION 'dedupe changed event identity/payload/timestamp';
    END IF;
END $$;

UPDATE public.users SET deleting_at = now() WHERE id = 'listen-a';
EXECUTE listen_write('listen-a', '40c325c0-2cee-489d-a8b6-fbe937155672', 't', 'start', 0, 3000);
DO $$ BEGIN
    IF (SELECT count(*) FROM public.listen_events WHERE user_id = 'listen-a') <> 2 THEN
        RAISE EXCEPTION 'account deletion fence allowed a new write';
    END IF;
END $$;
-- Account erasure cascades history; other owners' data survives.
DELETE FROM public.users WHERE id = 'listen-a';
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM public.listen_events WHERE user_id = 'listen-a')
       OR (SELECT count(*) FROM public.listen_events WHERE user_id = 'listen-b') <> 1 THEN
        RAISE EXCEPTION 'account erasure left history or crossed owners';
    END IF;
END $$;
DEALLOCATE listen_write;
ROLLBACK;
