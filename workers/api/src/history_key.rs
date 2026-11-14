//! Inert parsing of canonical play ledger keys.
//!
//! Extracted from https://github.com/TogetherWeOwn/nntunev2/pull/982
//! (head ee56314b99f31a0a4d7dee53c450be6c376c16dc).

/// Extract the play identifier from a canonical `play:{user}:{track}:{play}` key.
///
/// Format validity alone does not establish ownership, delivery, metering truth,
/// or privacy eligibility.
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

#[cfg(test)]
mod tests {
    use super::play_id_from_key;
    use crate::ledger::derive_play_idempotency_key;

    #[test]
    fn canonical_keys_preserve_opaque_play_id_and_maximum_lengths() {
        for (user, track, play) in [
            ("u".to_string(), "t".to_string(), "p".to_string()),
            ("User_A-9".into(), "Track_Z-2".into(), "Play_Az-09_".into()),
            ("u".repeat(128), "t".repeat(512), "P".repeat(512)),
        ] {
            let key = derive_play_idempotency_key(&user, &track, &play).unwrap();
            assert_eq!(play_id_from_key(&key).as_deref(), Some(play.as_str()));
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
