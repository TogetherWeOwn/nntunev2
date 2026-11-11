//! Per-account library-preference slice: durable likes, taste signals,
//! private listening sessions, and private-play marks.
//!
//! Native core for the `/v1/me/likes*`, `/v1/me/taste`,
//! `/v1/me/history`, and `/v1/me/private-session` routes. Pure validation
//! plus the parameterized statements the Worker entry binds against
//! PlanetScale Postgres via Hyperdrive — never string-interpolated SQL.
//!
//! Sources and rules (the HTTP layer maps store/auth failures; this
//! module never sees them):
//! - Owner scoping lives in the SQL: every statement filters or binds the
//!   server-authenticated `user_id`, and no statement accepts an owner
//!   from request data. Writes carry the `users.deleting_at IS NULL FOR
//!   SHARE` fence like every other user-owned writer, and all three
//!   tables FK to `users (id) ON DELETE CASCADE` so account erasure
//!   removes preference data with the account (migration 0041).
//! - Targets are validated catalogue members: a `track` target is the
//!   `id` already served by `GET /v1/library` / `GET /v1/tracks` — a
//!   recording MBID for matched rows or the ingest `job_id` for
//!   unmatched rows — and a `release` target is a release MBID.
//!   Membership resolves against the ingest `library` ledger (own rows
//!   or shared `user_id IS NULL` rows, ADR 0013); a non-member or
//!   malformed target is the handler's 404, indistinguishable (the same
//!   no-oracle posture as `GET /v1/releases/{mbid}`).
//! - Signals: one row per `(user_id, kind, target_id)` in
//!   `account_taste`; a new signal replaces the prior state (upsert).
//!   Likes are `signal = 'love'` written by the likes routes; unlike
//!   deletes a `love` row only, leaving a `hide`/`ban`/`snooze` intact.
//!   `snooze_until` is non-NULL only on `snooze` rows; expired snoozes
//!   read as cleared (list filters them server-side).
//! - Private listening: `private_sessions` rows name the caller's active
//!   private `(user_id, session_id)` pairs with a server-side TTL. At
//!   `POST /v1/plays` grant issuance an active match marks the minted
//!   play in `private_plays`, keyed by the derived
//!   `play:{user}:{track}:{play}` ledger idempotency key — before any
//!   bytes are metered, so a private play is suppressed from history
//!   and taste exposure from first use. Marks are written only at grant
//!   time: nothing back-fills old plays and nothing un-marks them.
//!   Billing is untouched — `stream_play_ledger` still records every
//!   metered byte; private plays consume plan hours exactly like public
//!   ones.
//! - History is real plays only: rows of `stream_play_ledger` (the
//!   billing truth) whose idempotency key carries the `play:` prefix
//!   and `bytes_served > 0` — metered delivery, never grant issuance or
//!   search — minus the `private_plays` marks.

/// Maximum rows the likes and taste lists return in one page. Slice 1
/// has no cursor (same posture as the catalogue collections); the bound
/// is bound as the statement `LIMIT`.
pub const TASTE_MAX_LIST: i64 = 500;

/// Maximum history rows one read returns: the recent-plays surface is a
/// bounded tail, not a cursor.
pub const HISTORY_MAX_LIST: i64 = 50;

/// Maximum items one likes-import body may carry. Per-item independent:
/// a rejected item never fails the batch and a retried import converges.
pub const IMPORT_MAX_ITEMS: usize = 500;

/// Bounded JSON allowance for 500 targets at the 512-byte target cap,
/// including item fields and ordinary serialization overhead.
pub const IMPORT_MAX_BODY_BYTES: usize = 320 * 1024;

/// Target id bound: opaque track ids (recording MBID or ingest `job_id`)
/// and release MBIDs share the ledger-column cap.
pub const MAX_TARGET_ID_LEN: usize = 512;

/// `target_id` echo bound inside an import `rejected` row: the echo
/// exists for client reconciliation, never for storage.
pub const IMPORT_REJECT_ID_MAX: usize = 64;

/// Snooze bounds: at least a minute, at most thirty days.
pub const SNOOZE_MIN_SECS: u64 = 60;
pub const SNOOZE_MAX_SECS: u64 = 2_592_000;

/// Server-side TTL for one private-session refresh.
pub const PRIVATE_SESSION_TTL_SECS: u64 = 21_600;

/// A signalable target class. Parsing is exact; anything else is the
/// caller's validation failure (body) or 404 (path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Track,
    Release,
}

impl TargetKind {
    pub fn parse(s: &str) -> Option<TargetKind> {
        match s {
            "track" => Some(TargetKind::Track),
            "release" => Some(TargetKind::Release),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TargetKind::Track => "track",
            TargetKind::Release => "release",
        }
    }
}

/// One durable taste signal. `Love` is the like; `Hide`/`Ban`/`Snooze`
/// are the suppression states; `Dislike` is the explicit negative.
/// Parsing is exact; anything else is `validation_failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TasteSignal {
    Love,
    Dislike,
    Hide,
    Ban,
    Snooze,
}

impl TasteSignal {
    pub fn parse(s: &str) -> Option<TasteSignal> {
        match s {
            "love" => Some(TasteSignal::Love),
            "dislike" => Some(TasteSignal::Dislike),
            "hide" => Some(TasteSignal::Hide),
            "ban" => Some(TasteSignal::Ban),
            "snooze" => Some(TasteSignal::Snooze),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TasteSignal::Love => "love",
            TasteSignal::Dislike => "dislike",
            TasteSignal::Hide => "hide",
            TasteSignal::Ban => "ban",
            TasteSignal::Snooze => "snooze",
        }
    }
}

/// One `account_taste` row as the store returns it. The HTTP layer maps
/// this into the wire item; stores never serialize it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasteDbRow {
    pub kind: String,
    pub target_id: String,
    pub signal: String,
    pub snooze_until: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One history row as the store returns it: `play_id` is parsed from the
/// persisted `play:{user}:{track}:{play}` ledger key server-side (the
/// key alphabet never carries a `:` inside a field), never echoed from
/// client input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDbRow {
    pub play_id: String,
    pub track_id: String,
    pub played_at: i64,
    pub stream_seconds: i64,
}

/// A target id the statements may bind: non-empty and under the column
/// cap. Shape beyond that (MBID for `release`) is the handler's check.
pub fn valid_target_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TARGET_ID_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn canonical_target_id(value: &str) -> String {
    if crate::catalogue::is_catalogue_mbid(value) {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    }
}

/// Extract the `play_id` field of a `play:{user}:{track}:{play}` ledger
/// key. Fields never contain `:` (`ledger::is_valid_key_field`), so a
/// plain split is exact; anything malformed yields `None` and the row is
/// skipped rather than half-parsed.
pub fn play_id_from_key(idempotency_key: &str) -> Option<String> {
    let rest = idempotency_key.strip_prefix("play:")?;
    let mut parts = rest.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(user), Some(track), Some(play_id), None)
            if crate::ledger::derive_play_idempotency_key(user, track, play_id).is_ok() =>
        {
            Some(play_id.to_string())
        }
        _ => None,
    }
}

/// Caller-member track target probe: true when the target names a
/// recording MBID carried by one of the caller's `library` rows (own or
/// shared) or the `job_id` of one such row. MBIDs compare normalized
/// (stored values are lowercase; `lower($2)` keeps a caller-supplied
/// uppercase MBID from silently missing); `job_id` compares exactly.
pub fn member_track_target_statement() -> &'static str {
    "SELECT 1 FROM public.library WHERE (user_id = $1 OR user_id IS NULL) AND (job_id = $2 OR EXISTS (SELECT 1 FROM unnest(recording_mbids) AS m WHERE lower(m) = lower($2))) LIMIT 1"
}

/// Caller-member release target probe: true when the target names the
/// release MBID of one of the caller's `library` rows (own or shared).
pub fn member_release_target_statement() -> &'static str {
    "SELECT 1 FROM public.library WHERE (user_id = $1 OR user_id IS NULL) AND lower(release_mbid) = lower($2) LIMIT 1"
}

/// The membership probe for a parsed kind.
pub fn member_target_statement(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Track => member_track_target_statement(),
        TargetKind::Release => member_release_target_statement(),
    }
}

/// Upsert one taste signal, fenced on the owner's `users` row
/// `FOR SHARE` like every user-owned writer: a deleting or nonexistent
/// account writes zero rows (the handler's `account_deleting` probe then
/// classifies the 409). `created_at` is the like time: a same-love replay
/// keeps the first stamp, while a non-love -> love transition (hide, ban,
/// dislike, live or expired snooze) restamps it to the write time so the
/// old signal's timestamp is never exposed as `liked_at`.
pub fn taste_upsert_statement() -> &'static str {
    "INSERT INTO public.account_taste (user_id, kind, target_id, signal, snooze_until, created_at, updated_at) SELECT $1, $2, $3, $4, $5, $6, $6 FROM (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, kind, target_id) DO UPDATE SET signal = EXCLUDED.signal, snooze_until = EXCLUDED.snooze_until, created_at = CASE WHEN EXCLUDED.signal = 'love' AND account_taste.signal <> 'love' THEN EXCLUDED.created_at ELSE account_taste.created_at END, updated_at = EXCLUDED.updated_at RETURNING 1"
}

/// Read back one stored row after a write (the likes PUT answers with
/// the persisted `created_at`/`signal`).
pub fn taste_get_statement() -> &'static str {
    "SELECT kind, target_id, signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND kind = $2 AND target_id = $3"
}

/// Unlike: delete only when the stored signal is `love` — a `hide`,
/// `ban` or `snooze` on the same target survives an unlike.
pub fn like_delete_statement() -> &'static str {
    "DELETE FROM public.account_taste USING (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND kind = $2 AND target_id = $3 AND signal = 'love' RETURNING 1"
}

/// Clear the whole stored signal on a target, whatever it is.
pub fn taste_delete_statement() -> &'static str {
    "DELETE FROM public.account_taste USING (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND kind = $2 AND target_id = $3 RETURNING 1"
}

/// The liked collection page: `love` rows newest-first with a
/// deterministic tiebreak, bounded.
pub fn likes_list_statement() -> &'static str {
    "SELECT kind, target_id, signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND signal = 'love' ORDER BY created_at DESC, kind ASC, target_id ASC LIMIT $2"
}

/// The taste collection page: every live signal newest-first. Expired
/// snoozes read as cleared — they are filtered here server-side, so the
/// caller never has to evaluate `snooze_until` itself.
pub fn taste_list_statement() -> &'static str {
    "SELECT kind, target_id, signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND (signal <> 'snooze' OR snooze_until IS NULL OR snooze_until > $2) ORDER BY updated_at DESC, kind ASC, target_id ASC LIMIT $3"
}

/// The recent-plays page: metered play rows of the billing ledger
/// (`play:`-prefixed idempotency keys with delivered bytes), minus the
/// caller's private-play marks. Grant issuance alone writes no ledger
/// row, so only real plays can appear.
pub fn history_list_statement() -> &'static str {
    "SELECT idempotency_key, track_id, recorded_at, stream_seconds::bigint AS stream_seconds FROM public.stream_play_ledger WHERE user_id = $1 AND idempotency_key LIKE 'play:%' AND bytes_served > 0 AND idempotency_key NOT IN (SELECT play_key FROM public.private_plays WHERE user_id = $1) ORDER BY recorded_at DESC, id DESC LIMIT $2"
}

/// Serialize privacy toggles and NEW-play issuance, including when no session
/// row exists. Hold until commit; probe privacy in a subsequent READ COMMITTED
/// statement so a toggle that committed while acquiring this lock is visible.
/// The user lock also fences account deletion. Continuations do not reclassify.
pub fn private_issuance_fence_statement() -> &'static str {
    "SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR UPDATE"
}

/// Upsert one private session with its server-side TTL, fenced like
/// every user-owned writer. A refresh replaces `expires_at`.
pub fn private_session_upsert_statement() -> &'static str {
    "INSERT INTO public.private_sessions (user_id, session_id, expires_at) SELECT $1, $2, $3 FROM (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, session_id) DO UPDATE SET expires_at = EXCLUDED.expires_at RETURNING 1"
}

/// Clear one private session (idempotent: a missing row deletes zero).
pub fn private_session_delete_statement() -> &'static str {
    "DELETE FROM public.private_sessions USING (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND session_id = $2 RETURNING 1"
}

/// Active private-session probe at grant issuance: true only when an
/// unexpired row names this `(user_id, session_id)`.
pub fn private_session_active_statement() -> &'static str {
    "SELECT 1 FROM public.private_sessions WHERE user_id = $1 AND session_id = $2 AND expires_at > $3 LIMIT 1"
}

/// Disambiguation probe for a zero-affected mark write: the mark exists
/// (convergent re-mark) or it does not (the writer fence refused it) —
/// the same pattern the ledger upsert prescribes.
pub fn private_play_probe_statement() -> &'static str {
    "SELECT 1 FROM public.private_plays WHERE user_id = $1 AND play_key = $2"
}

/// Mark one minted play private before its grant is issued, keyed by the
/// derived ledger idempotency key so the history read can subtract it.
/// Idempotent on the row key; fenced like every user-owned writer.
pub fn private_play_mark_statement() -> &'static str {
    "INSERT INTO public.private_plays (user_id, play_key, marked_at) SELECT $1, $2, $3 FROM (SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, play_key) DO NOTHING RETURNING 1"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_validation_and_normalization_preserve_opaque_ids() {
        let mbid = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA";
        assert_eq!(canonical_target_id(mbid), mbid.to_ascii_lowercase());
        assert_eq!(canonical_target_id("Job_A-7"), "Job_A-7");
        assert!(valid_target_id("Job_A-7"));
        for value in ["", "a/b", "a:b", "a\0b", "é", &"a".repeat(513)] {
            assert!(!valid_target_id(value), "{value:?}");
        }
    }

    #[test]
    fn history_key_parser_accepts_only_meterable_play_identities() {
        assert_eq!(play_id_from_key("play:u:t:p").as_deref(), Some("p"));
        for key in [
            "random-window",
            "play::t:p",
            "play:u::p",
            "play:u:t:",
            "play:u:t:p:extra",
            "play:u:t:p/invalid",
        ] {
            assert_eq!(play_id_from_key(key), None, "{key}");
        }
    }
}
