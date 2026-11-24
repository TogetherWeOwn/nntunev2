WITH permitted AS MATERIALIZED (
    SELECT i.id FROM public.invite_codes i
    JOIN public.users recipient ON recipient.id = $1 AND recipient.id = i.redeemed_by
    JOIN public.users issuer ON issuer.id = $2 AND issuer.id = i.issued_by
    -- Check the private listening predicate on the locked rows themselves.
    WHERE i.id = $3 AND recipient.id <> issuer.id
      AND issuer.role = 'admin' AND issuer.deleting_at IS NULL AND recipient.deleting_at IS NULL
      AND i.kind = 'admin' AND i.email = recipient.email AND i.code_hash = recipient.invite_code_hash
      AND i.used AND i.redeemed_at < i.expires_at AND i.revoked_at IS NULL
    FOR SHARE OF i, recipient, issuer
), publication AS MATERIALIZED (
    SELECT i.id AS invite_id, s.track_id, s.accepted_seq, s.object_id
    FROM permitted i CROSS JOIN public.catalogue_s3_sources s
    JOIN public.storage_validated_digest_winners w USING (object_id)
    JOIN public.storage_library_namespaces n
      ON (n.id, n.environment_id) = (w.library_id, w.environment_id)
    WHERE s.track_id = $4::text::uuid AND s.source_job_id = $5 AND s.accepted_seq > 0
      AND (w.environment_id, w.library_id) = ($6::text::uuid, $7::text::uuid)
      AND w.plaintext_sha256 = $8 AND n.owner_id IS NULL
      AND extract(epoch FROM clock_timestamp()) < $9
      AND NOT w.quarantined AND w.deleting_at IS NULL
      AND w.retain_until > clock_timestamp() AND w.delete_until <= clock_timestamp()
    FOR SHARE OF s
), written AS (
    INSERT INTO public.private_library_admissions AS a (invite_id, recipient_id, track_id, accepted_seq, object_id)
    SELECT invite_id, $1, track_id, accepted_seq, object_id FROM publication
    ON CONFLICT (invite_id, track_id) DO UPDATE
      SET accepted_seq = EXCLUDED.accepted_seq, object_id = EXCLUDED.object_id
      WHERE a.recipient_id = EXCLUDED.recipient_id AND (a.accepted_seq < EXCLUDED.accepted_seq
         OR (a.accepted_seq, a.object_id) = (EXCLUDED.accepted_seq, EXCLUDED.object_id))
    RETURNING 1
)
SELECT EXISTS (SELECT FROM written)
