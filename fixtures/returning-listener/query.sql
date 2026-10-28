-- SQLite reference query for the in-memory synthetic fixture only.
-- All timestamps and :as_of are UTC Unix seconds. Never point this at real data.
WITH eligible_accounts AS (
  SELECT DISTINCT account_id, human_key
  FROM accounts
  WHERE account_class = 'eligible' AND consent = 1
    AND human_key IS NOT NULL AND trim(human_key, char(32, 9, 10, 11, 12, 13)) <> ''
),
humans AS (
  SELECT DISTINCT human_key FROM eligible_accounts
),
ranked_first AS (
  SELECT a.human_key, f.completed_at, f.session_id,
         ROW_NUMBER() OVER (
           PARTITION BY a.human_key ORDER BY f.completed_at, f.journey_id, f.session_id
         ) AS position
  FROM eligible_accounts a JOIN first_journeys f ON f.account_id = a.account_id
  WHERE f.completed_at <= :as_of AND f.session_id IS NOT NULL AND trim(f.session_id, char(32, 9, 10, 11, 12, 13)) <> ''
    AND f.authenticated = 1 AND f.provider_setup = 1 AND f.owned_content = 1
    AND f.advancing_audio = 1 AND f.range_seek = 1
),
firsts AS (
  SELECT human_key, completed_at, session_id FROM ranked_first WHERE position = 1
),
qualifying_returns AS (
  SELECT DISTINCT a.human_key, r.completed_at
  FROM eligible_accounts a
  JOIN returns r ON r.account_id = a.account_id
  JOIN firsts f ON f.human_key = a.human_key
  WHERE r.completed_at > f.completed_at AND r.completed_at <= f.completed_at + 7 * 86400
    AND r.completed_at <= :as_of AND r.session_id IS NOT NULL AND trim(r.session_id, char(32, 9, 10, 11, 12, 13)) <> ''
    AND r.session_id <> f.session_id AND r.authenticated = 1
    AND r.listener_initiated = 1 AND r.advancing_audio = 1
),
reconciled AS (
  SELECT 'human:' || h.human_key AS subject,
    CASE
      WHEN f.human_key IS NULL THEN 'no_qualifying_first'
      WHEN EXISTS (SELECT 1 FROM qualifying_returns r WHERE r.human_key = h.human_key
                   AND r.completed_at > :as_of - 30 * 86400) THEN 'counted'
      WHEN EXISTS (SELECT 1 FROM qualifying_returns r WHERE r.human_key = h.human_key)
        THEN 'returned_outside_window'
      WHEN :as_of <= f.completed_at + 7 * 86400 THEN 'pending'
      ELSE 'non_return'
    END AS classification
  FROM humans h LEFT JOIN firsts f ON f.human_key = h.human_key
  UNION ALL
  SELECT DISTINCT 'account:' || account_id,
    CASE WHEN account_class = 'eligible' AND consent = 1
              AND (human_key IS NULL OR trim(human_key, char(32, 9, 10, 11, 12, 13)) = '')
         THEN 'uncertain_identity' ELSE 'excluded' END
  FROM accounts
  WHERE account_class IS NULL OR account_class <> 'eligible' OR consent IS NOT 1
     OR human_key IS NULL OR trim(human_key, char(32, 9, 10, 11, 12, 13)) = ''
)
SELECT subject, classification FROM reconciled ORDER BY subject;
