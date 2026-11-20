-- ADR 0013: NULL library owner represents the shared catalogue.
SET search_path = public, pg_catalog;

ALTER TABLE public.library ALTER COLUMN user_id DROP NOT NULL;
