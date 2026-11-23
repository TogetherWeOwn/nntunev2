BEGIN;
CREATE FUNCTION pg_temp.private_listening_access(text, text, bigint[])
RETURNS boolean LANGUAGE sql AS :'private_listening_access_sql';
\ir ../fixtures/private_listening_authority.sql
ROLLBACK;
