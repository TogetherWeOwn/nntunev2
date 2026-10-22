//! Test-connection lease-duration pins (tests-only).
//!
//! Contract under test (`workers/api/src/handlers.rs:60`, wired at `:2545`):
//!
//! - `TEST_CONNECTION_LEASE_SECS == 20`. The handler passes that const (not
//!   a bare literal) into `acquire_test_connection_lease` alongside
//!   `vault::MAX_TEST_CONCURRENCY`, so the bounded probe holds its slot
//!   for exactly 20 seconds.
//! - A lease acquired with that duration holds its slot before expiry and
//!   frees it once the clock passes 20s (strict `expires > now` retain,
//!   same shape as the in-module fake).
//!
//! Sensitivity (verified by construction, production code untouched):
//!
//! - Changing the const (e.g. to `30`) breaks the `lease_duration_literal`
//!   source pin, which is the drift detector: the behavioral tests run
//!   against the in-test pinned copy, so they stay green and only the
//!   literal pin flips.
//! - Rewiring the handler to pass a bare literal instead of the const
//!   breaks the `handler_wires_lease_const` pin.
//! - Relaxing the retain to `>=` keeps the slot held at `NOW + 20` and
//!   breaks the free-after-expiry pin; shortening the fake duration breaks
//!   the hold-before-expiry pin.
//!
//! Out of scope: slot-count saturation, invite windows, and any handler
//! behavior change. No network, no real clock; the fake store below mirrors
//! the `pr30_findings_acceptance.rs` lease shape with a controllable clock.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --manifest-path workers/api/Cargo.toml --test test_connection_lease_duration_acceptance
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers::{self, Store};
use nntune_api::{auth, session, vault};

const NOW: u64 = 1_700_000_000;
/// Pinned copy of the handler const; the source-text test below proves it
/// cannot drift without turning red.
const PINNED_LEASE_SECS: u64 = 20;

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

/// Minimal lease fake: same `(user, slot) -> (token, expires)` shape as the
/// shared acceptance fake, with a controllable clock so expiry is exact.
#[derive(Default)]
struct FakeStore {
    now: Cell<u64>,
    test_connection_leases: RefCell<HashMap<(String, u32), (String, u64)>>,
}

impl FakeStore {
    fn with_now(now: u64) -> Self {
        Self {
            now: Cell::new(now),
            test_connection_leases: RefCell::new(HashMap::new()),
        }
    }

    fn set_now(&self, now: u64) {
        self.now.set(now);
    }

    fn expiry(&self, user_id: &str, slot: u32) -> Option<u64> {
        self.test_connection_leases
            .borrow()
            .get(&(user_id.to_string(), slot))
            .map(|(_, expires)| *expires)
    }
}

impl Store for FakeStore {
    async fn user_by_email(
        &self,
        _email: &str,
    ) -> Result<Option<handlers::UserRecord>, handlers::StoreError> {
        Ok(None)
    }

    async fn insert_user(&self, _user: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
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

    async fn acquire_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
        lease_secs: u64,
        max_concurrency: u32,
    ) -> Result<bool, handlers::StoreError> {
        let now = self.now.get();
        let expires_at = now.saturating_add(lease_secs);
        let mut leases = self.test_connection_leases.borrow_mut();
        leases.retain(|_, (_, expires)| *expires > now);
        for slot in 0..max_concurrency {
            let key = (user_id.to_string(), slot);
            if let std::collections::hash_map::Entry::Vacant(entry) = leases.entry(key) {
                entry.insert((token.to_string(), expires_at));
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn release_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<(), handlers::StoreError> {
        self.test_connection_leases
            .borrow_mut()
            .retain(|(owner, _), (lease_token, _)| owner != user_id || lease_token != token);
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
fn lease_duration_literal_is_20() {
    const SOURCE: &str = include_str!("../src/handlers.rs");
    assert!(
        SOURCE.contains("const TEST_CONNECTION_LEASE_SECS: u64 = 20;"),
        "test-connection lease duration must stay exactly 20s"
    );
    assert_eq!(
        PINNED_LEASE_SECS, 20,
        "pinned copy must match the handler const"
    );
}

#[test]
fn handler_wires_lease_const_not_bare_literal() {
    const SOURCE: &str = include_str!("../src/handlers.rs");
    let normalized = SOURCE
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    assert!(
        normalized.contains(
            "acquire_test_connection_lease( &claims.user_id, &lease_token, \
             test_connection_lease_secs, vault::max_test_concurrency,"
        ),
        "handler must pass TEST_CONNECTION_LEASE_SECS (not a bare 20) with \
         vault::MAX_TEST_CONCURRENCY"
    );
}

#[test]
fn lease_holds_slot_before_expiry() {
    let store = FakeStore::with_now(NOW);
    assert!(
        block_on(store.acquire_test_connection_lease(
            "lease-user",
            "lease-token-a",
            PINNED_LEASE_SECS,
            1
        ))
        .expect("first acquire works"),
        "first lease with the pinned 20s duration must be admitted"
    );
    assert_eq!(
        store.expiry("lease-user", 0),
        Some(NOW + 20),
        "stored expiry must be exactly NOW + 20s"
    );
    // One second before expiry the single slot is still held.
    store.set_now(NOW + 19);
    assert!(
        !block_on(store.acquire_test_connection_lease(
            "lease-user",
            "lease-token-b",
            PINNED_LEASE_SECS,
            1
        ))
        .expect("second acquire works"),
        "slot must stay held 19s into the 20s lease"
    );
    assert_eq!(
        store.expiry("lease-user", 0),
        Some(NOW + 20),
        "failed acquire must not move the stored expiry"
    );
}

#[test]
fn lease_frees_slot_once_clock_passes_20s() {
    let store = FakeStore::with_now(NOW);
    assert!(block_on(store.acquire_test_connection_lease(
        "lease-user",
        "lease-token-a",
        PINNED_LEASE_SECS,
        1
    ))
    .expect("first acquire works"));
    // At exactly NOW + 20 the strict `expires > now` retain prunes the
    // expired row, so the slot is reusable.
    store.set_now(NOW + 20);
    assert!(
        block_on(store.acquire_test_connection_lease(
            "lease-user",
            "lease-token-b",
            PINNED_LEASE_SECS,
            1
        ))
        .expect("re-acquire works"),
        "slot must free once the clock passes 20s"
    );
    assert_eq!(
        store.expiry("lease-user", 0),
        Some(NOW + 20 + 20),
        "re-acquired lease must expire 20s after re-acquisition"
    );
}
