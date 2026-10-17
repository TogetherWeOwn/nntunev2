-- NNTune v2 migration 0007: bind challenge rows to their user.
--
-- `auth_challenges.user_id` was nullable without a foreign key, so a
-- concurrent `register_start` reap could delete a resuming user between
-- another request's `user_by_email` and `insert_challenge`: the insert then
-- succeeded orphaned and the ceremony died later at credential insert.
-- With this FK the concurrent delete fails fast at challenge insert, and
-- `register_start` heals by recreating the user under the same id and
-- retrying once. Unbound (NULL) login challenges for unknown emails are
-- unaffected. Orphaned rows from pre-FK deploys are removed first so the
-- constraint creates cleanly; challenges live at most five minutes anyway.

DELETE FROM auth_challenges
WHERE user_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM users WHERE users.id = auth_challenges.user_id);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'auth_challenges_user_id_fkey'
    ) THEN
        ALTER TABLE auth_challenges
            ADD CONSTRAINT auth_challenges_user_id_fkey
            FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
    END IF;
END
$$;
