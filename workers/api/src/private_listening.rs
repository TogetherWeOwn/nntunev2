use crate::auth::{InviteRecord, INVITE_KIND_ADMIN};
use serde::Deserialize;

/// Time-bounded policy referencing this environment's owner-issued invites.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateListening {
    pub(crate) owner_user_id: String,
    pub(crate) invite_ids: Vec<i64>,
    pub(crate) expires_at: u64,
    pub(crate) library_scope: Option<crate::private_library::Scope>,
}

impl PrivateListening {
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 1024 || !raw.trim_ascii_start().starts_with('{') {
            return None;
        }
        let policy: Self = serde_json::from_str(raw).ok()?;
        if !crate::ledger::is_valid_key_field(&policy.owner_user_id, 128)
            || !(1..=i64::MAX as u64).contains(&policy.expires_at)
            || policy.invite_ids.len() > 8
            || policy.invite_ids.iter().any(|id| *id <= 0)
            || policy
                .library_scope
                .as_ref()
                .is_some_and(|scope| !scope.valid())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_requires_objects_and_a_postgres_representable_deadline() {
        let id = "a0000000-0000-4000-8000-000000000001";
        let policy = |expires, scope| {
            json!({
                "owner_user_id": "owner", "invite_ids": [1],
                "expires_at": expires, "library_scope": scope
            })
            .to_string()
        };
        let scope = json!({"environment_id": id, "library_id": id});
        assert!(PrivateListening::parse(&policy(i64::MAX as u64, scope.clone())).is_some());
        assert!(PrivateListening::parse(&policy(i64::MAX as u64 + 1, scope)).is_none());
        assert!(PrivateListening::parse(&policy(100, json!([id, id]))).is_none());
        assert!(PrivateListening::parse(r#"["owner",[1],100,null]"#).is_none());
    }
}
