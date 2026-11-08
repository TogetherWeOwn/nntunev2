//! Validated encrypted replacement, deliberately not dispatched by a route.
//! The eventual caller must first verify session ownership, freshness and CSRF.
//! Shared-budget recovery/observability must be resolved before enabling it.

use std::future::Future;

use crate::{
    account_deletion::FencedWriteOutcome,
    handlers::{Store, StoreError},
    listenbrainz::{LinkInput, LinkSummary, StoredLink, Username},
    rotation::KekSource,
};

use super::{ApiRequest, WireError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkFailure {
    InvalidInput,
    Provider(WireError),
    Unavailable,
    AccountDeleting,
    NoAccount,
}

trait Validator {
    fn validate(
        &self,
        request: &ApiRequest,
        expected: &Username,
    ) -> impl Future<Output = Result<Username, WireError>>;
}

struct SharedValidator<'a, S>(&'a S);

impl<S: Store> Validator for SharedValidator<'_, S> {
    async fn validate(
        &self,
        request: &ApiRequest,
        expected: &Username,
    ) -> Result<Username, WireError> {
        // Only the coordinator may call the transport. It persists cooldown
        // before even an upstream validation error is interpreted here.
        let reply = super::coordinator::exchange(self.0, request).await?;
        let body = reply.body?;
        let head = reply.head.ok_or(WireError::Unavailable)?;
        super::validated_username(head.status, &body, expected)
    }
}

pub(crate) async fn link(
    store: &impl Store,
    keks: &impl KekSource,
    input: LinkInput,
    user_id: &str,
    kek_version: u32,
    source_ip: &str,
    now: u64,
) -> Result<LinkSummary, LinkFailure> {
    link_with(
        &SharedValidator(store),
        keks,
        input,
        user_id,
        kek_version,
        |link| async move {
            store
                .replace_listenbrainz_link(user_id, &link, source_ip, now)
                .await
        },
    )
    .await
}

async fn link_with<F: Future<Output = Result<FencedWriteOutcome, StoreError>>>(
    validator: &impl Validator,
    keks: &impl KekSource,
    input: LinkInput,
    user_id: &str,
    kek_version: u32,
    replace: impl FnOnce(StoredLink) -> F,
) -> Result<LinkSummary, LinkFailure> {
    if kek_version == 0 || i32::try_from(kek_version).is_err() {
        return Err(LinkFailure::InvalidInput);
    }
    // Fail before spending provider quota if the owner/key/context cannot be
    // sealed. This candidate is never written without upstream validation.
    let candidate = input
        .seal(keks, kek_version, user_id)
        .map_err(|_| LinkFailure::InvalidInput)?;
    let expected = input.username;
    let request = ApiRequest::validation(input.token);
    let canonical = validator
        .validate(&request, &expected)
        .await
        .map_err(LinkFailure::Provider)?;
    // Defense at the write boundary, even with an alternate validator.
    if !canonical.as_str().eq_ignore_ascii_case(expected.as_str()) {
        return Err(LinkFailure::Provider(WireError::Rejected));
    }
    let token = request.token;
    let validated = if canonical == candidate.username {
        drop(token);
        candidate
    } else {
        // The username is authenticated envelope context. A canonical spelling
        // change requires resealing, never editing metadata around ciphertext.
        LinkInput {
            username: canonical,
            token,
        }
        .seal(keks, kek_version, user_id)
        .map_err(|_| LinkFailure::Unavailable)?
    };
    let summary = validated.summary();
    match replace(validated)
        .await
        .map_err(|_| LinkFailure::Unavailable)?
    {
        FencedWriteOutcome::Written => Ok(summary),
        FencedWriteOutcome::AccountDeleting => Err(LinkFailure::AccountDeleting),
        FencedWriteOutcome::NoRow => Err(LinkFailure::NoAccount),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::listenbrainz::Token;

    const TOKEN: &str = "0b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";

    struct Keys;
    impl KekSource for Keys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            (version == 1).then_some(&[7; 32])
        }
    }

    struct FakeValidator {
        result: Result<Username, WireError>,
        calls: Cell<usize>,
    }

    impl Validator for FakeValidator {
        async fn validate(
            &self,
            request: &ApiRequest,
            expected: &Username,
        ) -> Result<Username, WireError> {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(request.url(), super::super::VALIDATE_TOKEN_URL);
            assert_eq!(request.authorization().as_str(), format!("Token {TOKEN}"));
            assert_eq!(expected.as_str(), "alice");
            self.result.clone()
        }
    }

    fn validator(result: Result<&str, WireError>) -> FakeValidator {
        FakeValidator {
            result: result.map(|name| Username::parse(name).unwrap()),
            calls: Cell::new(0),
        }
    }

    fn input() -> LinkInput {
        LinkInput {
            username: Username::parse("alice").unwrap(),
            token: Token::parse(TOKEN).unwrap(),
        }
    }

    #[tokio::test]
    async fn validation_precedes_single_encrypted_write_and_canonical_context() {
        for name in ["alice", "Alice"] {
            let v = &validator(Ok(name));
            let writes = &RefCell::new(Vec::new());
            let result = link_with(v, &Keys, input(), "u1", 1, |row| async move {
                assert_eq!(v.calls.get(), 1);
                assert_eq!(row.username.as_str(), name);
                assert_eq!(row.user_id, "u1");
                assert_eq!(row.kek_version, 1);
                assert_eq!(row.open(&Keys, "u1").unwrap().expose(), TOKEN);
                assert!(row.open(&Keys, "u2").is_err());
                let mut swapped = row.clone();
                swapped.username = Username::parse("other").unwrap();
                assert!(swapped.open(&Keys, "u1").is_err());
                writes.borrow_mut().push(row);
                Ok(FencedWriteOutcome::Written)
            })
            .await
            .unwrap();
            assert_eq!(writes.borrow().len(), 1);
            let json = serde_json::to_string(&result).unwrap();
            assert!(json.contains(name));
            assert!(!json.contains(TOKEN));
            assert!(!json.contains("ciphertext"));
        }
    }

    #[tokio::test]
    async fn every_validation_failure_and_mismatched_identity_leave_old_link_untouched() {
        for reply in [
            Err(WireError::Rejected),
            Err(WireError::RateLimited),
            Err(WireError::Unavailable),
            Err(WireError::InvalidResponse),
            Ok("other"),
        ] {
            let v = validator(reply);
            let result = link_with(&v, &Keys, input(), "u1", 1, |_| async {
                panic!("invalid validation attempted replacement")
            })
            .await;
            assert!(matches!(result, Err(LinkFailure::Provider(_))));
            assert_eq!(v.calls.get(), 1);
        }
    }

    #[tokio::test]
    async fn invalid_owner_or_key_never_contacts_provider_or_store() {
        for (user, version) in [
            ("", 1),
            ("bad/owner", 1),
            ("u1", 0),
            ("u1", 2),
            ("u1", u32::MAX),
        ] {
            let v = validator(Ok("alice"));
            let result = link_with(&v, &Keys, input(), user, version, |_| async {
                panic!("invalid context attempted replacement")
            })
            .await;
            assert_eq!(result, Err(LinkFailure::InvalidInput));
            assert_eq!(v.calls.get(), 0);
        }
    }

    #[tokio::test]
    async fn persistence_and_deletion_failures_never_report_linked_or_retry() {
        for (write, expected) in [
            (Err(StoreError), LinkFailure::Unavailable),
            (
                Ok(FencedWriteOutcome::AccountDeleting),
                LinkFailure::AccountDeleting,
            ),
            (Ok(FencedWriteOutcome::NoRow), LinkFailure::NoAccount),
        ] {
            let v = validator(Ok("alice"));
            let writes = Cell::new(0);
            let result = link_with(&v, &Keys, input(), "u1", 1, |_| async {
                writes.set(writes.get() + 1);
                write
            })
            .await;
            assert_eq!(result, Err(expected));
            assert_eq!(writes.get(), 1);
            assert_eq!(v.calls.get(), 1);
        }
    }
}
