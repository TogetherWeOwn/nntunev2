//! Executed acceptance tests for the production PostgreSQL enrollment seams.
//!
//! Loaded as a child module of worker_entry so private production functions,
//! not copied SQL or a second store implementation, are exercised. The ordinary
//! native suite lists these as ignored; api-migrations explicitly requires and
//! runs every test below against its migrated, throwaway PostgreSQL service.

use super::{complete_registration_on, prune_expired_challenges_on, reap_unverified_users_on};
use crate::auth::{InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential};
use std::future::Future;
use std::time::Duration;
use tokio_postgres::{Client, NoTls};

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const TEST_TIMEOUT: Duration = Duration::from_secs(15);
// Cleanup is called at REAP_NOW, after the aged fixtures' proofs (expiring at
// NOW + 10) have expired, while enrollment itself claimed them at NOW.
const REAP_NOW: u64 = NOW + 20;
const REAP_TTL: u64 = 3600;
const CLEANUP_BOUND: Duration = Duration::from_secs(2);

fn run_test(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("native test runtime")
        .block_on(async {
            tokio::time::timeout(TEST_TIMEOUT, test)
                .await
                .expect("disposable PostgreSQL acceptance test exceeded 15 seconds");
        });
}

async fn connect() -> Client {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("NNTUNE_TEST_DATABASE_URL is required; database tests never silently skip");
    let config: tokio_postgres::Config = url.parse().expect("parse disposable database URL");
    assert_eq!(
        config.get_dbname(),
        Some("nntune_ci"),
        "only the explicitly named disposable nntune_ci database is permitted"
    );
    assert_eq!(config.get_hosts().len(), 1, "one disposable test host only");
    match &config.get_hosts()[0] {
        tokio_postgres::config::Host::Tcp(host) => assert!(
            matches!(host.as_str(), "localhost" | "127.0.0.1"),
            "only the loopback disposable CI test service is permitted"
        ),
        _ => panic!("a disposable TCP test service is required"),
    }
    let (client, connection) = config
        .connect(NoTls)
        .await
        .expect("connect disposable PostgreSQL");
    tokio::spawn(async move {
        connection
            .await
            .expect("disposable PostgreSQL connection driver");
    });
    client
        .batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '3s'")
        .await
        .expect("bound disposable database operations");
    client
}

struct Fixture {
    challenge: PasskeyChallenge,
    magic: MagicLinkRecord,
    invite: InviteRecord,
    credential: PasskeyCredential,
}

impl Fixture {
    async fn seed(client: &Client) -> Self {
        let prefix = format!("atomic-registration-{:016x}", rand::random::<u64>());
        let user_id = format!("{prefix}-user");
        let email = format!("{prefix}@example.com");
        let challenge = PasskeyChallenge {
            id: format!("{prefix}-challenge"),
            rp_id: RP.to_string(),
            origin: ORIGIN.to_string(),
            issued_at: NOW - 10,
            expires_at: NOW + 300,
            consumed: false,
        };
        let magic = MagicLinkRecord {
            token_hash: format!("{prefix}-magic-hash"),
            user_id: user_id.clone(),
            email: email.clone(),
            issued_at: NOW - 10,
            expires_at: NOW + 300,
            used: false,
        };
        let invite = InviteRecord {
            code_hash: format!("{prefix}-invite-hash"),
            used: false,
            created_at: NOW - 10,
        };
        let credential = PasskeyCredential {
            cred_id_b64url: format!("{prefix}-credential"),
            user_id: user_id.clone(),
            public_key: vec![1, 2, 3, 4],
            sign_count: 7,
            registered_at: NOW,
        };
        client
            .execute(
                "INSERT INTO users (id, email, created_at) VALUES ($1, $2, $3)",
                &[&user_id, &email, &(NOW as i64)],
            )
            .await
            .expect("seed disposable user");
        client
            .execute(
                "INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, consumed, purpose) VALUES ($1, $2, $3, $4, $5, FALSE, 'register')",
                &[&challenge.id, &user_id, &challenge.rp_id, &challenge.origin, &(challenge.expires_at as i64)],
            )
            .await
            .expect("seed registration challenge");
        client
            .execute(
                "INSERT INTO magic_link_tokens (token_hash, user_id, email, issued_at, expires_at, used) VALUES ($1, $2, $3, $4, $5, FALSE)",
                &[&magic.token_hash, &user_id, &email, &(magic.issued_at as i64), &(magic.expires_at as i64)],
            )
            .await
            .expect("seed emailed proof");
        client
            .execute(
                "INSERT INTO invite_codes (code_hash, used, created_at) VALUES ($1, FALSE, $2)",
                &[&invite.code_hash, &(invite.created_at as i64)],
            )
            .await
            .expect("seed invite proof");
        Self {
            challenge,
            magic,
            invite,
            credential,
        }
    }

    async fn assert_state(&self, client: &Client, expected: (bool, bool, bool, i64)) {
        let row = client
            .query_one(
                "SELECT c.consumed, m.used, i.used, (SELECT count(*) FROM webauthn_credentials WHERE user_id = $4), (SELECT count(*) FROM sessions WHERE user_id = $4) FROM auth_challenges c, magic_link_tokens m, invite_codes i WHERE c.id = $1 AND m.token_hash = $2 AND i.code_hash = $3",
                &[&self.challenge.id, &self.magic.token_hash, &self.invite.code_hash, &self.credential.user_id],
            )
            .await
            .expect("observe committed database state");
        let actual: (bool, bool, bool, i64) = (row.get(0), row.get(1), row.get(2), row.get(3));
        assert_eq!(
            actual, expected,
            "challenge, magic, invite, credential count"
        );
        assert_eq!(
            row.get::<_, i64>(4),
            0,
            "the storage transaction cannot issue a session"
        );
    }

    async fn assert_credential(&self, client: &Client) {
        let row = client
            .query_one(
                "SELECT cred_id, public_key, sign_count, registered_at FROM webauthn_credentials WHERE user_id = $1",
                &[&self.credential.user_id],
            )
            .await
            .expect("read committed credential");
        assert_eq!(row.get::<_, String>(0), self.credential.cred_id_b64url);
        assert_eq!(row.get::<_, Vec<u8>>(1), vec![1, 2, 3, 4]);
        assert_eq!(row.get::<_, i64>(2), 7);
        assert_eq!(row.get::<_, i64>(3), NOW as i64);
    }
}

// Removing a claim, commit, or insertion must change committed row state.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_commit_without_invite_is_single_use() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            None,
            &fixture.credential,
            NOW,
        )
        .await
        .expect("registration commits without an optional invite");
        fixture.assert_state(&client, (true, true, false, 1)).await;
        fixture.assert_credential(&client).await;

        let mut replay_credential = fixture.credential.clone();
        replay_credential.cred_id_b64url.push_str("-replay");
        assert!(
            complete_registration_on(
                &mut client,
                &fixture.challenge,
                &fixture.magic,
                None,
                &replay_credential,
                NOW,
            )
            .await
            .is_err(),
            "replayed proofs must not enroll another credential"
        );
        fixture.assert_state(&client, (true, true, false, 1)).await;
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_commit_consumes_optional_invite() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            Some(&fixture.invite),
            &fixture.credential,
            NOW,
        )
        .await
        .expect("registration with invite commits");
        // In particular, removing the production invite claim fails here.
        fixture.assert_state(&client, (true, true, true, 1)).await;
        fixture.assert_credential(&client).await;
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_duplicate_rolls_back_and_same_proofs_retry() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        client.execute(
            "INSERT INTO webauthn_credentials (cred_id, user_id, public_key, sign_count, registered_at) VALUES ($1, $2, $3, 0, $4)",
            &[&fixture.credential.cred_id_b64url, &fixture.credential.user_id, &vec![9u8], &(NOW as i64)],
        ).await.expect("seed duplicate credential in disposable database");
        assert!(
            complete_registration_on(
                &mut client,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                &fixture.credential,
                NOW,
            )
            .await
            .is_err(),
            "duplicate insertion must fail"
        );
        fixture
            .assert_state(&client, (false, false, false, 1))
            .await;
        let original = client
            .query_one(
                "SELECT public_key FROM webauthn_credentials WHERE cred_id = $1",
                &[&fixture.credential.cred_id_b64url],
            )
            .await
            .expect("original credential survives rollback");
        assert_eq!(original.get::<_, Vec<u8>>(0), vec![9]);

        client
            .execute(
                "DELETE FROM webauthn_credentials WHERE cred_id = $1 AND user_id = $2",
                &[
                    &fixture.credential.cred_id_b64url,
                    &fixture.credential.user_id,
                ],
            )
            .await
            .expect("remove only the disposable duplicate fixture");
        complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            Some(&fixture.invite),
            &fixture.credential,
            NOW,
        )
        .await
        .expect("the exact same proofs retry successfully after rollback");
        fixture.assert_state(&client, (true, true, true, 1)).await;
        fixture.assert_credential(&client).await;
    });
}

// Snapshot expiry accepts equality, but the conditional claim requires > NOW.
// Changing the production affected-row test from != 1 to > 1 commits here.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_zero_row_magic_claim_rolls_back_earlier_challenge() {
    run_test(async {
        let mut client = connect().await;
        let mut fixture = Fixture::seed(&client).await;
        fixture.magic.expires_at = NOW;
        client
            .execute(
                "UPDATE magic_link_tokens SET expires_at = $1 WHERE token_hash = $2",
                &[&(NOW as i64), &fixture.magic.token_hash],
            )
            .await
            .expect("seed exact magic-expiry equality boundary");
        assert!(
            complete_registration_on(
                &mut client,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                &fixture.credential,
                NOW,
            )
            .await
            .is_err(),
            "a zero-row later claim must abort enrollment"
        );
        fixture
            .assert_state(&client, (false, false, false, 0))
            .await;
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_challenge_loser_spends_no_later_proof() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        client
            .execute(
                "UPDATE auth_challenges SET consumed = TRUE WHERE id = $1",
                &[&fixture.challenge.id],
            )
            .await
            .expect("another request wins the challenge after snapshot");
        assert!(
            complete_registration_on(
                &mut client,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                &fixture.credential,
                NOW,
            )
            .await
            .is_err(),
            "stale challenge snapshot must lose"
        );
        fixture.assert_state(&client, (true, false, false, 0)).await;
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_invite_loser_rolls_back_both_earlier_claims() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        client
            .execute(
                "UPDATE invite_codes SET used = TRUE WHERE code_hash = $1",
                &[&fixture.invite.code_hash],
            )
            .await
            .expect("another request wins the invite after snapshot");
        assert!(
            complete_registration_on(
                &mut client,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                &fixture.credential,
                NOW,
            )
            .await
            .is_err(),
            "stale invite snapshot must lose, not bypass the invite claim"
        );
        fixture.assert_state(&client, (false, false, true, 0)).await;
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_competing_transactions_enroll_exactly_one_credential() {
    run_test(async {
        let mut first = connect().await;
        let mut second = connect().await;
        let fixture = Fixture::seed(&first).await;
        let mut other_credential = fixture.credential.clone();
        other_credential.cred_id_b64url.push_str("-competitor");
        let (left, right) = futures_util::future::join(
            complete_registration_on(
                &mut first,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                &fixture.credential,
                NOW,
            ),
            complete_registration_on(
                &mut second,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                &other_credential,
                NOW,
            ),
        )
        .await;
        assert_eq!(
            usize::from(left.is_ok()) + usize::from(right.is_ok()),
            1,
            "one winner and one generic loser for the same proofs"
        );
        fixture.assert_state(&first, (true, true, true, 1)).await;
        let winner = first
            .query_one(
                "SELECT cred_id FROM webauthn_credentials WHERE user_id = $1",
                &[&fixture.credential.user_id],
            )
            .await
            .expect("one committed winner")
            .get::<_, String>(0);
        if left.is_ok() {
            assert_eq!(winner, fixture.credential.cred_id_b64url);
        } else {
            assert_eq!(winner, other_credential.cred_id_b64url);
        }
    });
}

// Replacing SKIP LOCKED with a waiting delete hits the 1-second test bound;
// omitting skipped-row retry leaves the locked expired fixture behind.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_challenge_cleanup_skips_locked_expired_row_and_retries() {
    run_test(async {
        let cleanup = connect().await;
        let mut locker = connect().await;
        let locked = Fixture::seed(&cleanup).await;
        let unlocked = Fixture::seed(&cleanup).await;
        let live = Fixture::seed(&cleanup).await;
        cleanup
            .execute(
                "UPDATE auth_challenges SET expires_at = $1 WHERE id IN ($2, $3)",
                &[&(NOW as i64), &locked.challenge.id, &unlocked.challenge.id],
            )
            .await
            .expect("expire two disposable challenges, retaining one live control");
        let transaction = locker
            .transaction()
            .await
            .expect("open competing transaction");
        transaction
            .query_one(
                "SELECT id FROM auth_challenges WHERE id = $1 FOR UPDATE",
                &[&locked.challenge.id],
            )
            .await
            .expect("hold an expired challenge lock on another connection");

        tokio::time::timeout(
            Duration::from_secs(1),
            prune_expired_challenges_on(&cleanup, NOW),
        )
        .await
        .expect("cleanup must not wait for the locked enrollment row")
        .expect("production cleanup succeeds under contention");
        let rows = cleanup
            .query(
                "SELECT id FROM auth_challenges WHERE id IN ($1, $2, $3)",
                &[
                    &locked.challenge.id,
                    &unlocked.challenge.id,
                    &live.challenge.id,
                ],
            )
            .await
            .expect("observe cleanup without acquiring the held row lock");
        let ids: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
        assert!(
            ids.contains(&locked.challenge.id),
            "locked expired row survives the first pass"
        );
        assert!(
            !ids.contains(&unlocked.challenge.id),
            "unlocked expired row is removed"
        );
        assert!(
            ids.contains(&live.challenge.id),
            "unexpired row is retained"
        );

        transaction
            .commit()
            .await
            .expect("release the held enrollment lock");
        prune_expired_challenges_on(&cleanup, NOW)
            .await
            .expect("retry production cleanup after release");
        let rows = cleanup
            .query(
                "SELECT id FROM auth_challenges WHERE id IN ($1, $2, $3)",
                &[
                    &locked.challenge.id,
                    &unlocked.challenge.id,
                    &live.challenge.id,
                ],
            )
            .await
            .expect("observe retried cleanup");
        assert_eq!(rows.len(), 1, "both expired rows are eventually removed");
        assert_eq!(rows[0].get::<_, String>(0), live.challenge.id);
    });
}

// --- Reap-before-prune coordination with an in-flight enrollment ----------

/// Make a fixture reap-eligible but for lock coordination: an old signup
/// whose challenge and emailed proof are still claimable at NOW yet expired
/// by REAP_NOW, when the production reaper and pruner run.
async fn age_signup(client: &Client, fixture: &mut Fixture) {
    fixture.challenge.expires_at = NOW + 10;
    fixture.magic.expires_at = NOW + 10;
    client
        .execute(
            "UPDATE users SET created_at = $1 WHERE id = $2",
            &[&((NOW - 100_000) as i64), &fixture.credential.user_id],
        )
        .await
        .expect("age the disposable signup past the reap cutoff");
    client
        .execute(
            "UPDATE auth_challenges SET expires_at = $1 WHERE id = $2",
            &[&((NOW + 10) as i64), &fixture.challenge.id],
        )
        .await
        .expect("shorten the disposable challenge expiry");
    client
        .execute(
            "UPDATE magic_link_tokens SET expires_at = $1 WHERE token_hash = $2",
            &[&((NOW + 10) as i64), &fixture.magic.token_hash],
        )
        .await
        .expect("shorten the disposable emailed-proof expiry");
}

async fn count(client: &Client, statement: &str, id: &str) -> i64 {
    client
        .query_one(statement, &[&id])
        .await
        .expect("observe disposable row counts without taking row locks")
        .get(0)
}

async fn user_count(client: &Client, user_id: &str) -> i64 {
    count(client, "SELECT count(*) FROM users WHERE id = $1", user_id).await
}

async fn challenge_count(client: &Client, challenge_id: &str) -> i64 {
    count(
        client,
        "SELECT count(*) FROM auth_challenges WHERE id = $1",
        challenge_id,
    )
    .await
}

/// Poll until the production enrollment backend reports an ungranted lock.
/// The invite row is the only contended lock in this setup, and program
/// order in `complete_registration_on` (parent lock, challenge claim, magic
/// claim, invite claim) then proves the earlier statements have executed in
/// the actual production function. This synchronizes on the lock wait
/// itself; elapsed time alone would prove nothing.
async fn await_enrollment_lock_wait(observer: &Client, enrollment_pid: i32) {
    for _ in 0..500 {
        let waiting = observer
            .query_one(
                "SELECT count(*) FROM pg_locks WHERE pid = $1 AND NOT granted",
                &[&enrollment_pid],
            )
            .await
            .expect("observe the enrollment backend's lock wait")
            .get::<_, i64>(0);
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("production enrollment never blocked on the contended invite claim");
}

struct BlockedEnrollment {
    fixture: Fixture,
    unrelated: Fixture,
    cleanup: Client,
    locker: Client,
    enrollment: Option<tokio::task::JoinHandle<(Client, Result<(), ()>)>>,
}

/// Start the actual production enrollment for an aged fixture and block it
/// on an invite row locked by an independent connection.
async fn blocked_enrollment() -> BlockedEnrollment {
    let cleanup = connect().await;
    let mut fixture = Fixture::seed(&cleanup).await;
    age_signup(&cleanup, &mut fixture).await;
    let mut unrelated = Fixture::seed(&cleanup).await;
    age_signup(&cleanup, &mut unrelated).await;

    let locker = connect().await;
    locker
        .batch_execute("BEGIN")
        .await
        .expect("open the invite-locker transaction");
    locker
        .query_one(
            "SELECT code_hash FROM invite_codes WHERE code_hash = $1 FOR UPDATE",
            &[&fixture.invite.code_hash],
        )
        .await
        .expect("hold the invite row on an independent connection");

    let mut enrollment_client = connect().await;
    // Enrollment deliberately waits on the invite while cleanup runs. Keep
    // that wait bounded, but beyond the cleanup phase; the 15-second test
    // bound still applies overall.
    enrollment_client
        .batch_execute("SET statement_timeout = '12s'; SET lock_timeout = '10s'")
        .await
        .expect("bound the deliberately blocked enrollment claim");
    let enrollment_pid = enrollment_client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("identify the enrollment backend")
        .get::<_, i32>(0);
    let (challenge, magic, invite, credential) = (
        fixture.challenge.clone(),
        fixture.magic.clone(),
        fixture.invite.clone(),
        fixture.credential.clone(),
    );
    let enrollment = tokio::spawn(async move {
        let result = complete_registration_on(
            &mut enrollment_client,
            &challenge,
            &magic,
            Some(&invite),
            &credential,
            NOW,
        )
        .await
        .map_err(|_| ());
        (enrollment_client, result)
    });
    await_enrollment_lock_wait(&cleanup, enrollment_pid).await;
    BlockedEnrollment {
        fixture,
        unrelated,
        cleanup,
        locker,
        enrollment: Some(enrollment),
    }
}

/// Run the production cleanup in register-start order (reap, then prune)
/// against the blocked enrollment. Both helpers must return promptly,
/// preserve the in-flight signup, and still reap the unrelated old signup.
async fn assert_reap_before_prune_defers(blocked: &mut BlockedEnrollment) {
    let fixture_user = blocked.fixture.credential.user_id.clone();
    let unrelated_user = blocked.unrelated.credential.user_id.clone();
    tokio::time::timeout(
        CLEANUP_BOUND,
        reap_unverified_users_on(&mut blocked.cleanup, REAP_NOW, REAP_TTL, None),
    )
    .await
    .expect("user reaper must not wait for the in-flight enrollment")
    .expect("production user reaper succeeds under enrollment contention");
    assert_eq!(
        user_count(&blocked.cleanup, &fixture_user).await,
        1,
        "reaper must preserve the in-flight enrollment's user"
    );
    assert_eq!(
        user_count(&blocked.cleanup, &unrelated_user).await,
        0,
        "reaper must still reap an unrelated eligible old signup"
    );
    tokio::time::timeout(
        CLEANUP_BOUND,
        prune_expired_challenges_on(&blocked.cleanup, REAP_NOW),
    )
    .await
    .expect("challenge pruner must not wait for the in-flight enrollment")
    .expect("production challenge pruner succeeds under enrollment contention");
    assert_eq!(
        challenge_count(&blocked.cleanup, &blocked.fixture.challenge.id).await,
        1,
        "pruner must defer the challenge claimed by the in-flight enrollment"
    );
    blocked
        .fixture
        .assert_state(&blocked.cleanup, (false, false, false, 0))
        .await;
}

async fn finish_enrollment(blocked: &mut BlockedEnrollment) -> Result<(), ()> {
    let enrollment = blocked
        .enrollment
        .take()
        .expect("enrollment task must be joined exactly once");
    let (_enrollment_client, result) = tokio::time::timeout(Duration::from_secs(10), enrollment)
        .await
        .expect("enrollment must finish once the invite lock is released")
        .expect("enrollment task must not panic");
    result
}

// Removing enrollment's parent lock (FOR KEY SHARE) lets the reaper select
// this user and stall on the claimed child rows, hitting the cleanup bound.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_before_prune_preserves_in_flight_enrollment() {
    run_test(async {
        let mut blocked = blocked_enrollment().await;
        assert_reap_before_prune_defers(&mut blocked).await;

        blocked
            .locker
            .batch_execute("COMMIT")
            .await
            .expect("release the invite lock without spending the invite");
        let result = finish_enrollment(&mut blocked).await;
        assert!(
            result.is_ok(),
            "enrollment must commit once the invite lock is released"
        );
        blocked
            .fixture
            .assert_state(&blocked.cleanup, (true, true, true, 1))
            .await;
        blocked.fixture.assert_credential(&blocked.cleanup).await;

        // Deferred cleanup retries after the protecting transaction ends:
        // the reaper now spares the credentialed user, and the pruner
        // removes the expired challenge it previously had to skip.
        let fixture_user = blocked.fixture.credential.user_id.clone();
        reap_unverified_users_on(&mut blocked.cleanup, REAP_NOW, REAP_TTL, None)
            .await
            .expect("retry production user reaper after enrollment commits");
        assert_eq!(
            user_count(&blocked.cleanup, &fixture_user).await,
            1,
            "retried reaper must spare the now-credentialed user"
        );
        prune_expired_challenges_on(&blocked.cleanup, REAP_NOW)
            .await
            .expect("retry production challenge pruner after enrollment commits");
        assert_eq!(
            challenge_count(&blocked.cleanup, &blocked.fixture.challenge.id).await,
            0,
            "retried pruner must remove the expired challenge"
        );
        assert_eq!(user_count(&blocked.cleanup, &fixture_user).await, 1);
        blocked.fixture.assert_credential(&blocked.cleanup).await;
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_after_enrollment_rollback_cleans_expired_signup() {
    run_test(async {
        let mut blocked = blocked_enrollment().await;
        assert_reap_before_prune_defers(&mut blocked).await;

        blocked
            .locker
            .execute(
                "UPDATE invite_codes SET used = TRUE WHERE code_hash = $1",
                &[&blocked.fixture.invite.code_hash],
            )
            .await
            .expect("another request spends the invite while enrollment waits");
        blocked
            .locker
            .batch_execute("COMMIT")
            .await
            .expect("release the invite lock after spending it");
        let result = finish_enrollment(&mut blocked).await;
        assert!(
            result.is_err(),
            "enrollment must roll back when its invite claim loses"
        );
        blocked
            .fixture
            .assert_state(&blocked.cleanup, (false, false, true, 0))
            .await;

        // With the protecting transaction gone and no credential committed,
        // retried reap-before-prune cleanup now removes the expired signup.
        let fixture_user = blocked.fixture.credential.user_id.clone();
        reap_unverified_users_on(&mut blocked.cleanup, REAP_NOW, REAP_TTL, None)
            .await
            .expect("retry production user reaper after enrollment rolls back");
        assert_eq!(
            user_count(&blocked.cleanup, &fixture_user).await,
            0,
            "retried reaper must remove the rolled-back expired signup"
        );
        assert_eq!(
            challenge_count(&blocked.cleanup, &blocked.fixture.challenge.id).await,
            0,
            "reaping the user must cascade to its expired challenge"
        );
        prune_expired_challenges_on(&blocked.cleanup, REAP_NOW)
            .await
            .expect("retry production challenge pruner after the reap");
    });
}

// Drive the two production reaper statements in production sequence so the
// delete's eligibility recheck can be observed against a candidate list
// selected before a credential existed. The credential is inserted by the
// reaper transaction itself: a cross-transaction insert would take the
// foreign-key parent lock and wait behind the candidate lock, so it cannot
// be the synchronizing seam here. What this pins is that the second
// statement reapplies the full eligibility filter to the locked ids rather
// than deleting by id alone; the statement sequencing and READ COMMITTED
// fresh snapshot are pinned with it in
// atomic_registration_reap_wiring_acceptance.rs.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_delete_rechecks_locked_candidates_before_deleting() {
    run_test(async {
        let mut reaper = connect().await;
        let mut enrolled = Fixture::seed(&reaper).await;
        age_signup(&reaper, &mut enrolled).await;
        let mut abandoned = Fixture::seed(&reaper).await;
        age_signup(&reaper, &mut abandoned).await;

        let cutoff = (REAP_NOW - REAP_TTL) as i64;
        let now = REAP_NOW as i64;
        let transaction = reaper
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)
            .start()
            .await
            .expect("open the reaper transaction as production does");
        let candidates = transaction
            .query(
                &crate::handlers::lock_unverified_users_sql(),
                &[&cutoff, &None::<&str>, &now],
            )
            .await
            .expect("lock reap candidates with the production statement");
        let ids: Vec<String> = candidates.iter().map(|row| row.get("id")).collect();
        assert!(
            ids.contains(&enrolled.credential.user_id)
                && ids.contains(&abandoned.credential.user_id),
            "both aged signups must be locked candidates before any credential exists"
        );
        transaction
            .execute(
                "INSERT INTO webauthn_credentials (cred_id, user_id, public_key, sign_count, registered_at) VALUES ($1, $2, $3, $4, $5)",
                &[
                    &enrolled.credential.cred_id_b64url,
                    &enrolled.credential.user_id,
                    &enrolled.credential.public_key.as_slice(),
                    &i64::from(enrolled.credential.sign_count),
                    &(NOW as i64),
                ],
            )
            .await
            .expect("a credential lands after candidate selection");
        transaction
            .execute(
                &crate::handlers::reap_unverified_users_sql(),
                &[&cutoff, &None::<&str>, &now, &ids],
            )
            .await
            .expect("delete locked candidates with the production statement");
        transaction
            .commit()
            .await
            .expect("commit the reaper transaction");

        assert_eq!(
            user_count(&reaper, &enrolled.credential.user_id).await,
            1,
            "the fresh recheck must spare the candidate that gained a credential"
        );
        enrolled.assert_credential(&reaper).await;
        assert_eq!(
            user_count(&reaper, &abandoned.credential.user_id).await,
            0,
            "the recheck must still reap the candidate with no credential"
        );
    });
}
