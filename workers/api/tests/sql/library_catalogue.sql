-- Regression test for the catalogue slice-1 reads
-- (workers/api/src/catalogue.rs: the app-only `library_page_statement`,
-- the three bounded mirror probes, and the app-side membership reads
-- `catalogue_member_releases_statement`,
-- `catalogue_member_track_pairs_statement`, and
-- `catalogue_release_membership_statement`, against migration 0035 plus
-- the mirror 0001/0016). Run after ALL migrations are applied (see
-- ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation RAISEs
-- and (with ON_ERROR_STOP) fails the run.
--
-- The store reads in split form (ADR 0002: the app `library` table and the
-- mirror `mb_*` tables live on different clusters, so no statement joins
-- across them): the page query crosses the ledger with one unnested
-- recording per row (matched content first, album order, unmatched rows
-- kept, poisoned stored MBIDs flowing through as text because the page
-- casts nothing), and the three probes enrich by well-formed MBID arrays
-- (the store pre-filters every element through `enrichment_key`; literals
-- stand in for the $n parameters below). The browse lists and the release
-- detail resolve only through the caller's ledger membership (ADR 0006
-- §6): member releases/pairs/recordings come from the app-side membership
-- reads (raw stored text, no `::uuid` cast — the `member_*` helpers
-- normalize in Rust), and the mirror lists probe exactly those member
-- values. The membership enumerations are uncapped (they span the
-- caller's whole ledger; the LIMIT bound applies to the mirror response
-- after matching), and the tracks list probe joins EXACT pairs via
-- aligned unnest columns — a same-recording row from another release
-- never reaches the response, and `retain_member_track_occurrences`
-- (pinned by the `catalogue` unit tests) stays as the post-filter safety
-- net. The release detail's track read filters to the caller's member
-- recordings on that release. Rust assembly (`assemble_library_rows`,
-- including repeated-recording positions) is likewise pinned by the
-- `catalogue` unit tests; this file pins the SQL halves: the page's
-- EMITTED order (observed directly in the production query's cursor),
-- verbatim pass-through, probe contents including the exact-pair tracks
-- probe, the membership reads (distinct member values, case-insensitive
-- detail gate), the LIMIT bound, per-user isolation, the three
-- member-scoped mirror collections, and the release detail read
-- (membership gate, member-only album-ordered tracks).

BEGIN;

-- The harness supplies the same file that library_page_statement includes.
-- Missing input is a psql error, never a skipped production-query check.
CREATE TEMPORARY TABLE catalogue_query (sql TEXT NOT NULL);
INSERT INTO catalogue_query VALUES (:'library_page_sql');

DO $$
DECLARE
    actual TEXT[];
    page_row RECORD;
    row_count_var INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-lib-1', 'lib-1@example.com', 1700000000),
           ('user-lib-2', 'lib-2@example.com', 1700000000);

    INSERT INTO mb_artist (mbid, name, sort_name, type, country)
    VALUES ('ffffffff-1111-2222-3333-444444444444'::uuid, 'Fixture Artist',
            'Artist, Fixture', 'Group', 'GB'),
           ('eeeeeeee-1111-2222-3333-444444444444'::uuid, 'Second Artist',
            'Artist, Second', NULL, NULL);

    INSERT INTO mb_release (mbid, artist_mbid, title, release_type, year, caa_url)
    VALUES ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            'ffffffff-1111-2222-3333-444444444444'::uuid,
            'Fixture Release', 'Album', 1983,
            'https://coverartarchive.example/release/aaaaaaaa'),
           ('dddddddd-1111-2222-3333-444444444444'::uuid,
            'eeeeeeee-1111-2222-3333-444444444444'::uuid,
            'Second Release', 'Album', 1984,
            'https://coverartarchive.example/release/dddddddd');

    INSERT INTO mb_recording (mbid, release_mbid, title, track_no, duration_ms)
    VALUES ('11111111-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track One', 1, 200000),
           ('22222222-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track Two', 2, 210000),
           ('33333333-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track Three', 3, 220000),
           ('44444444-1111-2222-3333-444444444444'::uuid,
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track Four', 4, 230000);

    -- The second release shares two recordings with the fixture release:
    -- the (dddddddd, 11111111) member pair for user-lib-1 below returns its
    -- row, while (dddddddd, 22222222) has no member pair and stays
    -- excluded. Track Four (44444444) sits on the fixture release in the
    -- mirror but in no caller's ledger, so the member-filtered release
    -- detail below drops it.
    INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no)
    VALUES ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '11111111-1111-2222-3333-444444444444'::uuid, 1, 1),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '22222222-1111-2222-3333-444444444444'::uuid, 1, 2),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '33333333-1111-2222-3333-444444444444'::uuid, 1, 3),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '44444444-1111-2222-3333-444444444444'::uuid, 1, 4),
           ('dddddddd-1111-2222-3333-444444444444'::uuid,
            '11111111-1111-2222-3333-444444444444'::uuid, 1, 1),
           ('dddddddd-1111-2222-3333-444444444444'::uuid,
            '22222222-1111-2222-3333-444444444444'::uuid, 1, 2);

    -- user-lib-1: one fully matched job (3 recordings), one matched job
    -- on the second release, one uppercase-release job (stored text flows
    -- through verbatim; the store lowercases before probing), one
    -- poisoned job (malformed stored MBIDs flow through as text because
    -- the page casts nothing — reaching any assertion below proves the
    -- page never evaluated a text::uuid cast), one unknown-release job,
    -- one unmatched job.
    INSERT INTO library (idempotency_key, job_id, user_id, release_mbid, recording_mbids)
    VALUES ('lib:key:A', 'lib-job-A', 'user-lib-1',
            'aaaaaaaa-1111-2222-3333-444444444444',
            '{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444}'),
           ('lib:key:F', 'lib-job-F', 'user-lib-1',
            'dddddddd-1111-2222-3333-444444444444',
            '{11111111-1111-2222-3333-444444444444}'),
           ('lib:key:E', 'lib-job-E', 'user-lib-1',
            'AAAAAAAA-1111-2222-3333-444444444444',
            '{11111111-1111-2222-3333-444444444444}'),
           ('lib:key:D', 'lib-job-D', 'user-lib-1',
            'not-a-uuid', '{also-not-a-uuid}'),
           ('lib:key:B', 'lib-job-B', 'user-lib-1',
            'bbbbbbbb-1111-2222-3333-444444444444',
            '{99999999-1111-2222-3333-444444444444}'),
           ('lib:key:C', 'lib-job-C', 'user-lib-1', NULL, '{}');

    -- user-lib-2: 55 single-track jobs for the LIMIT bound pin.
    INSERT INTO library (idempotency_key, job_id, user_id, release_mbid, recording_mbids)
    SELECT 'lib:key:bound:' || g, 'lib-bound-' || lpad(g::text, 3, '0'), 'user-lib-2',
           'aaaaaaaa-1111-2222-3333-444444444444',
           '{11111111-1111-2222-3333-444444444444}'::text[]
    FROM generate_series(1, 55) AS g;

    CREATE TEMPORARY TABLE lib_page (
        job_id TEXT, release_mbid TEXT, recording_mbids TEXT[],
        recording_mbid TEXT, recording_ord BIGINT, matched_last BOOLEAN
    );
    -- Observe the production query's result sequence directly. Accumulate
    -- before storing rows for the content checks: a later table scan or
    -- sorted aggregate cannot hide a broken production ORDER BY.
    actual := ARRAY[]::TEXT[];
    FOR page_row IN EXECUTE (SELECT sql FROM catalogue_query)
        USING 'user-lib-1'::text, 50::bigint
    LOOP
        actual := array_append(actual, page_row.job_id || ':' || page_row.recording_ord::text);
        INSERT INTO lib_page VALUES (page_row.job_id, page_row.release_mbid,
            page_row.recording_mbids, page_row.recording_mbid,
            page_row.recording_ord, page_row.matched_last);
    END LOOP;
    IF actual IS DISTINCT FROM ARRAY['lib-job-F:1', 'lib-job-E:1', 'lib-job-D:1', 'lib-job-B:1',
                       'lib-job-A:1', 'lib-job-A:2', 'lib-job-A:3',
                       'lib-job-C:0'] THEN
        RAISE EXCEPTION 'library page emission order wrong: %', actual;
    END IF;

    -- Only the unmatched upload sorts last.
    SELECT count(*) INTO row_count_var FROM lib_page WHERE matched_last;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'matched_last mislabels rows: %', row_count_var;
    END IF;

    -- Uppercase stored MBIDs pass through verbatim (no lower(), no
    -- cast): normalization before probing is the store's job.
    SELECT array_agg(release_mbid) INTO actual
    FROM (SELECT release_mbid FROM lib_page WHERE job_id = 'lib-job-E') s;
    IF actual IS DISTINCT FROM ARRAY['AAAAAAAA-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'uppercase release MBID did not pass through: %', actual;
    END IF;

    -- Poisoned stored MBIDs pass through as text too.
    SELECT count(*) INTO row_count_var FROM lib_page
    WHERE job_id = 'lib-job-D'
      AND release_mbid = 'not-a-uuid'
      AND recording_mbid = 'also-not-a-uuid';
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'poisoned MBIDs did not pass through as text';
    END IF;

    -- The unmatched upload stays visible with NULL recording/release.
    SELECT count(*) INTO row_count_var FROM lib_page
    WHERE job_id = 'lib-job-C'
      AND recording_mbid IS NULL AND release_mbid IS NULL
      AND cardinality(recording_mbids) = 0 AND recording_ord = 0;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'unmatched library row missing or misshaped';
    END IF;

    -- Recordings probe (`library_recordings_statement` shape): the
    -- page's well-formed recording MBIDs ($1) enrich known rows; the
    -- unknown 999... MBID returns nothing (NULL enrichment downstream).
    SELECT array_agg(recording_mbid ORDER BY recording_mbid) INTO actual FROM (
        SELECT mbid::text AS recording_mbid, title AS recording_title, duration_ms
        FROM mb_recording WHERE mbid = ANY('{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444}'::uuid[])
    ) s;
    IF actual IS DISTINCT FROM ARRAY['11111111-1111-2222-3333-444444444444',
                       '22222222-1111-2222-3333-444444444444',
                       '33333333-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'recordings probe content wrong: %', actual;
    END IF;

    -- Tracks probe (`tracks_list_statement` shape): the exact-pair join.
    -- Aligned unnest columns pair back to the caller's member pairs
    -- (literals stand in for $1/$2, pair-aligned element by element: the
    -- five well-formed member pairs for user-lib-1), so the (second
    -- release, Track Two) cross row is EXCLUDED even though the second
    -- release and Track Two are each members elsewhere.
    SELECT array_agg(release_mbid || ':' || track_no::text ORDER BY release_mbid, medium_no, track_no) INTO actual FROM (
        SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid,
               rec.title, rec.duration_ms, t.medium_no, t.track_no, r.title AS release_title,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        JOIN mb_release r ON r.mbid = t.release_mbid
        LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        JOIN unnest('{aaaaaaaa-1111-2222-3333-444444444444,aaaaaaaa-1111-2222-3333-444444444444,aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[],
                    '{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444,11111111-1111-2222-3333-444444444444}'::uuid[]) AS p(rel, rec)
        ON t.release_mbid = p.rel AND t.recording_mbid = p.rec
        ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT 50
    ) s;
    IF actual IS DISTINCT FROM ARRAY['aaaaaaaa-1111-2222-3333-444444444444:1',
                       'aaaaaaaa-1111-2222-3333-444444444444:2',
                       'aaaaaaaa-1111-2222-3333-444444444444:3',
                       'dddddddd-1111-2222-3333-444444444444:1'] THEN
        RAISE EXCEPTION 'track collection must return exactly the member pairs: %', actual;
    END IF;
    -- The excluded cross row proves the join is exact, not two ANY probes.
    SELECT count(*) INTO row_count_var FROM (
        SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid
        FROM mb_track t
        JOIN unnest('{aaaaaaaa-1111-2222-3333-444444444444,aaaaaaaa-1111-2222-3333-444444444444,aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[],
                    '{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444,11111111-1111-2222-3333-444444444444}'::uuid[]) AS p(rel, rec)
        ON t.release_mbid = p.rel AND t.recording_mbid = p.rec
    ) s
    WHERE release_mbid = 'dddddddd-1111-2222-3333-444444444444'
      AND recording_mbid = '22222222-1111-2222-3333-444444444444';
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'exact-pair join leaked the non-member cross row';
    END IF;

    -- Occurrence positions ride the release's own rows (Track Four is an
    -- unowned mirror row: it appears here but drops out of the
    -- member-filtered release detail below).
    SELECT array_agg(medium_no::text || '.' || track_no::text ORDER BY medium_no, track_no) INTO actual FROM (
        SELECT medium_no, track_no FROM mb_track
        WHERE release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::uuid
    ) s;
    IF actual IS DISTINCT FROM ARRAY['1.1', '1.2', '1.3', '1.4'] THEN
        RAISE EXCEPTION 'occurrence positions wrong: %', actual;
    END IF;

    -- Releases probe (`library_releases_statement` shape): the release
    -- plus artist join for the page's well-formed release MBIDs ($1);
    -- the unknown bbbbbbbb release returns nothing. Both page branches
    -- carry release MBIDs, so an unmatched upload naming a release
    -- enriches here too.
    SELECT array_agg(release_mbid ORDER BY release_mbid) INTO actual FROM (
        SELECT r.mbid::text AS release_mbid, r.title AS release_title,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        WHERE r.mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
    ) s;
    IF actual IS DISTINCT FROM ARRAY['aaaaaaaa-1111-2222-3333-444444444444',
                       'dddddddd-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'releases probe content wrong: %', actual;
    END IF;
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS release_mbid, a.name AS artist_name
        FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        WHERE r.mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
    ) s WHERE release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'
      AND artist_name = 'Fixture Artist';
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'releases probe missed the artist join';
    END IF;

    -- Membership reads (`catalogue_member_*_statement` shapes): distinct
    -- member MBIDs as raw stored text (no `::uuid` cast — the store
    -- normalizes in Rust, so uppercase and poisoned values flow through
    -- here and are dropped only at probe-bind time). The enumerations are
    -- uncapped: no LIMIT, so the whole ledger is visible to the bounded
    -- mirror responses.
    SELECT array_agg(release_mbid ORDER BY release_mbid COLLATE "C") INTO actual FROM (
        SELECT DISTINCT release_mbid FROM library
        WHERE user_id = 'user-lib-1' AND release_mbid IS NOT NULL
        ORDER BY release_mbid ASC
    ) s;
    IF actual IS DISTINCT FROM ARRAY['AAAAAAAA-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444',
                       'bbbbbbbb-1111-2222-3333-444444444444',
                       'dddddddd-1111-2222-3333-444444444444',
                       'not-a-uuid'] THEN
        RAISE EXCEPTION 'member releases wrong: %', actual;
    END IF;
    SELECT array_agg(pair ORDER BY pair COLLATE "C") INTO actual FROM (
        SELECT DISTINCT l.release_mbid AS release_mbid, u.recording_mbid AS recording_mbid,
               l.release_mbid || ':' || u.recording_mbid AS pair
        FROM library l
        JOIN LATERAL (SELECT value AS recording_mbid
        FROM unnest(l.recording_mbids) AS u2(value)) AS u ON true
        WHERE l.user_id = 'user-lib-1' AND l.release_mbid IS NOT NULL
        AND coalesce(cardinality(l.recording_mbids), 0) > 0
        ORDER BY release_mbid ASC, recording_mbid ASC
    ) s;
    IF actual IS DISTINCT FROM ARRAY['AAAAAAAA-1111-2222-3333-444444444444:11111111-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444:11111111-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444:22222222-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444:33333333-1111-2222-3333-444444444444',
                       'bbbbbbbb-1111-2222-3333-444444444444:99999999-1111-2222-3333-444444444444',
                       'dddddddd-1111-2222-3333-444444444444:11111111-1111-2222-3333-444444444444',
                       'not-a-uuid:also-not-a-uuid'] THEN
        RAISE EXCEPTION 'member pairs wrong: %', actual;
    END IF;
    -- Release recordings (`catalogue_member_release_recordings_statement`
    -- shape): the caller's distinct recordings on one release, uncapped,
    -- case-insensitive like the detail gate (the uppercase job's recording
    -- collapses into the same member set the store probes).
    SELECT array_agg(recording_mbid ORDER BY recording_mbid) INTO actual FROM (
        SELECT DISTINCT u.recording_mbid AS recording_mbid
        FROM library l
        JOIN LATERAL (SELECT value AS recording_mbid
        FROM unnest(l.recording_mbids) AS u2(value)) AS u ON true
        WHERE l.user_id = 'user-lib-1'
          AND lower(l.release_mbid) = 'aaaaaaaa-1111-2222-3333-444444444444'
          AND coalesce(cardinality(l.recording_mbids), 0) > 0
        ORDER BY recording_mbid ASC
    ) s;
    IF actual IS DISTINCT FROM ARRAY['11111111-1111-2222-3333-444444444444',
                       '22222222-1111-2222-3333-444444444444',
                       '33333333-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'member release recordings wrong: %', actual;
    END IF;
    -- A release the caller never touched yields no member recordings.
    SELECT count(*) INTO row_count_var FROM (
        SELECT DISTINCT u.recording_mbid AS recording_mbid
        FROM library l
        JOIN LATERAL (SELECT value AS recording_mbid
        FROM unnest(l.recording_mbids) AS u2(value)) AS u ON true
        WHERE l.user_id = 'user-lib-2'
          AND lower(l.release_mbid) = 'dddddddd-1111-2222-3333-444444444444'
          AND coalesce(cardinality(l.recording_mbids), 0) > 0
    ) s;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'non-member release must own no member recordings';
    END IF;
    -- Detail gate (`catalogue_release_membership_statement` shape):
    -- case-insensitive, so the uppercase job counts as membership of the
    -- lowercase release; a non-member release reads zero rows.
    SELECT count(*) INTO row_count_var FROM (
        SELECT 1 FROM library
        WHERE user_id = 'user-lib-1'
          AND lower(release_mbid) = 'aaaaaaaa-1111-2222-3333-444444444444' LIMIT 1
    ) s;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'detail gate missed a member release';
    END IF;
    SELECT count(*) INTO row_count_var FROM (
        SELECT 1 FROM library
        WHERE user_id = 'user-lib-2'
          AND lower(release_mbid) = 'dddddddd-1111-2222-3333-444444444444' LIMIT 1
    ) s;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'detail gate found a non-member release';
    END IF;
    -- user-lib-2 owns exactly one release and one pair: the membership
    -- enumerations are uncapped (the 55 identical jobs collapse to one
    -- distinct member each).
    SELECT array_agg(release_mbid ORDER BY release_mbid) INTO actual FROM (
        SELECT DISTINCT release_mbid FROM library
        WHERE user_id = 'user-lib-2' AND release_mbid IS NOT NULL
        ORDER BY release_mbid ASC
    ) s;
    IF actual IS DISTINCT FROM ARRAY['aaaaaaaa-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'user-lib-2 member releases wrong: %', actual;
    END IF;

    -- Empty enrichment keys probe zero rows (a page with no well-formed
    -- MBIDs still binds the arrays; the casts must hold on empty input).
    SELECT count(*) INTO row_count_var FROM mb_recording WHERE mbid = ANY('{}'::uuid[]);
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty recordings probe must return no rows';
    END IF;
    -- Empty member-track pairs probe zero rows: with no pairs the exact-pair
    -- join (`JOIN unnest($1, $2)`) binds two empty uuid arrays and matches
    -- nothing; the casts must hold on empty input.
    SELECT count(*) INTO row_count_var FROM mb_track t
    JOIN unnest('{}'::uuid[], '{}'::uuid[]) AS p(rel, rec)
      ON t.release_mbid = p.rel AND t.recording_mbid = p.rec;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty tracks probe must return no rows';
    END IF;
    SELECT count(*) INTO row_count_var FROM mb_release r WHERE r.mbid = ANY('{}'::uuid[]);
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty releases probe must return no rows';
    END IF;
    -- Empty member arrays bind the same way: a caller with no ledger owns
    -- no artists, no releases, and no tracks.
    SELECT count(*) INTO row_count_var FROM mb_artist
    WHERE mbid IN (SELECT r.artist_mbid FROM mb_release r WHERE r.mbid = ANY('{}'::uuid[]));
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty member artist list must return no rows';
    END IF;
    SELECT count(*) INTO row_count_var FROM mb_release r WHERE r.mbid = ANY('{}'::uuid[]);
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty member release list must return no rows';
    END IF;
    SELECT count(*) INTO row_count_var FROM mb_track t
    JOIN unnest('{}'::uuid[], '{}'::uuid[]) AS p(rel, rec)
      ON t.release_mbid = p.rel AND t.recording_mbid = p.rec;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty member track list must return no rows';
    END IF;

    -- Per-user isolation: no user-lib-2 row may appear on the lib-1 page.
    SELECT count(*) INTO row_count_var FROM lib_page WHERE job_id LIKE 'lib-bound-%';
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'library page leaked another user''s rows';
    END IF;

    -- LIMIT bound: 55 jobs in, exactly 50 rows out.
    SELECT count(*) INTO row_count_var FROM (
        SELECT l.job_id, l.release_mbid, l.recording_mbids, u.recording_mbid, u.recording_ord,
               (l.release_mbid IS NULL) AS matched_last
        FROM library l
        LEFT JOIN LATERAL (SELECT value AS recording_mbid, ord AS recording_ord
               FROM unnest(l.recording_mbids) WITH ORDINALITY AS u2(value, ord)) AS u ON true
        WHERE l.user_id = 'user-lib-2' AND coalesce(cardinality(l.recording_mbids), 0) > 0
        UNION ALL
        SELECT l.job_id, l.release_mbid, l.recording_mbids, NULL::text AS recording_mbid,
               0::bigint AS recording_ord, (l.release_mbid IS NULL) AS matched_last
        FROM library l
        WHERE l.user_id = 'user-lib-2' AND coalesce(cardinality(l.recording_mbids), 0) = 0
        ORDER BY matched_last ASC, job_id DESC, recording_ord ASC LIMIT 50
    ) s;
    IF row_count_var <> 50 THEN
        RAISE EXCEPTION 'library bound did not cap at 50: %', row_count_var;
    END IF;

    -- Artist collection (`artists_list_statement` shape): MBID order,
    -- CHAR(2) country decoded, scoped to the caller's member releases
    -- (literals stand in for user-lib-1's well-formed $1 array: both
    -- releases plus the unknown bbbbbbbb, which simply has no mirror row).
    SELECT array_agg(mbid::text ORDER BY mbid) INTO actual FROM (
        SELECT mbid::text AS mbid, name, sort_name, type, country::text AS country
        FROM mb_artist WHERE mbid IN (SELECT r.artist_mbid FROM mb_release r
        WHERE r.mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[]))
        ORDER BY mbid ASC LIMIT 50
    ) s;
    IF actual IS DISTINCT FROM ARRAY['eeeeeeee-1111-2222-3333-444444444444',
                       'ffffffff-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'artist collection order wrong: %', actual;
    END IF;
    -- user-lib-2's single member release owns exactly one artist.
    SELECT array_agg(mbid::text ORDER BY mbid) INTO actual FROM (
        SELECT mbid::text AS mbid FROM mb_artist
        WHERE mbid IN (SELECT r.artist_mbid FROM mb_release r
        WHERE r.mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444}'::uuid[]))
        ORDER BY mbid ASC LIMIT 50
    ) s;
    IF actual IS DISTINCT FROM ARRAY['ffffffff-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'user-lib-2 artist collection wrong: %', actual;
    END IF;

    -- Release collection (`releases_list_statement` shape) carries the
    -- artist join, scoped to the same member releases.
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        WHERE r.mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
        ORDER BY r.mbid ASC LIMIT 50
    ) s WHERE mbid = 'aaaaaaaa-1111-2222-3333-444444444444'
      AND artist_name = 'Fixture Artist' AND year = 1983;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'release collection missed the artist join';
    END IF;
    -- The unknown member release contributes nothing: two mirror rows.
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid
        FROM mb_release r
        WHERE r.mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
        ORDER BY r.mbid ASC LIMIT 50
    ) s;
    IF row_count_var <> 2 THEN
        RAISE EXCEPTION 'member release collection wrong: %', row_count_var;
    END IF;

    -- Track collection (`tracks_list_statement` shape): release, then true
    -- album position, joining the member pairs ($1, $2) as aligned unnest
    -- columns so each pair binds one exact (release, recording) join key.
    -- The columns below are user-lib-1's distinct normalized pairs in
    -- `member_pair_columns` order (job E's uppercase release merges with
    -- job A's; job B's unknown release matches no mirror row). The
    -- (second release, Track Two) row has no (dddddddd, 22222222) member
    -- pair (job F pairs dddddddd with 11111111 only), so the join never
    -- returns it — unlike the old independent-ANY probe, nothing is left
    -- for a post-filter to drop.
    -- (track_no is SMALLINT: cast to text so the TEXT[] accumulator matches.)
    SELECT array_agg(track_no::text ORDER BY release_mbid, medium_no, track_no) INTO actual FROM (
        SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid,
               rec.title, rec.duration_ms, t.medium_no, t.track_no, r.title AS release_title,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        JOIN mb_release r ON r.mbid = t.release_mbid
        LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        JOIN unnest('{aaaaaaaa-1111-2222-3333-444444444444,aaaaaaaa-1111-2222-3333-444444444444,aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[],
                    '{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444,11111111-1111-2222-3333-444444444444}'::uuid[]) AS p(rel, rec)
          ON t.release_mbid = p.rel AND t.recording_mbid = p.rec
        ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT 50
    ) s;
    IF actual IS DISTINCT FROM ARRAY['1', '2', '3', '1'] THEN
        RAISE EXCEPTION 'track collection order wrong: %', actual;
    END IF;
    -- user-lib-2's single member pair probes exactly one occurrence: one
    -- aligned pair row binds one exact (release, recording) join key.
    SELECT count(*) INTO row_count_var FROM (
        SELECT t.release_mbid::text AS release_mbid
        FROM mb_track t
        JOIN unnest('{aaaaaaaa-1111-2222-3333-444444444444}'::uuid[],
                    '{11111111-1111-2222-3333-444444444444}'::uuid[]) AS p(rel, rec)
          ON t.release_mbid = p.rel AND t.recording_mbid = p.rec
        ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT 50
    ) s;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'user-lib-2 track collection wrong: %', row_count_var;
    END IF;

    -- Release detail: the membership gate runs first (pinned above), then
    -- the known read returns one row, unknown reads none (the 404 probe),
    -- and tracks arrive in album order.
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid, r.title, r.release_type, r.year,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        WHERE r.mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::text::uuid
    ) s;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'release detail missed a known release';
    END IF;
    SELECT count(*) INTO row_count_var FROM (
        SELECT r.mbid::text AS mbid FROM mb_release r
        WHERE r.mbid = 'cccccccc-1111-2222-3333-444444444444'::text::uuid
    ) s;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'release detail found an unknown release';
    END IF;
    -- (`release_tracks_statement` shape): the member-recordings filter
    -- (`AND recording_mbid = ANY($2)`) keeps member occurrences only, in
    -- album order. The literals stand in for user-lib-1's member recordings
    -- on aaaaaaaa (111/222/333): unowned 444 is on the release in the mirror
    -- but drops out of the member detail, as does non-member cccccccc.
    SELECT array_agg(recording_mbid ORDER BY medium_no, track_no, recording_mbid) INTO actual FROM (
        SELECT t.recording_mbid::text AS recording_mbid, rec.title, rec.duration_ms,
               t.medium_no, t.track_no
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        WHERE t.release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::text::uuid
          AND t.recording_mbid = ANY('{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444}'::uuid[])
        ORDER BY t.medium_no ASC, t.track_no ASC, rec.mbid ASC LIMIT 1000
    ) s;
    IF actual IS DISTINCT FROM ARRAY['11111111-1111-2222-3333-444444444444',
                       '22222222-1111-2222-3333-444444444444',
                       '33333333-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'release tracks order wrong: %', actual;
    END IF;
END $$;

-- Shared publication/read-back uses the exact included production statements.
CREATE TEMPORARY TABLE shared_publish_query (sql TEXT NOT NULL);
INSERT INTO shared_publish_query VALUES (:'library_shared_upsert_sql');
DO $$
DECLARE
    caller TEXT;
    page_row RECORD;
    publish_count INTEGER;
    page_count INTEGER;
    before_replay INTEGER;
    members UUID[];
    recordings UUID[];
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-lib-shared', 'lib-shared@example.com', 1700000000);
    INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('job_shared', 'user-lib-1', 'ingest:fixture:shared', 1700000000, 'done');
    INSERT INTO mb_release (mbid, artist_mbid, title)
    VALUES ('cccccccc-1111-2222-3333-444444444444',
            'ffffffff-1111-2222-3333-444444444444', 'Shared Release');
    INSERT INTO mb_recording (mbid, release_mbid, title)
    VALUES ('55555555-1111-2222-3333-444444444444',
            'cccccccc-1111-2222-3333-444444444444', 'Shared Track');
    INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no)
    VALUES ('cccccccc-1111-2222-3333-444444444444',
            '55555555-1111-2222-3333-444444444444', 1, 1);
    -- No library owner is bound, even while the acting admin is deleting.
    UPDATE users SET deleting_at = to_timestamp(1700000001) WHERE id = 'user-lib-1';
    FOR publish_count IN 1..2 LOOP
        EXECUTE (SELECT sql FROM shared_publish_query)
            USING 'job_shared'::text, 'ingest:fixture:shared'::text,
                  'cccccccc-1111-2222-3333-444444444444'::text,
                  ARRAY['55555555-1111-2222-3333-444444444444']::text[];
        IF (SELECT count(*) FROM library WHERE idempotency_key = 'ingest:fixture:shared'
            AND user_id IS NULL) <> 1 THEN
            RAISE EXCEPTION 'shared publish must converge on one NULL-owned row';
        END IF;
        FOREACH caller IN ARRAY ARRAY['user-lib-1', 'user-lib-shared'] LOOP
            page_count := 0;
            FOR page_row IN EXECUTE (SELECT sql FROM catalogue_query) USING caller, 50::bigint LOOP
                page_count := page_count + 1;
                IF page_row.job_id = 'job_shared' THEN
                    IF page_row.recording_mbid <> '55555555-1111-2222-3333-444444444444' THEN
                        RAISE EXCEPTION 'shared recording missing';
                    END IF;
                ELSIF caller = 'user-lib-shared' THEN
                    RAISE EXCEPTION 'fresh reader received a private row';
                END IF;
            END LOOP;
            IF page_count <> (CASE WHEN caller = 'user-lib-1' THEN 9 ELSE 1 END) THEN
                RAISE EXCEPTION 'own plus shared page count wrong: %', page_count;
            END IF;
            IF caller = 'user-lib-shared' THEN
                IF publish_count = 1 THEN before_replay := page_count;
                ELSIF page_count <> before_replay THEN
                    RAISE EXCEPTION 'replay changed catalogue read-back count';
                END IF;
            END IF;
        END LOOP;
    END LOOP;
    -- An existing publishing job cannot promote a private idempotency-key collision.
    EXECUTE (SELECT sql FROM shared_publish_query)
        USING 'job_shared'::text, 'lib:key:A'::text,
              'cccccccc-1111-2222-3333-444444444444'::text,
              ARRAY['55555555-1111-2222-3333-444444444444']::text[];
    GET DIAGNOSTICS page_count = ROW_COUNT;
    IF page_count <> 0 THEN RAISE EXCEPTION 'shared publish updated a private row'; END IF;
    IF NOT EXISTS (SELECT 1 FROM library WHERE idempotency_key = 'lib:key:A'
        AND user_id = 'user-lib-1' AND job_id = 'lib-job-A'
        AND release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'
        AND recording_mbids = ARRAY['11111111-1111-2222-3333-444444444444',
            '22222222-1111-2222-3333-444444444444',
            '33333333-1111-2222-3333-444444444444']::text[]) THEN
        RAISE EXCEPTION 'private collision changed ownership or content';
    END IF;
    page_count := 0;
    FOR page_row IN EXECUTE (SELECT sql FROM catalogue_query)
        USING 'user-lib-shared'::text, 50::bigint LOOP
        page_count := page_count + 1;
        IF page_row.job_id <> 'job_shared' THEN
            RAISE EXCEPTION 'private collision leaked to a fresh reader';
        END IF;
    END LOOP;
    IF page_count <> 1 THEN RAISE EXCEPTION 'private collision changed fresh read-back'; END IF;

    -- A missing job refuses both an initial insert and a conflicting replay.
    EXECUTE (SELECT sql FROM shared_publish_query)
        USING 'job_missing'::text, 'ingest:fixture:missing'::text, NULL::text, '{}'::text[];
    GET DIAGNOSTICS page_count = ROW_COUNT;
    IF page_count <> 0 THEN RAISE EXCEPTION 'missing job published a shared row'; END IF;
    DELETE FROM ingest_jobs WHERE id = 'job_shared';
    EXECUTE (SELECT sql FROM shared_publish_query)
        USING 'job_shared'::text, 'ingest:fixture:shared'::text, NULL::text, '{}'::text[];
    GET DIAGNOSTICS page_count = ROW_COUNT;
    IF page_count <> 0 THEN RAISE EXCEPTION 'missing job overwrote an existing shared row'; END IF;

    -- The fresh reader's browse/detail membership admits only the shared release.
    SELECT array_agg(release_mbid::uuid) INTO members FROM (
        SELECT DISTINCT release_mbid FROM library
        WHERE (user_id = 'user-lib-shared' OR user_id IS NULL) AND release_mbid IS NOT NULL
    ) s;
    IF members IS DISTINCT FROM ARRAY['cccccccc-1111-2222-3333-444444444444'::uuid] THEN
        RAISE EXCEPTION 'shared browse membership leaked private or non-member releases';
    END IF;
    IF (SELECT count(*) FROM mb_release WHERE mbid = ANY(members)) <> 1
       OR (SELECT count(*) FROM mb_artist WHERE mbid IN
           (SELECT artist_mbid FROM mb_release WHERE mbid = ANY(members))) <> 1 THEN
        RAISE EXCEPTION 'shared browse missed mirror titles/artist';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM library
        WHERE (user_id = 'user-lib-shared' OR user_id IS NULL)
        AND lower(release_mbid) = 'cccccccc-1111-2222-3333-444444444444')
       OR EXISTS (SELECT 1 FROM library
        WHERE (user_id = 'user-lib-shared' OR user_id IS NULL)
        AND lower(release_mbid) = 'dddddddd-1111-2222-3333-444444444444') THEN
        RAISE EXCEPTION 'shared release detail membership is not isolated';
    END IF;
    SELECT array_agg(recording_mbid::uuid) INTO recordings FROM (
        SELECT DISTINCT u.recording_mbid FROM library l
        CROSS JOIN unnest(l.recording_mbids) AS u(recording_mbid)
        WHERE (l.user_id = 'user-lib-shared' OR l.user_id IS NULL)
          AND lower(l.release_mbid) = 'cccccccc-1111-2222-3333-444444444444'
    ) s;
    IF (SELECT count(*) FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        WHERE t.release_mbid = ANY(members) AND t.recording_mbid = ANY(recordings)
          AND rec.title = 'Shared Track') <> 1 THEN
        RAISE EXCEPTION 'shared member detail missed its rich track';
    END IF;
    -- Exercise the page's empty-array UNION branch for shared rows too.
    INSERT INTO library (idempotency_key, job_id, user_id, recording_mbids)
    VALUES ('lib:shared:unmatched', 'job_shared_unmatched', NULL, '{}');
    page_count := 0;
    FOR page_row IN EXECUTE (SELECT sql FROM catalogue_query)
        USING 'user-lib-shared'::text, 50::bigint LOOP
        page_count := page_count + 1;
    END LOOP;
    IF page_count <> 2 THEN RAISE EXCEPTION 'shared unmatched row was hidden'; END IF;
END $$;

ROLLBACK;
