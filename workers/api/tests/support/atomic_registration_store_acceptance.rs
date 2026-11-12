//! Executed acceptance tests for the production PostgreSQL enrollment seams
//! and the admin-foundation write transactions (migration 0028).
//!
//! Loaded as a child module of worker_entry so private production functions,
//! not copied SQL or a second store implementation, are exercised. The ordinary
//! native suite lists these as ignored; api-migrations explicitly requires and
//! runs every test below against its migrated, throwaway PostgreSQL service.

use super::{
    admin_ingest_delete_on, admin_ingest_upload_on, admin_issue_invite_on, admin_revoke_invite_on,
    admin_set_app_setting_on, admin_set_user_role_on, challenge_by_id_on, complete_registration_on,
    insert_challenge_on, prune_expired_challenges_on, reap_unverified_users_on,
};
use crate::admin_ingest::{
    AdminIngestAttempt, AdminIngestDeleteOutcome, AdminIngestUploadOutcome, FailureReason,
    IngestOriginalEnvelope, INGEST_UPLOAD_AUDIT_ACTION,
};
use crate::auth::{InviteRecord, MagicLinkRecord, PasskeyChallenge, PasskeyCredential};
use crate::handlers::{
    AdminInviteIssueOutcome, AdminInviteRevokeOutcome, AdminWriteOutcome, InviteBudgetReceipt,
};
use crate::vault::{audit_event_extended, AuditEvent};
use std::future::Future;
use std::time::Duration;
use tokio_postgres::error::SqlState;
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

// A hostaddr overrides host resolution in tokio-postgres. Reject all such
// overrides, including loopback ones, before this destructive bank can connect.
// Configuration-only tests below never open a socket or read credentials.
fn validate_disposable_database_config(
    config: &tokio_postgres::Config,
) -> Result<(), &'static str> {
    if config.get_dbname() != Some("nntune_ci") {
        return Err("only the explicitly named disposable nntune_ci database is permitted");
    }
    if !config.get_hostaddrs().is_empty() {
        return Err("hostaddr overrides are not permitted for the disposable test service");
    }
    if config.get_hosts().len() != 1 {
        return Err("one disposable test host only");
    }
    match &config.get_hosts()[0] {
        tokio_postgres::config::Host::Tcp(host)
            if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") =>
        {
            Ok(())
        }
        _ => Err("only the loopback disposable TCP test service is permitted"),
    }
}

#[test]
fn disposable_database_config_accepts_standard_loopback_service() {
    for input in [
        "postgresql://localhost/nntune_ci",
        "host=127.0.0.1 dbname=nntune_ci",
        "host=::1 dbname=nntune_ci",
    ] {
        let config = input
            .parse()
            .expect("parse credential-free loopback config");
        assert_eq!(validate_disposable_database_config(&config), Ok(()));
    }
}

// Removing the hostaddr guard must fail here without ever attempting a
// connection, even when the advertised hostname and database are permitted.
#[test]
fn disposable_database_config_rejects_hostaddr_overrides() {
    for input in [
        "host=localhost hostaddr=192.0.2.10 dbname=nntune_ci",
        "host=127.0.0.1 hostaddr=2001:db8::10 dbname=nntune_ci",
        "host=localhost hostaddr=127.0.0.1 dbname=nntune_ci",
        "host=localhost hostaddr=::1 dbname=nntune_ci",
        "host=localhost hostaddr=127.0.0.1,192.0.2.10 dbname=nntune_ci",
        "host=localhost hostaddr=::1,2001:db8::10 dbname=nntune_ci",
        "host=localhost hostaddr=127.0.0.1,::1 dbname=nntune_ci",
        "hostaddr=192.0.2.10 dbname=nntune_ci",
    ] {
        let config = input
            .parse()
            .expect("parse credential-free address override");
        assert_eq!(
            validate_disposable_database_config(&config),
            Err("hostaddr overrides are not permitted for the disposable test service"),
            "hostaddr override must be rejected before connecting"
        );
    }
}

#[test]
fn disposable_database_config_rejects_wrong_or_missing_database() {
    for input in [
        "host=localhost dbname=postgres",
        "host=localhost dbname=nntune_ci_backup",
        "host=localhost",
    ] {
        let config = input
            .parse()
            .expect("parse credential-free database config");
        assert_eq!(
            validate_disposable_database_config(&config),
            Err("only the explicitly named disposable nntune_ci database is permitted")
        );
    }
}

#[test]
fn disposable_database_config_rejects_nonloopback_and_multiple_hosts() {
    for input in [
        "host=db.example.com dbname=nntune_ci",
        "host=192.0.2.10 dbname=nntune_ci",
        "host=2001:db8::10 dbname=nntune_ci",
        "host=localhost,127.0.0.1 dbname=nntune_ci",
        "host=localhost,db.example.com dbname=nntune_ci",
        "host=/tmp dbname=nntune_ci",
        "dbname=nntune_ci",
    ] {
        let config = input.parse().expect("parse credential-free host config");
        assert!(
            validate_disposable_database_config(&config).is_err(),
            "a nonloopback, ambiguous or missing host must be rejected before connecting"
        );
    }
}

async fn connect() -> Client {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("NNTUNE_TEST_DATABASE_URL is required; database tests never silently skip");
    let config: tokio_postgres::Config = url.parse().expect("parse disposable database URL");
    validate_disposable_database_config(&config)
        .expect("validate disposable database configuration before connecting");
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
    invite_audit: AuditEvent,
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
        // The record mirrors what `prepare_invite_redemption` hands the
        // enrollment transaction: spend stamps already written, the
        // conditional claim binds them.
        let mut invite = InviteRecord::new_issued(
            format!("{prefix}-invite-hash"),
            None,
            crate::auth::INVITE_KIND_ADMIN,
            None,
            NOW - 10,
            NOW + 604_800,
        );
        invite.used = true;
        invite.redeemed_by = Some(user_id.clone());
        invite.redeemed_at = Some(NOW);
        let invite_audit = audit_event_extended(
            "system",
            crate::auth::INVITE_REDEEMED_AUDIT_ACTION,
            true,
            "127.0.0.1",
            NOW,
            Some(&user_id),
            serde_json::json!({"invite_id": serde_json::Value::Null}),
        );
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
                "INSERT INTO invite_codes (code_hash, used, created_at, expires_at) VALUES ($1, FALSE, $2, $3)",
                &[
                    &invite.code_hash,
                    &(invite.created_at as i64),
                    &(invite.expires_at as i64),
                ],
            )
            .await
            .expect("seed invite proof");
        Self {
            challenge,
            magic,
            invite,
            invite_audit,
            credential,
        }
    }

    async fn seed_receipted(client: &Client) -> Self {
        let mut fixture = Self::seed(client).await;
        fixture.challenge.id.push_str("-receipted");
        insert_challenge_on(
            client,
            &fixture.challenge,
            Some(&fixture.credential.user_id),
            "register",
            Some(&receipt_for(&fixture, "enrollment")),
        )
        .await
        .expect("insert a fresh challenge and receipt through production SQL");
        fixture
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
            Some(&fixture.invite_audit),
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
        let fixture = Fixture::seed_receipted(&client).await;
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
                Some(&fixture.invite_audit),
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
            Some(&fixture.invite_audit),
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
                Some(&fixture.invite_audit),
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
                Some(&fixture.invite_audit),
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
                Some(&fixture.invite_audit),
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
fn postgres_receipted_registration_audit_failure_rolls_back_and_retries() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed_receipted(&client).await;
        let receipt = receipt_for(&fixture, "enrollment");
        // A trigger scoped to this subject is disposable and removed before
        // retry. It cannot affect other fixtures in this isolated CI database.
        let subject = &fixture.credential.user_id;
        assert!(subject
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        client
            .batch_execute(&format!(
            "CREATE FUNCTION pg_temp.refuse_receipt_audit() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN IF NEW.subject_user_id = '{subject}' AND NEW.action = 'invite.redeemed' \
             THEN RAISE EXCEPTION 'fixture audit failure'; END IF; RETURN NEW; END $$; \
             CREATE TRIGGER refuse_receipt_audit BEFORE INSERT ON audit_log \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_receipt_audit();"
        ))
            .await
            .expect("install fixture-scoped audit insertion fault");
        assert!(complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            Some(&fixture.invite),
            Some(&fixture.invite_audit),
            &fixture.credential,
            NOW,
        )
        .await
        .is_err());
        fixture
            .assert_state(&client, (false, false, false, 0))
            .await;
        let row = challenge_by_id_on(&client, &fixture.challenge.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.invite_budget_receipt, Some(receipt.clone()));
        client
            .batch_execute("DROP TRIGGER refuse_receipt_audit ON audit_log")
            .await
            .expect("remove disposable audit fault before retry");
        complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            Some(&fixture.invite),
            Some(&fixture.invite_audit),
            &fixture.credential,
            NOW,
        )
        .await
        .expect("same receipted proofs retry after audit failure");
        fixture.assert_state(&client, (true, true, true, 1)).await;
        let audits: i64 = client.query_one(
            "SELECT count(*) FROM audit_log WHERE subject_user_id = $1 AND action = 'invite.redeemed'",
            &[subject],
        ).await.unwrap().get(0);
        assert_eq!(audits, 1);
        assert_eq!(
            challenge_by_id_on(&client, &fixture.challenge.id)
                .await
                .unwrap()
                .unwrap()
                .invite_budget_receipt,
            Some(receipt)
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_competing_transactions_enroll_exactly_one_credential() {
    run_test(async {
        let mut first = connect().await;
        let mut second = connect().await;
        let fixture = Fixture::seed_receipted(&first).await;
        let mut other_credential = fixture.credential.clone();
        other_credential.cred_id_b64url.push_str("-competitor");
        let (left, right) = futures_util::future::join(
            complete_registration_on(
                &mut first,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                Some(&fixture.invite_audit),
                &fixture.credential,
                NOW,
            ),
            complete_registration_on(
                &mut second,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                Some(&fixture.invite_audit),
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
    let (challenge, magic, invite, invite_audit, credential) = (
        fixture.challenge.clone(),
        fixture.magic.clone(),
        fixture.invite.clone(),
        fixture.invite_audit.clone(),
        fixture.credential.clone(),
    );
    let enrollment = tokio::spawn(async move {
        let result = complete_registration_on(
            &mut enrollment_client,
            &challenge,
            &magic,
            Some(&invite),
            Some(&invite_audit),
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
    let mut enrollment = blocked
        .enrollment
        .take()
        .expect("enrollment task must be joined exactly once");
    let joined = tokio::time::timeout(Duration::from_secs(10), &mut enrollment).await;
    let (_enrollment_client, result) = match joined {
        Ok(joined) => joined.expect("enrollment task must not panic"),
        Err(_) => {
            enrollment.abort();
            let _ = enrollment.await;
            panic!("enrollment must finish once the invite lock is released");
        }
    };
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

// Removing the SQL cap (or draining several batches inside one helper call)
// must leave no overflow row and fail here. A post-buffer truncate can leave
// the overflow but still fails the independently observed candidate row count.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_bounded_batch_drains_cap_plus_one() {
    run_test(async {
        let mut cleanup = connect().await;
        // Other cases deliberately retain rows. Hold their parents on another
        // connection so only this case's new backlog can consume the batch.
        let mut other_rows = connect().await;
        let isolation = other_rows
            .transaction()
            .await
            .expect("isolate prior fixtures");
        isolation
            .query("SELECT id FROM users FOR UPDATE SKIP LOCKED", &[])
            .await
            .expect("protect prior disposable fixtures from this batch");

        let candidate_sql = crate::handlers::lock_unverified_users_sql();
        let words: Vec<_> = candidate_sql.split_whitespace().collect();
        // The ceiling is independent of the chosen production batch size;
        // changing a safe cap within 1..=1000 does not require changing this
        // fixture. With no SQL cap, still run the real helper against 1001
        // rows so baseline RED reports its unbounded cleanup behavior.
        let cap = words
            .windows(2)
            .rev()
            .find(|pair| pair[0] == "LIMIT")
            .and_then(|pair| pair[1].parse::<usize>().ok())
            .unwrap_or(1000);
        assert!(
            (1..=1000).contains(&cap),
            "candidate SQL must cap each reaper call at a finite batch of 1..=1000 users"
        );
        let prefix = format!("reap-batch-{:016x}", rand::random::<u64>());
        let ids: Vec<String> = (0..=cap).map(|index| format!("{prefix}-{index}")).collect();
        cleanup
            .execute(
                "INSERT INTO users (id, email, created_at) SELECT id, id || '@example.com', $2 FROM unnest($1::TEXT[]) AS seed(id)",
                &[&ids, &((NOW - 100_000) as i64)],
            )
            .await
            .expect("seed exactly cap plus one eligible disposable signups");

        let cutoff = (REAP_NOW - REAP_TTL) as i64;
        let now = REAP_NOW as i64;
        let selection = cleanup
            .transaction()
            .await
            .expect("observe candidate query");
        let selected = selection
            .query(&candidate_sql, &[&cutoff, &None::<&str>, &now])
            .await
            .expect("execute the real candidate SQL before any client-side truncation")
            .len();
        selection
            .rollback()
            .await
            .expect("release observed candidate locks");

        tokio::time::timeout(
            CLEANUP_BOUND,
            reap_unverified_users_on(&mut cleanup, REAP_NOW, REAP_TTL, None),
        )
        .await
        .expect("one bounded reaper batch must return promptly")
        .expect("run the production reaper on the cap-plus-one backlog");
        let remaining = cleanup
            .query_one("SELECT count(*) FROM users WHERE id = ANY($1)", &[&ids])
            .await
            .expect("observe overflow after one production reaper call")
            .get::<_, i64>(0);
        assert_eq!(
            remaining, 1,
            "one reaper call must leave exactly the cap-plus-one overflow row"
        );
        assert_eq!(
            selected, cap,
            "candidate selection must enforce the batch cap in SQL before ids are buffered"
        );
        tokio::time::timeout(
            CLEANUP_BOUND,
            reap_unverified_users_on(&mut cleanup, REAP_NOW, REAP_TTL, None),
        )
        .await
        .expect("the deferred reaper batch must return promptly")
        .expect("a later call drains the overflow");
        assert_eq!(
            cleanup
                .query_one("SELECT count(*) FROM users WHERE id = ANY($1)", &[&ids])
                .await
                .expect("observe eventual cleanup")
                .get::<_, i64>(0),
            0,
            "a later reaper call must drain the overflow rather than strand it"
        );
        isolation
            .rollback()
            .await
            .expect("release prior fixture locks");
    });
}

// Dropping/corrupting the Some(id) binding must reap this otherwise eligible
// signup and its expired dependents. The final None call proves that no
// credential, live proof, age boundary or fixture lock was sparing it.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_preserves_explicit_protected_user_and_dependents() {
    run_test(async {
        let mut cleanup = connect().await;
        let mut other_rows = connect().await;
        let isolation = other_rows
            .transaction()
            .await
            .expect("isolate prior fixtures");
        isolation
            .query("SELECT id FROM users FOR UPDATE SKIP LOCKED", &[])
            .await
            .expect("protect prior disposable fixtures from this case");
        let mut protected = Fixture::seed(&cleanup).await;
        age_signup(&cleanup, &mut protected).await;
        let mut control = Fixture::seed(&cleanup).await;
        age_signup(&cleanup, &mut control).await;
        let protected_id = protected.credential.user_id.as_str();

        for _ in 0..2 {
            tokio::time::timeout(
                CLEANUP_BOUND,
                reap_unverified_users_on(&mut cleanup, REAP_NOW, REAP_TTL, Some(protected_id)),
            )
            .await
            .expect("protected-user cleanup must return promptly")
            .expect("run the real reaper with Some(protected_id)");
            assert_eq!(
                user_count(&cleanup, protected_id).await,
                1,
                "protected signup must survive the production reaper"
            );
            assert_eq!(
                challenge_count(&cleanup, &protected.challenge.id).await,
                1,
                "the protected signup's expired challenge must survive"
            );
            assert_eq!(
                count(
                    &cleanup,
                    "SELECT count(*) FROM magic_link_tokens WHERE token_hash = $1",
                    &protected.magic.token_hash
                )
                .await,
                1,
                "the protected signup's expired emailed proof must survive"
            );
            protected
                .assert_state(&cleanup, (false, false, false, 0))
                .await;
            assert_eq!(
                user_count(&cleanup, &control.credential.user_id).await,
                0,
                "an unprotected eligible signup must still be reaped"
            );
            assert_eq!(challenge_count(&cleanup, &control.challenge.id).await, 0);
            assert_eq!(
                count(
                    &cleanup,
                    "SELECT count(*) FROM magic_link_tokens WHERE token_hash = $1",
                    &control.magic.token_hash
                )
                .await,
                0,
                "reaping the unprotected control must cascade to its emailed proof"
            );
        }

        reap_unverified_users_on(&mut cleanup, REAP_NOW, REAP_TTL, None)
            .await
            .expect("remove only the explicit protection on a later call");
        assert_eq!(
            user_count(&cleanup, protected_id).await,
            0,
            "the protected signup must otherwise be reap-eligible"
        );
        assert_eq!(challenge_count(&cleanup, &protected.challenge.id).await, 0);
        assert_eq!(
            count(
                &cleanup,
                "SELECT count(*) FROM magic_link_tokens WHERE token_hash = $1",
                &protected.magic.token_hash
            )
            .await,
            0
        );
        isolation
            .rollback()
            .await
            .expect("release prior fixture locks");
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

// Removing SKIP LOCKED from the real PgStore issuance cleanup must hit the
// named contention assertion, not merely change a SQL-source pin. The old
// proof expires after enrollment claims it but before replacement issuance.
#[cfg(feature = "postgres-tests")]
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_magic_issuance_skips_locked_expired_proof_and_retries() {
    use crate::handlers::Store;

    run_test(async {
        let mut blocked = blocked_enrollment().await;
        let mut expired = blocked.fixture.magic.clone();
        expired.token_hash.push_str("-unlocked-expired");
        expired.expires_at = REAP_NOW;
        let mut valid = blocked.fixture.magic.clone();
        valid.token_hash.push_str("-valid-at-issuance");
        valid.expires_at = REAP_NOW + 1;
        for record in [&expired, &valid] {
            blocked
                .cleanup
                .execute(
                    "INSERT INTO magic_link_tokens (token_hash, user_id, email, issued_at, expires_at, used) VALUES ($1, $2, $3, $4, $5, FALSE)",
                    &[&record.token_hash, &record.user_id, &record.email, &(record.issued_at as i64), &(record.expires_at as i64)],
                )
                .await
                .expect("seed same-user expiry-boundary controls");
        }
        let mut replacement = blocked.fixture.magic.clone();
        replacement.token_hash.push_str("-replacement");
        replacement.issued_at = REAP_NOW;
        replacement.expires_at = REAP_NOW + 300;
        let observed_ids = vec![
            blocked.fixture.magic.token_hash.clone(),
            replacement.token_hash.clone(),
            expired.token_hash.clone(),
            valid.token_hash.clone(),
            blocked.unrelated.magic.token_hash.clone(),
        ];
        // This seam replaces connection acquisition only. SQL, binds,
        // deadlines and execution are the actual production adapter's.
        let store = super::session_retry_store_for_postgres_test(connect().await);
        let issuance = tokio::time::timeout(CLEANUP_BOUND, store.insert_magic(&replacement)).await;
        let issued_promptly = matches!(issuance, Ok(Ok(())));
        // Observe committed state while enrollment still owns the old proof
        // and still waits on the locked invite. Save results, not assertions,
        // until both enrollment and the invite-locker have settled.
        let before_release = blocked
            .cleanup
            .query(
                "SELECT token_hash FROM magic_link_tokens WHERE token_hash = ANY($1)",
                &[&observed_ids],
            )
            .await;
        let released = blocked.locker.batch_execute("COMMIT").await;
        let enrollment = finish_enrollment(&mut blocked).await;
        released.expect("release invite after replacement issuance attempt");
        assert!(
            issued_promptly,
            "magic issuance must not wait for locked expired cleanup"
        );
        assert!(
            enrollment.is_ok(),
            "settle enrollment after releasing its invite"
        );
        let before_release: Vec<String> = before_release
            .expect("observe token state before releasing the invite")
            .iter()
            .map(|row| row.get(0))
            .collect();
        assert!(
            before_release.contains(&replacement.token_hash),
            "replacement must be committed before the invite is released"
        );
        assert!(
            before_release.contains(&blocked.fixture.magic.token_hash),
            "the enrollment-locked expired proof must survive issuance"
        );
        assert!(
            !before_release.contains(&expired.token_hash),
            "unlocked same-user proof expiring exactly at issued_at must be removed"
        );
        assert!(
            before_release.contains(&valid.token_hash),
            "same-user proof valid at issued_at must survive, regardless of wall clock"
        );
        assert!(
            before_release.contains(&blocked.unrelated.magic.token_hash),
            "expired proof for another user must survive issuance"
        );
        blocked.fixture.assert_credential(&blocked.cleanup).await;
        assert_eq!(
            count(
                &blocked.cleanup,
                "SELECT count(*) FROM magic_link_tokens WHERE token_hash = $1",
                &blocked.fixture.magic.token_hash,
            )
            .await,
            1,
            "settling enrollment alone must not clean the deferred expired proof"
        );

        let mut retry = replacement.clone();
        retry.token_hash.push_str("-retry");
        let store = super::session_retry_store_for_postgres_test(connect().await);
        tokio::time::timeout(CLEANUP_BOUND, store.insert_magic(&retry))
            .await
            .expect("unlocked issuance cleanup retry must return promptly")
            .expect("retry the actual issuance adapter after enrollment commits");
        let after_retry: Vec<String> = blocked
            .cleanup
            .query(
                "SELECT token_hash FROM magic_link_tokens WHERE token_hash = ANY($1)",
                &[&observed_ids],
            )
            .await
            .expect("observe deferred cleanup retry")
            .iter()
            .map(|row| row.get(0))
            .collect();
        assert!(
            !after_retry.contains(&blocked.fixture.magic.token_hash),
            "later issuance must remove the now-unlocked expired old proof"
        );
        assert!(!after_retry.contains(&expired.token_hash));
        for retained in [
            &replacement.token_hash,
            &valid.token_hash,
            &blocked.unrelated.magic.token_hash,
        ] {
            assert!(
                after_retry.contains(retained),
                "valid and other-user controls survive retry"
            );
        }
        assert_eq!(
            count(
                &blocked.cleanup,
                "SELECT count(*) FROM magic_link_tokens WHERE token_hash = $1",
                &retry.token_hash,
            )
            .await,
            1,
            "retry issuance commits its new token"
        );
    });
}

// Committing after selection and using a fresh transaction for deletion
// preserves the copied-SQL recheck case above, but must fail this real-helper
// test: its selected parent must remain locked while the reaper is paused.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_holds_selected_parent_lock_through_delete() {
    run_test(async {
        let mut reaper = connect().await;
        let mut other_rows = connect().await;
        let isolation = other_rows
            .transaction()
            .await
            .expect("isolate prior fixtures from selection");
        isolation
            .query("SELECT id FROM users FOR UPDATE SKIP LOCKED", &[])
            .await
            .expect("hold prior fixture parents outside the tested reaper");
        let mut fixture = Fixture::seed(&reaper).await;
        age_signup(&reaper, &mut fixture).await;
        let observer = connect().await;
        observer
            .batch_execute("BEGIN")
            .await
            .expect("open independent lock observer");
        let (selected_tx, selected_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let resumed = std::rc::Rc::new(std::cell::Cell::new(false));
        let hook_resumed = resumed.clone();
        super::REAPER_SELECTION_PAUSE.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "the one-shot reaper pause must start empty"
            );
            *slot.borrow_mut() = Some(Box::pin(async move {
                let _ = selected_tx.send(());
                hook_resumed.set(matches!(
                    tokio::time::timeout(CLEANUP_BOUND, resume_rx).await,
                    Ok(Ok(()))
                ));
            }));
        });
        let observe_then_resume = async {
            let selected = tokio::time::timeout(CLEANUP_BOUND, selected_rx).await;
            let lock = observer
                .query_one(
                    "SELECT id FROM users WHERE id = $1 FOR KEY SHARE NOWAIT",
                    &[&fixture.credential.user_id],
                )
                .await;
            // NOWAIT errors abort this observer transaction; success means the
            // mutant acquired a lock. Roll back either way BEFORE the reaper
            // resumes, then join both futures before asserting the outcome.
            let released = observer.batch_execute("ROLLBACK").await;
            let _ = resume_tx.send(());
            (selected, lock, released)
        };
        let (reaped, (selected, lock, released)) = futures_util::future::join(
            tokio::time::timeout(
                TEST_TIMEOUT,
                reap_unverified_users_on(&mut reaper, REAP_NOW, REAP_TTL, None),
            ),
            observe_then_resume,
        )
        .await;
        super::REAPER_SELECTION_PAUSE.with(|slot| {
            slot.borrow_mut().take();
        });
        isolation
            .rollback()
            .await
            .expect("release prior fixture locks");
        released.expect("release every observer lock before resuming deletion");
        assert!(
            matches!(selected, Ok(Ok(()))),
            "real reaper must reach the selection pause"
        );
        assert!(
            resumed.get(),
            "reaper must resume from the observer, not a pause timeout"
        );
        assert!(
            lock.as_ref().is_err_and(|error| {
                error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)
            }),
            "reaper must retain selected parent lock through delete"
        );
        reaped
            .expect("real paused reaper must settle within the test bound")
            .expect("real reaper must commit after the observer releases its locks");
        assert_eq!(user_count(&observer, &fixture.credential.user_id).await, 0);
        assert_eq!(challenge_count(&observer, &fixture.challenge.id).await, 0);
        assert_eq!(
            count(
                &observer,
                "SELECT count(*) FROM magic_link_tokens WHERE token_hash = $1",
                &fixture.magic.token_hash,
            )
            .await,
            0,
            "the real reaper must finish deletion and cascading cleanup after resume"
        );
    });
}

// ---------------------------------------------------------------------------
// Admin foundation (migration 0028): the production role/settings write
// transactions, including their behaviour behind a contending lock holder.
// The SQL-level shapes are also pinned by tests/sql/admin_foundation.sql;
// these tests prove the Rust transaction orchestration (locked read, write
// and extended audit committing together) and that the recorded old value
// is the value actually replaced, never a stale snapshot read.

async fn seed_admin_test_users(client: &Client) -> (String, String) {
    let prefix = format!("admin-foundation-{:016x}", rand::random::<u64>());
    let actor_id = format!("{prefix}-actor");
    let subject_id = format!("{prefix}-subject");
    for (id, email, role) in [
        (
            actor_id.as_str(),
            format!("{prefix}-actor@example.com"),
            "admin",
        ),
        (
            subject_id.as_str(),
            format!("{prefix}-subject@example.com"),
            "member",
        ),
    ] {
        // The actor seeds as an admin: the write transactions now
        // re-read the actor's role under their locks, and these tests
        // exercise authorized writes except where they demote the
        // actor deliberately.
        client
            .execute(
                "INSERT INTO users (id, email, created_at, role) VALUES ($1, $2, $3, $4)",
                &[&id, &email, &(NOW as i64), &role],
            )
            .await
            .expect("seed admin acceptance user");
    }
    (actor_id, subject_id)
}

async fn role_of(client: &Client, user_id: &str) -> String {
    client
        .query_one("SELECT role FROM users WHERE id = $1", &[&user_id])
        .await
        .expect("read seeded user role")
        .get(0)
}

/// (actor, old, new) for every role-change audit row on the subject, in
/// commit order (`audit_log.id` is a sequence assigned at insert).
async fn role_audit_chain(client: &Client, subject_id: &str) -> Vec<(String, String, String)> {
    client
        .query(
            "SELECT user_id, details ->> 'old', details ->> 'new' FROM audit_log \
             WHERE action = 'user.role_changed' AND subject_user_id = $1 ORDER BY id",
            &[&subject_id],
        )
        .await
        .expect("read role audit chain")
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_role_change_audits_each_replaced_value() {
    run_test(async {
        let mut client = connect().await;
        let (actor, subject) = seed_admin_test_users(&client).await;

        let previous =
            admin_set_user_role_on(&mut client, &actor, &subject, "admin", "198.51.100.9", NOW)
                .await
                .expect("promote subject");
        assert_eq!(
            previous,
            AdminWriteOutcome::Applied(Some("member".to_string()))
        );
        assert_eq!(role_of(&client, &subject).await, "admin");

        let previous =
            admin_set_user_role_on(&mut client, &actor, &subject, "member", "198.51.100.9", NOW)
                .await
                .expect("demote subject");
        assert_eq!(
            previous,
            AdminWriteOutcome::Applied(Some("admin".to_string()))
        );
        assert_eq!(role_of(&client, &subject).await, "member");

        assert_eq!(
            role_audit_chain(&client, &subject).await,
            vec![
                (actor.clone(), "member".to_string(), "admin".to_string()),
                (actor.clone(), "admin".to_string(), "member".to_string()),
            ],
            "audit chain must carry the actor and each replaced value"
        );

        // A missing subject writes and audits nothing, and is not an error.
        let missing = format!("{subject}-missing");
        let previous =
            admin_set_user_role_on(&mut client, &actor, &missing, "admin", "198.51.100.9", NOW)
                .await
                .expect("missing subject is not a store error");
        assert_eq!(previous, AdminWriteOutcome::SubjectMissing);
        assert!(role_audit_chain(&client, &missing).await.is_empty());
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_reap_preserves_admin_stub_accounts() {
    run_test(async {
        let mut cleanup = connect().await;
        let mut other_rows = connect().await;
        let isolation = other_rows
            .transaction()
            .await
            .expect("isolate prior fixtures");
        isolation
            .query("SELECT id FROM users FOR UPDATE SKIP LOCKED", &[])
            .await
            .expect("protect prior disposable fixtures from this case");

        // Two aged stubs with no credential, session, vault or live
        // proof: identical but for the role. Only the member is
        // unverified-signup garbage; the admin (e.g. an operator
        // bootstrap promotion that never completed enrollment) must
        // survive the production reaper.
        let prefix = format!("reap-role-{:016x}", rand::random::<u64>());
        let member_id = format!("{prefix}-member");
        let admin_id = format!("{prefix}-admin");
        for (id, role) in [(&member_id, "member"), (&admin_id, "admin")] {
            cleanup
                .execute(
                    "INSERT INTO users (id, email, created_at, role) VALUES ($1, $2, $3, $4)",
                    &[
                        id,
                        &format!("{id}@example.com"),
                        &((NOW - 2 * REAP_TTL) as i64),
                        &role,
                    ],
                )
                .await
                .expect("seed aged stub user");
        }

        reap_unverified_users_on(&mut cleanup, REAP_NOW, REAP_TTL, None)
            .await
            .expect("run the real reaper");
        assert_eq!(
            user_count(&cleanup, &member_id).await,
            0,
            "the aged member stub must be reaped"
        );
        assert_eq!(
            user_count(&cleanup, &admin_id).await,
            1,
            "the aged admin stub must survive the reaper"
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_role_change_on_a_deleting_subject_writes_nothing() {
    run_test(async {
        let mut client = connect().await;
        let (actor, subject) = seed_admin_test_users(&client).await;
        client
            .execute(
                "UPDATE users SET deleting_at = now() WHERE id = $1",
                &[&subject],
            )
            .await
            .expect("schedule the subject for deletion");

        // ADR 0002 rule 3 fences the write once `deleting_at` is set:
        // the distinct outcome comes back, the role is untouched and
        // no audit row claims a change that never happened.
        let outcome =
            admin_set_user_role_on(&mut client, &actor, &subject, "admin", "198.51.100.9", NOW)
                .await
                .expect("a deleting subject is not a store error");
        assert_eq!(outcome, AdminWriteOutcome::AccountDeleting);
        assert_eq!(role_of(&client, &subject).await, "member");
        assert!(role_audit_chain(&client, &subject).await.is_empty());
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_role_change_rolls_back_when_the_audit_insert_fails() {
    run_test(async {
        let mut client = connect().await;
        let (actor, subject) = seed_admin_test_users(&client).await;

        // Force every audit insert naming this test's subject to fail.
        // The names and the WHEN clause are scoped to the test's random
        // subject id, so a concurrent test's audit rows are untouched.
        let tag = format!("{:016x}", rand::random::<u64>());
        client
            .batch_execute(&format!(
                "CREATE FUNCTION reject_audit_{tag}() RETURNS trigger \
                 LANGUAGE plpgsql AS $$ BEGIN \
                 RAISE EXCEPTION 'forced audit failure'; END $$; \
                 CREATE TRIGGER reject_audit_{tag} BEFORE INSERT ON audit_log \
                 FOR EACH ROW WHEN (NEW.subject_user_id = '{subject}') \
                 EXECUTE FUNCTION reject_audit_{tag}()"
            ))
            .await
            .expect("install the audit-rejecting trigger");

        // The role UPDATE and its audit row commit together or not at
        // all: the failed insert aborts the transaction, so the write
        // errors and the stored role is the pre-write value.
        let outcome =
            admin_set_user_role_on(&mut client, &actor, &subject, "admin", "198.51.100.9", NOW)
                .await;
        assert!(
            outcome.is_err(),
            "a failed audit insert must fail the whole write"
        );
        assert_eq!(role_of(&client, &subject).await, "member");
        assert!(role_audit_chain(&client, &subject).await.is_empty());

        client
            .batch_execute(&format!(
                "DROP TRIGGER reject_audit_{tag} ON audit_log; \
                 DROP FUNCTION reject_audit_{tag}()"
            ))
            .await
            .expect("remove the audit-rejecting trigger");
        let outcome =
            admin_set_user_role_on(&mut client, &actor, &subject, "admin", "198.51.100.9", NOW)
                .await
                .expect("the same write succeeds once the trigger is gone");
        assert_eq!(
            outcome,
            AdminWriteOutcome::Applied(Some("member".to_string()))
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_role_change_waits_on_row_lock_and_audits_committed_value() {
    run_test(async {
        let holder = connect().await;
        let (actor, subject) = seed_admin_test_users(&holder).await;

        // The write locks in the fixed production order (actor row,
        // then subject row), so the holder takes the actor row first
        // and both rows wait behind one uncommitted promotion of the
        // subject. A snapshot read would keep seeing the subject as
        // 'member'; the production FOR UPDATE reads must wait for the
        // commit and then see 'admin' — and had the holder demoted the
        // actor instead, the re-check would refuse the write (pinned
        // by the demotion regressions below).
        holder.batch_execute("BEGIN").await.expect("begin holder");
        holder
            .execute("SELECT id FROM users WHERE id = $1 FOR UPDATE", &[&actor])
            .await
            .expect("holder locks the actor row");
        holder
            .execute("UPDATE users SET role = 'admin' WHERE id = $1", &[&subject])
            .await
            .expect("holder takes the row lock with its update");

        let (actor_task, subject_task) = (actor.clone(), subject.clone());
        let contender = tokio::spawn(async move {
            let mut client = connect().await;
            admin_set_user_role_on(
                &mut client,
                &actor_task,
                &subject_task,
                "member",
                "198.51.100.9",
                NOW,
            )
            .await
        });

        // While the holder's transaction is open the contender cannot
        // finish (its lock wait is bounded at 3s by connect(), and the
        // holder commits long before that), so this cannot be a flake.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !contender.is_finished(),
            "role change must block behind the holder's row lock"
        );

        holder.batch_execute("COMMIT").await.expect("commit holder");
        let previous = contender
            .await
            .expect("contender task")
            .expect("contender write after lock release");
        assert_eq!(
            previous,
            AdminWriteOutcome::Applied(Some("admin".to_string())),
            "the returned/audited old value must be the committed value replaced"
        );
        assert_eq!(role_of(&holder, &subject).await, "member");
        assert_eq!(
            role_audit_chain(&holder, &subject).await,
            vec![(actor.clone(), "admin".to_string(), "member".to_string())]
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_setting_writes_serialize_per_key() {
    run_test(async {
        let mut client = connect().await;
        let (actor, _) = seed_admin_test_users(&client).await;
        const KEY: &str = "invite_default_expiry_days";

        // The disposable database is migrated fresh for the job, so the
        // row holds the migration seed. Restore both touched seeds at the
        // end so a local rerun starts from the same state.
        let seeded: String = client
            .query_one("SELECT value FROM app_settings WHERE key = $1", &[&KEY])
            .await
            .expect("seeded setting present")
            .get(0);
        assert_eq!(seeded, "7");

        let previous =
            admin_set_app_setting_on(&mut client, &actor, KEY, "14", "198.51.100.9", NOW)
                .await
                .expect("first setting write");
        assert_eq!(previous, AdminWriteOutcome::Applied(Some("7".to_string())));
        let previous =
            admin_set_app_setting_on(&mut client, &actor, KEY, "30", "198.51.100.9", NOW)
                .await
                .expect("second setting write");
        assert_eq!(previous, AdminWriteOutcome::Applied(Some("14".to_string())));

        // Hold the per-key advisory lock behind an uncommitted rewrite to
        // 9; the contender must wait and then replace 9, not the stale 30
        // a snapshot read would see.
        let holder = connect().await;
        holder.batch_execute("BEGIN").await.expect("begin holder");
        holder
            .execute(
                "SELECT pg_advisory_xact_lock(hashtext('app_settings:' || $1))",
                &[&KEY],
            )
            .await
            .expect("holder takes the advisory lock");
        holder
            .execute(
                "UPDATE app_settings SET value = '9' WHERE key = $1",
                &[&KEY],
            )
            .await
            .expect("holder rewrites the value");

        let actor_task = actor.clone();
        let contender = tokio::spawn(async move {
            let mut client = connect().await;
            admin_set_app_setting_on(&mut client, &actor_task, KEY, "21", "198.51.100.9", NOW).await
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !contender.is_finished(),
            "setting write must block behind the per-key advisory lock"
        );
        holder.batch_execute("COMMIT").await.expect("commit holder");
        let previous = contender
            .await
            .expect("contender task")
            .expect("contender write after lock release");
        assert_eq!(previous, AdminWriteOutcome::Applied(Some("9".to_string())));

        let chain: Vec<(Option<String>, String)> = client
            .query(
                "SELECT details ->> 'old', details ->> 'new' FROM audit_log \
                 WHERE action = 'settings.changed' AND details ->> 'key' = $1 \
                   AND user_id = $2 ORDER BY id",
                &[&KEY, &actor],
            )
            .await
            .expect("read settings audit chain")
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        assert_eq!(
            chain,
            vec![
                (Some("7".to_string()), "14".to_string()),
                (Some("14".to_string()), "30".to_string()),
                (Some("9".to_string()), "21".to_string()),
            ],
            "audit chain must record each value actually replaced"
        );

        // First write of an absent key reports no previous value; the
        // audit row records JSON null for old.
        client
            .execute(
                "DELETE FROM app_settings WHERE key = 'invite_member_per_window'",
                &[],
            )
            .await
            .expect("clear reserved key");
        let previous = admin_set_app_setting_on(
            &mut client,
            &actor,
            "invite_member_per_window",
            "5",
            "198.51.100.9",
            NOW,
        )
        .await
        .expect("first write of absent key");
        assert_eq!(previous, AdminWriteOutcome::Applied(None));
        let old_is_json_null: bool = client
            .query_one(
                "SELECT details -> 'old' = 'null'::jsonb FROM audit_log \
                 WHERE action = 'settings.changed' \
                   AND details ->> 'key' = 'invite_member_per_window' AND user_id = $1",
                &[&actor],
            )
            .await
            .expect("read first-write audit row")
            .get(0);
        assert!(
            old_is_json_null,
            "absent previous value audits as JSON null"
        );

        admin_set_app_setting_on(&mut client, &actor, KEY, "7", "198.51.100.9", NOW)
            .await
            .expect("restore seeded expiry");
        admin_set_app_setting_on(
            &mut client,
            &actor,
            "invite_member_per_window",
            "2",
            "198.51.100.9",
            NOW,
        )
        .await
        .expect("restore seeded per-window value");
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_write_refused_after_actor_demotion() {
    run_test(async {
        let mut client = connect().await;
        let (actor, subject) = seed_admin_test_users(&client).await;
        const KEY: &str = "invite_member_min_paid_months";

        // The guard authorized this actor when they were an admin, but
        // by the time the write transaction runs another admin has
        // demoted them. The in-transaction re-check must refuse BOTH
        // write kinds: no role change, no setting change, no audit row.
        client
            .execute("UPDATE users SET role = 'member' WHERE id = $1", &[&actor])
            .await
            .expect("demote the actor mid-flight");

        let role_outcome =
            admin_set_user_role_on(&mut client, &actor, &subject, "admin", "198.51.100.9", NOW)
                .await
                .expect("role write by demoted actor is a typed refusal, not an error");
        assert_eq!(role_outcome, AdminWriteOutcome::ActorNotAdmin);
        assert_eq!(role_of(&client, &subject).await, "member");
        assert!(
            role_audit_chain(&client, &subject).await.is_empty(),
            "a refused role write audits nothing"
        );

        let setting_outcome =
            admin_set_app_setting_on(&mut client, &actor, KEY, "99", "198.51.100.9", NOW)
                .await
                .expect("settings write by demoted actor is a typed refusal, not an error");
        assert_eq!(setting_outcome, AdminWriteOutcome::ActorNotAdmin);
        let stored: String = client
            .query_one("SELECT value FROM app_settings WHERE key = $1", &[&KEY])
            .await
            .expect("seeded setting still present")
            .get(0);
        assert_eq!(stored, "3", "the refusal replaced nothing");
        let audits: i64 = client
            .query_one(
                "SELECT count(*) FROM audit_log \
                 WHERE action = 'settings.changed' AND user_id = $1",
                &[&actor],
            )
            .await
            .expect("count refused-actor settings audits")
            .get(0);
        assert_eq!(audits, 0, "a refused settings write audits nothing");

        // A never-admin actor is the same refusal: the re-check does
        // not trust the caller to have been authorized at all.
        let stranger =
            admin_set_user_role_on(&mut client, &subject, &actor, "member", "198.51.100.9", NOW)
                .await
                .expect("role write by a member actor is a typed refusal");
        assert_eq!(stranger, AdminWriteOutcome::ActorNotAdmin);
        assert_eq!(role_of(&client, &actor).await, "member");
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_simultaneous_opposing_demotions_keep_one_admin() {
    run_test(async {
        let client = connect().await;
        let (admin_a, admin_b) = seed_admin_test_users(&client).await;
        client
            .execute("UPDATE users SET role = 'admin' WHERE id = $1", &[&admin_b])
            .await
            .expect("promote the second admin");

        // Both admins pass the handler guard concurrently, then each
        // tries to demote the other. The shared serialization lock
        // runs the two transactions in some order: the first commits,
        // the second re-reads its own role as 'member' and applies
        // nothing — so exactly one admin survives instead of zero,
        // and the fixed lock order keeps the pair deadlock-free.
        let (a1, b1) = (admin_a.clone(), admin_b.clone());
        let demote_b = tokio::spawn(async move {
            let mut client = connect().await;
            admin_set_user_role_on(&mut client, &a1, &b1, "member", "198.51.100.9", NOW).await
        });
        let (a2, b2) = (admin_a.clone(), admin_b.clone());
        let demote_a = tokio::spawn(async move {
            let mut client = connect().await;
            admin_set_user_role_on(&mut client, &b2, &a2, "member", "198.51.100.9", NOW).await
        });
        let outcomes = [
            demote_b
                .await
                .expect("demote-b task")
                .expect("demote-b write"),
            demote_a
                .await
                .expect("demote-a task")
                .expect("demote-a write"),
        ];
        let applied = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, AdminWriteOutcome::Applied(_)))
            .count();
        let refused = outcomes
            .iter()
            .filter(|outcome| **outcome == AdminWriteOutcome::ActorNotAdmin)
            .count();
        assert_eq!(
            (applied, refused),
            (1, 1),
            "exactly one demotion applies and one is refused: {outcomes:?}"
        );

        let roles = [
            role_of(&client, &admin_a).await,
            role_of(&client, &admin_b).await,
        ];
        assert_eq!(
            roles.iter().filter(|role| role.as_str() == "admin").count(),
            1,
            "opposing demotions must never reach zero admins: {roles:?}"
        );

        // Exactly one audit row landed across both subjects.
        let audits = role_audit_chain(&client, &admin_a).await.len()
            + role_audit_chain(&client, &admin_b).await.len();
        assert_eq!(audits, 1, "only the applied demotion audits");
    });
}

// The production issue/revoke transactions (migration 0033): each
// commits with exactly one audit row that references the invite by
// surrogate row id — never by code plaintext or hash — and a demoted
// actor's re-check refuses both writes.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_invite_issue_and_revoke_audit_by_row_id() {
    run_test(async {
        let mut client = connect().await;
        let (actor, subject) = seed_admin_test_users(&client).await;
        let code_hash = format!("invite-bank-{:016x}", rand::random::<u64>());
        let record = InviteRecord::new_issued(
            code_hash.clone(),
            Some(actor.clone()),
            crate::auth::INVITE_KIND_ADMIN,
            Some("invitee@example.com".to_string()),
            NOW,
            NOW + 604_800,
        );

        // A member actor is refused by the transaction's role re-check
        // and writes nothing.
        let refused = admin_issue_invite_on(&mut client, &subject, &record, "198.51.100.9", NOW)
            .await
            .expect("member issue attempt is a typed refusal");
        assert_eq!(refused, AdminInviteIssueOutcome::ActorNotAdmin);

        let invite_id =
            match admin_issue_invite_on(&mut client, &actor, &record, "198.51.100.9", NOW)
                .await
                .expect("admin issue commits")
            {
                AdminInviteIssueOutcome::Issued(id) => id,
                other => panic!("expected Issued, got {other:?}"),
            };
        let row = client
            .query_one(
                "SELECT issued_by, kind, email, expires_at, used FROM invite_codes WHERE id = $1",
                &[&invite_id],
            )
            .await
            .expect("read issued invite");
        assert_eq!(row.get::<_, String>(0), actor);
        assert_eq!(row.get::<_, String>(1), "admin");
        assert_eq!(row.get::<_, String>(2), "invitee@example.com");
        assert_eq!(row.get::<_, i64>(3), (NOW + 604_800) as i64);
        assert!(!row.get::<_, bool>(4));

        let audits = client
            .query(
                "SELECT action, user_id, details::text FROM audit_log \
                 WHERE action IN ('invite.issued', 'invite.revoked') \
                   AND details ->> 'invite_id' = $1 ORDER BY id",
                &[&invite_id.to_string()],
            )
            .await
            .expect("read invite audit rows");
        assert_eq!(audits.len(), 1, "exactly the issue audit so far");
        assert_eq!(audits[0].get::<_, String>(0), "invite.issued");
        assert_eq!(audits[0].get::<_, String>(1), actor);
        assert!(
            !audits[0].get::<_, String>(2).contains(&code_hash),
            "the issue audit must reference the row id, not the hash"
        );

        let revoked = admin_revoke_invite_on(&mut client, &actor, invite_id, "198.51.100.9", NOW)
            .await
            .expect("admin revoke commits");
        assert_eq!(revoked, AdminInviteRevokeOutcome::Revoked);
        let again = admin_revoke_invite_on(&mut client, &actor, invite_id, "198.51.100.9", NOW)
            .await
            .expect("re-revoke is a typed idempotent outcome");
        assert_eq!(again, AdminInviteRevokeOutcome::AlreadyRevoked);
        let audits = client
            .query(
                "SELECT action FROM audit_log \
                 WHERE action IN ('invite.issued', 'invite.revoked') \
                   AND details ->> 'invite_id' = $1 ORDER BY id",
                &[&invite_id.to_string()],
            )
            .await
            .expect("read invite audit rows after revoke");
        assert_eq!(
            audits
                .iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>(),
            vec!["invite.issued".to_string(), "invite.revoked".to_string()],
            "issue and the first revoke audit once each"
        );
        let row = client
            .query_one(
                "SELECT revoked_at, revoked_by FROM invite_codes WHERE id = $1",
                &[&invite_id],
            )
            .await
            .expect("read revoked invite");
        assert_eq!(row.get::<_, i64>(0), NOW as i64);
        assert_eq!(row.get::<_, String>(1), actor);
    });
}

// Revocation that commits after the signup snapshot was taken wins:
// the enrollment's conditional claim matches zero rows and the whole
// transaction rolls back, spending neither earlier proof.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_invite_revocation_wins_against_stale_redemption_snapshot() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        client
            .execute(
                "UPDATE invite_codes SET revoked_at = $1 WHERE code_hash = $2",
                &[&(NOW as i64), &fixture.invite.code_hash],
            )
            .await
            .expect("a revocation commits after the snapshot");
        assert!(
            complete_registration_on(
                &mut client,
                &fixture.challenge,
                &fixture.magic,
                Some(&fixture.invite),
                Some(&fixture.invite_audit),
                &fixture.credential,
                NOW,
            )
            .await
            .is_err(),
            "a revoked invite must lose at claim time, not at snapshot time only"
        );
        fixture
            .assert_state(&client, (false, false, false, 0))
            .await;
        let redeemed = client
            .query_one(
                "SELECT count(*) FROM audit_log WHERE action = 'invite.redeemed' AND subject_user_id = $1",
                &[&fixture.credential.user_id],
            )
            .await
            .expect("count redemption audits")
            .get::<_, i64>(0);
        assert_eq!(redeemed, 0, "the rolled-back enrollment audits nothing");
    });
}

// The winning enrollment stamps the spend (redeemed_by/at) and the
// invite tree (users.invited_by) and writes the `invite.redeemed`
// audit in the same commit; the audit carries no code material.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_invite_claim_stamps_spend_and_invited_by() {
    run_test(async {
        let mut client = connect().await;
        let (issuer, _subject) = seed_admin_test_users(&client).await;
        let mut fixture = Fixture::seed(&client).await;
        fixture.invite.issued_by = Some(issuer.clone());
        client
            .execute(
                "UPDATE invite_codes SET issued_by = $1 WHERE code_hash = $2",
                &[&issuer, &fixture.invite.code_hash],
            )
            .await
            .expect("attribute the seeded invite to its issuer");
        complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            Some(&fixture.invite),
            Some(&fixture.invite_audit),
            &fixture.credential,
            NOW,
        )
        .await
        .expect("registration with invite commits");
        fixture.assert_state(&client, (true, true, true, 1)).await;

        let row = client
            .query_one(
                "SELECT i.redeemed_by, i.redeemed_at, u.invited_by \
                 FROM invite_codes i, users u \
                 WHERE i.code_hash = $1 AND u.id = $2",
                &[&fixture.invite.code_hash, &fixture.credential.user_id],
            )
            .await
            .expect("read spend stamps and invite tree");
        assert_eq!(
            row.get::<_, String>(0),
            fixture.credential.user_id,
            "redeemed_by is the new account"
        );
        assert_eq!(row.get::<_, i64>(1), NOW as i64);
        assert_eq!(
            row.get::<_, String>(2),
            issuer,
            "users.invited_by is the invite's issuer"
        );

        let audits = client
            .query(
                "SELECT details::text FROM audit_log \
                 WHERE action = 'invite.redeemed' AND subject_user_id = $1",
                &[&fixture.credential.user_id],
            )
            .await
            .expect("read redemption audit");
        assert_eq!(audits.len(), 1, "exactly one redemption audit commits");
        assert!(
            !audits[0]
                .get::<_, String>(0)
                .contains(&fixture.invite.code_hash),
            "the redemption audit must not carry the code hash"
        );
    });
}

// Regression (review round 1): deleting the issuer between the
// `invite_by_hash` snapshot and the enrollment commit must not break
// an otherwise valid invite. The issued_by FK nulls the stored row
// (ON DELETE SET NULL), so the stamp derives `invited_by` from the
// claimed row in-transaction and lands NULL; stamping the stale
// snapshot id instead violated the `users.invited_by` FK and rolled
// the enrollment back forever.
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_registration_invite_claim_survives_issuer_deletion() {
    run_test(async {
        let mut client = connect().await;
        let (issuer, _subject) = seed_admin_test_users(&client).await;
        let mut fixture = Fixture::seed(&client).await;
        fixture.invite.issued_by = Some(issuer.clone());
        client
            .execute(
                "UPDATE invite_codes SET issued_by = $1 WHERE code_hash = $2",
                &[&issuer, &fixture.invite.code_hash],
            )
            .await
            .expect("attribute the seeded invite to its issuer");
        // The issuer disappears after the snapshot was taken; the
        // fixture's invite record still carries the stale id.
        client
            .execute("DELETE FROM users WHERE id = $1", &[&issuer])
            .await
            .expect("delete the issuer between lookup and claim");
        complete_registration_on(
            &mut client,
            &fixture.challenge,
            &fixture.magic,
            Some(&fixture.invite),
            Some(&fixture.invite_audit),
            &fixture.credential,
            NOW,
        )
        .await
        .expect("registration with an orphaned invite still commits");
        fixture.assert_state(&client, (true, true, true, 1)).await;

        let row = client
            .query_one(
                "SELECT i.redeemed_by, i.issued_by, u.invited_by \
                 FROM invite_codes i, users u \
                 WHERE i.code_hash = $1 AND u.id = $2",
                &[&fixture.invite.code_hash, &fixture.credential.user_id],
            )
            .await
            .expect("read spend stamps after issuer deletion");
        assert_eq!(
            row.get::<_, String>(0),
            fixture.credential.user_id,
            "the orphaned invite is still spent by the new account"
        );
        assert_eq!(
            row.get::<_, Option<String>>(1),
            None,
            "the stored issuer was nulled by ON DELETE SET NULL"
        );
        assert_eq!(
            row.get::<_, Option<String>>(2),
            None,
            "users.invited_by derives from the claimed row, not the stale snapshot"
        );
    });
}

// --- Admin ingest execution coverage (migration 0034) -----------------------
//
// The route suite and the FakeStore prove behavior; the statement-contract
// test pins the query shapes. The tests below execute the real
// `admin_ingest_*` production transactions against migrated disposable
// PostgreSQL, so a wrong column, bind type, decoder width or transaction
// step fails here instead of in production.

// Synthetic admin-ingest envelope: obviously synthetic placeholder bytes
// (never NZB content, file names, subjects or message IDs), sized to
// satisfy the migration 0034 envelope CHECKs (12-byte nonces, 48-byte
// wrapped DEK).
fn synthetic_ingest_envelope() -> IngestOriginalEnvelope {
    IngestOriginalEnvelope {
        dek_wrapped_nonce: [0x9e; crate::admin_ingest::NONCE_LEN],
        dek_wrapped: vec![0x5a; 48],
        payload_nonce: [0x3c; crate::admin_ingest::NONCE_LEN],
        ciphertext: vec![0x77; 64],
    }
}

// One accepted-upload attempt for `actor`. `tag` namespaces the
// content-derived ids of a single test (charset-clean, well under the
// 64-character id bound); the content hash is fresh randomness in the
// required 64-hex shape.
fn accepted_ingest_attempt(actor: &str, tag: &str, now: u64) -> AdminIngestAttempt {
    let sha = format!("{:064x}", rand::random::<u128>());
    AdminIngestAttempt {
        actor_user_id: actor.to_string(),
        job_id: format!("adm_ai_{tag}_{:016x}", rand::random::<u64>()),
        idempotency_key: format!("admin-ingest:{sha}:upload"),
        content_sha256_hex: sha,
        nzb_bytes: 1_048_576,
        file_count: Some(12),
        segment_count: Some(480),
        failure: None,
        original: Some(synthetic_ingest_envelope()),
        kek_version: 1,
        source_ip: "198.51.100.9".to_string(),
        now,
    }
}

async fn ingest_row_counts(client: &Client, job_id: &str) -> (i64, i64) {
    let jobs: i64 = client
        .query_one("SELECT count(*) FROM ingest_jobs WHERE id = $1", &[&job_id])
        .await
        .expect("count job rows")
        .get(0);
    let originals: i64 = client
        .query_one(
            "SELECT count(*) FROM ingest_originals WHERE job_id = $1",
            &[&job_id],
        )
        .await
        .expect("count original rows")
        .get(0);
    (jobs, originals)
}

// Every upload audit for one job: (actor, outcome, reason) in commit
// order-independent form, each with a null subject and ok TRUE.
async fn ingest_audit_chain(
    client: &Client,
    job_id: &str,
) -> Vec<(String, String, Option<String>)> {
    let mut chain: Vec<(String, String, Option<String>)> = client
        .query(
            "SELECT user_id, details ->> 'outcome', details ->> 'reason_code', \
             subject_user_id, ok FROM audit_log \
             WHERE action = $1 AND details ->> 'job_id' = $2",
            &[&INGEST_UPLOAD_AUDIT_ACTION, &job_id],
        )
        .await
        .expect("read ingest audit chain")
        .iter()
        .map(|row| {
            assert_eq!(
                row.get::<_, Option<String>>(3),
                None,
                "ingest audits never name a subject user"
            );
            assert!(row.get::<_, bool>(4), "ingest audits record ok");
            (row.get(0), row.get(1), row.get(2))
        })
        .collect();
    chain.sort();
    chain
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_ingest_upload_accept_then_duplicate_converges() {
    run_test(async {
        let mut client = connect().await;
        let (admin, _) = seed_admin_test_users(&client).await;
        let (admin2, _) = seed_admin_test_users(&client).await;
        let attempt = accepted_ingest_attempt(&admin, "accept", NOW);
        let job_id = attempt.job_id.clone();

        // Fresh acceptance executes the whole transaction: job row,
        // original row and accepted audit, decoded back redacted.
        let outcome = admin_ingest_upload_on(&mut client, &attempt)
            .await
            .expect("accept upload executes");
        let AdminIngestUploadOutcome::Stored { job, deduplicated } = outcome else {
            panic!("fresh upload must store, got {outcome:?}");
        };
        assert!(!deduplicated, "first upload is not a duplicate");
        assert_eq!(job.job_id, job_id);
        assert_eq!(job.db_status, "queued");
        assert_eq!(job.accepted_at, NOW);
        assert_eq!(job.nzb_bytes, 1_048_576);
        assert_eq!(job.file_count, Some(12));
        assert_eq!(job.segment_count, Some(480));
        assert_eq!(job.failure_reason, None);
        assert!(job.has_original, "accepted upload stores the original");
        assert_eq!(job.uploader_user_id, admin);
        assert_eq!(ingest_row_counts(&client, &job_id).await, (1, 1));
        let source: String = client
            .query_one("SELECT source FROM ingest_jobs WHERE id = $1", &[&job_id])
            .await
            .expect("read job source")
            .get(0);
        assert_eq!(source, "admin");
        assert_eq!(
            ingest_audit_chain(&client, &job_id).await,
            vec![(admin.clone(), "accepted".to_string(), None)],
            "acceptance audits once with no reason code"
        );

        // Same bytes again: the locked re-select converges — same job,
        // no new rows, only a duplicate audit.
        let replay = admin_ingest_upload_on(&mut client, &attempt)
            .await
            .expect("duplicate upload executes");
        let AdminIngestUploadOutcome::Stored { job, deduplicated } = replay else {
            panic!("replay must converge, got {replay:?}");
        };
        assert!(deduplicated, "replay reports deduplicated");
        assert_eq!(job.job_id, job_id);
        assert_eq!(ingest_row_counts(&client, &job_id).await, (1, 1));
        let mut converged = vec![
            (admin.clone(), "accepted".to_string(), None),
            (admin.clone(), "duplicate".to_string(), None),
        ];
        converged.sort();
        assert_eq!(
            ingest_audit_chain(&client, &job_id).await,
            converged,
            "replay converges without new job/original rows"
        );

        // A second admin uploading the same bytes converges on the
        // same job: job identity is content-derived, never uploader.
        // The first writer keeps the uploader slot; the replay only
        // audits its own duplicate.
        let second = AdminIngestAttempt {
            actor_user_id: admin2.clone(),
            job_id: attempt.job_id.clone(),
            idempotency_key: attempt.idempotency_key.clone(),
            content_sha256_hex: attempt.content_sha256_hex.clone(),
            nzb_bytes: attempt.nzb_bytes,
            file_count: attempt.file_count,
            segment_count: attempt.segment_count,
            failure: attempt.failure,
            original: Some(synthetic_ingest_envelope()),
            kek_version: attempt.kek_version,
            source_ip: attempt.source_ip.clone(),
            now: attempt.now,
        };
        let cross = admin_ingest_upload_on(&mut client, &second)
            .await
            .expect("second-admin upload executes");
        let AdminIngestUploadOutcome::Stored { job, deduplicated } = cross else {
            panic!("second admin must converge, got {cross:?}");
        };
        assert!(deduplicated, "second admin converges as duplicate");
        assert_eq!(job.job_id, job_id);
        assert_eq!(
            job.uploader_user_id, admin,
            "the first writer keeps the uploader slot"
        );
        assert_eq!(ingest_row_counts(&client, &job_id).await, (1, 1));
        let mut both = vec![
            (admin.clone(), "accepted".to_string(), None),
            (admin.clone(), "duplicate".to_string(), None),
            (admin2.clone(), "duplicate".to_string(), None),
        ];
        both.sort();
        assert_eq!(
            ingest_audit_chain(&client, &job_id).await,
            both,
            "each replay audits exactly its own duplicate"
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_ingest_rejected_upload_stores_failed_without_original() {
    run_test(async {
        let mut client = connect().await;
        let (admin, member) = seed_admin_test_users(&client).await;
        let sha = format!("{:064x}", rand::random::<u128>());
        let job_id = format!("adm_ai_reject_{:016x}", rand::random::<u64>());
        let rejected = AdminIngestAttempt {
            actor_user_id: admin.clone(),
            job_id: job_id.clone(),
            idempotency_key: format!("admin-ingest:{sha}:upload"),
            content_sha256_hex: sha,
            nzb_bytes: 9_000_000,
            file_count: None,
            segment_count: None,
            failure: Some(FailureReason::TooLarge),
            original: None,
            kek_version: 1,
            source_ip: "198.51.100.9".to_string(),
            now: NOW,
        };

        // A rejected upload still executes its transaction: a failed
        // job row with the fixed-vocabulary reason, no original, and
        // a rejected audit carrying the reason code.
        let outcome = admin_ingest_upload_on(&mut client, &rejected)
            .await
            .expect("rejected upload executes");
        let AdminIngestUploadOutcome::Stored { job, deduplicated } = outcome else {
            panic!("rejection must store a failed row, got {outcome:?}");
        };
        assert!(!deduplicated, "first rejection is not a duplicate");
        assert_eq!(job.db_status, "failed");
        assert_eq!(job.failure_reason, Some("too_large".to_string()));
        assert!(!job.has_original, "rejections store no original");
        assert_eq!(ingest_row_counts(&client, &job_id).await, (1, 0));
        assert_eq!(
            ingest_audit_chain(&client, &job_id).await,
            vec![(
                admin.clone(),
                "rejected".to_string(),
                Some("too_large".to_string()),
            )],
            "rejection audits once with its reason code"
        );

        // A non-admin actor is refused before any write: no job row,
        // no original, no audit.
        let other_sha = format!("{:064x}", rand::random::<u128>());
        let other_id = format!("adm_ai_reject_{:016x}", rand::random::<u64>());
        let stranger = AdminIngestAttempt {
            actor_user_id: member.clone(),
            job_id: other_id.clone(),
            idempotency_key: format!("admin-ingest:{other_sha}:upload"),
            content_sha256_hex: other_sha,
            nzb_bytes: 1_048_576,
            file_count: Some(12),
            segment_count: Some(480),
            failure: None,
            original: Some(synthetic_ingest_envelope()),
            kek_version: 1,
            source_ip: "198.51.100.9".to_string(),
            now: NOW,
        };
        assert_eq!(
            admin_ingest_upload_on(&mut client, &stranger)
                .await
                .expect("member upload is a typed refusal, not an error"),
            AdminIngestUploadOutcome::ActorNotAdmin
        );
        assert_eq!(ingest_row_counts(&client, &other_id).await, (0, 0));
        assert!(
            ingest_audit_chain(&client, &other_id).await.is_empty(),
            "a refused upload audits nothing"
        );

        // A demoted-mid-request actor is refused the same way: the
        // write transaction re-reads the role under its locks.
        client
            .execute("UPDATE users SET role = 'member' WHERE id = $1", &[&admin])
            .await
            .expect("demote the actor mid-flight");
        let demoted_sha = format!("{:064x}", rand::random::<u128>());
        let demoted_id = format!("adm_ai_reject_{:016x}", rand::random::<u64>());
        let demoted = AdminIngestAttempt {
            actor_user_id: admin.clone(),
            job_id: demoted_id.clone(),
            idempotency_key: format!("admin-ingest:{demoted_sha}:upload"),
            content_sha256_hex: demoted_sha,
            nzb_bytes: 1_048_576,
            file_count: Some(12),
            segment_count: Some(480),
            failure: None,
            original: Some(synthetic_ingest_envelope()),
            kek_version: 1,
            source_ip: "198.51.100.9".to_string(),
            now: NOW,
        };
        assert_eq!(
            admin_ingest_upload_on(&mut client, &demoted)
                .await
                .expect("demoted upload is a typed refusal, not an error"),
            AdminIngestUploadOutcome::ActorNotAdmin
        );
        assert_eq!(ingest_row_counts(&client, &demoted_id).await, (0, 0));
        assert!(
            ingest_audit_chain(&client, &demoted_id).await.is_empty(),
            "a demoted actor's upload audits nothing"
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_ingest_delete_then_reupload_is_fresh() {
    run_test(async {
        let mut client = connect().await;
        let (admin, member) = seed_admin_test_users(&client).await;
        let first = accepted_ingest_attempt(&admin, "del", NOW);
        let first_id = first.job_id.clone();
        let second = accepted_ingest_attempt(&admin, "keep", NOW);
        let second_id = second.job_id.clone();
        for attempt in [&first, &second] {
            admin_ingest_upload_on(&mut client, attempt)
                .await
                .expect("seed upload executes");
        }

        // The delete primitive removes the original and the job in
        // one transaction; the other job is untouched.
        assert_eq!(
            admin_ingest_delete_on(&mut client, &admin, &first_id)
                .await
                .expect("delete executes"),
            AdminIngestDeleteOutcome::Deleted
        );
        assert_eq!(ingest_row_counts(&client, &first_id).await, (0, 0));
        assert_eq!(
            ingest_row_counts(&client, &second_id).await,
            (1, 1),
            "deleting one job leaves the other job and original alone"
        );

        // A second delete of the same id reports Missing; deleting
        // an id that never existed does too.
        assert_eq!(
            admin_ingest_delete_on(&mut client, &admin, &first_id)
                .await
                .expect("repeat delete executes"),
            AdminIngestDeleteOutcome::Missing
        );
        let absent = format!("adm_ai_absent_{:016x}", rand::random::<u64>());
        assert_eq!(
            admin_ingest_delete_on(&mut client, &admin, &absent)
                .await
                .expect("absent delete executes"),
            AdminIngestDeleteOutcome::Missing
        );

        // A non-admin delete is refused and removes nothing.
        assert_eq!(
            admin_ingest_delete_on(&mut client, &member, &second_id)
                .await
                .expect("member delete is a typed refusal, not an error"),
            AdminIngestDeleteOutcome::ActorNotAdmin
        );
        assert_eq!(
            ingest_row_counts(&client, &second_id).await,
            (1, 1),
            "a refused delete removes nothing"
        );

        // Re-uploading the deleted bytes lands a fresh job: the dedupe
        // entry went away with the delete, so this is a new acceptance.
        let fresh = admin_ingest_upload_on(&mut client, &first)
            .await
            .expect("re-upload executes");
        let AdminIngestUploadOutcome::Stored { job, deduplicated } = fresh else {
            panic!("re-upload after delete must store fresh, got {fresh:?}");
        };
        assert!(
            !deduplicated,
            "re-upload after delete is a fresh acceptance"
        );
        assert_eq!(job.job_id, first_id);
        assert_eq!(ingest_row_counts(&client, &first_id).await, (1, 1));
        assert_eq!(
            ingest_row_counts(&client, &second_id).await,
            (1, 1),
            "the fresh upload leaves the other job alone"
        );
    });
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_admin_ingest_list_and_get_read_redacted_jobs() {
    run_test(async {
        let mut client = connect().await;
        let (admin, _) = seed_admin_test_users(&client).await;
        let tag = format!("lst{:016x}", rand::random::<u64>());
        let old = accepted_ingest_attempt(&admin, &format!("{tag}old"), NOW);
        let mut new = accepted_ingest_attempt(&admin, &format!("{tag}new"), NOW + 30);
        new.failure = Some(FailureReason::NotNzb);
        new.nzb_bytes = 64;
        new.file_count = None;
        new.segment_count = None;
        new.original = None;
        for attempt in [&old, &new] {
            admin_ingest_upload_on(&mut client, attempt)
                .await
                .expect("seed upload executes");
        }

        // The readers run through the real store with an injected
        // disposable client: no copied SQL, the production decoder.
        // The injected client is single-use (the seam `take()`s it on
        // connect), so each store call below gets a fresh connection.
        let page = crate::handlers::Store::admin_ingest_list(
            &super::session_retry_store_for_postgres_test(connect().await),
            None,
            100,
            0,
        )
        .await
        .expect("list executes");
        let mine: Vec<&crate::admin_ingest::StoredAdminIngestJob> = page
            .iter()
            .filter(|job| job.job_id.starts_with(&format!("adm_ai_{tag}")))
            .collect();
        assert_eq!(
            mine.iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            vec![new.job_id.as_str(), old.job_id.as_str()],
            "list is newest-first across this test's jobs"
        );
        assert_eq!(mine[0].db_status, "failed");
        assert_eq!(
            mine[0].failure_reason,
            Some("not_nzb".to_string()),
            "the failed job carries its fixed-vocabulary reason"
        );
        assert!(!mine[0].has_original, "the failed job has no original");
        assert_eq!(mine[1].db_status, "queued");
        assert!(mine[1].has_original, "the accepted job has its original");
        for job in &mine {
            assert_eq!(
                job.uploader_user_id, admin,
                "readers echo the uploading admin and nothing else"
            );
        }

        // The status filter narrows to the failed job only.
        let failed = crate::handlers::Store::admin_ingest_list(
            &super::session_retry_store_for_postgres_test(connect().await),
            Some("failed"),
            100,
            0,
        )
        .await
        .expect("filtered list executes");
        let failed_mine: Vec<&str> = failed
            .iter()
            .filter(|job| job.job_id.starts_with(&format!("adm_ai_{tag}")))
            .map(|job| job.job_id.as_str())
            .collect();
        assert_eq!(failed_mine, vec![new.job_id.as_str()]);

        // Single reads are redacted by construction: ids, counts and
        // the failure reason only — the struct has no field for the
        // content hash, bytes or segment data.
        let got = crate::handlers::Store::admin_ingest_get(
            &super::session_retry_store_for_postgres_test(connect().await),
            &new.job_id,
        )
        .await
        .expect("get executes")
        .expect("stored job reads back");
        assert_eq!(got.job_id, new.job_id);
        assert_eq!(got.nzb_bytes, 64);
        assert_eq!(got.failure_reason, Some("not_nzb".to_string()));
        assert!(!got.has_original);
        assert_eq!(
            crate::handlers::Store::admin_ingest_get(
                &super::session_retry_store_for_postgres_test(connect().await),
                &format!("adm_ai_{tag}_absent"),
            )
            .await
            .expect("absent get executes"),
            None
        );
    });
}

// --- Invite-budget receipts on challenges (migration 0041) ----------------

fn receipt_challenge(fixture: &Fixture, suffix: &str, expires_at: u64) -> PasskeyChallenge {
    PasskeyChallenge {
        id: format!("{}-{suffix}", fixture.challenge.id),
        rp_id: RP.to_string(),
        origin: ORIGIN.to_string(),
        issued_at: NOW - 10,
        expires_at,
        consumed: false,
    }
}

fn receipt_for(fixture: &Fixture, tag: &str) -> InviteBudgetReceipt {
    InviteBudgetReceipt {
        invite_code_hash: format!("{}-{tag}-code", fixture.challenge.id),
        email_budget_key: format!("{}-{tag}-email", fixture.challenge.id),
    }
}

async fn receipt_columns(client: &Client, id: &str) -> Option<(Option<String>, Option<String>)> {
    client
        .query_opt(
            "SELECT invite_budget_code_hash, invite_budget_email_key FROM auth_challenges WHERE id = $1",
            &[&id],
        )
        .await
        .expect("observe receipt columns")
        .map(|row| (row.get(0), row.get(1)))
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_challenge_receipt_roundtrips_and_legacy_rows_stay_null() {
    run_test(async {
        let mut client = connect().await;
        let fixture = Fixture::seed(&client).await;
        let user_id = fixture.credential.user_id.as_str();

        // A challenge written before receipts existed (raw legacy INSERT).
        let legacy = challenge_by_id_on(&client, &fixture.challenge.id)
            .await
            .expect("production read of legacy row")
            .expect("legacy row exists");
        assert_eq!(legacy.invite_budget_receipt, None);

        // Production insert with a receipt round-trips exactly.
        let with_receipt = receipt_challenge(&fixture, "receipt", NOW + 300);
        let receipt = receipt_for(&fixture, "a");
        insert_challenge_on(
            &client,
            &with_receipt,
            Some(user_id),
            "register",
            Some(&receipt),
        )
        .await
        .expect("production insert with receipt");
        let row = challenge_by_id_on(&client, &with_receipt.id)
            .await
            .expect("production read")
            .expect("row exists");
        assert_eq!(row.invite_budget_receipt, Some(receipt.clone()));
        assert_eq!(row.user_id.as_deref(), Some(user_id));
        assert_eq!(row.purpose, "register");
        assert!(!row.challenge.consumed);

        // None leaves both columns NULL (register, login and unbound login).
        let plain = receipt_challenge(&fixture, "plain", NOW + 300);
        insert_challenge_on(&client, &plain, Some(user_id), "register", None)
            .await
            .expect("production insert without receipt");
        let login = receipt_challenge(&fixture, "login", NOW + 300);
        insert_challenge_on(&client, &login, None, "login", None)
            .await
            .expect("unbound login insert");
        for id in [&plain.id, &login.id] {
            assert_eq!(receipt_columns(&client, id).await, Some((None, None)));
            assert_eq!(
                challenge_by_id_on(&client, id)
                    .await
                    .expect("read")
                    .expect("row")
                    .invite_budget_receipt,
                None
            );
        }

        // Consuming the challenge (the SingleUseClaim shape) stays allowed
        // and keeps the receipt.
        let claimed = client
            .execute(
                "UPDATE auth_challenges SET consumed = TRUE WHERE id = $1 AND consumed = FALSE",
                &[&with_receipt.id],
            )
            .await
            .expect("consume receipt-bearing challenge");
        assert_eq!(claimed, 1);
        let row = challenge_by_id_on(&client, &with_receipt.id)
            .await
            .expect("read consumed")
            .expect("row");
        assert!(row.challenge.consumed);
        assert_eq!(row.invite_budget_receipt, Some(receipt));

        // Database shape: a receipt needs purpose register and a user.
        for (suffix, user, purpose) in [
            ("login-receipt", Some(user_id), "login"),
            ("reauth-receipt", Some(user_id), "reauth"),
            ("unbound-receipt", None, "register"),
        ] {
            let bad = receipt_challenge(&fixture, suffix, NOW + 300);
            let bad_receipt = receipt_for(&fixture, suffix);
            insert_challenge_on(&client, &bad, user, purpose, Some(&bad_receipt))
                .await
                .expect_err("receipt outside a bound register challenge is refused");
            assert_eq!(receipt_columns(&client, &bad.id).await, None);
        }

        // A half-populated pair is refused by the database ...
        let half = receipt_challenge(&fixture, "half", NOW + 300);
        let err = client
            .execute(
                "INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, consumed, purpose, invite_budget_code_hash) VALUES ($1, $2, $3, $4, $5, FALSE, 'register', 'only-code')",
                &[&half.id, &user_id, &half.rp_id, &half.origin, &(half.expires_at as i64)],
            )
            .await
            .expect_err("partial pair violates the both-or-neither check");
        assert_eq!(err.code(), Some(&SqlState::CHECK_VIOLATION));

        // ... and if corrupt state ever existed, the production read fails
        // closed. Constraint dropped only inside a rolled-back transaction.
        let transaction = client
            .transaction()
            .await
            .expect("open scratch transaction");
        transaction
            .batch_execute(
                "ALTER TABLE public.auth_challenges DROP CONSTRAINT auth_challenges_invite_budget_receipt_check",
            )
            .await
            .expect("drop check inside rolled-back transaction");
        for (suffix, code, email) in [
            ("half-code", Some("only-code"), None::<&str>),
            ("half-email", None::<&str>, Some("only-email")),
        ] {
            let corrupt = receipt_challenge(&fixture, suffix, NOW + 300);
            transaction
                .execute(
                    "INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, consumed, purpose, invite_budget_code_hash, invite_budget_email_key) VALUES ($1, $2, $3, $4, $5, FALSE, 'register', $6, $7)",
                    &[&corrupt.id, &user_id, &corrupt.rp_id, &corrupt.origin, &(corrupt.expires_at as i64), &code, &email],
                )
                .await
                .expect("seed corrupt half pair in scratch transaction");
            assert!(
                challenge_by_id_on(&transaction, &corrupt.id).await.is_err(),
                "half-populated receipt pair fails closed"
            );
        }
        transaction
            .rollback()
            .await
            .expect("roll back scratch corruption");
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_challenge_receipt_and_binding_are_immutable_once_set() {
    run_test(async {
        let client = connect().await;
        let fixture = Fixture::seed(&client).await;
        let other = Fixture::seed(&client).await;
        let user_id = fixture.credential.user_id.as_str();
        let bound = receipt_challenge(&fixture, "bound", NOW + 300);
        let receipt = receipt_for(&fixture, "bound");
        insert_challenge_on(&client, &bound, Some(user_id), "register", Some(&receipt))
            .await
            .expect("production insert with receipt");

        let other_user = other.credential.user_id.as_str();
        let mutations = [
            "SET invite_budget_code_hash = 'other'".to_string(),
            "SET invite_budget_email_key = 'other'".to_string(),
            "SET invite_budget_code_hash = NULL, invite_budget_email_key = NULL".to_string(),
            "SET invite_budget_code_hash = NULL".to_string(),
            "SET id = id || '-moved'".to_string(),
            format!("SET user_id = '{other_user}'"),
            "SET user_id = NULL".to_string(),
            "SET purpose = 'login'".to_string(),
            "SET rp_id = 'evil.example.com'".to_string(),
            "SET expires_at = expires_at + 1".to_string(),
        ];
        for set in &mutations {
            let err = client
                .execute(
                    &format!("UPDATE auth_challenges {set} WHERE id = $1"),
                    &[&bound.id],
                )
                .await
                .expect_err(set);
            assert_eq!(err.code(), Some(&SqlState::RESTRICT_VIOLATION), "{set}");
        }
        let err = client
            .execute(
                "UPDATE auth_challenges SET origin = 'https://evil.example.com' WHERE id = $1",
                &[&bound.id],
            )
            .await
            .expect_err("origin is part of the immutable binding");
        assert_eq!(err.code(), Some(&SqlState::RESTRICT_VIOLATION));

        let row = challenge_by_id_on(&client, &bound.id)
            .await
            .expect("read after refused updates")
            .expect("row survives");
        assert_eq!(row.invite_budget_receipt, Some(receipt.clone()));
        assert_eq!(row.user_id.as_deref(), Some(user_id));
        assert_eq!(row.purpose, "register");
        assert_eq!(row.challenge.expires_at, NOW + 300);
        assert_eq!(row.challenge.origin, ORIGIN);
        assert_eq!(row.challenge.rp_id, RP);

        // A no-op rewrite of the same tuple plus consume is allowed.
        assert_eq!(
            client
                .execute(
                    "UPDATE auth_challenges SET consumed = TRUE, expires_at = expires_at WHERE id = $1",
                    &[&bound.id],
                )
                .await
                .expect("consume receipt-bearing challenge"),
            1
        );

        // A receipt can never be attached retroactively to a null row,
        // whether the row is a legacy one or a fresh receiptless insert.
        let plain = receipt_challenge(&fixture, "plain", NOW + 300);
        insert_challenge_on(&client, &plain, Some(user_id), "register", None)
            .await
            .expect("receiptless insert");
        for id in [&fixture.challenge.id, &plain.id] {
            for set in [
                "SET invite_budget_code_hash = 'c', invite_budget_email_key = 'e'",
                "SET invite_budget_code_hash = 'c'",
                "SET invite_budget_email_key = 'e'",
            ] {
                let err = client
                    .execute(
                        &format!("UPDATE auth_challenges {set} WHERE id = $1"),
                        &[id],
                    )
                    .await
                    .expect_err("retroactive receipt attach is refused");
                assert_eq!(err.code(), Some(&SqlState::RESTRICT_VIOLATION), "{set}");
            }
            assert_eq!(receipt_columns(&client, id).await, Some((None, None)));
        }
        // Null rows keep their ordinary claim semantics.
        assert_eq!(
            client
                .execute(
                    "UPDATE auth_challenges SET consumed = TRUE WHERE id = $1 AND consumed = FALSE",
                    &[&plain.id],
                )
                .await
                .expect("claim null-receipt row"),
            1
        );
    });
}

#[test]
#[ignore = "requires migrated disposable PostgreSQL; required by api-migrations"]
fn postgres_challenge_receipt_delete_reinsert_after_concurrent_reap_is_fresh() {
    run_test(async {
        let cleanup = connect().await;
        let mut locker = connect().await;
        let fixture = Fixture::seed(&cleanup).await;
        let user_id = fixture.credential.user_id.as_str();
        let expiring = receipt_challenge(&fixture, "reaped", NOW);
        let first = receipt_for(&fixture, "first");
        insert_challenge_on(&cleanup, &expiring, Some(user_id), "register", Some(&first))
            .await
            .expect("production insert of expiring receipt row");

        // A concurrent holder pins the expired row: the production prune
        // skips it, and the receipt is unchanged.
        let transaction = locker
            .transaction()
            .await
            .expect("open competing transaction");
        transaction
            .query_one(
                "SELECT id FROM auth_challenges WHERE id = $1 FOR UPDATE",
                &[&expiring.id],
            )
            .await
            .expect("hold the expired receipt row");
        tokio::time::timeout(
            Duration::from_secs(1),
            prune_expired_challenges_on(&cleanup, NOW),
        )
        .await
        .expect("prune must not wait for the locked row")
        .expect("production prune under contention");
        let row = challenge_by_id_on(&cleanup, &expiring.id)
            .await
            .expect("read skipped row")
            .expect("locked row survives the first pass");
        assert_eq!(row.invite_budget_receipt, Some(first.clone()));

        transaction.commit().await.expect("release the held row");
        prune_expired_challenges_on(&cleanup, NOW)
            .await
            .expect("production prune after release");
        assert_eq!(receipt_columns(&cleanup, &expiring.id).await, None);

        // The reaped id is a fresh INSERT: it may carry a different receipt
        // (or none), and nothing from the deleted row is resurrected.
        let second = receipt_for(&fixture, "second");
        let mut reinserted = expiring.clone();
        reinserted.expires_at = NOW + 300;
        insert_challenge_on(
            &cleanup,
            &reinserted,
            Some(user_id),
            "register",
            Some(&second),
        )
        .await
        .expect("reinsert reaped id with a new receipt");
        let row = challenge_by_id_on(&cleanup, &expiring.id)
            .await
            .expect("read reinserted row")
            .expect("reinserted row");
        assert_eq!(row.invite_budget_receipt, Some(second));
        assert!(!row.challenge.consumed);
        assert_eq!(row.challenge.expires_at, NOW + 300);

        // Reaped and reinserted again without a receipt: stays null.
        cleanup
            .execute("DELETE FROM auth_challenges WHERE id = $1", &[&expiring.id])
            .await
            .expect("delete reinserted row");
        insert_challenge_on(&cleanup, &reinserted, Some(user_id), "register", None)
            .await
            .expect("reinsert without receipt");
        assert_eq!(
            receipt_columns(&cleanup, &expiring.id).await,
            Some((None, None))
        );
    });
}
