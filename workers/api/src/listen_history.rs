//! Client-reported listening history, never an input to billing or play leases.
//!
//! History is retained for the account lifetime by default. Explicit history
//! clearing and account erasure remove it; meter receipts keep their own policy.

use serde::{Deserialize, Serialize};

pub const EVENTS_PATH: &str = "/v1/plays/events";
pub const EXPORT_PATH: &str = "/v1/plays/export";
pub const EXPORT_PAGE_SIZE: usize = 100;
pub const EVENTS_PER_MINUTE: u32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListenEvent {
    Start,
    Complete,
    Skip,
}

impl ListenEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Complete => "complete",
            Self::Skip => "skip",
        }
    }

    pub const fn phase(self) -> i16 {
        match self {
            Self::Start => 0,
            Self::Complete | Self::Skip => 1,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenEventBody {
    pub playback_id: String,
    pub track_id: String,
    pub event: ListenEvent,
}

impl ListenEventBody {
    pub fn valid(&self) -> bool {
        let id = self.playback_id.as_bytes();
        id.len() == 36
            && id.iter().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
                }
            })
            && !self.track_id.is_empty()
            && self.track_id.len() <= 128
            && !self.track_id.chars().any(char::is_control)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListenRow {
    /// Export cursor only; every read still filters on the authenticated owner.
    pub id: String,
    pub playback_id: String,
    pub track_id: String,
    pub event: ListenEvent,
    /// Server acceptance time, not a claim about elapsed streaming seconds.
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    Conflict,
    AccountDeleting,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportBody {
    pub before_id: Option<String>,
}

pub fn export_cursor(value: Option<&str>) -> Result<Option<i64>, &'static str> {
    match value {
        None => Ok(None),
        Some(value)
            if !value.is_empty()
                && value.len() <= 19
                && value.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .map(Some)
                .ok_or("invalid before_id")
        }
        Some(_) => Err("invalid before_id"),
    }
}

/// `$1` owner, `$2` playback UUID, `$3` track reference, `$4` event, `$5`
/// phase, `$6` server timestamp. Start and terminal occupy separate immutable
/// slots. A changed retry (including complete -> skip) returns no row.
/// Terminal events must match an existing start; no fabricated terminal-only
/// histories, and a late old-player terminal cannot end a different playback.
/// The user lock fences account deletion in the same statement as the write.
pub const INSERT_SQL: &str = include_str!("listen_history_insert.sql");

/// `$1` owner, `$2` exclusive cursor (nullable), `$3` bounded page size + 1.
/// The lookahead row determines next_cursor without an unbounded export query.
pub const EXPORT_SQL: &str = r#"
SELECT id, playback_id, track_id, event, recorded_at
FROM public.listen_events
WHERE user_id = $1 AND ($2::bigint IS NULL OR id < $2)
ORDER BY id DESC LIMIT $3
"#;

/// Explicit history clear and the account-erasure executor share this owner
/// predicate. No meter/outbox/lease tables participate. The users FK additionally
/// cascades account deletion, so an executor cannot leave orphan history behind.
pub const DELETE_SQL: &str = "DELETE FROM public.listen_events WHERE user_id = $1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_validation_bounds_identity_and_rejects_unknown_fields() {
        let valid = ListenEventBody {
            playback_id: "40c325c0-2cee-489d-a8b6-fbe937155671".into(),
            track_id: "catalogue-42".into(),
            event: ListenEvent::Start,
        };
        assert!(valid.valid());
        for playback_id in [
            "",
            "40c325c02cee489da8b6fbe937155671",
            "40C325C0-2CEE-489D-A8B6-FBE937155671",
        ] {
            assert!(!ListenEventBody {
                playback_id: playback_id.into(),
                ..valid.clone()
            }
            .valid());
        }
        for track_id in [String::new(), "x".repeat(129), "track\n".into()] {
            assert!(!ListenEventBody {
                track_id,
                ..valid.clone()
            }
            .valid());
        }
        assert!(serde_json::from_str::<ListenEventBody>(
            r#"{"playback_id":"40c325c0-2cee-489d-a8b6-fbe937155671","track_id":"t","event":"start","user_id":"other"}"#
        ).is_err());
        assert!(serde_json::from_str::<ListenEventBody>(
            r#"{"playback_id":"40c325c0-2cee-489d-a8b6-fbe937155671","track_id":"t","event":"pause"}"#
        ).is_err());
        assert_eq!(ListenEvent::Complete.phase(), ListenEvent::Skip.phase());
        assert_ne!(ListenEvent::Start.phase(), ListenEvent::Skip.phase());
    }

    #[test]
    fn export_cursor_is_bounded_positive_and_exclusive() {
        assert_eq!(export_cursor(None), Ok(None));
        assert_eq!(export_cursor(Some("42")), Ok(Some(42)));
        for value in ["", "0", "-1", "+1", " 1", "1;DELETE", "9223372036854775808"] {
            assert!(export_cursor(Some(value)).is_err(), "{value}");
        }
    }
}
