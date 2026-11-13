//! Inert, scoped authorization for a future programmatic ingest session API.
//! Grants and job ownership must come from trusted server-side state.

use crate::native_rest::{self, CredentialError, NativeRestRecord, Pepper, Scope};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Create,
    Status,
    Abort,
    Reuse,
}

/// A server-side grant bound to one credential and one library in one environment.
/// It does not replace bearer validation and must never be populated from the request.
pub struct Grant<'a> {
    pub credential_id: &'a str,
    pub environment: &'a str,
    pub library_id: &'a str,
    pub actions: &'a [Action],
}

/// Ownership loaded from trusted job state, not from caller-supplied fields.
pub struct Job<'a> {
    pub id: &'a str,
    pub owner_id: &'a str,
    pub environment: &'a str,
    pub library_id: &'a str,
}

/// Authenticate, then enforce the trusted grant and job using generic errors.
pub fn authorize(
    credential: Option<&NativeRestRecord>,
    secret: Option<&str>,
    peppers: &[Pepper<'_>],
    now: u64,
    grant: Option<&Grant<'_>>,
    environment: &str,
    library_id: &str,
    action: Action,
    job: Option<(&str, &Job<'_>)>,
) -> Result<(), CredentialError> {
    let record = credential.ok_or(CredentialError::Unauthenticated)?;
    let secret = secret.ok_or(CredentialError::Unauthenticated)?;
    // This contract expires at the deadline, unlike legacy bearer auth.
    if now >= record.expires_at {
        return Err(CredentialError::Unauthenticated);
    }
    native_rest::authorize(
        record,
        secret,
        Scope::IngestSubmit,
        now,
        &record.user_id,
        peppers,
    )?;
    let grant = grant.ok_or(CredentialError::Forbidden)?;
    if record.id.is_empty()
        || record.user_id.is_empty()
        || environment.is_empty()
        || library_id.is_empty()
        || grant.credential_id != record.id
        || grant.environment != environment
        || grant.library_id != library_id
        || !grant.actions.contains(&action)
    {
        return Err(CredentialError::Forbidden);
    }
    match (action, job) {
        (Action::Create, None) => Ok(()),
        (Action::Status | Action::Abort | Action::Reuse, Some((id, job)))
            if !id.is_empty()
                && id == job.id
                && job.owner_id == record.user_id
                && job.environment == environment
                && job.library_id == library_id =>
        {
            Ok(())
        }
        _ => Err(CredentialError::Forbidden),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "synthetic-test-value";
    const PEPPER: &[u8] = b"synthetic-test-pepper";

    fn record() -> NativeRestRecord {
        NativeRestRecord {
            id: "credential".into(),
            user_id: "owner".into(),
            key_prefix: String::new(),
            key_hmac: native_rest::credential_hmac(SECRET, PEPPER),
            pepper_version: 1,
            scopes: vec![Scope::IngestSubmit],
            created_at: 0,
            expires_at: 10,
            revoked_at: None,
        }
    }

    #[test]
    fn bearer_and_narrow_grant_are_both_required() {
        let mut record = record();
        let peppers = [Pepper {
            version: 1,
            bytes: PEPPER,
        }];
        let grant = Grant {
            credential_id: "credential",
            environment: "test",
            library_id: "library",
            actions: &[Action::Create],
        };
        let check = |record: Option<&NativeRestRecord>, secret, now, grant| {
            authorize(
                record,
                secret,
                &peppers,
                now,
                grant,
                "test",
                "library",
                Action::Create,
                None,
            )
        };
        let unauth = Err(CredentialError::Unauthenticated);
        let forbidden = Err(CredentialError::Forbidden);
        assert_eq!(check(Some(&record), Some(SECRET), 9, Some(&grant)), Ok(()));
        assert_eq!(check(Some(&record), Some(SECRET), 10, Some(&grant)), unauth);
        assert_eq!(check(None, None, 1, Some(&grant)), unauth);
        assert_eq!(check(Some(&record), None, 1, Some(&grant)), unauth);
        assert_eq!(check(Some(&record), Some("wrong"), 1, Some(&grant)), unauth);
        assert_eq!(check(Some(&record), Some(SECRET), 11, Some(&grant)), unauth);
        assert_eq!(check(Some(&record), Some(SECRET), 1, None), forbidden);
        let other = Grant {
            credential_id: "other",
            ..grant
        };
        let wrong_grant = check(Some(&record), Some(SECRET), 1, Some(&other));
        record.scopes.clear();
        let no_scope = check(Some(&record), Some(SECRET), 1, Some(&grant));
        assert_eq!((wrong_grant, no_scope), (forbidden, forbidden));
        record.revoked_at = Some(1);
        assert_eq!(check(Some(&record), Some(SECRET), 1, Some(&grant)), unauth);
    }

    #[test]
    fn job_actions_require_matching_owner_and_scope() {
        let record = record();
        let peppers = [Pepper {
            version: 1,
            bytes: PEPPER,
        }];
        let grant = Grant {
            credential_id: "credential",
            environment: "test",
            library_id: "library",
            actions: &[Action::Status],
        };
        let mut job = Job {
            id: "job",
            owner_id: "owner",
            environment: "test",
            library_id: "library",
        };
        let check = |action, env, library, id, job| {
            authorize(
                Some(&record),
                Some(SECRET),
                &peppers,
                1,
                Some(&grant),
                env,
                library,
                action,
                Some((id, job)),
            )
        };
        let forbidden = Err(CredentialError::Forbidden);
        let status = || check(Action::Status, "test", "library", "job", &job);
        assert_eq!(status(), Ok(()));
        for (action, env, library, id) in [
            (Action::Abort, "test", "library", "job"),
            (Action::Status, "other", "library", "job"),
            (Action::Status, "test", "other", "job"),
            (Action::Status, "test", "library", "other"),
        ] {
            assert_eq!(check(action, env, library, id, &job), forbidden);
        }
        job.owner_id = "other";
        let wrong_owner = check(Action::Status, "test", "library", "job", &job);
        assert_eq!(wrong_owner, forbidden);
    }
}
