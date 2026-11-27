//! Inert parsing of canonical play ledger keys.
//!
//! Extracted from https://github.com/TogetherWeOwn/nntunev2/pull/982
//! (head ee56314b99f31a0a4d7dee53c450be6c376c16dc).

use crate::ledger::{
    is_valid_key_field, MAX_IDEMPOTENCY_KEY_LEN, MAX_PLAY_ID_LEN, MAX_TRACK_ID_LEN, MAX_USER_ID_LEN,
};

/// Extract the play identifier from a canonical `play:{user}:{track}:{play}` key.
///
/// Format validity alone does not establish ownership, delivery, metering truth,
/// or privacy eligibility.
pub fn play_id_from_key(idempotency_key: &str) -> Option<String> {
    key_parts(idempotency_key).map(|(_, _, play)| play.to_owned())
}

fn key_parts(key: &str) -> Option<(&str, &str, &str)> {
    if key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return None;
    }
    let (user, rest) = key.strip_prefix("play:")?.split_once(':')?;
    let (track, play) = rest.split_once(':')?;
    (is_valid_key_field(user, MAX_USER_ID_LEN)
        && is_valid_key_field(track, MAX_TRACK_ID_LEN)
        && is_valid_key_field(play, MAX_PLAY_ID_LEN))
    .then_some((user, track, play))
}

/// Latest `marked_at` the database accepts (`private_plays` CHECK).
pub const MAX_MARKED_AT: u64 = 253_402_300_799;

/// Mirror of the DB boundary `marked_at >= 0 AND marked_at <= 253402300799`.
pub fn valid_mark_time(marked_at: u64) -> bool {
    marked_at <= MAX_MARKED_AT
}

/// Validate a canonical key against the server-provided owner and track.
/// A valid binding alone is not evidence of a play or authority to issue one.
/// Adapted from PR #982's preserved patch.
pub fn matches_owner_track(key: &str, user: &str, track: &str) -> bool {
    key_parts(key).is_some_and(|(owner, target, _)| owner == user && target == track)
}

#[cfg(test)]
mod tests {
    use super::play_id_from_key;

    use crate::ledger::derive_play_idempotency_key;

    #[test]
    fn binding_requires_exact_owner_and_track() {
        let key = "play:Alice:Track:Play";
        assert!(super::matches_owner_track(key, "Alice", "Track"));
        for (key, owner, track) in [
            (key, "alice", "Track"),
            (key, "Alice", "track"),
            ("play:Bob:Track:Play", "Alice", "Track"),
            ("play:Alice:Track:Play:extra", "Alice", "Track"),
            ("play:Alice:Track:", "Alice", "Track"),
        ] {
            assert!(!super::matches_owner_track(key, owner, track));
        }
    }

    #[test]
    fn mark_time_matches_database_boundary() {
        assert!(super::valid_mark_time(0));
        assert!(super::valid_mark_time(253_402_300_799));
        assert!(!super::valid_mark_time(253_402_300_800));
        assert!(!super::valid_mark_time(u64::MAX));
    }

    #[test]
    fn canonical_keys_preserve_opaque_play_id_and_maximum_lengths() {
        for (user, track, play) in [
            ("u".to_string(), "t".to_string(), "p".to_string()),
            ("User_A-9".into(), "Track_Z-2".into(), "Play_Az-09_".into()),
            ("u".repeat(128), "t".repeat(512), "P".repeat(512)),
        ] {
            let key = derive_play_idempotency_key(&user, &track, &play).unwrap();
            assert_eq!(play_id_from_key(&key).as_deref(), Some(play.as_str()));
            assert!(super::matches_owner_track(&key, &user, &track));
        }
    }

    #[test]
    fn malformed_keys_are_rejected() {
        for key in [
            "random-window",
            "play::t:p",
            "play:u::p",
            "play:u:t:",
            "play:u:t:p:extra",
            "play:u:t:p/invalid",
            "",
            "play",
            "play:",
            "play:u",
            "play:u:t",
            "play:u:t:p:",
            ":play:u:t:p",
            "Play:u:t:p",
            "PLAY:u:t:p",
            "other:u:t:p",
            "play:u:t:p\n",
        ] {
            assert_eq!(play_id_from_key(key), None, "{key:?}");
        }
    }

    #[test]
    fn rejects_invalid_and_overlong_components_in_every_position() {
        for invalid in ["", " ", "a b", "\t", "\n", "\0", "é", "/", ".", "+", ":"] {
            for position in 0..3 {
                let mut fields = ["u", "t", "p"];
                fields[position] = invalid;
                let key = format!("play:{}:{}:{}", fields[0], fields[1], fields[2]);
                assert_eq!(play_id_from_key(&key), None, "{key:?}");
            }
        }
        for (position, length) in [(0, 129), (1, 513), (2, 513)] {
            let overlong = "x".repeat(length);
            let mut fields = ["u", "t", "p"];
            fields[position] = &overlong;
            let key = format!("play:{}:{}:{}", fields[0], fields[1], fields[2]);
            assert_eq!(play_id_from_key(&key), None, "position {position}");
        }
    }
}
