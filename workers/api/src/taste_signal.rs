//! Inert taste-signal vocabulary and snooze duration bounds.

/// Snooze durations are inclusive, from one minute to thirty days.
pub const SNOOZE_MIN_SECS: u64 = 60;
pub const SNOOZE_MAX_SECS: u64 = 2_592_000;

/// One durable taste signal, parsed case-sensitively.
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

/// Checks the inclusive snooze duration range, in seconds.
pub fn valid_snooze_seconds(seconds: u64) -> bool {
    (SNOOZE_MIN_SECS..=SNOOZE_MAX_SECS).contains(&seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_values_parse_and_serialize_exactly() {
        for (text, signal) in [
            ("love", TasteSignal::Love),
            ("dislike", TasteSignal::Dislike),
            ("hide", TasteSignal::Hide),
            ("ban", TasteSignal::Ban),
            ("snooze", TasteSignal::Snooze),
        ] {
            assert_eq!(TasteSignal::parse(text), Some(signal));
            assert_eq!(signal.as_str(), text);
            assert_eq!(TasteSignal::parse(signal.as_str()), Some(signal));
        }
    }

    #[test]
    fn signal_parser_rejects_unknown_and_case_variants() {
        for text in [
            "", "unknown", "Love", "LOVE", "Dislike", "DISLIKE", "Hide", "HIDE", "Ban", "BAN",
            "Snooze", "SNOOZE", " love", "love ", "snooze\n",
        ] {
            assert_eq!(TasteSignal::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn snooze_duration_boundaries_are_inclusive() {
        assert_eq!(SNOOZE_MIN_SECS, 60);
        assert_eq!(SNOOZE_MAX_SECS, 2_592_000);
        for seconds in [0, SNOOZE_MIN_SECS - 1, SNOOZE_MAX_SECS + 1, u64::MAX] {
            assert!(!valid_snooze_seconds(seconds), "{seconds}");
        }
        for seconds in [
            SNOOZE_MIN_SECS,
            SNOOZE_MIN_SECS + 1,
            SNOOZE_MAX_SECS - 1,
            SNOOZE_MAX_SECS,
        ] {
            assert!(valid_snooze_seconds(seconds), "{seconds}");
        }
    }
}
