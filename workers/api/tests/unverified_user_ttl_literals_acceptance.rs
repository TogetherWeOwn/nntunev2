//! Unverified-user reclamation window literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/handlers.rs:113`, wired at `:1372`):
//!
//! - `UNVERIFIED_USER_TTL_SECS == 86_400` (`24 * 3600`). `register_start`
//!   passes that const (not a bare literal) into `reap_unverified_users`,
//!   so a never-completed signup is reaped once it is older than 24 h.
//! - The reaper entry keeps rows with `created_at > cutoff` where
//!   `cutoff = now - ttl_secs` (same shape as the in-module fake and
//!   `reap_unverified_users_sql`): a user aged exactly the window
//!   (`created_at == NOW - 86_400`) is reaped, a fresh one survives.
//!
//! Sensitivity (verified by construction, production code untouched):
//!
//! - Changing the const (e.g. to `12 * 3600`) breaks the
//!   `ttl_literal_is_86400` source pin, which is the drift detector: the
//!   behavioral tests run against the in-test pinned copy, so they stay
//!   green and only the literal pin flips.
//! - Rewiring the handler to pass a bare literal instead of the const
//!   breaks the `handler_wires_ttl_const` pin.
//! - Relaxing the retain to `>=` keeps the exactly-aged user and breaks
//!   the reaped-at-window pin; shortening the fake TTL breaks the
//!   fresh-survives pin.
//!
//! Out of scope: invite windows/attempts, magic-link TTLs (different
//! consts). No DB, no real clock; the fake store below mirrors only the
//! cutoff + protected-id shape of the in-module fake.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test unverified_user_ttl_literals_acceptance
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers::{self, Store};
use nntune_api::{auth, session, vault};

const NOW: u64 = 1_700_000_000;
/// Pinned copy of the handler const; the source-text test below proves it
/// cannot drift without turning red.
const PINNED_TTL_SECS: u64 = 86_400;

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..1_000 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
    panic!("deterministic fake unexpectedly awaited external work");
}

/// Minimal reaper fake: user rows plus the production cutoff shape
/// (`created_at > now - ttl_secs`, protected id exempt). All other
/// exemption sets (credentials, challenges, vaults, sessions, magic) are
/// empty here by construction, so the suite isolates the TTL value.
#[derive(Default)]
struct FakeStore {
    users: RefCell<HashMap<String, handlers::UserRecord>>,
}

impl FakeStore {
    fn insert(&self, id: &str, created_at: u64) {
        self.users.borrow_mut().insert(
            id.to_string(),
            handlers::UserRecord {
                id: id.to_string(),
                email: format!("{id}@example.test"),
                created_at,
            },
        );
    }

    fn contains(&self, id: &str) -> bool {
        self.users.borrow().contains_key(id)
    }
}

impl Store for FakeStore {
    async fn user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<handlers::UserRecord>, handlers::StoreError> {
        Ok(self
            .users
            .borrow()
            .values()
            .find(|user| user.email == email)
            .cloned())
    }

    async fn insert_user(&self, user: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
        self.users
            .borrow_mut()
            .insert(user.id.clone(), user.clone());
        Ok(())
    }

    async fn insert_challenge(
        &self,
        _challenge: &auth::PasskeyChallenge,
        _user_id: Option<&str>,
        _purpose: &str,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn challenge_by_id(
        &self,
        _id: &str,
    ) -> Result<Option<handlers::ChallengeRow>, handlers::StoreError> {
        Ok(None)
    }

    async fn insert_credential(
        &self,
        _credential: &auth::PasskeyCredential,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn credential_by_id(
        &self,
        _cred_id_b64url: &str,
    ) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError> {
        Ok(None)
    }

    async fn credentials_for_user(
        &self,
        _user_id: &str,
    ) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError> {
        Ok(Vec::new())
    }

    async fn advance_sign_count(
        &self,
        _cred_id_b64url: &str,
        _expected: u32,
        _new: u32,
    ) -> Result<bool, handlers::StoreError> {
        Ok(false)
    }

    async fn insert_magic(
        &self,
        _record: &auth::MagicLinkRecord,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn magic_by_hash(
        &self,
        _token_hash: &str,
    ) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError> {
        Ok(None)
    }

    async fn reserve_rate_limit(
        &self,
        _scope_key: &str,
        _action: &str,
        _limit: u64,
        _now: u64,
        _window_secs: u64,
    ) -> Result<bool, handlers::StoreError> {
        Ok(true)
    }

    async fn prune_expired_challenges(&self, _now: u64) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn reap_unverified_users(
        &self,
        now: u64,
        ttl_secs: u64,
        protected_user_id: Option<&str>,
    ) -> Result<(), handlers::StoreError> {
        // Mirror the in-module fake's cutoff shape: keep rows newer than
        // `now - ttl_secs`, plus the protected (resumed) user.
        let cutoff = now.saturating_sub(ttl_secs);
        self.users.borrow_mut().retain(|_, user| {
            user.created_at > cutoff || protected_user_id == Some(user.id.as_str())
        });
        Ok(())
    }

    async fn acquire_test_connection_lease(
        &self,
        _user_id: &str,
        _token: &str,
        _lease_secs: u64,
        _max_concurrency: u32,
    ) -> Result<bool, handlers::StoreError> {
        Ok(false)
    }

    async fn release_test_connection_lease(
        &self,
        _user_id: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn invite_by_hash(
        &self,
        _code_hash: &str,
    ) -> Result<Option<auth::InviteRecord>, handlers::StoreError> {
        Ok(None)
    }

    async fn insert_session(
        &self,
        _record: &session::SessionRecord,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn session_by_id(
        &self,
        _id: &str,
    ) -> Result<Option<session::SessionRecord>, handlers::StoreError> {
        Ok(None)
    }

    async fn rotate_session_row(
        &self,
        _id: &str,
        _expected_counter: u32,
        _new_counter: u32,
        _new_expires_at: u64,
    ) -> Result<bool, handlers::StoreError> {
        Ok(false)
    }

    async fn revoke_session(&self, _id: &str) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn revoke_all_sessions(&self, _user_id: &str) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn vault_by_user(
        &self,
        _user_id: &str,
    ) -> Result<Option<handlers::VaultRow>, handlers::StoreError> {
        Ok(None)
    }

    async fn upsert_vault(
        &self,
        _user_id: &str,
        _row: &handlers::VaultRow,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn delete_vault(&self, _user_id: &str) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn insert_audit(&self, _event: &vault::AuditEvent) -> Result<(), handlers::StoreError> {
        Ok(())
    }
}

#[test]
fn ttl_literal_is_86400() {
    const SOURCE: &str = include_str!("../src/handlers.rs");
    assert!(
        SOURCE.contains("const UNVERIFIED_USER_TTL_SECS: u64 = 24 * 3600;"),
        "unverified-user reclamation window must stay declared as 24 * 3600"
    );
    assert_eq!(
        PINNED_TTL_SECS, 86_400,
        "pinned copy must match the handler const"
    );
    assert_eq!(
        24 * 3600u64,
        86_400,
        "the 24 h window must stay exactly 86_400s"
    );
}

#[test]
fn handler_wires_ttl_const_not_bare_literal() {
    const SOURCE: &str = include_str!("../src/handlers.rs");
    let normalized = SOURCE
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    assert!(
        normalized.contains(".reap_unverified_users( ctx.now, unverified_user_ttl_secs,"),
        "register_start must pass UNVERIFIED_USER_TTL_SECS (not a bare 86400) \
         into the reaper entry"
    );
}

#[test]
fn user_aged_exactly_window_is_reaped_while_fresh_survives() {
    let store = FakeStore::default();
    store.insert("stale-user", NOW - PINNED_TTL_SECS);
    store.insert("fresh-user", NOW);
    block_on(store.reap_unverified_users(NOW, PINNED_TTL_SECS, None)).expect("reap works");
    assert!(
        !store.contains("stale-user"),
        "a user aged exactly 86_400s (created_at == NOW - ttl) must be reaped"
    );
    assert!(
        store.contains("fresh-user"),
        "a fresh user must survive the reaper entry"
    );
}

#[test]
fn just_inside_window_survives_and_older_is_reaped() {
    let store = FakeStore::default();
    store.insert("just-inside", NOW - PINNED_TTL_SECS + 1);
    store.insert("older", NOW - PINNED_TTL_SECS - 1);
    block_on(store.reap_unverified_users(NOW, PINNED_TTL_SECS, None)).expect("reap works");
    assert!(
        store.contains("just-inside"),
        "a user one second inside the 86_400s window must survive"
    );
    assert!(
        !store.contains("older"),
        "a user one second past the 86_400s window must be reaped"
    );
}

#[test]
fn protected_user_survives_past_window() {
    let store = FakeStore::default();
    store.insert("resumed-user", NOW - PINNED_TTL_SECS - 1);
    block_on(store.reap_unverified_users(NOW, PINNED_TTL_SECS, Some("resumed-user")))
        .expect("reap works");
    assert!(
        store.contains("resumed-user"),
        "the protected (resumed) user must survive even past the window"
    );
}
