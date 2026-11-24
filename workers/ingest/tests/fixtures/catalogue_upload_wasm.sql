INSERT INTO public.users (id, email, created_at) VALUES
    ('catalogue-owner', 'catalogue@example.invalid', 1),
    ('catalogue-other', 'other@example.invalid', 1);
INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('catalogue-old', 'catalogue-owner', 'first', 100, 'done');
INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
    VALUES ('catalogue-new', 'catalogue-owner', 'second', 101, 'done');
