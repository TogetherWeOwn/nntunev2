//! Bounded, wrap-only KEK rotation for one ListenBrainz link (per user).
//!
//! Source only: no scheduler, route, CLI or fleet scan calls this yet. One
//! call handles exactly one `user_id`:
//!
//! 1. reject a zero target version before touching the store;
//! 2. read the user's row and verify it is the captured owner's row;
//! 3. reject a target that is not strictly newer than the row's version;
//! 4. [`StoredLink::rewrap`] (DEK-only; the token is never decrypted and
//!    the payload nonce and ciphertext are preserved byte-for-byte) using
//!    the existing [`KekSource`];
//! 5. one envelope-conditional compare-and-set against the full captured
//!    row. A lost CAS (relink, concurrent rotation, unlink) is reported and
//!    never retried or overwritten here: the caller reloads on a later run.
//!
//! Results are scalar enums only: no envelope, key, username or error detail
//! can reach a caller or a log. This module emits no log calls.

use std::future::Future;

use crate::{
    handlers::{Store, StoreError},
    listenbrainz::{LinkError, RewrapOutcome, StoredLink},
    rotation::KekSource,
};

/// The two store operations the orchestrator needs. Blanket-implemented for
/// every [`Store`], so production reuses `listenbrainz_link` and
/// `cas_rewrap_listenbrainz_link`; tests supply a small fake.
pub trait LinkRewrapStore {
    fn read_link(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<StoredLink>, StoreError>>;
    fn cas_rewrap_link(
        &self,
        user_id: &str,
        captured: &StoredLink,
        replacement: &StoredLink,
        now: u64,
    ) -> impl Future<Output = Result<RewrapOutcome, StoreError>>;
}

impl<S: Store> LinkRewrapStore for S {
    fn read_link(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<StoredLink>, StoreError>> {
        self.listenbrainz_link(user_id)
    }

    fn cas_rewrap_link(
        &self,
        user_id: &str,
        captured: &StoredLink,
        replacement: &StoredLink,
        now: u64,
    ) -> impl Future<Output = Result<RewrapOutcome, StoreError>> {
        self.cas_rewrap_listenbrainz_link(user_id, captured, replacement, now)
    }
}

/// Scalar result of one per-user rotation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRotationOutcome {
    /// The wrap moved to the target version.
    Rewrapped,
    /// No link row (never linked, or unlinked/deleted).
    NoLink,
    /// The owner is being deleted; nothing was written.
    AccountDeleting,
    /// The captured row changed before the CAS; nothing was written and it
    /// was not retried. A later run reloads the current row.
    ClaimLost,
    /// Target version is zero.
    InvalidTargetVersion,
    /// Target is not strictly newer than the row's version (already
    /// rotated or a downgrade); nothing was written.
    NotIncreasing,
    /// `user_id` is not a valid owner id.
    InvalidUser,
    /// The store returned a row that does not belong to `user_id`.
    OwnerMismatch,
    /// The row's current or the target key version is not available.
    UnknownKey,
    /// Malformed stored envelope shape.
    Corrupt,
    /// Unwrap/authentication failure (wrong key, tampered or swapped row).
    Crypto,
    /// Read or CAS store failure; the write state is unknown only for CAS,
    /// which is safe to re-run because the CAS is conditional.
    StoreFailed,
}

fn map_link_error(e: LinkError) -> LinkRotationOutcome {
    match e {
        LinkError::InvalidUserId => LinkRotationOutcome::InvalidUser,
        LinkError::UnknownKey => LinkRotationOutcome::UnknownKey,
        LinkError::InvalidRewrapVersion => LinkRotationOutcome::NotIncreasing,
        LinkError::Corrupt => LinkRotationOutcome::Corrupt,
        // Username/token/mbid errors cannot arise from a rewrap; fail closed.
        LinkError::Crypto
        | LinkError::InvalidUsername
        | LinkError::InvalidToken
        | LinkError::InvalidMbid => LinkRotationOutcome::Crypto,
    }
}

/// Rewrap `user_id`'s ListenBrainz link to `target_version`, once.
pub async fn rotate_link(
    store: &impl LinkRewrapStore,
    keks: &impl KekSource,
    user_id: &str,
    target_version: u32,
    now: u64,
) -> LinkRotationOutcome {
    if target_version == 0 {
        return LinkRotationOutcome::InvalidTargetVersion;
    }
    let captured = match store.read_link(user_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return LinkRotationOutcome::NoLink,
        Err(StoreError) => return LinkRotationOutcome::StoreFailed,
    };
    if captured.user_id != user_id {
        return LinkRotationOutcome::OwnerMismatch;
    }
    if target_version <= captured.kek_version {
        return LinkRotationOutcome::NotIncreasing;
    }
    let replacement = match captured.rewrap(keks, user_id, target_version) {
        Ok(r) => r,
        Err(e) => return map_link_error(e),
    };
    // Defensive: the CAS must only ever move the wrap and version.
    let (c, r) = (&captured.envelope, &replacement.envelope);
    if replacement.user_id != user_id
        || replacement.username != captured.username
        || replacement.kek_version != target_version
        || c.payload_nonce != r.payload_nonce
        || c.ciphertext != r.ciphertext
    {
        return LinkRotationOutcome::Crypto;
    }
    match store
        .cas_rewrap_link(user_id, &captured, &replacement, now)
        .await
    {
        Ok(RewrapOutcome::Rewrapped) => LinkRotationOutcome::Rewrapped,
        Ok(RewrapOutcome::ClaimLost) => LinkRotationOutcome::ClaimLost,
        Ok(RewrapOutcome::AccountDeleting) => LinkRotationOutcome::AccountDeleting,
        Ok(RewrapOutcome::NoRow) => LinkRotationOutcome::NoLink,
        Err(StoreError) => LinkRotationOutcome::StoreFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listenbrainz::LinkInput;
    use std::cell::{Cell, RefCell};
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Noop;
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }

    /// Fakes complete immediately; a pending future is a test bug.
    fn block_on<F: Future>(f: F) -> F::Output {
        let waker = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&waker);
        let mut f = pin!(f);
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("fake store future pending"),
        }
    }

    struct Keys;
    impl KekSource for Keys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            match version {
                1 => Some(&[7; 32]),
                2 => Some(&[9; 32]),
                3 => Some(&[11; 32]),
                _ => None,
            }
        }
    }

    struct OnlyKeys(Vec<u32>);
    impl KekSource for OnlyKeys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            if !self.0.contains(&version) {
                return None;
            }
            Keys.kek(version)
        }
    }

    const TOKEN: &str = "0b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";

    fn sealed(user: &str, version: u32) -> StoredLink {
        LinkInput::new("alice_lb", TOKEN)
            .unwrap()
            .seal(&Keys, version, user)
            .unwrap()
    }

    #[derive(Default)]
    struct Fake {
        row: RefCell<Option<StoredLink>>,
        read_err: bool,
        cas_err: bool,
        forced: Cell<Option<RewrapOutcome>>,
        /// Replace the row between read and CAS (concurrent relink).
        swap_on_cas: RefCell<Option<StoredLink>>,
        reads: Cell<u32>,
        cas_calls: Cell<u32>,
    }

    fn same(a: &StoredLink, b: &StoredLink) -> bool {
        a.user_id == b.user_id
            && a.username == b.username
            && a.kek_version == b.kek_version
            && a.envelope.dek_wrapped == b.envelope.dek_wrapped
            && a.envelope.dek_wrapped_nonce == b.envelope.dek_wrapped_nonce
            && a.envelope.payload_nonce == b.envelope.payload_nonce
            && a.envelope.ciphertext == b.envelope.ciphertext
    }

    impl LinkRewrapStore for Fake {
        async fn read_link(&self, _user_id: &str) -> Result<Option<StoredLink>, StoreError> {
            self.reads.set(self.reads.get() + 1);
            if self.read_err {
                return Err(StoreError);
            }
            Ok(self.row.borrow().clone())
        }

        async fn cas_rewrap_link(
            &self,
            _user_id: &str,
            captured: &StoredLink,
            replacement: &StoredLink,
            _now: u64,
        ) -> Result<RewrapOutcome, StoreError> {
            self.cas_calls.set(self.cas_calls.get() + 1);
            if self.cas_err {
                return Err(StoreError);
            }
            if let Some(newer) = self.swap_on_cas.borrow_mut().take() {
                *self.row.borrow_mut() = Some(newer);
            }
            if let Some(o) = self.forced.get() {
                return Ok(o);
            }
            let mut row = self.row.borrow_mut();
            match row.as_ref() {
                None => Ok(RewrapOutcome::NoRow),
                Some(cur) if same(cur, captured) => {
                    *row = Some(replacement.clone());
                    Ok(RewrapOutcome::Rewrapped)
                }
                Some(_) => Ok(RewrapOutcome::ClaimLost),
            }
        }
    }

    fn fake_with(row: StoredLink) -> Fake {
        Fake {
            row: RefCell::new(Some(row)),
            ..Fake::default()
        }
    }

    fn run(f: &Fake, keks: &impl KekSource, user: &str, target: u32) -> LinkRotationOutcome {
        block_on(rotate_link(f, keks, user, target, 1000))
    }

    #[test]
    fn rewrap_preserves_payload_and_moves_wrap() {
        let original = sealed("u-1", 1);
        let f = fake_with(original.clone());
        assert_eq!(run(&f, &Keys, "u-1", 3), LinkRotationOutcome::Rewrapped);
        let now = f.row.borrow().clone().unwrap();
        assert_eq!(now.kek_version, 3);
        assert_eq!(now.username, original.username);
        assert_eq!(now.envelope.payload_nonce, original.envelope.payload_nonce);
        assert_eq!(now.envelope.ciphertext, original.envelope.ciphertext);
        assert_ne!(now.envelope.dek_wrapped, original.envelope.dek_wrapped);
        let token = now.open(&Keys, "u-1").unwrap();
        assert_eq!(token.expose(), TOKEN);
        assert_eq!(f.cas_calls.get(), 1);
    }

    #[test]
    fn zero_target_is_rejected_before_any_store_call() {
        let f = fake_with(sealed("u-1", 1));
        assert_eq!(
            run(&f, &Keys, "u-1", 0),
            LinkRotationOutcome::InvalidTargetVersion
        );
        assert_eq!(f.reads.get(), 0);
    }

    #[test]
    fn equal_or_lower_target_is_not_increasing_and_writes_nothing() {
        for (row_v, target) in [(2, 2), (3, 2), (2, 1)] {
            let f = fake_with(sealed("u-1", row_v));
            assert_eq!(
                run(&f, &Keys, "u-1", target),
                LinkRotationOutcome::NotIncreasing
            );
            assert_eq!(f.cas_calls.get(), 0);
            assert_eq!(f.row.borrow().as_ref().unwrap().kek_version, row_v);
        }
    }

    #[test]
    fn missing_row_is_no_link() {
        let f = Fake::default();
        assert_eq!(run(&f, &Keys, "u-1", 2), LinkRotationOutcome::NoLink);
        assert_eq!(f.cas_calls.get(), 0);
    }

    #[test]
    fn row_for_another_owner_is_rejected() {
        let f = fake_with(sealed("u-2", 1));
        assert_eq!(run(&f, &Keys, "u-1", 2), LinkRotationOutcome::OwnerMismatch);
        assert_eq!(f.cas_calls.get(), 0);
    }

    #[test]
    fn invalid_user_id_is_scalar_error() {
        let f = fake_with(sealed("u-1", 1));
        let mut row = f.row.borrow().clone().unwrap();
        row.user_id = "bad id!".into();
        *f.row.borrow_mut() = Some(row);
        assert_eq!(
            run(&f, &Keys, "bad id!", 2),
            LinkRotationOutcome::InvalidUser
        );
        assert_eq!(f.cas_calls.get(), 0);
    }

    #[test]
    fn unknown_old_or_new_key_fails_without_write() {
        let f = fake_with(sealed("u-1", 1));
        assert_eq!(
            run(&f, &OnlyKeys(vec![2]), "u-1", 2),
            LinkRotationOutcome::UnknownKey
        );
        assert_eq!(
            run(&f, &OnlyKeys(vec![1]), "u-1", 2),
            LinkRotationOutcome::UnknownKey
        );
        assert_eq!(run(&f, &Keys, "u-1", 9), LinkRotationOutcome::UnknownKey);
        assert_eq!(f.cas_calls.get(), 0);
        assert_eq!(f.row.borrow().as_ref().unwrap().kek_version, 1);
    }

    #[test]
    fn corrupt_and_tampered_wraps_fail_closed_without_write() {
        let mut short = sealed("u-1", 1);
        short.envelope.dek_wrapped.pop();
        let f = fake_with(short);
        assert_eq!(run(&f, &Keys, "u-1", 2), LinkRotationOutcome::Corrupt);

        let mut flipped = sealed("u-1", 1);
        flipped.envelope.dek_wrapped[0] ^= 1;
        let f2 = fake_with(flipped);
        assert_eq!(run(&f2, &Keys, "u-1", 2), LinkRotationOutcome::Crypto);

        let mut bad_ct = sealed("u-1", 1);
        bad_ct.envelope.ciphertext.pop();
        let f3 = fake_with(bad_ct);
        assert_eq!(run(&f3, &Keys, "u-1", 2), LinkRotationOutcome::Corrupt);
        assert_eq!(
            f.cas_calls.get() + f2.cas_calls.get() + f3.cas_calls.get(),
            0
        );
    }

    #[test]
    fn stale_cas_is_not_overwritten_or_retried() {
        let f = fake_with(sealed("u-1", 1));
        // A same-version relink lands between read and CAS.
        let relinked = LinkInput::new("alice_lb", "1b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b")
            .unwrap()
            .seal(&Keys, 1, "u-1")
            .unwrap();
        *f.swap_on_cas.borrow_mut() = Some(relinked.clone());
        assert_eq!(run(&f, &Keys, "u-1", 2), LinkRotationOutcome::ClaimLost);
        assert_eq!(f.cas_calls.get(), 1);
        assert_eq!(f.reads.get(), 1);
        assert!(same(f.row.borrow().as_ref().unwrap(), &relinked));
    }

    #[test]
    fn store_cas_outcomes_map_to_scalars() {
        for (forced, want) in [
            (RewrapOutcome::ClaimLost, LinkRotationOutcome::ClaimLost),
            (
                RewrapOutcome::AccountDeleting,
                LinkRotationOutcome::AccountDeleting,
            ),
            (RewrapOutcome::NoRow, LinkRotationOutcome::NoLink),
            (RewrapOutcome::Rewrapped, LinkRotationOutcome::Rewrapped),
        ] {
            let f = fake_with(sealed("u-1", 1));
            f.forced.set(Some(forced));
            assert_eq!(run(&f, &Keys, "u-1", 2), want);
            assert_eq!(f.cas_calls.get(), 1);
        }
    }

    #[test]
    fn deletion_between_read_and_cas_is_no_link() {
        let f = fake_with(sealed("u-1", 1));
        f.forced.set(Some(RewrapOutcome::NoRow));
        assert_eq!(run(&f, &Keys, "u-1", 2), LinkRotationOutcome::NoLink);
    }

    #[test]
    fn store_errors_are_scalar_and_not_retried() {
        let f = Fake {
            read_err: true,
            ..fake_with(sealed("u-1", 1))
        };
        assert_eq!(run(&f, &Keys, "u-1", 2), LinkRotationOutcome::StoreFailed);
        assert_eq!(f.cas_calls.get(), 0);

        let g = Fake {
            cas_err: true,
            ..fake_with(sealed("u-1", 1))
        };
        assert_eq!(run(&g, &Keys, "u-1", 2), LinkRotationOutcome::StoreFailed);
        assert_eq!(g.cas_calls.get(), 1);
        assert_eq!(g.row.borrow().as_ref().unwrap().kek_version, 1);
    }
}
