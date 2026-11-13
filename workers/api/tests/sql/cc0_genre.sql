-- Disposable CI database only. All fixtures are rolled back.
\getenv genre_test_marker GENRE_TEST_DISPOSABLE
BEGIN;
SELECT set_config('nntune.genre_test_marker', :'genre_test_marker', true);
DO $$
BEGIN
    IF current_database() <> 'nntune_ci'
        OR current_setting('nntune.genre_test_marker', true) IS DISTINCT FROM 'nntune-genre-ci' THEN
        RAISE EXCEPTION 'genre regression requires the disposable nntune_ci database';
    END IF;
END $$;

INSERT INTO public.mb_artist (mbid, name) VALUES
    ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'synthetic artist');
INSERT INTO public.genre (qid, label) VALUES ('Q1', 'one'), ('Q2', 'two');
INSERT INTO public.genre_root (root_id, display_name, sort_order, retired) VALUES
    ('active', 'Active', 1, false), ('retired', 'Retired', 2, true);
INSERT INTO public.genre_rollup VALUES ('Q1', 'active'), ('Q1', 'retired');
INSERT INTO public.artist_genre VALUES
    ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'Q1', 'wikidata'),
    ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'Q1', 'file_tag');

DO $$
BEGIN
    IF (SELECT count(*) FROM public.artist_genre_root) <> 1 OR NOT EXISTS (
        SELECT 1 FROM public.artist_genre_root WHERE root_id = 'active'
    ) THEN
        RAISE EXCEPTION 'active root deduplication or retired filter failed';
    END IF;
END $$;

-- An unresolved manual membership must suppress both other sources.
INSERT INTO public.artist_genre VALUES
    ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'Q2', 'manual');
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM public.artist_genre_root) THEN
        RAISE EXCEPTION 'unresolved manual assignment did not suppress other sources';
    END IF;
END $$;
INSERT INTO public.genre_rollup VALUES ('Q2', 'active');
DO $$
BEGIN
    IF (SELECT count(*) FROM public.artist_genre_root) <> 1 THEN
        RAISE EXCEPTION 'resolved manual assignment did not win';
    END IF;
    BEGIN
        INSERT INTO public.artist_genre VALUES
            ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'Q1', 'scraped');
        RAISE EXCEPTION 'invalid source accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.artist_genre VALUES
            ('bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb', 'Q1', 'manual');
        RAISE EXCEPTION 'missing artist accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.artist_genre VALUES
            ('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'Q999', 'manual');
        RAISE EXCEPTION 'missing genre accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO public.genre_rollup VALUES ('Q1', 'missing');
        RAISE EXCEPTION 'missing root accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
END $$;
ROLLBACK;
