-- Opt-in staging contract. See docs/ops.md for the required catalog pin,
-- isolated writer OID, session inputs and withdrawal order.
BEGIN;
SET LOCAL search_path = pg_catalog;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';

DO $contract$
DECLARE
    action text := current_setting('nntune.private_trial_lock_action');
    writer_oid oid := current_setting('nntune.private_trial_writer_oid')::oid;
    writer_name name;
    relation regclass;
BEGIN
    IF current_database() <> 'nntunev2_staging' OR action NOT IN ('grant', 'revoke') THEN
        RAISE EXCEPTION 'private trial lock target or action refused';
    END IF;
    SELECT rolname INTO STRICT writer_name FROM pg_roles WHERE oid = writer_oid;

    IF action = 'grant' THEN
        IF NOT EXISTS (SELECT FROM pg_roles WHERE oid = writer_oid AND rolcanlogin
                       AND NOT (rolsuper OR rolinherit OR rolcreaterole OR rolcreatedb
                                OR rolreplication OR rolbypassrls))
           OR EXISTS (SELECT FROM pg_auth_members WHERE member = writer_oid
                      OR (roleid = writer_oid AND (inherit_option OR set_option))) THEN
            RAISE EXCEPTION 'private trial writer must be isolated';
        END IF;
        CREATE FUNCTION public.reject_private_trial_publication_update() RETURNS trigger
        LANGUAGE plpgsql SECURITY INVOKER SET search_path = pg_catalog AS $guard$
        BEGIN
            RAISE EXCEPTION 'private trial writer may only lock this relation'
                USING ERRCODE = '42501';
        END
        $guard$;
        REVOKE ALL ON FUNCTION public.reject_private_trial_publication_update() FROM PUBLIC;
        EXECUTE format('COMMENT ON FUNCTION public.reject_private_trial_publication_update() IS %L', writer_oid);
    ELSIF obj_description(to_regprocedure('public.reject_private_trial_publication_update()'), 'pg_proc')
              IS DISTINCT FROM writer_oid::text THEN
        RAISE EXCEPTION 'private trial lock writer does not match the installed grant';
    END IF;

    FOREACH relation IN ARRAY ARRAY['public.users', 'public.sessions', 'public.invite_codes',
                                     'public.ingest_jobs', 'public.tracks']::regclass[] LOOP
        IF action = 'grant' THEN
            IF has_any_column_privilege(writer_oid, relation, 'UPDATE') THEN
                RAISE EXCEPTION 'private trial writer already has UPDATE access';
            END IF;
            -- A statement trigger also rejects UPDATEs that would match zero rows.
            -- Resolve the pinned OID at execution so a role rename preserves the guard.
            EXECUTE format('CREATE TRIGGER private_trial_lock_only BEFORE UPDATE ON %s
                FOR EACH STATEMENT WHEN (CURRENT_USER = pg_catalog.pg_get_userbyid(%L::pg_catalog.oid))
                EXECUTE FUNCTION public.reject_private_trial_publication_update()', relation, writer_oid);
            EXECUTE format('ALTER TABLE %s ENABLE ALWAYS TRIGGER private_trial_lock_only', relation);
            EXECUTE format('GRANT UPDATE (id) ON %s TO %I', relation, writer_name);
            IF NOT has_column_privilege(writer_oid, relation, 'id', 'UPDATE') THEN
                RAISE EXCEPTION 'private trial lock grant did not take effect';
            END IF;
        ELSE
            EXECUTE format('REVOKE UPDATE (id) ON %s FROM %I', relation, writer_name);
            IF has_any_column_privilege(writer_oid, relation, 'UPDATE') THEN
                RAISE EXCEPTION 'private trial writer still has UPDATE access';
            END IF;
            EXECUTE format('DROP TRIGGER private_trial_lock_only ON %s', relation);
        END IF;
    END LOOP;

    IF action = 'revoke' THEN
        DROP FUNCTION public.reject_private_trial_publication_update();
    END IF;
END
$contract$;
COMMIT;
