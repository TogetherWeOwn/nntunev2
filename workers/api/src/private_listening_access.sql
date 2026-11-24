SELECT EXISTS (
    SELECT 1 FROM users u WHERE u.id = $1 AND u.deleting_at IS NULL
      AND ((u.id = $2 AND u.role = 'admin') OR EXISTS (
        SELECT 1 FROM invite_codes i JOIN users owner ON owner.id = i.issued_by
        WHERE i.id = ANY($3) AND i.kind = 'admin' AND i.issued_by = $2
          AND owner.role = 'admin' AND owner.deleting_at IS NULL
          AND i.code_hash = u.invite_code_hash AND i.email = u.email
          AND i.used AND i.redeemed_by = u.id AND i.redeemed_at IS NOT NULL
          AND i.redeemed_at < i.expires_at AND i.revoked_at IS NULL
      ))
)
