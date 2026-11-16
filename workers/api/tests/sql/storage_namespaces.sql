-- Inactive namespace constraints, exercised on the disposable migrated database.
BEGIN;
CREATE FUNCTION pg_temp.expect_error(statement TEXT, expected TEXT) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    BEGIN
        EXECUTE statement;
    EXCEPTION WHEN OTHERS THEN
        IF SQLSTATE = expected THEN RETURN; END IF;
        RAISE EXCEPTION 'expected %, got % for %', expected, SQLSTATE, statement;
    END;
    RAISE EXCEPTION 'expected % but statement succeeded: %', expected, statement;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT FROM public.storage_environments) OR
       EXISTS (SELECT FROM public.storage_library_namespaces) THEN
        RAISE EXCEPTION 'namespace migration must not seed identities';
    END IF;
END $$;
INSERT INTO public.users (id, email, created_at) VALUES ('namespace-owner', 'namespace-owner@example.com', 1);
INSERT INTO public.storage_environments VALUES
    ('00000000-0000-4000-8000-000000000001'), ('00000000-0000-4000-8000-000000000002');
INSERT INTO public.storage_library_namespaces VALUES
    ('10000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', NULL),
    ('10000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000001', 'namespace-owner'),
    ('10000000-0000-4000-8000-000000000003', '00000000-0000-4000-8000-000000000002', 'namespace-owner');

SELECT pg_temp.expect_error(s, '22P02') FROM (VALUES
    ($q$INSERT INTO public.storage_environments VALUES ('malformed')$q$),
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('malformed','00000000-0000-4000-8000-000000000001',NULL)$q$)
) AS cases(s);
SELECT pg_temp.expect_error(s, '23514') FROM (VALUES
    ($q$INSERT INTO public.storage_environments VALUES ('00000000-0000-0000-0000-000000000000')$q$),
    ($q$INSERT INTO public.storage_environments VALUES ('00000000-0000-1000-8000-000000000003')$q$),
    ($q$INSERT INTO public.storage_environments VALUES ('00000000-0000-4000-c000-000000000003')$q$),
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('00000000-0000-0000-0000-000000000000','00000000-0000-4000-8000-000000000001',NULL)$q$),
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('10000000-0000-1000-8000-000000000004','00000000-0000-4000-8000-000000000001',NULL)$q$),
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('10000000-0000-4000-c000-000000000004','00000000-0000-4000-8000-000000000001',NULL)$q$),
    ($q$UPDATE public.storage_environments SET id='00000000-0000-4000-8000-000000000004' WHERE id='00000000-0000-4000-8000-000000000001'$q$),
    ($q$UPDATE public.storage_library_namespaces SET id='10000000-0000-4000-8000-000000000004' WHERE id='10000000-0000-4000-8000-000000000002'$q$),
    ($q$UPDATE public.storage_library_namespaces SET environment_id='00000000-0000-4000-8000-000000000002' WHERE id='10000000-0000-4000-8000-000000000002'$q$),
    ($q$UPDATE public.storage_library_namespaces SET owner_id=NULL WHERE id='10000000-0000-4000-8000-000000000002'$q$)
) AS cases(s);
SELECT pg_temp.expect_error(s, '23503') FROM (VALUES
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('10000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000099',NULL)$q$),
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('10000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000001','missing-owner')$q$),
    ($q$DELETE FROM public.storage_environments WHERE id='00000000-0000-4000-8000-000000000001'$q$),
    ($q$DELETE FROM public.users WHERE id='namespace-owner'$q$)
) AS cases(s);
SELECT pg_temp.expect_error(s, '23505') FROM (VALUES
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('10000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000001',NULL)$q$),
    ($q$INSERT INTO public.storage_library_namespaces VALUES ('10000000-0000-4000-8000-000000000004','00000000-0000-4000-8000-000000000001','namespace-owner')$q$)
) AS cases(s);
ROLLBACK;
