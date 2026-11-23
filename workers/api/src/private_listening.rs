use crate::auth::{InviteRecord, INVITE_KIND_ADMIN};
use serde::Deserialize;

/// Time-bounded policy referencing this environment's owner-issued invites.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateListening {
    pub(crate) owner_user_id: String,
    pub(crate) invite_ids: Vec<i64>,
    pub(crate) expires_at: u64,
}

impl PrivateListening {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 1024 {
            return None;
        }
        let mut policy: Self = serde_json::from_str(raw).ok()?;
        let owner = &policy.owner_user_id;
        if owner.is_empty()
            || owner.len() > 128
            || !owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || policy.expires_at == 0
            || policy.invite_ids.len() > 8
            || policy.invite_ids.iter().any(|id| *id <= 0)
        {
            return None;
        }
        policy.invite_ids.sort_unstable();
        policy.invite_ids.dedup();
        Some(policy)
    }

    pub(crate) fn accepts_invite(&self, invite: &InviteRecord, now: u64) -> bool {
        now < self.expires_at
            && self.invite_ids.contains(&invite.id)
            && invite.kind == INVITE_KIND_ADMIN
            && invite.issued_by.as_deref() == Some(&self.owner_user_id)
            && invite.email.is_some()
    }
}

// The enrollment transaction stamps both sides of the invite binding.
pub(crate) const ACCESS_SQL: &str = "SELECT EXISTS (
    SELECT 1 FROM users u WHERE u.id = $1 AND u.deleting_at IS NULL
      AND ((u.id = $2 AND u.role = 'admin') OR EXISTS (
        SELECT 1 FROM invite_codes i JOIN users owner ON owner.id = i.issued_by
        WHERE i.id = ANY($3) AND i.kind = 'admin' AND i.issued_by = $2
          AND owner.role = 'admin' AND owner.deleting_at IS NULL
          AND i.code_hash = u.invite_code_hash AND i.email = u.email
          AND i.used AND i.redeemed_by = u.id AND i.redeemed_at IS NOT NULL
          AND i.redeemed_at < i.expires_at AND i.revoked_at IS NULL
      ))
)";
