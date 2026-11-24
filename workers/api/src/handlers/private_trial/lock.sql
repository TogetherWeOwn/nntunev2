SELECT recipient.id
FROM public.users issuer
JOIN public.sessions s ON s.user_id = issuer.id AND s.id = $2
JOIN public.invite_codes i ON i.issued_by = issuer.id AND i.id = $3
JOIN public.users recipient ON recipient.id = i.redeemed_by
WHERE issuer.id = $1 AND issuer.role = 'admin' AND issuer.deleting_at IS NULL
  AND NOT s.revoked AND s.created_at = $4 AND s.expires_at = $5 AND s.rotation_counter = $6
  AND extract(epoch FROM clock_timestamp()) < least($5, $8)
  AND floor(extract(epoch FROM clock_timestamp())) <= $7::bigint
  AND recipient.id <> issuer.id AND recipient.deleting_at IS NULL
  AND i.kind = 'admin' AND i.email = recipient.email AND i.email LIKE '%@example.test'
  AND i.code_hash = recipient.invite_code_hash AND i.used
  AND i.redeemed_at < i.expires_at AND i.revoked_at IS NULL
FOR SHARE OF issuer, s, i, recipient
