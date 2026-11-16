SET search_path = public, pg_catalog;

-- Inactive identities only: neither these rows nor their IDs confer authority.
CREATE TABLE IF NOT EXISTS public.storage_environments (
    id UUID PRIMARY KEY CHECK (id::text ~ '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$')
);

CREATE TABLE IF NOT EXISTS public.storage_library_namespaces (
    id UUID PRIMARY KEY CHECK (id::text ~ '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'),
    environment_id UUID NOT NULL REFERENCES public.storage_environments (id) ON DELETE RESTRICT,
    owner_id TEXT REFERENCES public.users (id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX IF NOT EXISTS storage_library_shared_per_environment
    ON public.storage_library_namespaces (environment_id) WHERE owner_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS storage_library_private_per_owner
    ON public.storage_library_namespaces (environment_id, owner_id) WHERE owner_id IS NOT NULL;

CREATE OR REPLACE FUNCTION public.storage_namespace_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'storage identity bindings are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS storage_environment_immutable ON public.storage_environments;
CREATE TRIGGER storage_environment_immutable BEFORE UPDATE ON public.storage_environments
    FOR EACH ROW EXECUTE FUNCTION public.storage_namespace_immutable();
DROP TRIGGER IF EXISTS storage_library_immutable ON public.storage_library_namespaces;
CREATE TRIGGER storage_library_immutable BEFORE UPDATE ON public.storage_library_namespaces
    FOR EACH ROW EXECUTE FUNCTION public.storage_namespace_immutable();
