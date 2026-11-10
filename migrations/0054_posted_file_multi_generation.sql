-- Allow a second immutable custody row for one plaintext hash while keeping
-- legacy first-wins semantics. The 0037 global UNIQUE on plaintext_sha256 is
-- replaced by a partial UNIQUE covering legacy rows only (generation_operation
-- IS NULL) plus a plain lookup index. Managed rows (generation_operation NOT
-- NULL) can share the hash with predecessors and retained history; the
-- registry (one row per hash) stays the sole selection authority, and
-- posted_files_generation_identity / operation FKs keep (id, hash) binding.
-- No ciphertext, AAD, keyring, plan or reference is rewritten; existing
-- custody rows are never backfilled, deleted or relabelled.
--
-- Legacy inserts (generation_operation IS NULL) keep ON CONFLICT DO NOTHING
-- first-wins against the partial index, and a BEFORE INSERT fence returns
-- NULL (zero rows) once the registry holds a non-null MANAGED selection for
-- the hash, so a legacy writer can never mint a second legacy winner after a
-- managed selection. A legacy selection falls through to the partial UNIQUE
-- index, which keeps raising the duplicate-hash violation. The fence takes
-- FOR SHARE on the registry row to serialize with promotion. The 0047 retirement gate is untouched and stays
-- OFF by default; this migration enables no rollout, drain, serving switch
-- or R2 cleanup by itself — the generation adapter's fenced admit/reuse/
-- promotion path owns those transitions.
SET search_path = public, pg_catalog;

ALTER TABLE public.posted_files DROP CONSTRAINT IF EXISTS posted_files_plaintext_sha256_key;
CREATE UNIQUE INDEX IF NOT EXISTS posted_files_plaintext_sha256_legacy_uq
    ON public.posted_files (plaintext_sha256) WHERE generation_operation IS NULL;
CREATE INDEX IF NOT EXISTS posted_files_plaintext_sha256_idx
    ON public.posted_files (plaintext_sha256);

CREATE OR REPLACE FUNCTION public.fence_legacy_insert_when_managed_selected() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
DECLARE
    selected_generation TEXT;
    selected_operation TEXT;
BEGIN
    IF NEW.generation_operation IS NOT NULL THEN
        RETURN NEW;
    END IF;
    SELECT r.selected_generation INTO selected_generation
        FROM public.posted_file_registry r
        WHERE r.plaintext_sha256 = NEW.plaintext_sha256 FOR SHARE;
    IF FOUND AND selected_generation IS NOT NULL THEN
        SELECT p.generation_operation INTO selected_operation
            FROM public.posted_files p
            WHERE p.id = selected_generation
              AND p.plaintext_sha256 = NEW.plaintext_sha256;
        IF selected_operation IS NOT NULL THEN
            RETURN NULL;
        END IF;
    END IF;
    RETURN NEW;
END $$;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_trigger
                   WHERE tgrelid = 'public.posted_files'::regclass
                     AND tgname = 'fence_legacy_insert_when_managed_selected') THEN
        CREATE TRIGGER fence_legacy_insert_when_managed_selected
            BEFORE INSERT ON public.posted_files
            FOR EACH ROW EXECUTE FUNCTION public.fence_legacy_insert_when_managed_selected();
    END IF;
END $$;
