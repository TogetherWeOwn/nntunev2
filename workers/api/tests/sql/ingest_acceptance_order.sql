-- Acceptance order is database-owned, survives admission replay and never
-- follows caller clocks. Exercise a populated upgrade and migration replay
-- inside the disposable migration lane's transaction, then restore its schema.
BEGIN;

ALTER TABLE public.ingest_jobs DROP COLUMN accepted_seq;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM public.ingest_jobs) THEN
        RAISE EXCEPTION 'acceptance-order fixture requires the disposable empty job ledger';
    END IF;
END
$$;

\ir ../../../../migrations/0038_ingest_acceptance_order.sql

INSERT INTO public.users (id, email, created_at)
VALUES ('user-acceptance-order', 'acceptance-order@example.com', 1700000000);
INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
VALUES ('order-empty-first', 'user-acceptance-order', 'order-empty-first', 1700000000, 'queued');
DO $$
BEGIN
    IF (SELECT accepted_seq FROM public.ingest_jobs WHERE id = 'order-empty-first') <> 1 THEN
        RAISE EXCEPTION 'empty upgrade did not initialize the first acceptance at one';
    END IF;
END
$$;
DELETE FROM public.ingest_jobs WHERE id = 'order-empty-first';
ALTER TABLE public.ingest_jobs DROP COLUMN accepted_seq;

INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
VALUES ('order-z', 'user-acceptance-order', 'order-z', 1700000001, 'queued'),
       ('order-b', 'user-acceptance-order', 'order-b', 1700000000, 'queued'),
       ('order-a', 'user-acceptance-order', 'order-a', 1700000000, 'queued');

\ir ../../../../migrations/0038_ingest_acceptance_order.sql

DO $$
DECLARE
    historical_max BIGINT;
    earlier_seq BIGINT;
    later_seq BIGINT;
    consumed_seq BIGINT;
    replay_seq BIGINT;
BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT accepted_seq, row_number() OVER (ORDER BY accepted_at, id) AS expected
            FROM public.ingest_jobs
        ) ranked WHERE accepted_seq <> expected
    ) THEN
        RAISE EXCEPTION 'historical jobs were not numbered by accepted_at then id';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'ingest_jobs'
          AND column_name = 'accepted_seq' AND data_type = 'bigint'
          AND is_nullable = 'NO' AND identity_generation = 'ALWAYS'
    ) THEN
        RAISE EXCEPTION 'accepted_seq is not a required BIGINT GENERATED ALWAYS identity';
    END IF;
    SELECT max(accepted_seq) INTO historical_max FROM public.ingest_jobs;

    INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('order-future-clock', 'user-acceptance-order', 'order-future-clock',
            253402300799, 'queued') RETURNING accepted_seq INTO earlier_seq;
    INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('order-past-clock', 'user-acceptance-order', 'order-past-clock', 0, 'queued')
    RETURNING accepted_seq INTO later_seq;
    IF earlier_seq <= historical_max OR later_seq <= earlier_seq THEN
        RAISE EXCEPTION 'new jobs followed timestamps or collided with historical numbering';
    END IF;

    -- The deterministic production admission conflict keeps the original row's
    -- order; the attempted identity allocation may leave a harmless gap.
    INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('order-future-clock', 'user-acceptance-order', 'ignored-replay', 0, 'queued')
    ON CONFLICT (id) DO NOTHING;
    SELECT accepted_seq INTO replay_seq FROM public.ingest_jobs WHERE id = 'order-future-clock';
    IF replay_seq <> earlier_seq THEN
        RAISE EXCEPTION 'admission replay changed the accepted sequence';
    END IF;

    -- A rolled-back admission consumes an identity, not an ordering slot that
    -- may later be reused. The only supported comparison is greater-than.
    BEGIN
        INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
        VALUES ('order-rollback', 'user-acceptance-order', 'order-rollback', 0, 'queued');
        RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback admission';
    EXCEPTION WHEN no_data_found THEN
        NULL;
    END;
    consumed_seq := currval(pg_get_serial_sequence('public.ingest_jobs', 'accepted_seq'));
    INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('order-after-rollback', 'user-acceptance-order', 'order-after-rollback', 0, 'queued')
    RETURNING accepted_seq INTO later_seq;
    IF later_seq <= consumed_seq
       OR EXISTS (SELECT 1 FROM public.ingest_jobs WHERE id = 'order-rollback') THEN
        RAISE EXCEPTION 'rollback did not preserve a monotonic gap';
    END IF;

    BEGIN
        INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, status, accepted_seq)
        VALUES ('order-forged', 'user-acceptance-order', 'order-forged', 'queued', later_seq + 100);
        RAISE EXCEPTION 'an ordinary insert supplied its own acceptance order';
    EXCEPTION WHEN generated_always THEN
        NULL;
    END;
    BEGIN
        INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, status, accepted_seq)
        OVERRIDING SYSTEM VALUE
        VALUES ('order-duplicate', 'user-acceptance-order', 'order-duplicate', 'queued', later_seq);
        RAISE EXCEPTION 'duplicate accepted_seq was not rejected';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
END
$$;

CREATE TEMP TABLE acceptance_order_before_replay ON COMMIT DROP AS
SELECT id, accepted_seq FROM public.ingest_jobs;
-- Neither deleting the highest row nor consuming an unused value lets a
-- migration replay rewind the sequence or renumber surviving rows.
DELETE FROM public.ingest_jobs WHERE id = 'order-after-rollback';
CREATE TEMP TABLE acceptance_order_high_water ON COMMIT DROP AS
SELECT nextval(pg_get_serial_sequence('public.ingest_jobs', 'accepted_seq')) AS consumed;

\ir ../../../../migrations/0038_ingest_acceptance_order.sql

DO $$
DECLARE
    next_seq BIGINT;
BEGIN
    IF EXISTS (
        SELECT 1 FROM public.ingest_jobs j
        JOIN acceptance_order_before_replay b USING (id)
        WHERE j.accepted_seq <> b.accepted_seq
    ) THEN
        RAISE EXCEPTION 'migration replay renumbered existing jobs';
    END IF;
    INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('order-after-replay', 'user-acceptance-order', 'order-after-replay', 0, 'queued')
    RETURNING accepted_seq INTO next_seq;
    IF next_seq <= (SELECT consumed FROM acceptance_order_high_water) THEN
        RAISE EXCEPTION 'migration replay rewound the identity sequence';
    END IF;
END
$$;

ROLLBACK;
