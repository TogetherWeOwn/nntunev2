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
-- §6): member releases/pairs come from the app-side membership reads
-- (raw stored text, no `::uuid` cast — the `member_*` helpers normalize in
-- Rust), and the mirror lists probe exactly those member MBIDs. The
-- tracks list probe is still a SUPERSET of the member pairs; exact-pair
-- retention is Rust's job (`retain_member_track_occurrences`, pinned by
-- the `catalogue` unit tests). Rust assembly (`assemble_library_rows`) is
-- likewise pinned by the `catalogue` unit tests; this file pins the SQL
-- halves: the page's EMITTED order (read back with `row_number() over ()`,
-- never re-sorted), verbatim pass-through, probe contents including the
-- tracks-probe superset, the membership reads (distinct member MBIDs,
-- case-insensitive detail gate), the LIMIT bound, per-user isolation, the
-- three member-scoped mirror collections, and the release detail read
-- (membership gate plus album-ordered tracks).

BEGIN;

DO $$
DECLARE
    actual TEXT[];
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
            'aaaaaaaa-1111-2222-3333-444444444444'::uuid, 'Track Three', 3, 220000);

    -- The second release shares two recordings with the fixture release:
    -- the tracks probe below must return both rows even though no page
    -- job pairs the second recording with the second release (the probe
    -- is a superset; exact-pair matching is Rust's job).
    INSERT INTO mb_track (release_mbid, recording_mbid, medium_no, track_no)
    VALUES ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '11111111-1111-2222-3333-444444444444'::uuid, 1, 1),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '22222222-1111-2222-3333-444444444444'::uuid, 1, 2),
           ('aaaaaaaa-1111-2222-3333-444444444444'::uuid,
            '33333333-1111-2222-3333-444444444444'::uuid, 1, 3),
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

    -- Library page for user-lib-1: the exact `library_page_statement`
    -- shape (literals stand in for $1/$2). App relations only — no
    -- mirror table is named here.
    CREATE TEMPORARY TABLE lib_page AS
    SELECT l.job_id, l.release_mbid, l.recording_mbids, u.recording_mbid, u.recording_ord,
           (l.release_mbid IS NULL) AS matched_last
    FROM library l
    LEFT JOIN LATERAL (SELECT value AS recording_mbid, ord AS recording_ord
           FROM unnest(l.recording_mbids) WITH ORDINALITY AS u2(value, ord)) AS u ON true
    WHERE l.user_id = 'user-lib-1' AND coalesce(cardinality(l.recording_mbids), 0) > 0
    UNION ALL
    SELECT l.job_id, l.release_mbid, l.recording_mbids, NULL::text AS recording_mbid,
           0::bigint AS recording_ord, (l.release_mbid IS NULL) AS matched_last
    FROM library l
    WHERE l.user_id = 'user-lib-1' AND coalesce(cardinality(l.recording_mbids), 0) = 0
    ORDER BY matched_last ASC, job_id DESC, recording_ord ASC LIMIT 50;

    -- EMITTED order pin: `row_number() over ()` with no outer ORDER BY
    -- numbers rows in emission sequence, so this fails if the statement
    -- ever stops emitting matched-first (job DESC), album order within a
    -- job, and the unmatched upload last. Unlike an array_agg with its
    -- own ORDER BY, nothing here can mask the statement's own ordering.
    SELECT array_agg(job_id || ':' || recording_ord::text ORDER BY seq) INTO actual
    FROM (SELECT row_number() OVER () AS seq, job_id, recording_ord FROM lib_page) s;
    IF actual <> ARRAY['lib-job-F:1', 'lib-job-E:1', 'lib-job-D:1', 'lib-job-B:1',
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
    IF actual <> ARRAY['AAAAAAAA-1111-2222-3333-444444444444'] THEN
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
    IF actual <> ARRAY['11111111-1111-2222-3333-444444444444',
                       '22222222-1111-2222-3333-444444444444',
                       '33333333-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'recordings probe content wrong: %', actual;
    END IF;

    -- Tracks probe (`library_tracks_statement` shape): the two ANY
    -- probes are independent, so the result is a SUPERSET of the page's
    -- exact pairs — the (second release, Track Two) row exists here even
    -- though no page job pairs them. `assemble_library_rows` matches
    -- exact pairs only, so this row never enriches a listing.
    SELECT count(*) INTO row_count_var FROM (
        SELECT release_mbid::text AS release_mbid, recording_mbid::text AS recording_mbid,
               medium_no, track_no FROM mb_track
        WHERE release_mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
          AND recording_mbid = ANY('{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444}'::uuid[])
    ) s;
    IF row_count_var <> 5 THEN
        RAISE EXCEPTION 'tracks probe must return the 3 exact pairs plus 2 cross pairs: %', row_count_var;
    END IF;
    SELECT count(*) INTO row_count_var FROM (
        SELECT release_mbid::text AS release_mbid, recording_mbid::text AS recording_mbid
        FROM mb_track
        WHERE release_mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
          AND recording_mbid = ANY('{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444}'::uuid[])
    ) s
    WHERE release_mbid = 'dddddddd-1111-2222-3333-444444444444'
      AND recording_mbid = '22222222-1111-2222-3333-444444444444';
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'tracks probe missed the unpaired cross row';
    END IF;

    -- Occurrence positions ride the release's own rows.
    SELECT array_agg(medium_no::text || '.' || track_no::text ORDER BY medium_no, track_no) INTO actual FROM (
        SELECT medium_no, track_no FROM mb_track
        WHERE release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::uuid
    ) s;
    IF actual <> ARRAY['1.1', '1.2', '1.3'] THEN
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
    IF actual <> ARRAY['aaaaaaaa-1111-2222-3333-444444444444',
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
    -- here and are dropped only at probe-bind time).
    SELECT array_agg(release_mbid ORDER BY release_mbid) INTO actual FROM (
        SELECT DISTINCT release_mbid FROM library
        WHERE user_id = 'user-lib-1' AND release_mbid IS NOT NULL
        ORDER BY release_mbid ASC LIMIT 50
    ) s;
    IF actual <> ARRAY['AAAAAAAA-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444',
                       'bbbbbbbb-1111-2222-3333-444444444444',
                       'dddddddd-1111-2222-3333-444444444444',
                       'not-a-uuid'] THEN
        RAISE EXCEPTION 'member releases wrong: %', actual;
    END IF;
    SELECT array_agg(pair ORDER BY pair) INTO actual FROM (
        SELECT DISTINCT l.release_mbid AS release_mbid, u.recording_mbid AS recording_mbid,
               l.release_mbid || ':' || u.recording_mbid AS pair
        FROM library l
        JOIN LATERAL (SELECT value AS recording_mbid
        FROM unnest(l.recording_mbids) AS u2(value)) AS u ON true
        WHERE l.user_id = 'user-lib-1' AND l.release_mbid IS NOT NULL
        AND coalesce(cardinality(l.recording_mbids), 0) > 0
        ORDER BY release_mbid ASC, recording_mbid ASC LIMIT 50
    ) s;
    IF actual <> ARRAY['AAAAAAAA-1111-2222-3333-444444444444:11111111-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444:11111111-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444:22222222-1111-2222-3333-444444444444',
                       'aaaaaaaa-1111-2222-3333-444444444444:33333333-1111-2222-3333-444444444444',
                       'bbbbbbbb-1111-2222-3333-444444444444:99999999-1111-2222-3333-444444444444',
                       'dddddddd-1111-2222-3333-444444444444:11111111-1111-2222-3333-444444444444',
                       'not-a-uuid:also-not-a-uuid'] THEN
        RAISE EXCEPTION 'member pairs wrong: %', actual;
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
    -- user-lib-2 owns exactly one release and one pair (the LIMIT pin's
    -- 55 identical jobs collapse to one distinct member each).
    SELECT array_agg(release_mbid ORDER BY release_mbid) INTO actual FROM (
        SELECT DISTINCT release_mbid FROM library
        WHERE user_id = 'user-lib-2' AND release_mbid IS NOT NULL
        ORDER BY release_mbid ASC LIMIT 50
    ) s;
    IF actual <> ARRAY['aaaaaaaa-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'user-lib-2 member releases wrong: %', actual;
    END IF;

    -- Empty enrichment keys probe zero rows (a page with no well-formed
    -- MBIDs still binds the arrays; the casts must hold on empty input).
    SELECT count(*) INTO row_count_var FROM mb_recording WHERE mbid = ANY('{}'::uuid[]);
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'empty recordings probe must return no rows';
    END IF;
    SELECT count(*) INTO row_count_var FROM mb_track
    WHERE release_mbid = ANY('{}'::uuid[]) AND recording_mbid = ANY('{}'::uuid[]);
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
    IF actual <> ARRAY['eeeeeeee-1111-2222-3333-444444444444',
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
    IF actual <> ARRAY['ffffffff-1111-2222-3333-444444444444'] THEN
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
    -- album position, scoped to the member release ($1) and recording ($2)
    -- arrays. The probe is still a SUPERSET — the (second release, Track
    -- Two) row has no (dddddddd, 22222222) member pair for user-lib-1 (job
    -- F pairs dddddddd with 11111111 only), so the store's exact-pair
    -- retention drops it after the probe.
    -- (track_no is SMALLINT: cast to text so the TEXT[] accumulator matches.)
    SELECT array_agg(track_no::text ORDER BY release_mbid, medium_no, track_no) INTO actual FROM (
        SELECT t.release_mbid::text AS release_mbid, t.recording_mbid::text AS recording_mbid,
               rec.title, rec.duration_ms, t.medium_no, t.track_no, r.title AS release_title,
               r.artist_mbid::text AS artist_mbid, a.name AS artist_name
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        JOIN mb_release r ON r.mbid = t.release_mbid
        LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
        WHERE t.release_mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444,bbbbbbbb-1111-2222-3333-444444444444,dddddddd-1111-2222-3333-444444444444}'::uuid[])
          AND t.recording_mbid = ANY('{11111111-1111-2222-3333-444444444444,22222222-1111-2222-3333-444444444444,33333333-1111-2222-3333-444444444444,99999999-1111-2222-3333-444444444444}'::uuid[])
        ORDER BY t.release_mbid ASC, t.medium_no ASC, t.track_no ASC LIMIT 50
    ) s;
    IF actual <> ARRAY['1', '2', '3', '1', '2'] THEN
        RAISE EXCEPTION 'track collection order wrong: %', actual;
    END IF;
    -- user-lib-2's single member pair probes exactly one occurrence.
    SELECT count(*) INTO row_count_var FROM (
        SELECT t.release_mbid::text AS release_mbid
        FROM mb_track t
        WHERE t.release_mbid = ANY('{aaaaaaaa-1111-2222-3333-444444444444}'::uuid[])
          AND t.recording_mbid = ANY('{11111111-1111-2222-3333-444444444444}'::uuid[])
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
    SELECT array_agg(recording_mbid ORDER BY medium_no, track_no, recording_mbid) INTO actual FROM (
        SELECT t.recording_mbid::text AS recording_mbid, rec.title, rec.duration_ms,
               t.medium_no, t.track_no
        FROM mb_track t JOIN mb_recording rec ON rec.mbid = t.recording_mbid
        WHERE t.release_mbid = 'aaaaaaaa-1111-2222-3333-444444444444'::text::uuid
        ORDER BY t.medium_no ASC, t.track_no ASC, rec.mbid ASC LIMIT 1000
    ) s;
    IF actual <> ARRAY['11111111-1111-2222-3333-444444444444',
                       '22222222-1111-2222-3333-444444444444',
                       '33333333-1111-2222-3333-444444444444'] THEN
        RAISE EXCEPTION 'release tracks order wrong: %', actual;
    END IF;
END $$;

ROLLBACK;
