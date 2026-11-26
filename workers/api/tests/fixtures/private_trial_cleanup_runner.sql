-- Disposable hosted CI only.
\ir storage_s3_locator_schema.sql
\ir ../../../ingest/tests/fixtures/catalogue_source_schema.sql
\ir ../../src/private_library_schema.sql

CREATE ROLE trial_cleanup_fixture LOGIN PASSWORD 'disposable-cleanup-password';
GRANT SELECT ON public.storage_validated_digest_winners, public.storage_object_intents,
  public.storage_s3_locator_intents, public.storage_digest_map_intents, public.storage_library_namespaces,
  public.storage_digest_inventory_intents, public.ingest_session_reservations, public.invite_codes,
  public.ingest_jobs, public.catalogue_s3_sources, public.private_library_admissions,
  public.storage_validated_digest_references TO trial_cleanup_fixture;
GRANT UPDATE (deleting_at) ON public.storage_validated_digest_winners TO trial_cleanup_fixture;
GRANT UPDATE (accepted_seq) ON public.catalogue_s3_sources TO trial_cleanup_fixture;
GRANT DELETE ON public.catalogue_s3_sources, public.private_library_admissions,
  public.storage_validated_digest_references TO trial_cleanup_fixture;
GRANT EXECUTE ON FUNCTION public.admit_storage_digest_delete(uuid,uuid,bytea,bigint,uuid,uuid,uuid),
  public.lock_storage_digest_winner(uuid,uuid,bytea,bigint,uuid,uuid,uuid) TO trial_cleanup_fixture;
