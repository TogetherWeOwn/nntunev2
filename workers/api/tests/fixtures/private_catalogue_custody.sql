-- Repoint a synthetic track to the original fully verified publisher object.
-- The newer catalogue fixture object exercises metadata fences only.
UPDATE public.catalogue_s3_sources s SET
    object_id = '73737373-7373-4073-8073-737373737373',
    accepted_seq = accepted_seq + 1
WHERE track_id = $1::text::uuid
RETURNING accepted_seq;
