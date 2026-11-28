SELECT pg_catalog.json_build_array(d.datname::text, d.oid, session_user::text, login_role.oid,
                                   current_user::text, effective_role.oid)::text
FROM pg_catalog.pg_database d
JOIN pg_catalog.pg_roles login_role ON login_role.rolname = session_user
JOIN pg_catalog.pg_roles effective_role ON effective_role.rolname = current_user
WHERE d.datname = pg_catalog.current_database();
