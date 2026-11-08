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
//! [`rotate_batch`] adds a bounded keyset candidate scan in front of that
//! per-user helper: one scan page (at most [`MAX_SCAN_LIMIT`] owner ids, in
//! ASCII byte keyset order, strictly after an optional typed cursor) and one
//! [`rotate_link`] per candidate. It validates the page (count, order, strict
//! cursor advancement) before ANY write and returns scalar counts plus a
//! resume cursor only. It does not persist progress, retry, schedule or
//! serialize jobs: the caller must durably record progress, run one job at a
//! time and repeat from the start for deferred or failed rows. Reaching the
//! end of the keyset means only that this pass ran out of candidates; it is
//! NOT proof that every link is rotated (poison, changed, late-created or
//! lost-CAS rows can remain), so it must not be used alone to retire a key.
//!
//! Results are scalar enums only: no envelope, key, username or error detail
//! can reach a caller or a log. This module emits no log calls.

use std::future::Future;

use crate::{
    handlers::{Store, StoreError},
    listenbrainz::{LinkError, RewrapOutcome, StoredLink},
    rotation::KekSource,
};

/// Largest candidate page one scan may request.
pub const MAX_SCAN_LIMIT: u32 = 100;

/// Opaque owner id used as a keyset cursor: the same 1..=64 byte ASCII
/// alphanumeric/`-`/`_` shape the credential scope accepts (account ids are
/// 22-char base64url, not UUIDs). No canonicalization or case folding; order
/// is plain ASCII byte order (`COLLATE "C"` in SQL). Not a secret, but never
/// listed in bulk by this module: only the resume cursor leaves a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerCursor(String);

impl OwnerCursor {
    pub fn parse(s: &str) -> Option<Self> {
        crate::listenbrainz::valid_user_id(s).then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reject bad scan arguments. Callers run this before connecting or querying.
pub(crate) fn check_scan_args(below_version: u32, limit: u32) -> Result<(), StoreError> {
    if below_version == 0
        || i32::try_from(below_version).is_err()
        || limit == 0
        || limit > MAX_SCAN_LIMIT
    {
        return Err(StoreError);
    }
    Ok(())
}

/// The store operations the orchestrator needs. Blanket-implemented for
/// every [`Store`], so production reuses `listenbrainz_link` and
/// `cas_rewrap_listenbrainz_link`; tests supply a small fake.
pub trait LinkRewrapStore {
    /// Owner ids whose link has `kek_version < below_version`, ascending,
    /// strictly after `after`, at most `limit` (1..=[`MAX_SCAN_LIMIT`]).
    fn list_candidates(
        &self,
        below_version: u32,
        after: Option<&OwnerCursor>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<OwnerCursor>, StoreError>>;
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
    fn list_candidates(
        &self,
        below_version: u32,
        after: Option<&OwnerCursor>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<OwnerCursor>, StoreError>> {
        self.listenbrainz_rotation_candidates(below_version, after, limit)
    }

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

/// Why a batch did not start rotating. Nothing was written in any variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationBatchError {
    /// Target version or limit out of range; no store call was made.
    InvalidArguments,
    /// The candidate scan failed; reported, not swallowed.
    ScanFailed,
    /// The store returned an oversize, unordered or non-advancing page.
    InvalidScan,
}

/// Scalar result of one batch. No identity list, envelope or key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationBatchReport {
    pub scanned: u32,
    pub rewrapped: u32,
    /// Row vanished (unlinked or deleted) before it could be rotated.
    pub no_link: u32,
    /// Reload showed the row already at or above the target (raced rotation).
    pub already_current: u32,
    /// Lost CAS or owner being deleted; nothing written. Needs a later pass.
    pub deferred: u32,
    /// Read/CAS store failure, unknown key, corrupt or unauthenticated row.
    /// Not retried; needs a later pass and likely operator attention.
    pub failed: u32,
    /// Pass this as the next cursor: last candidate scanned, else the input.
    pub resume_after: Option<OwnerCursor>,
    /// The page was short, so this keyset pass has no further candidates.
    /// Not global readiness: see the module docs.
    pub end_of_keyset: bool,
}

/// Rotate one scan page to `target_version`. One scan, then exactly one
/// [`rotate_link`] per candidate (it reloads the full row and does one CAS;
/// nothing is retried). The page is validated before any write.
pub async fn rotate_batch(
    store: &impl LinkRewrapStore,
    keks: &impl KekSource,
    target_version: u32,
    after: Option<&OwnerCursor>,
    limit: u32,
    now: u64,
) -> Result<RotationBatchReport, RotationBatchError> {
    check_scan_args(target_version, limit).map_err(|_| RotationBatchError::InvalidArguments)?;
    let candidates = store
        .list_candidates(target_version, after, limit)
        .await
        .map_err(|StoreError| RotationBatchError::ScanFailed)?;
    if candidates.len() > limit as usize {
        return Err(RotationBatchError::InvalidScan);
    }
    let mut previous = after;
    for c in &candidates {
        if previous.is_some_and(|p| c.as_str() <= p.as_str()) {
            return Err(RotationBatchError::InvalidScan);
        }
        previous = Some(c);
    }
    let mut report = RotationBatchReport {
        scanned: candidates.len() as u32,
        rewrapped: 0,
        no_link: 0,
        already_current: 0,
        deferred: 0,
        failed: 0,
        resume_after: candidates.last().or(after).cloned(),
        end_of_keyset: candidates.len() < limit as usize,
    };
    for c in &candidates {
        let counter = match rotate_link(store, keks, c.as_str(), target_version, now).await {
            LinkRotationOutcome::Rewrapped => &mut report.rewrapped,
            LinkRotationOutcome::NoLink => &mut report.no_link,
            LinkRotationOutcome::NotIncreasing => &mut report.already_current,
            LinkRotationOutcome::ClaimLost | LinkRotationOutcome::AccountDeleting => {
                &mut report.deferred
            }
            LinkRotationOutcome::InvalidTargetVersion
            | LinkRotationOutcome::InvalidUser
            | LinkRotationOutcome::OwnerMismatch
            | LinkRotationOutcome::UnknownKey
            | LinkRotationOutcome::Corrupt
            | LinkRotationOutcome::Crypto
            | LinkRotationOutcome::StoreFailed => &mut report.failed,
        };
        *counter += 1;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listenbrainz::LinkInput;
    use std::cell::{Cell, RefCell};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Fakes complete immediately; a pending future is a test bug.
    fn block_on<F: Future>(f: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
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
        async fn list_candidates(
            &self,
            _below_version: u32,
            _after: Option<&OwnerCursor>,
            _limit: u32,
        ) -> Result<Vec<OwnerCursor>, StoreError> {
            Err(StoreError)
        }

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

    // ---- batch orchestration ----

    use std::collections::{BTreeMap, HashMap, HashSet};

    fn uid(n: u32) -> String {
        format!("acct{n:018}")
    }

    fn cur(n: u32) -> OwnerCursor {
        OwnerCursor::parse(&uid(n)).unwrap()
    }

    #[derive(Default)]
    struct Multi {
        rows: RefCell<BTreeMap<String, StoredLink>>,
        /// Replaces the scan result (misbehaving store).
        scan_override: RefCell<Option<Vec<OwnerCursor>>>,
        scan_err: bool,
        scans: RefCell<Vec<(u32, Option<String>, u32)>>,
        vanish_before_read: RefCell<HashSet<String>>,
        swap_on_cas: RefCell<HashMap<String, StoredLink>>,
        cas_users: RefCell<Vec<String>>,
        reads: RefCell<Vec<String>>,
    }

    impl LinkRewrapStore for Multi {
        async fn list_candidates(
            &self,
            below: u32,
            after: Option<&OwnerCursor>,
            limit: u32,
        ) -> Result<Vec<OwnerCursor>, StoreError> {
            self.scans
                .borrow_mut()
                .push((below, after.map(|a| a.as_str().to_owned()), limit));
            if self.scan_err {
                return Err(StoreError);
            }
            if let Some(v) = self.scan_override.borrow().clone() {
                return Ok(v);
            }
            Ok(self
                .rows
                .borrow()
                .iter()
                .filter(|(k, r)| {
                    r.kek_version < below && after.is_none_or(|a| k.as_str() > a.as_str())
                })
                .take(limit as usize)
                .map(|(k, _)| OwnerCursor::parse(k).unwrap())
                .collect())
        }

        async fn read_link(&self, user_id: &str) -> Result<Option<StoredLink>, StoreError> {
            self.reads.borrow_mut().push(user_id.to_owned());
            if self.vanish_before_read.borrow().contains(user_id) {
                self.rows.borrow_mut().remove(user_id);
            }
            Ok(self.rows.borrow().get(user_id).cloned())
        }

        async fn cas_rewrap_link(
            &self,
            user_id: &str,
            captured: &StoredLink,
            replacement: &StoredLink,
            _now: u64,
        ) -> Result<RewrapOutcome, StoreError> {
            self.cas_users.borrow_mut().push(user_id.to_owned());
            if let Some(newer) = self.swap_on_cas.borrow_mut().remove(user_id) {
                self.rows.borrow_mut().insert(user_id.to_owned(), newer);
            }
            let mut rows = self.rows.borrow_mut();
            match rows.get(user_id) {
                None => Ok(RewrapOutcome::NoRow),
                Some(cur) if same(cur, captured) => {
                    rows.insert(user_id.to_owned(), replacement.clone());
                    Ok(RewrapOutcome::Rewrapped)
                }
                Some(_) => Ok(RewrapOutcome::ClaimLost),
            }
        }
    }

    fn multi(versions: &[(u32, u32)]) -> Multi {
        let m = Multi::default();
        for (n, v) in versions {
            m.rows.borrow_mut().insert(uid(*n), sealed(&uid(*n), *v));
        }
        m
    }

    fn batch(
        m: &Multi,
        target: u32,
        after: Option<&OwnerCursor>,
        limit: u32,
    ) -> Result<RotationBatchReport, RotationBatchError> {
        block_on(rotate_batch(m, &Keys, target, after, limit, 1000))
    }

    fn version_of(m: &Multi, n: u32) -> Option<u32> {
        m.rows.borrow().get(&uid(n)).map(|r| r.kek_version)
    }

    #[test]
    fn cursor_accepts_the_credential_scope_id_shape_only() {
        assert!(OwnerCursor::parse(&uid(1)).is_some());
        let max = "a".repeat(64);
        for ok in ["u-1", "A_b-9", "dGVzdC1hY2NvdW50LTAwMQ", max.as_str()] {
            assert_eq!(OwnerCursor::parse(ok).unwrap().as_str(), ok, "{ok}");
        }
        let long = "a".repeat(65);
        for bad in [
            "",
            "bad id!",
            "a.b",
            "a/b",
            "a+b",
            "a=",
            "\u{e9}",
            long.as_str(),
        ] {
            assert!(OwnerCursor::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn opaque_ids_page_in_ascii_byte_order_without_case_folding() {
        // Digits < uppercase < `_` < lowercase; `-` < digits. 22-char
        // base64url account ids included.
        let ids = [
            "dGVzdC1hY2NvdW50LTAwMQ",
            "Zm9vYmFyYmF6cXV4MTIzNA",
            "a-b",
            "a_b",
            "aB",
            "ab",
            "0AAAAAAAAAAAAAAAAAAAAA",
            "-AAAAAAAAAAAAAAAAAAAAA",
        ];
        let m = Multi::default();
        for id in ids {
            m.rows.borrow_mut().insert(id.to_owned(), sealed(id, 1));
        }
        let mut want: Vec<&str> = ids.to_vec();
        want.sort_unstable();
        assert_eq!(
            want,
            [
                "-AAAAAAAAAAAAAAAAAAAAA",
                "0AAAAAAAAAAAAAAAAAAAAA",
                "Zm9vYmFyYmF6cXV4MTIzNA",
                "a-b",
                "aB",
                "a_b",
                "ab",
                "dGVzdC1hY2NvdW50LTAwMQ"
            ]
        );
        let mut after: Option<OwnerCursor> = None;
        let mut seen = Vec::new();
        loop {
            let r = batch(&m, 2, after.as_ref(), 3).unwrap();
            seen.extend(m.cas_users.borrow().iter().skip(seen.len()).cloned());
            after = r.resume_after;
            if r.end_of_keyset {
                break;
            }
        }
        assert_eq!(seen, want);
        assert!(m.rows.borrow().values().all(|r| r.kek_version == 2));
    }

    #[test]
    fn bad_scan_arguments_fail_before_any_store_call() {
        let m = multi(&[(1, 1)]);
        for (target, limit) in [(0, 10), (2, 0), (2, MAX_SCAN_LIMIT + 1), (2, u32::MAX)] {
            assert_eq!(
                batch(&m, target, None, limit),
                Err(RotationBatchError::InvalidArguments)
            );
        }
        assert_eq!(
            batch(&m, i32::MAX as u32 + 1, None, 10),
            Err(RotationBatchError::InvalidArguments)
        );
        assert!(m.scans.borrow().is_empty());
        assert!(m.reads.borrow().is_empty());
        assert!(check_scan_args(2, MAX_SCAN_LIMIT).is_ok());
        assert!(check_scan_args(i32::MAX as u32, 1).is_ok());
    }

    #[test]
    fn one_batch_is_bounded_ordered_and_resumes_by_cursor() {
        let m = multi(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 3)]);
        let r = batch(&m, 2, None, 3).unwrap();
        assert_eq!((r.scanned, r.rewrapped), (3, 3));
        assert!(!r.end_of_keyset);
        assert_eq!(r.resume_after, Some(cur(3)));
        assert_eq!(*m.cas_users.borrow(), vec![uid(1), uid(2), uid(3)]);
        assert_eq!(version_of(&m, 4), Some(1));
        assert_eq!(m.scans.borrow().len(), 1);

        let r = batch(&m, 2, r.resume_after.as_ref(), 3).unwrap();
        assert_eq!((r.scanned, r.rewrapped), (1, 1));
        assert!(r.end_of_keyset);
        assert_eq!(r.resume_after, Some(cur(4)));
        // Row 5 (already above target) was never a candidate or touched.
        assert_eq!(version_of(&m, 5), Some(3));
        assert_eq!(m.cas_users.borrow().len(), 4);

        // Empty page keeps the input cursor and reports the end.
        let r = batch(&m, 2, Some(&cur(4)), 3).unwrap();
        assert_eq!(r.scanned, 0);
        assert!(r.end_of_keyset);
        assert_eq!(r.resume_after, Some(cur(4)));
    }

    #[test]
    fn misbehaving_scan_is_rejected_before_any_read_or_write() {
        let cases: Vec<(Vec<OwnerCursor>, Option<OwnerCursor>)> = vec![
            (vec![cur(2), cur(1)], None),
            (vec![cur(1), cur(1)], None),
            (vec![cur(1), cur(2), cur(3)], None),
            (vec![cur(2)], Some(cur(2))),
            (vec![cur(1)], Some(cur(2))),
            (vec![cur(3), cur(2)], Some(cur(1))),
        ];
        for (page, after) in cases {
            let m = multi(&[(1, 1), (2, 1), (3, 1)]);
            *m.scan_override.borrow_mut() = Some(page);
            assert_eq!(
                batch(&m, 2, after.as_ref(), 2),
                Err(RotationBatchError::InvalidScan)
            );
            assert!(m.reads.borrow().is_empty());
            assert!(m.cas_users.borrow().is_empty());
            assert!(m.rows.borrow().values().all(|r| r.kek_version == 1));
        }
    }

    #[test]
    fn scan_failure_is_reported_and_nothing_is_mutated() {
        let m = Multi {
            scan_err: true,
            ..multi(&[(1, 1)])
        };
        assert_eq!(batch(&m, 2, None, 5), Err(RotationBatchError::ScanFailed));
        assert!(m.reads.borrow().is_empty());
        assert!(m.cas_users.borrow().is_empty());
        assert_eq!(version_of(&m, 1), Some(1));
    }

    #[test]
    fn poison_lost_cas_and_deleted_rows_are_counted_and_skipped_without_retry() {
        let m = multi(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
        // 2: poison (tampered wrap). 3: relinked between read and CAS.
        m.rows
            .borrow_mut()
            .get_mut(&uid(2))
            .unwrap()
            .envelope
            .dek_wrapped[0] ^= 1;
        let poisoned = m.rows.borrow().get(&uid(2)).unwrap().clone();
        let relinked = LinkInput::new("alice_lb", "1b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b")
            .unwrap()
            .seal(&Keys, 1, &uid(3))
            .unwrap();
        m.swap_on_cas.borrow_mut().insert(uid(3), relinked.clone());
        m.vanish_before_read.borrow_mut().insert(uid(4));
        let r = batch(&m, 2, None, 10).unwrap();
        assert_eq!(r.scanned, 5);
        assert_eq!(
            (
                r.rewrapped,
                r.failed,
                r.deferred,
                r.no_link,
                r.already_current
            ),
            (2, 1, 1, 1, 0)
        );
        assert!(r.end_of_keyset);
        assert_eq!(r.resume_after, Some(cur(5)));
        assert!(same(m.rows.borrow().get(&uid(2)).unwrap(), &poisoned));
        assert!(same(m.rows.borrow().get(&uid(3)).unwrap(), &relinked));
        assert!(m.rows.borrow().get(&uid(4)).is_none());
        // One read per candidate; one CAS for each non-poison, non-vanished row.
        assert_eq!(m.reads.borrow().len(), 5);
        assert_eq!(*m.cas_users.borrow(), vec![uid(1), uid(3), uid(5)]);
    }

    #[test]
    fn batch_never_downgrades_or_rewrites_current_rows() {
        let m = multi(&[(1, 2), (2, 3)]);
        // A store that wrongly lists current rows: reload refuses to downgrade.
        *m.scan_override.borrow_mut() = Some(vec![cur(1), cur(2)]);
        let r = batch(&m, 2, None, 5).unwrap();
        assert_eq!((r.scanned, r.already_current, r.rewrapped), (2, 2, 0));
        assert!(m.cas_users.borrow().is_empty());
        assert_eq!((version_of(&m, 1), version_of(&m, 2)), (Some(2), Some(3)));
    }

    #[test]
    fn unknown_target_key_is_failed_per_row_without_write() {
        let m = multi(&[(1, 1), (2, 1)]);
        let r = batch(&m, 9, None, 5).unwrap();
        assert_eq!((r.scanned, r.failed, r.rewrapped), (2, 2, 0));
        assert!(m.cas_users.borrow().is_empty());
    }
}
