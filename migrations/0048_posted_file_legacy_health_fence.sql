-- Retire ID/hash-only health callbacks for registered legacy custody. Their
-- old flags and timestamps are not generation/round-bound evidence. Managed
-- generations already reject these writes through migration 0037's protector.
-- Registration does not manufacture new verified/retained health or rewrite
-- legacy custody, envelope/AAD, references, evidence, budgets or timestamps.
--
-- This is a separate trigger: replay of the historical 0037 protector cannot
-- restore legacy callback authority. Unregistered legacy rows keep their old
-- compatibility path. This does NOT prove that incompatible workers are drained
-- (including requests already in flight), fence their network side effects, or
-- enable replacement/reuse/promotion/publication/cleanup. The unique plaintext
-- hash constraint stays in place until the complete rollout can be proved.

SET search_path = public, pg_catalog;

CREATE OR REPLACE FUNCTION fence_registered_posted_file_health() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF OLD.generation_operation IS NULL
       AND (NEW.post_copy, NEW.verify_copy, NEW.verified_count,
            NEW.two_spool_verified, NEW.repost_exhausted,
            NEW.post_verified_at, NEW.last_retention_check)
           IS DISTINCT FROM
           (OLD.post_copy, OLD.verify_copy, OLD.verified_count,
            OLD.two_spool_verified, OLD.repost_exhausted,
            OLD.post_verified_at, OLD.last_retention_check)
       AND EXISTS (SELECT 1 FROM public.posted_file_registry r
                   WHERE r.plaintext_sha256 = OLD.plaintext_sha256) THEN
        RAISE EXCEPTION 'registered generation requires fenced health writes';
    END IF;
    RETURN NEW;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_files'::regclass
                     AND tgname = 'fence_registered_posted_file_health') THEN
        CREATE TRIGGER fence_registered_posted_file_health
            BEFORE UPDATE OF post_copy, verify_copy, verified_count,
                two_spool_verified, repost_exhausted, post_verified_at,
                last_retention_check ON posted_files
            FOR EACH ROW EXECUTE FUNCTION fence_registered_posted_file_health();
    END IF;
END $$;
