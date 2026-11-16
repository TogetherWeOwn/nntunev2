-- Assertions are untrusted and do not elect or authorize a reusable object.
BEGIN;
CREATE FUNCTION pg_temp.expect_error(statement TEXT, expected TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
    BEGIN EXECUTE statement;
    EXCEPTION WHEN OTHERS THEN
        IF SQLSTATE = expected THEN RETURN; END IF;
        RAISE EXCEPTION 'expected %, got % for %', expected, SQLSTATE, statement;
    END;
    RAISE EXCEPTION 'expected % but succeeded: %', expected, statement;
END $$;
DO $$ BEGIN
    IF EXISTS (SELECT FROM public.storage_plaintext_digest_claim_intents) THEN
        RAISE EXCEPTION 'migration must not seed claims';
    END IF;
END $$;
INSERT INTO public.storage_environments VALUES
    ('00000000-0000-4000-8000-000000000001'), ('00000000-0000-4000-8000-000000000002');
INSERT INTO public.storage_library_namespaces VALUES
    ('10000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', NULL),
    ('10000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000002', NULL);
INSERT INTO public.storage_object_intents VALUES
    ('20000000-0000-4000-8000-000000000001', '30000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001', '40000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab',48),'hex')),
    ('20000000-0000-4000-8000-000000000002', '30000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001', '40000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab',48),'hex')),
    ('20000000-0000-4000-8000-000000000003', '30000000-0000-4000-8000-000000000003', '00000000-0000-4000-8000-000000000002', '10000000-0000-4000-8000-000000000002', '40000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab',48),'hex')),
    ('20000000-0000-4000-8000-000000000004', '30000000-0000-4000-8000-000000000004', '00000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001', '40000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab',48),'hex'));
-- Duplicate identity claims in one scope and in another scope remain possible.
INSERT INTO public.storage_plaintext_digest_claim_intents
SELECT object_id, environment_id, library_id, plaintext_bytes, decode(repeat('ab',32),'hex')
FROM public.storage_object_intents WHERE object_id <> '20000000-0000-4000-8000-000000000004';
SELECT pg_temp.expect_error(s, code) FROM (VALUES
    ($q$INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('20000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001',1,decode(repeat('cd',32),'hex'))$q$, '23505'),
    ($q$INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('20000000-0000-4000-8000-000000000099','00000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001',1,decode(repeat('ab',32),'hex'))$q$, '23503'),
    ($q$INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('20000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000002','10000000-0000-4000-8000-000000000002',1,decode(repeat('ab',32),'hex'))$q$, '23503'),
    ($q$INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('20000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001',2,decode(repeat('ab',32),'hex'))$q$, '23503'),
    ($q$INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('20000000-0000-4000-8000-000000000099','00000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001',1,decode(repeat('ab',31),'hex'))$q$, '23514'),
    ($q$INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('20000000-0000-4000-8000-000000000099','00000000-0000-4000-8000-000000000001','10000000-0000-4000-8000-000000000001',0,decode(repeat('ab',32),'hex'))$q$, '23514'),
    ($q$UPDATE public.storage_plaintext_digest_claim_intents SET plaintext_sha256 = plaintext_sha256$q$, '23514'),
    ($q$DELETE FROM public.storage_plaintext_digest_claim_intents$q$, '23514'),
    ($q$TRUNCATE public.storage_plaintext_digest_claim_intents$q$, '23514')
) AS cases(s, code);
ROLLBACK;
