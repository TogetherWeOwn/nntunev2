-- Substitute the original cryptographically verified publisher object for a
-- synthetic occurrence whose replacement exercised revision fences.
WITH updated AS (
    UPDATE public.catalogue_s3_sources SET
        object_id = '73737373-7373-4073-8073-737373737373',
        accepted_seq = accepted_seq + 1
    WHERE track_id = $1::text::uuid
    RETURNING accepted_seq, object_id
), removed AS (
    DELETE FROM public.storage_validated_digest_references WHERE reference_id = $1::text::uuid
)
SELECT accepted_seq, encode(w.plaintext_sha256, 'hex')
FROM updated JOIN public.storage_validated_digest_winners w USING (object_id);
