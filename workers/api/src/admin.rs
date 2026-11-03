//! Admin foundation vocabulary: user roles and the typed `app_settings`
//! keys (migration 0027). Pure decisions only — no DB, no clock — so the
//! guard, the handlers and the settings accessors share one definition
//! of what a role or a setting value may be.
//!
//! Roles are read fresh from `users.role` on every admin request; nothing
//! here is ever minted into a session token. Setting values are stored
//! as decimal strings in `app_settings.value`; this module owns the
//! parse, the bounds and the built-in default used when a row is absent
//! (a database that predates the seed behaves exactly like the seed).

/// The only two role values `users.role` may hold (CHECK constraint in
/// migration 0027). Anything else read back from the DB is not an admin.
pub const ROLE_MEMBER: &str = "member";
pub const ROLE_ADMIN: &str = "admin";

/// Exact-match role parse for request bodies: unknown, case or
/// whitespace variants reject — the allowlist never trims or folds.
pub fn parse_role(raw: &str) -> Option<&'static str> {
    match raw {
        ROLE_MEMBER => Some(ROLE_MEMBER),
        ROLE_ADMIN => Some(ROLE_ADMIN),
        _ => None,
    }
}

/// True iff a stored `users.role` value grants admin. A missing row
/// (`None`) or an unrecognized value never does.
pub fn is_admin_role(stored: Option<&str>) -> bool {
    stored == Some(ROLE_ADMIN)
}

/// Audit action for an admin-driven role change (actor in `user_id`,
/// target in `subject_user_id`, old-to-new in `details`).
pub const ROLE_CHANGED_AUDIT_ACTION: &str = "user.role_changed";

/// Audit action for an admin-driven settings write (old-to-new in
/// `details`).
pub const SETTINGS_CHANGED_AUDIT_ACTION: &str = "settings.changed";

/// One typed `app_settings` key. The set is closed: a key outside this
/// enum is not writable (or readable) through the admin API, so the
/// table cannot grow untyped free-form entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    /// Days an issued invite stays redeemable when the issuer does not
    /// pick an expiry.
    InviteDefaultExpiryDays,
    /// Cumulative paid months a member needs before they may issue
    /// member invites (post-M1; key reserved and seeded now).
    InviteMemberMinPaidMonths,
    /// Member invites issuable per member per invite window (post-M1;
    /// key reserved and seeded now).
    InviteMemberPerWindow,
}

/// Every typed key, in stable display order.
pub const ALL_SETTING_KEYS: &[SettingKey] = &[
    SettingKey::InviteDefaultExpiryDays,
    SettingKey::InviteMemberMinPaidMonths,
    SettingKey::InviteMemberPerWindow,
];

impl SettingKey {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKey::InviteDefaultExpiryDays => "invite_default_expiry_days",
            SettingKey::InviteMemberMinPaidMonths => "invite_member_min_paid_months",
            SettingKey::InviteMemberPerWindow => "invite_member_per_window",
        }
    }

    /// Exact-match key parse for request paths.
    pub fn parse(raw: &str) -> Option<SettingKey> {
        ALL_SETTING_KEYS
            .iter()
            .copied()
            .find(|key| key.as_str() == raw)
    }

    /// Value used when no `app_settings` row exists yet. Matches the
    /// migration 0027 seed, so a pre-seed database behaves identically.
    pub fn default_value(self) -> u32 {
        match self {
            SettingKey::InviteDefaultExpiryDays => 7,
            SettingKey::InviteMemberMinPaidMonths => 3,
            SettingKey::InviteMemberPerWindow => 2,
        }
    }

    /// Inclusive bounds for a written value. Invite expiry mirrors the
    /// per-invite 1-30 day admin range; the member-invite knobs accept
    /// 0 (feature effectively closed / no wait) up to generous caps.
    fn bounds(self) -> (u32, u32) {
        match self {
            SettingKey::InviteDefaultExpiryDays => (1, 30),
            SettingKey::InviteMemberMinPaidMonths => (0, 600),
            SettingKey::InviteMemberPerWindow => (0, 10_000),
        }
    }

    /// Validate a candidate value from a request body.
    pub fn validate(self, value: u32) -> Option<u32> {
        let (min, max) = self.bounds();
        (min..=max).contains(&value).then_some(value)
    }

    /// Parse a stored decimal string. Strict: digits only, no sign, no
    /// whitespace, no leading-zero or overflow games — and the result
    /// must satisfy the same bounds as a fresh write, so a hand-edited
    /// row outside the range fails closed instead of being honored.
    pub fn parse_stored(self, stored: &str) -> Option<u32> {
        if stored.is_empty()
            || !stored.bytes().all(|b| b.is_ascii_digit())
            || (stored.len() > 1 && stored.starts_with('0'))
        {
            return None;
        }
        stored.parse::<u32>().ok().and_then(|v| self.validate(v))
    }

    /// The stored representation of an already-validated value.
    pub fn render(value: u32) -> String {
        value.to_string()
    }

    /// Resolve the effective value: a present row must parse (a corrupt
    /// row is an error, never a silent default); an absent row yields
    /// the built-in default.
    pub fn resolve(self, stored: Option<&str>) -> Option<u32> {
        match stored {
            Some(raw) => self.parse_stored(raw),
            None => Some(self.default_value()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_parse_is_exact_match_only() {
        assert_eq!(parse_role("member"), Some(ROLE_MEMBER));
        assert_eq!(parse_role("admin"), Some(ROLE_ADMIN));
        assert_eq!(parse_role("Admin"), None);
        assert_eq!(parse_role(" admin"), None);
        assert_eq!(parse_role("admin "), None);
        assert_eq!(parse_role("owner"), None);
        assert_eq!(parse_role(""), None);
    }

    #[test]
    fn only_the_exact_admin_value_grants_admin() {
        assert!(is_admin_role(Some("admin")));
        assert!(!is_admin_role(Some("member")));
        assert!(!is_admin_role(Some("Admin")));
        assert!(!is_admin_role(Some("")));
        assert!(!is_admin_role(None));
    }

    #[test]
    fn setting_keys_round_trip_and_reject_lookalikes() {
        for key in ALL_SETTING_KEYS {
            assert_eq!(SettingKey::parse(key.as_str()), Some(*key));
        }
        assert_eq!(SettingKey::parse("invite_default_expiry_day"), None);
        assert_eq!(SettingKey::parse("INVITE_DEFAULT_EXPIRY_DAYS"), None);
        assert_eq!(SettingKey::parse(""), None);
    }

    #[test]
    fn defaults_match_the_migration_seed() {
        assert_eq!(SettingKey::InviteDefaultExpiryDays.default_value(), 7);
        assert_eq!(SettingKey::InviteMemberMinPaidMonths.default_value(), 3);
        assert_eq!(SettingKey::InviteMemberPerWindow.default_value(), 2);
        for key in ALL_SETTING_KEYS {
            assert_eq!(key.resolve(None), Some(key.default_value()));
        }
    }

    #[test]
    fn expiry_bounds_are_one_to_thirty_days() {
        let key = SettingKey::InviteDefaultExpiryDays;
        assert_eq!(key.validate(0), None);
        assert_eq!(key.validate(1), Some(1));
        assert_eq!(key.validate(30), Some(30));
        assert_eq!(key.validate(31), None);
    }

    #[test]
    fn member_invite_knobs_accept_zero_and_reject_past_their_caps() {
        assert_eq!(SettingKey::InviteMemberMinPaidMonths.validate(0), Some(0));
        assert_eq!(
            SettingKey::InviteMemberMinPaidMonths.validate(600),
            Some(600)
        );
        assert_eq!(SettingKey::InviteMemberMinPaidMonths.validate(601), None);
        assert_eq!(SettingKey::InviteMemberPerWindow.validate(0), Some(0));
        assert_eq!(
            SettingKey::InviteMemberPerWindow.validate(10_000),
            Some(10_000)
        );
        assert_eq!(SettingKey::InviteMemberPerWindow.validate(10_001), None);
    }

    #[test]
    fn stored_parse_is_strict_decimal_within_bounds() {
        let key = SettingKey::InviteDefaultExpiryDays;
        assert_eq!(key.parse_stored("7"), Some(7));
        assert_eq!(key.parse_stored("07"), None);
        assert_eq!(key.parse_stored(" 7"), None);
        assert_eq!(key.parse_stored("7 "), None);
        assert_eq!(key.parse_stored("+7"), None);
        assert_eq!(key.parse_stored("-1"), None);
        assert_eq!(key.parse_stored("7.0"), None);
        assert_eq!(key.parse_stored(""), None);
        // Out-of-bounds stored values fail closed, like corrupt ones.
        assert_eq!(key.parse_stored("45"), None);
        assert_eq!(key.parse_stored("99999999999999999999"), None);
        assert_eq!(key.resolve(Some("junk")), None);
        assert_eq!(key.resolve(Some("14")), Some(14));
    }
}
