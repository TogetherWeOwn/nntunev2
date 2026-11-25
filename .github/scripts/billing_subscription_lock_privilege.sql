-- The owned billing writer takes SHARE ROW EXCLUSIVE on this relation, even
-- for a stale no-op. Check the contract's table-level UPDATE grant, including
-- inherited grants. Column-level UPDATE does not qualify for the lock.
SELECT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_roles AS writer
    CROSS JOIN pg_catalog.pg_class AS subscriptions
    JOIN pg_catalog.pg_namespace AS schema ON schema.oid = subscriptions.relnamespace
    WHERE pg_catalog.current_database()::text = :'database'
      AND writer.rolname::text = :'billing_role'
      AND writer.rolcanlogin
      AND schema.nspname = 'public'
      AND subscriptions.relname = 'subscriptions'
      AND subscriptions.relkind = 'r'
      AND pg_catalog.has_table_privilege(writer.oid, subscriptions.oid, 'UPDATE')
) AS subscription_table_update;
