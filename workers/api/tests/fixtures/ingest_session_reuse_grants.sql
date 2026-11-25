-- Disposable-only reuse permissions. The existing winner lock reads the
-- winner row and requires UPDATE privilege. Reference ACLs stay separate.
GRANT SELECT, UPDATE (upload_until) ON public.storage_validated_digest_winners TO upload_fixture_writer;
GRANT EXECUTE ON FUNCTION public.lock_storage_digest_winner(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID)
    TO upload_fixture_writer;
