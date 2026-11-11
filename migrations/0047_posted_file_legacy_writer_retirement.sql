-- Prepare an explicit, monotonic database cutoff for incompatible writers.
-- Installation leaves it OFF: activation is a separately authorized rollout
-- operation, after compatible consumers are ready. This migration does not
-- claim a deployed-worker drain or fence network requests already in flight.
-- It does not enable multiple generations, replacement, serving or R2 cleanup.
-- Existing custody, health, references and the unique hash remain unchanged.
--
-- Every legacy insert (including ON CONFLICT DO NOTHING) and changed legacy
-- health write holds a shared row lock through its transaction. Retirement's
-- UPDATE waits for those transactions; later writers see retired, or fail
-- serialization under an older Repeatable Read/Serializable snapshot. No
-- transaction here spans network work. Managed writes keep their existing
-- operation/round fences; a true no-op has no new health authority.

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS posted_file_legacy_writer_gate (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    retired BOOLEAN NOT NULL DEFAULT FALSE
);
INSERT INTO posted_file_legacy_writer_gate (singleton) VALUES (TRUE)
    ON CONFLICT (singleton) DO NOTHING;

CREATE OR REPLACE FUNCTION protect_posted_file_legacy_writer_gate() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'legacy writer retirement is monotonic';
    END IF;
    IF OLD.retired AND NOT NEW.retired THEN
        RAISE EXCEPTION 'legacy writer retirement is monotonic';
    END IF;
    RETURN NEW;
END $$;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_file_legacy_writer_gate'::regclass
                     AND tgname = 'protect_posted_file_legacy_writer_gate') THEN
        CREATE TRIGGER protect_posted_file_legacy_writer_gate
            BEFORE UPDATE OR DELETE ON posted_file_legacy_writer_gate
            FOR EACH ROW EXECUTE FUNCTION protect_posted_file_legacy_writer_gate();
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_file_legacy_writer_gate'::regclass
                     AND tgname = 'protect_posted_file_legacy_writer_gate_truncate') THEN
        CREATE TRIGGER protect_posted_file_legacy_writer_gate_truncate
            BEFORE TRUNCATE ON posted_file_legacy_writer_gate
            FOR EACH STATEMENT EXECUTE FUNCTION protect_posted_file_legacy_writer_gate();
    END IF;
END $$;

CREATE OR REPLACE FUNCTION fence_retired_posted_file_writes() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
DECLARE
    is_retired BOOLEAN;
BEGIN
    IF NEW.generation_operation IS NOT NULL THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' THEN
        IF (NEW.post_copy, NEW.verify_copy, NEW.verified_count,
                NEW.two_spool_verified, NEW.repost_exhausted,
                NEW.post_verified_at, NEW.last_retention_check)
           IS NOT DISTINCT FROM
           (OLD.post_copy, OLD.verify_copy, OLD.verified_count,
                OLD.two_spool_verified, OLD.repost_exhausted,
                OLD.post_verified_at, OLD.last_retention_check) THEN
            RETURN NEW;
        END IF;
    END IF;

    SELECT retired INTO is_retired FROM public.posted_file_legacy_writer_gate
        WHERE singleton FOR SHARE;
    IF NOT FOUND OR is_retired THEN
        RAISE EXCEPTION 'legacy posted file writes are retired';
    END IF;
    RETURN NEW;
END $$;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_files'::regclass
                     AND tgname = 'fence_retired_posted_file_writes') THEN
        CREATE TRIGGER fence_retired_posted_file_writes
            BEFORE INSERT OR UPDATE ON posted_files
            FOR EACH ROW EXECUTE FUNCTION fence_retired_posted_file_writes();
    END IF;
END $$;
