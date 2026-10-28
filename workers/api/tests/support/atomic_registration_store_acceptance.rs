//! Executed acceptance tests for the production PostgreSQL enrollment seams.
//!
//! Loaded as a child module of worker_entry so private production functions,
//! not copied SQL or a second store implementation, are exercised. The ordinary
//! native suite lists these as ignored; api-migrations explicitly requires and
//! runs every test below against its migrated, throwaway PostgreSQL service.

use super::{complete_registration_on, prune_expired_challenges_on};
use crate::auth::{InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential};
use std::future::Future;
use std::time::Duration;
use tokio_postgres::{Client, NoTls};

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

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
