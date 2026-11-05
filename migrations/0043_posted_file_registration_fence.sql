-- Serialize new registry admission with old health writes on existing legacy
-- custody. A registry-visibility check alone cannot fence a concurrent first
-- registration, especially a callback holding an older transaction snapshot.
--
-- The byte-identical UPDATE takes the custody row's write lock AND creates a
-- tuple version. A callback ordered before registration may finish first; a
-- Read Committed callback ordered after it sees the committed registry in the
-- volatile 0042 trigger. An older Repeatable Read/Serializable writer instead
-- fails serialization against the new tuple version. A row lock alone would
-- not invalidate that old snapshot. Rollback restores the old compatibility
-- path. ON CONFLICT registration/replay still passes through this trigger.
--
-- No logical custody, AAD, health, timestamp, reference or budget changes. This
-- does not fence absent-row legacy inserts, retire old reads/network requests,
-- prove a deployed-worker drain (including transactions predating this DDL),
-- or enable replacement/reuse/promotion/publication/cleanup. The unique hash
-- constraint and the 0037/0042 protections remain intact. Registration takes
-- this legacy custody lock before acquiring its registry-row write lock;
-- callbacks never acquire a registry-row lock in the opposite order.

SET search_path = public, pg_catalog;

CREATE OR REPLACE FUNCTION serialize_posted_file_registration() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    UPDATE public.posted_files SET post_copy = post_copy
        WHERE plaintext_sha256 = NEW.plaintext_sha256
          AND generation_operation IS NULL;
    RETURN NEW;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_file_registry'::regclass
                     AND tgname = 'serialize_posted_file_registration') THEN
        CREATE TRIGGER serialize_posted_file_registration
            BEFORE INSERT ON posted_file_registry
            FOR EACH ROW EXECUTE FUNCTION serialize_posted_file_registration();
    END IF;
END $$;
