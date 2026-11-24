AND EXISTS (
    SELECT FROM public.sessions
    WHERE id = $4 AND user_id = $1 AND NOT revoked
      AND created_at = $5 AND expires_at = $6 AND rotation_counter = $7
)
