-- Disposable hosted CI only. Seed the committed publication and immutable storage pins.
\ir storage_s3_locator_schema.sql
\ir ../../../ingest/tests/fixtures/catalogue_source_schema.sql
\ir ../../src/private_library_schema.sql

INSERT INTO public.users (id,email,created_at,role) VALUES
  ('cleanup-issuer','cleanup-owner@example.test',1,'admin'),
  ('cleanup-listener','cleanup-listener@example.test',1,'member');
INSERT INTO public.invite_codes (id,code_hash,used,created_at,issued_by,email,expires_at,redeemed_by,redeemed_at)
VALUES (1931,'cleanup-fixture-code-hash',true,1,'cleanup-issuer','cleanup-listener@example.test',20,'cleanup-listener',2);
INSERT INTO public.ingest_jobs (id,user_id,idempotency_key,accepted_at,status)
VALUES ('cleanup-fixture-job','cleanup-issuer','cleanup-fixture-job',1,'done');
INSERT INTO public.tracks (id,release_id,title,medium_no,track_no)
VALUES (:'track',gen_random_uuid(),'Synthetic cleanup control',1,1);

INSERT INTO public.storage_environments VALUES (:'env');
INSERT INTO public.storage_library_namespaces VALUES (:'lib', :'env', NULL);
INSERT INTO public.storage_object_intents VALUES
  (:'object', :'generation', :'env', :'lib', :'config', 177175, 177271, decode(repeat('00', 48), 'hex'));
INSERT INTO public.storage_plaintext_digest_claim_intents VALUES
  (:'object', :'env', :'lib', 177175, decode('c935d3368a1f2f22ca87b8ccfa6b9facae070e6e07be1e16abff513835a5f259', 'hex'));
INSERT INTO public.storage_digest_map_intents (map_id,object_id,environment_id,library_id,plaintext_bytes)
VALUES (:'map', :'object', :'env', :'lib', 177175);
INSERT INTO public.storage_digest_map_page_intents (map_id,chunk_count,page_index,expected_hash)
VALUES (:'map', 3, 0, decode(repeat('00', 32), 'hex'));
INSERT INTO public.storage_digest_inventory_intents VALUES (:'map', :'media_version', ARRAY[:'page_version']);
INSERT INTO public.storage_object_envelope_intents
SELECT object_id, :'root'::uuid, decode('4e4e544d454430310002', 'hex') || uuid_send(:'root')
    || uuid_send(environment_id) || uuid_send(library_id) || uuid_send(object_id)
    || uuid_send(generation_id) || uuid_send(provider_configuration_version_id)
    || int8send(plaintext_bytes) || int8send(envelope_bytes) || header_tag || decode(repeat('00', 60), 'hex')
FROM public.storage_object_intents WHERE object_id = :'object';
INSERT INTO public.storage_validated_digest_winners
    (object_id,environment_id,library_id,plaintext_bytes,plaintext_sha256,root_version_id,retain_until)
SELECT object_id,environment_id,library_id,plaintext_bytes,plaintext_sha256,:'root',clock_timestamp() + interval '1 hour'
FROM public.storage_plaintext_digest_claim_intents WHERE object_id = :'object';
INSERT INTO public.storage_s3_locator_intents (object_id,root_version_id,s3_bucket,s3_key,s3_version_id)
VALUES (:'object', :'root', :'bucket', :'media_key', :'media_version');
INSERT INTO public.ingest_session_reservations
  (id,library_id,principal_id,idempotency_key,fingerprint,absolute_deadline,idle_deadline,terminal_reason,
   terminal_at,completed_map_id,multipart_object_id,multipart_bucket)
VALUES (:'session', :'lib', 'cleanup-issuer', 'cleanup-fixture-job', decode(repeat('01', 32), 'hex'),
        statement_timestamp(), statement_timestamp(), 'completed', statement_timestamp(), :'map', :'object', :'bucket');
INSERT INTO public.catalogue_s3_sources
SELECT :'track', id, accepted_seq, :'object' FROM public.ingest_jobs WHERE id = 'cleanup-fixture-job';
INSERT INTO public.private_library_admissions
SELECT 1931, 'cleanup-listener', track_id, accepted_seq, object_id
FROM public.catalogue_s3_sources WHERE track_id = :'track';
INSERT INTO public.storage_validated_digest_references VALUES (:'env', :'lib', :'track', :'object');

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
