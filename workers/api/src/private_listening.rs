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
        let policy: Self = serde_json::from_str(raw).ok()?;
        if !crate::ledger::is_valid_key_field(&policy.owner_user_id, 128)
            || policy.expires_at == 0
            || policy.invite_ids.len() > 8
            || policy.invite_ids.iter().any(|id| *id <= 0)
        {
            return None;
        }
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

pub(crate) const ACCESS_SQL: &str = include_str!("private_listening_access.sql");
