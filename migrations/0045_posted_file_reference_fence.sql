-- Bind retained checks to one incarnation of a concrete generation reference.
-- Deleting and recreating the same (kind, owner, generation) must not revive a
-- delayed read callback. Existing references receive fresh opaque tokens, not
-- verification/retention evidence; all logical associations and custody survive.
-- Inserts use a fresh default token; UPDATE cannot move an association or carry
-- its token to a different owner/generation. True no-op replay remains valid.
-- This does not retire deployed readers or enable promotion, reuse or cleanup.

SET search_path = public, pg_catalog;

ALTER TABLE posted_file_references ADD COLUMN IF NOT EXISTS reference_token TEXT
    NOT NULL DEFAULT (gen_random_uuid()::TEXT)
    CHECK (reference_token ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$');
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                   WHERE conrelid = 'public.posted_file_references'::regclass
                     AND conname = 'posted_file_reference_token_unique') THEN
        ALTER TABLE posted_file_references ADD CONSTRAINT posted_file_reference_token_unique
            UNIQUE (reference_token);
    END IF;
END $$;

CREATE OR REPLACE FUNCTION protect_posted_file_reference() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'posted generation reference is immutable';
    END IF;
    RETURN NEW;
END $$;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_file_references'::regclass
                     AND tgname = 'protect_posted_file_reference') THEN
        CREATE TRIGGER protect_posted_file_reference BEFORE UPDATE ON posted_file_references
            FOR EACH ROW EXECUTE FUNCTION protect_posted_file_reference();
    END IF;
END $$;
