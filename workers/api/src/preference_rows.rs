//! Inert store-facing preference and history row representations.

/// Read the exact resolved identity; callers filter expired snoozes when presenting taste.
pub fn taste_get_statement() -> &'static str {
    "SELECT kind, target_id, signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND kind = $2 AND target_id = $3"
}

pub fn taste_list_statement() -> &'static str {
    "SELECT kind, target_id, signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND (signal <> 'snooze' OR snooze_until > $2) ORDER BY updated_at DESC LIMIT 500"
}

/// Bounded owner-scoped likes collection.
pub fn likes_list_statement() -> &'static str {
    "SELECT kind, target_id, signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND signal = 'love' ORDER BY created_at DESC LIMIT 500"
}

/// Upsert under the account deletion fence, preserving a replayed love's timestamp.
/// Adapted from the preference writer in PR #982.
pub fn taste_upsert_statement() -> &'static str {
    "INSERT INTO public.account_taste (user_id, kind, target_id, signal, snooze_until, created_at, updated_at) SELECT $1, $2, $3, $4, $5, $6, $6 FROM (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, kind, target_id) DO UPDATE SET signal = EXCLUDED.signal, snooze_until = EXCLUDED.snooze_until, created_at = CASE WHEN EXCLUDED.signal = 'love' AND account_taste.signal <> 'love' THEN EXCLUDED.created_at ELSE account_taste.created_at END, updated_at = EXCLUDED.updated_at RETURNING kind, target_id, signal, snooze_until, created_at, updated_at"
}

/// Delete an exact row while holding the same account fence as the writer.
/// The final row distinguishes an absent account from an absent preference.
pub fn taste_delete_statement() -> &'static str {
    "WITH writer_fence AS MATERIALIZED (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE), deleted AS (DELETE FROM public.account_taste AS taste USING writer_fence WHERE taste.user_id = writer_fence.id AND taste.kind = $2 AND taste.target_id = $3 AND (NOT $4 OR taste.signal = 'love') RETURNING 1) SELECT EXISTS (SELECT 1 FROM deleted) FROM writer_fence"
}

/// Inert exact-owner idempotent mark, adapted from the private-play writer in
/// PR #982. The account fence yields no row for a missing or deleting owner;
/// a replay inserts nothing and keeps the first mark. A mark is never
/// authority to issue a grant.
pub fn private_play_mark_statement() -> &'static str {
    "WITH writer_fence AS MATERIALIZED (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE), inserted AS (INSERT INTO public.private_plays (user_id, play_key, marked_at) SELECT id, $2, $3 FROM writer_fence ON CONFLICT (user_id, play_key) DO NOTHING RETURNING 1) SELECT EXISTS (SELECT 1 FROM inserted) FROM writer_fence"
}

/// Inert exact-owner observation, adapted from PR #982; not authority to issue.
pub fn private_play_probe_statement() -> &'static str {
    "SELECT EXISTS (SELECT 1 FROM public.private_plays WHERE user_id = $1 AND play_key = $2) FROM public.users WHERE id = $1 AND deleting_at IS NULL"
}

/// Observation only: activation must recheck after acquiring the account fence.
pub fn private_session_active_statement() -> &'static str {
    "SELECT EXISTS (SELECT 1 FROM public.private_sessions WHERE user_id = $1 AND session_id = $2 AND expires_at > EXTRACT(EPOCH FROM statement_timestamp())) FROM public.users WHERE id = $1 AND deleting_at IS NULL AND char_length($2) BETWEEN 1 AND 128"
}

pub fn private_session_upsert_statement() -> &'static str {
    "INSERT INTO public.private_sessions (user_id, session_id, expires_at) VALUES ($1, $2, $3) ON CONFLICT (user_id, session_id) DO UPDATE SET expires_at = EXCLUDED.expires_at"
}

pub fn private_session_delete_statement() -> &'static str {
    "DELETE FROM public.private_sessions WHERE user_id = $1 AND session_id = $2"
}

/// Bounded metered plays with identities bound to their ledger owner and track.
/// Adapted from the history reader in PR #982; invalid identities are filtered
/// before the limit so they cannot displace valid recent plays.
pub fn history_list_statement() -> &'static str {
    "SELECT idempotency_key, track_id, recorded_at, stream_seconds::bigint AS stream_seconds FROM public.stream_play_ledger AS ledger WHERE user_id = $1 AND bytes_served > 0 AND user_id COLLATE \"C\" ~ '^[A-Za-z0-9_-]+$' AND track_id COLLATE \"C\" ~ '^[A-Za-z0-9_-]+$' AND octet_length(user_id) <= 128 AND octet_length(track_id) <= 512 AND split_part(idempotency_key, ':', 4) COLLATE \"C\" ~ '^[A-Za-z0-9_-]+$' AND octet_length(split_part(idempotency_key, ':', 4)) <= 512 AND idempotency_key = 'play:' || user_id || ':' || track_id || ':' || split_part(idempotency_key, ':', 4) AND NOT EXISTS (SELECT 1 FROM public.private_plays AS private WHERE private.user_id = ledger.user_id AND private.play_key = ledger.idempotency_key) ORDER BY recorded_at DESC, idempotency_key COLLATE \"C\" DESC LIMIT 50"
}

/// A taste row as returned by a store; its fields are not validated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasteDbRow {
    pub kind: String,
    pub target_id: String,
    pub signal: String,
    pub snooze_until: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A history row as returned by a store; its fields are not validated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDbRow {
    pub play_id: String,
    pub track_id: String,
    pub played_at: i64,
    pub stream_seconds: i64,
}

#[cfg(test)]
mod tests {
    use crate::preference_rows::{HistoryDbRow, TasteDbRow};

    #[test]
    fn taste_row_preserves_fields_and_optional_snooze() {
        let row = TasteDbRow {
            kind: "Album".into(),
            target_id: "Opaque-ID-aA".into(),
            signal: "Snooze".into(),
            snooze_until: Some(-7),
            created_at: -12,
            updated_at: 42,
        };
        assert_eq!(row.kind, "Album");
        assert_eq!(row.target_id, "Opaque-ID-aA");
        assert_eq!(row.signal, "Snooze");
        assert_eq!(row.snooze_until, Some(-7));
        assert_eq!(row.created_at, -12);
        assert_eq!(row.updated_at, 42);
        let mut copy = row.clone();
        assert_eq!(copy, row);
        copy.snooze_until = None;
        assert_eq!(copy.snooze_until, None);
        assert_ne!(copy, row);
        copy.snooze_until = row.snooze_until;
        copy.target_id = "opaque-id-aa".into();
        assert_ne!(copy, row);
        copy.target_id = row.target_id.clone();
        copy.updated_at = row.created_at;
        assert_ne!(copy, row);
    }

    #[test]
    fn history_row_preserves_fields_and_clone_equality() {
        let row = HistoryDbRow {
            play_id: "Play-aA".into(),
            track_id: "Track-Bb".into(),
            played_at: -123,
            stream_seconds: -9,
        };
        assert_eq!(row.play_id, "Play-aA");
        assert_eq!(row.track_id, "Track-Bb");
        assert_eq!(row.played_at, -123);
        assert_eq!(row.stream_seconds, -9);
        let mut copy = row.clone();
        assert_eq!(copy, row);
        copy.play_id = "play-aa".into();
        assert_ne!(copy, row);
        copy.play_id = row.play_id.clone();
        copy.stream_seconds = row.played_at;
        assert_ne!(copy, row);
    }
}
