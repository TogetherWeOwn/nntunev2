-- Inactive sealing-intent constraints, exercised on the disposable migrated database.
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

-- Builds an INSERT; defaults are a valid 65536-byte intent in environment 1 / library 1.
CREATE FUNCTION pg_temp.ins(obj TEXT DEFAULT '20000000-0000-4000-8000-000000000009',
    gen TEXT DEFAULT '30000000-0000-4000-8000-000000000009',
    env TEXT DEFAULT '00000000-0000-4000-8000-000000000001',
    lib TEXT DEFAULT '10000000-0000-4000-8000-000000000001',
    cfg TEXT DEFAULT '40000000-0000-4000-8000-000000000001',
    p BIGINT DEFAULT 65536, e BIGINT DEFAULT 65600, h INT DEFAULT 48) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT format('INSERT INTO public.storage_object_intents VALUES (%L,%L,%L,%L,%L,%s,%s,decode(repeat(%L,%s),%L))',
        obj, gen, env, lib, cfg, p, e, 'ab', h, 'hex')
$$;

CREATE FUNCTION pg_temp.run(statement TEXT) RETURNS VOID LANGUAGE plpgsql AS $$ BEGIN EXECUTE statement; END $$;

DO $$ BEGIN
    IF EXISTS (SELECT FROM public.storage_object_intents) THEN
        RAISE EXCEPTION 'intent migration must not seed rows';
    END IF;
END $$;
INSERT INTO public.users (id, email, created_at) VALUES ('intent-owner', 'intent-owner@example.com', 1);
INSERT INTO public.storage_environments VALUES
    ('00000000-0000-4000-8000-000000000001'), ('00000000-0000-4000-8000-000000000002');
INSERT INTO public.storage_library_namespaces VALUES
    ('10000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000001', NULL),
    ('10000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000002', 'intent-owner');

-- Valid: minimum, exact chunk boundary, one byte over (two chunks), maximum, and a second environment.
SELECT pg_temp.run(s) FROM (VALUES
    (pg_temp.ins('20000000-0000-4000-8000-000000000001','30000000-0000-4000-8000-000000000001', p := 1, e := 65)),
    (pg_temp.ins('20000000-0000-4000-8000-000000000002','30000000-0000-4000-8000-000000000002')),
    (pg_temp.ins('20000000-0000-4000-8000-000000000003','30000000-0000-4000-8000-000000000003', p := 65537, e := 65617)),
    (pg_temp.ins('20000000-0000-4000-8000-000000000004','30000000-0000-4000-8000-000000000004', p := 8589934592, e := 8592031792)),
    (pg_temp.ins('20000000-0000-4000-8000-000000000005','30000000-0000-4000-8000-000000000005',
        '00000000-0000-4000-8000-000000000002','10000000-0000-4000-8000-000000000002'))
) AS cases(s);

SELECT pg_temp.expect_error(s, '22P02') FROM (VALUES
    (pg_temp.ins(obj := 'malformed')),
    (pg_temp.ins(cfg := 'malformed'))
) AS cases(s);
SELECT pg_temp.expect_error(s, '23514') FROM (VALUES
    (pg_temp.ins(obj := '00000000-0000-0000-0000-000000000000')),
    (pg_temp.ins(obj := '20000000-0000-1000-8000-000000000009')),
    (pg_temp.ins(obj := '20000000-0000-4000-c000-000000000009')),
    (pg_temp.ins(gen := '00000000-0000-0000-0000-000000000000')),
    (pg_temp.ins(gen := '30000000-0000-1000-8000-000000000009')),
    (pg_temp.ins(gen := '30000000-0000-4000-c000-000000000009')),
    (pg_temp.ins(obj := '20000000-0000-4000-8000-000000000009', gen := '20000000-0000-4000-8000-000000000009')),
    (pg_temp.ins(cfg := '00000000-0000-0000-0000-000000000000')),
    (pg_temp.ins(cfg := '40000000-0000-1000-8000-000000000001')),
    (pg_temp.ins(cfg := '40000000-0000-4000-c000-000000000001')),
    (pg_temp.ins(p := 0, e := 48)),
    (pg_temp.ins(p := 8589934593, e := 8592031809)),
    (pg_temp.ins(e := 65599)),
    (pg_temp.ins(e := 65601)),
    (pg_temp.ins(p := 65537, e := 65600)),
    (pg_temp.ins(h := 47)),
    (pg_temp.ins(h := 49)),
    ($q$UPDATE public.storage_object_intents SET plaintext_bytes = plaintext_bytes$q$),
    ($q$UPDATE public.storage_object_intents SET environment_id = '00000000-0000-4000-8000-000000000002' WHERE object_id = '20000000-0000-4000-8000-000000000001'$q$),
    ($q$DELETE FROM public.storage_object_intents WHERE object_id = '20000000-0000-4000-8000-000000000001'$q$),
    ($q$TRUNCATE public.storage_object_intents$q$)
) AS cases(s);
SELECT pg_temp.expect_error(s, '23503') FROM (VALUES
    (pg_temp.ins(env := '00000000-0000-4000-8000-000000000099')),
    (pg_temp.ins(lib := '10000000-0000-4000-8000-000000000099')),
    -- Library 2 belongs to environment 2: a cross-scope pairing is rejected.
    (pg_temp.ins(lib := '10000000-0000-4000-8000-000000000002'))
) AS cases(s);
SELECT pg_temp.expect_error(s, '23505') FROM (VALUES
    (pg_temp.ins(obj := '20000000-0000-4000-8000-000000000001')),
    (pg_temp.ins(gen := '30000000-0000-4000-8000-000000000001')),
    -- A generation ID is never reusable across scopes either.
    (pg_temp.ins('20000000-0000-4000-8000-000000000008','30000000-0000-4000-8000-000000000005',
        '00000000-0000-4000-8000-000000000002','10000000-0000-4000-8000-000000000002'))
) AS cases(s);
SELECT pg_temp.expect_error(s, '23001') FROM (VALUES
    ($q$DELETE FROM public.storage_library_namespaces WHERE id = '10000000-0000-4000-8000-000000000001'$q$),
    ($q$DELETE FROM public.storage_environments WHERE id = '00000000-0000-4000-8000-000000000001'$q$)
) AS cases(s);

DO $$ BEGIN
    IF (SELECT count(*) FROM public.storage_object_intents) <> 5 THEN
        RAISE EXCEPTION 'rejected statements must leave exactly the five valid intents';
    END IF;
END $$;
ROLLBACK;
