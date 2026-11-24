UPDATE public.tracks SET duration_ms = 60000 WHERE id = $1::text::uuid RETURNING 1;
