//! Real-PostgreSQL tests for the source-owned posted-generation preparation
//! adapter (`nntune_ingest::posted_generations`) against migrations 0037–0045.
//!
//! The full bank runs through `run_posted_generation_tests.py` on Actions:
//! ordinary cases first, irreversible retirement LAST, on a freshly migrated
//! throwaway `nntune_ci` service. `NNTUNE_TEST_DATABASE_URL` is required
//! (a missing value fails, it never skips) and is validated as a
//! loopback, single-host, no-hostaddr configuration for `nntune_ci` before
//! any connection is opened.
//!
//! Scope: preparation only. Migration 0037 keeps the unique plaintext hash
//! constraint, so failed-generation replacement, reuse, promotion and
//! publication are NOT implemented or exercised here, and nothing in this
//! file shows the whole finding fixed. Fixtures are synthetic sealed byte
//! buffers keyed by database UUIDs; custody rows are retained by design, so
//! nothing is cleaned up. Failures print only static labels: no connection
//! URL, query text or row data.
#![cfg(feature = "postgres-tests")]

use nntune_ingest::posted_files::{
    insert_posted_file_sql, select_posted_file_id_by_sha_sql, spool_copy_state_db_str,
    PostedFileParams,
};
use nntune_ingest::posted_generations::{
    acquire_attempt, admit, observe, posting_fence_is_current, recover, AdmissionOutcome,
    AttemptHandle, AttemptOutcome, CandidateMint, Observation, StoreError,
};
use nntune_ingest::two_spool::SpoolCopyState;
use std::future::Future;
use std::sync::RwLock;
use std::time::Duration;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Config, NoTls};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);
const MIGRATION_0037: &str = include_str!("../../../migrations/0037_posted_file_generations.sql");
const MIGRATION_0038: &str = include_str!("../../../migrations/0038_posted_file_rechecks.sql");
const MIGRATION_0039: &str =
    include_str!("../../../migrations/0039_posted_file_content_checks.sql");

const MIGRATION_0040: &str =
    include_str!("../../../migrations/0040_posted_file_recheck_cursor.sql");
const MIGRATION_0041: &str =
    include_str!("../../../migrations/0041_posted_file_presence_provenance.sql");
const MIGRATION_0042: &str =
    include_str!("../../../migrations/0042_posted_file_legacy_health_fence.sql");
const MIGRATION_0043: &str =
    include_str!("../../../migrations/0043_posted_file_registration_fence.sql");
const MIGRATION_0044: &str =
    include_str!("../../../migrations/0044_posted_file_reference_fence.sql");
const MIGRATION_0045: &str =
    include_str!("../../../migrations/0045_posted_file_legacy_writer_retirement.sql");

#[path = "support/posted_generation_abandon.rs"]
mod posted_generation_abandon;
#[path = "support/posted_generation_checkpoint.rs"]
mod posted_generation_checkpoint;
#[path = "support/posted_generation_content.rs"]
mod posted_generation_content;
#[path = "support/posted_generation_driver.rs"]
mod posted_generation_driver;
#[path = "support/posted_generation_failure.rs"]
mod posted_generation_failure;
#[path = "support/posted_generation_health.rs"]
mod posted_generation_health;
#[path = "support/posted_generation_legacy_fence.rs"]
mod posted_generation_legacy_fence;
#[path = "support/posted_generation_presence.rs"]
mod posted_generation_presence;
#[path = "support/posted_generation_presence_provenance.rs"]
mod posted_generation_presence_provenance;
#[path = "support/posted_generation_rechecks.rs"]
mod posted_generation_rechecks;
#[path = "support/posted_generation_repair_content.rs"]
mod posted_generation_repair_content;
#[path = "support/posted_generation_retained.rs"]
mod posted_generation_retained;
#[path = "support/posted_generation_takeover.rs"]
mod posted_generation_takeover;
// Irreversible on the disposable service; the Actions runner runs this LAST.
#[path = "support/posted_generation_retirement.rs"]
mod zz_posted_generation_retirement;

// The replay test runs DDL that takes ACCESS EXCLUSIVE locks. It holds the
// write side so no other test in this process is mid-statement.
static SCHEMA_GATE: RwLock<()> = RwLock::new(());

fn run_blocking(exclusive: bool, test: impl Future<Output = ()>) {
    let _shared;
    let _exclusive;
    if exclusive {
        _exclusive = Some(SCHEMA_GATE.write().unwrap_or_else(|e| e.into_inner()));
        _shared = None;
    } else {
        _shared = Some(SCHEMA_GATE.read().unwrap_or_else(|e| e.into_inner()));
        _exclusive = None;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("native test runtime")
        .block_on(async {
            tokio::time::timeout(TEST_TIMEOUT, test)
                .await
                .expect("posted generation PostgreSQL test exceeded 20 seconds");
        });
}

fn run_test(test: impl Future<Output = ()>) {
    run_blocking(false, test);
}

// ---- configuration guard (no socket, no credentials read) ----------------

fn validate_disposable_database_config(config: &Config) -> Result<(), &'static str> {
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
fn disposable_config_accepts_only_single_loopback_nntune_ci() {
    for input in [
        "postgresql://localhost/nntune_ci",
        "host=127.0.0.1 dbname=nntune_ci",
        "host=::1 dbname=nntune_ci",
    ] {
        let config: Config = input.parse().expect("parse credential-free config");
        assert_eq!(validate_disposable_database_config(&config), Ok(()));
    }
}

#[test]
fn disposable_config_rejects_hostaddr_nonloopback_multihost_and_wrong_database() {
    for input in [
        "host=localhost hostaddr=127.0.0.1 dbname=nntune_ci",
        "hostaddr=192.0.2.10 dbname=nntune_ci",
        "host=db.example.com dbname=nntune_ci",
        "host=192.0.2.10 dbname=nntune_ci",
        "host=localhost,127.0.0.1 dbname=nntune_ci",
        "host=/tmp dbname=nntune_ci",
        "dbname=nntune_ci",
        "host=localhost dbname=postgres",
        "host=localhost dbname=nntune_ci_backup",
        "host=localhost",
    ] {
        let config: Config = input.parse().expect("parse credential-free config");
        assert!(
            validate_disposable_database_config(&config).is_err(),
            "an unsafe disposable database configuration must be rejected before connecting"
        );
    }
}

fn database_config() -> Config {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").unwrap_or_else(|_| {
        panic!("NNTUNE_TEST_DATABASE_URL is required; database tests never silently skip")
    });
    let config = url
        .parse::<Config>()
        .unwrap_or_else(|_| panic!("NNTUNE_TEST_DATABASE_URL is not a valid configuration"));
    if let Err(reason) = validate_disposable_database_config(&config) {
        panic!("{reason}");
    }
    config
}

// ---- small helpers that never print database detail ----------------------

trait Quiet<T> {
    fn quiet(self, ctx: &'static str) -> T;
}

impl<T> Quiet<T> for Result<T, tokio_postgres::Error> {
    fn quiet(self, ctx: &'static str) -> T {
        match self {
            Ok(value) => value,
            Err(_) => panic!("{ctx}"),
        }
    }
}

async fn connect() -> Client {
    let config = database_config();
    let (client, connection) = config
        .connect(NoTls)
        .await
        .unwrap_or_else(|_| panic!("connect disposable PostgreSQL"));
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            "SET statement_timeout = '10s'; SET lock_timeout = '10s'; \
             SET idle_in_transaction_session_timeout = '15s'",
        )
        .await
        .quiet("bound disposable database operations");
    client
}

async fn count(db: &Client, sql: &str, params: &[&(dyn ToSql + Sync)]) -> i64 {
    db.query_one(sql, params).await.quiet("count query").get(0)
}

fn expect_db_error<T>(
    result: Result<T, tokio_postgres::Error>,
    code: &SqlState,
    message: Option<&str>,
    ctx: &'static str,
) {
    match result {
        Ok(_) => panic!("{ctx}: statement was accepted"),
        Err(e) => {
            let Some(db) = e.as_db_error() else {
                panic!("{ctx}: not a database error")
            };
            assert!(db.code() == code, "{ctx}: unexpected SQLSTATE");
            if let Some(expected) = message {
                assert!(
                    db.message() == expected,
                    "{ctx}: unexpected trigger message"
                );
            }
        }
    }
}

async fn db_uuid(db: &Client) -> String {
    db.query_one("SELECT gen_random_uuid()::text", &[])
        .await
        .quiet("mint fixture uuid")
        .get(0)
}

async fn digest(db: &Client, label: &str, uuid: &str) -> String {
    db.query_one(
        "SELECT encode(sha256(convert_to($1::text || ':' || $2::text, 'UTF8')), 'hex')",
        &[&label, &uuid],
    )
    .await
    .quiet("derive fixture digest")
    .get(0)
}

async fn seed_user(db: &Client, tag: &str) -> String {
    let owner = format!("pg-owner-{tag}");
    db.execute(
        "INSERT INTO users (id, email, created_at) VALUES ($1, $2, 1700000000)",
        &[&owner, &format!("{owner}@example.invalid")],
    )
    .await
    .quiet("seed fixture user");
    owner
}

async fn seed_job(db: &Client, tag: &str) -> (String, String) {
    let owner = seed_user(db, tag).await;
    let job = format!("pg-job-{tag}");
    db.execute(
        "INSERT INTO ingest_jobs (id, user_id, idempotency_key, status) \
         VALUES ($1, $2, $3, 'running')",
        &[&job, &owner, &format!("pg-idem-{tag}")],
    )
    .await
    .quiet("seed fixture job");
    (job, owner)
}

/// Synthetic sealed custody. Byte buffers are fill patterns, never real
/// envelopes, NZB bytes, article identities, keys, names or segment lists.
#[derive(Clone)]
struct Sealed {
    op: String,
    generation: String,
    plan: String,
    hash: String,
    header: Vec<u8>,
    key: Vec<u8>,
    key_wrap_nonce: Vec<u8>,
    key_wrapped: Vec<u8>,
    key_payload_nonce: Vec<u8>,
    manifest: Vec<u8>,
    manifest_wrap_nonce: Vec<u8>,
    manifest_wrapped: Vec<u8>,
    manifest_payload_nonce: Vec<u8>,
}

impl Sealed {
    fn new(uuid: &str, hash: &str, plan: &str, fill: u8) -> Self {
        let bytes = |n: usize, salt: u8| vec![fill.wrapping_add(salt); n];
        Sealed {
            op: format!("pg-op-{uuid}"),
            generation: format!("pg-gen-{uuid}"),
            plan: plan.to_owned(),
            hash: hash.to_owned(),
            header: bytes(24, 1),
            key: bytes(48, 2),
            key_wrap_nonce: bytes(12, 3),
            key_wrapped: bytes(48, 4),
            key_payload_nonce: bytes(12, 5),
            manifest: bytes(32, 6),
            manifest_wrap_nonce: bytes(12, 7),
            manifest_wrapped: bytes(48, 8),
            manifest_payload_nonce: bytes(12, 9),
        }
    }

    fn custody(&self) -> PostedFileParams<'_> {
        PostedFileParams {
            plaintext_sha256: &self.hash,
            backbone_post: "pool-a",
            backbone_verify: "pool-b",
            article_count: 3,
            verified_count: 0,
            original_nzb_sha256: None,
            post_copy: SpoolCopyState::Unknown,
            verify_copy: SpoolCopyState::Unknown,
            two_spool_verified: false,
            repost_exhausted: false,
            plaintext_size: 1024,
            article_payload_size: 512,
            header_nonce: &self.header,
            file_key_encrypted: &self.key,
            file_key_kek_version: 1,
            file_key_dek_wrapped_nonce: &self.key_wrap_nonce,
            file_key_dek_wrapped: &self.key_wrapped,
            file_key_payload_nonce: &self.key_payload_nonce,
            file_key_payload_format: 1,
            manifest_encrypted: &self.manifest,
            manifest_kek_version: 1,
            manifest_dek_wrapped_nonce: &self.manifest_wrap_nonce,
            manifest_dek_wrapped: &self.manifest_wrapped,
            manifest_payload_nonce: &self.manifest_payload_nonce,
            manifest_payload_format: 1,
        }
    }

    fn mint(&self) -> CandidateMint<'_> {
        CandidateMint {
            operation_id: &self.op,
            generation_id: &self.generation,
            plan_sha256: &self.plan,
            custody: self.custody(),
        }
    }
}

fn mint_with<'a>(
    s: &'a Sealed,
    change: impl FnOnce(&mut PostedFileParams<'a>),
) -> CandidateMint<'a> {
    let mut custody = s.custody();
    change(&mut custody);
    CandidateMint {
        custody,
        ..s.mint()
    }
}

struct Case {
    uuid: String,
    job: String,
    owner: String,
    sealed: Sealed,
}

async fn case(db: &Client) -> Case {
    let uuid = db_uuid(db).await;
    let hash = digest(db, "content", &uuid).await;
    let plan = digest(db, "plan", &uuid).await;
    let (job, owner) = seed_job(db, &uuid).await;
    Case {
        sealed: Sealed::new(&uuid, &hash, &plan, 0x10),
        uuid,
        job,
        owner,
    }
}

async fn acquire(db: &mut Client, job: &str, owner: &str, expected: i64) -> AttemptHandle {
    match acquire_attempt(db, job, owner, expected).await {
        Ok(AttemptOutcome::Acquired(handle)) => handle,
        _ => panic!("attempt was not acquired"),
    }
}

async fn try_admit(
    db: &mut Client,
    attempt: &AttemptHandle,
    s: &Sealed,
) -> Result<AdmissionOutcome, StoreError> {
    let observation = observe(db, &s.hash).await.expect("observe hash");
    admit(db, attempt, &observation, &s.mint()).await
}

async fn admit_new(db: &mut Client, attempt: &AttemptHandle, s: &Sealed) -> AdmissionOutcome {
    try_admit(db, attempt, s).await.expect("admit")
}

async fn admitted(
    db: &mut Client,
    attempt: &AttemptHandle,
    s: &Sealed,
) -> nntune_ingest::posted_generations::GenerationHandle {
    match admit_new(db, attempt, s).await {
        AdmissionOutcome::Admitted(handle, _) => handle,
        _ => panic!("candidate was not admitted"),
    }
}

async fn custody_rows(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1 OR id = $2",
        &[&s.hash, &s.generation],
    )
    .await
}

async fn operation_rows(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_operations WHERE plaintext_sha256 = $1 OR id = $2",
        &[&s.hash, &s.op],
    )
    .await
}

async fn snapshot(db: &Client, s: &Sealed) -> Vec<Option<String>> {
    let row = db
        .query_one(
            "SELECT (SELECT md5(t::text) FROM posted_files t WHERE t.id = $1), \
             (SELECT md5(t::text) FROM posted_file_registry t WHERE t.plaintext_sha256 = $3), \
             (SELECT md5(t::text) FROM posted_file_operations t WHERE t.id = $2), \
             (SELECT md5(t::text) FROM posted_file_health t WHERE t.generation_id = $1), \
             (SELECT md5(string_agg(t::text, ',' ORDER BY t.kind, t.owner_id)) \
                FROM posted_file_references t WHERE t.generation_id = $1)",
            &[&s.generation, &s.op, &s.hash],
        )
        .await
        .quiet("snapshot admitted bundle");
    (0..5).map(|i| row.get(i)).collect()
}

// ---- tests ----------------------------------------------------------------

#[test]
fn postgres_absent_admission_persists_exact_bundle_with_clean_health() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let outcome = admit_new(&mut db, &attempt, s).await;
        let AdmissionOutcome::Admitted(handle, bundle) = outcome else {
            panic!("absent hash was not admitted")
        };
        assert_eq!(handle.generation_id(), s.generation);
        assert_eq!(handle.operation_id(), s.op);
        assert_eq!(bundle.generation_id(), s.generation);
        assert_eq!(bundle.plan_sha256(), s.plan);
        assert!(bundle.custody().expect("stored custody") == s.custody());
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("current posting fence"));
        assert_eq!(format!("{bundle:?}"), "StoredGeneration([REDACTED])");

        // Persisted and read back through independent queries.
        let r = db
            .query_one(
                "SELECT generation_operation, plan_sha256, plaintext_sha256, article_count, \
                 verified_count, post_copy, verify_copy, two_spool_verified, repost_exhausted, \
                 plaintext_size, article_payload_size, header_nonce, file_key_encrypted, \
                 file_key_envelope_version, file_key_dek_wrapped_nonce, file_key_dek_wrapped, \
                 file_key_payload_nonce, manifest_encrypted, manifest_envelope_version, \
                 manifest_dek_wrapped_nonce, manifest_dek_wrapped, manifest_payload_nonce \
                 FROM posted_files WHERE id = $1",
                &[&s.generation],
            )
            .await
            .quiet("read back custody");
        assert_eq!(r.get::<_, String>("generation_operation"), s.op);
        assert_eq!(r.get::<_, String>("plan_sha256"), s.plan);
        assert_eq!(r.get::<_, String>("plaintext_sha256"), s.hash);
        assert_eq!(r.get::<_, i32>("article_count"), 3);
        assert_eq!(r.get::<_, i32>("verified_count"), 0);
        assert_eq!(r.get::<_, String>("post_copy"), "unknown");
        assert_eq!(r.get::<_, String>("verify_copy"), "unknown");
        assert!(!r.get::<_, bool>("two_spool_verified"));
        assert!(!r.get::<_, bool>("repost_exhausted"));
        assert_eq!(r.get::<_, i64>("plaintext_size"), 1024);
        assert_eq!(r.get::<_, i64>("article_payload_size"), 512);
        assert_eq!(r.get::<_, i16>("file_key_envelope_version"), 2);
        assert_eq!(r.get::<_, i16>("manifest_envelope_version"), 2);
        assert!(r.get::<_, Vec<u8>>("header_nonce") == s.header);
        assert!(r.get::<_, Vec<u8>>("file_key_encrypted") == s.key);
        assert!(r.get::<_, Vec<u8>>("file_key_dek_wrapped_nonce") == s.key_wrap_nonce);
        assert!(r.get::<_, Vec<u8>>("file_key_dek_wrapped") == s.key_wrapped);
        assert!(r.get::<_, Vec<u8>>("file_key_payload_nonce") == s.key_payload_nonce);
        assert!(r.get::<_, Vec<u8>>("manifest_encrypted") == s.manifest);
        assert!(r.get::<_, Vec<u8>>("manifest_dek_wrapped_nonce") == s.manifest_wrap_nonce);
        assert!(r.get::<_, Vec<u8>>("manifest_dek_wrapped") == s.manifest_wrapped);
        assert!(r.get::<_, Vec<u8>>("manifest_payload_nonce") == s.manifest_payload_nonce);

        let reg = db
            .query_one(
                "SELECT selected_generation, revision, pending_operation \
                 FROM posted_file_registry WHERE plaintext_sha256 = $1",
                &[&s.hash],
            )
            .await
            .quiet("read back registry");
        assert_eq!(reg.get::<_, Option<String>>(0), None);
        assert_eq!(reg.get::<_, i64>(1), 1);
        assert_eq!(reg.get::<_, Option<String>>(2), Some(s.op.clone()));

        let op = db
            .query_one(
                "SELECT o.generation_id, o.job_id, o.attempt, o.state, o.registry_revision, \
                 o.predecessor, o.lease_until = a.lease_until AS lease_matches, \
                 a.token = o.attempt_token AS token_matches \
                 FROM posted_file_operations o JOIN posted_file_job_attempts a ON a.job_id = o.job_id \
                 WHERE o.id = $1 AND o.plaintext_sha256 = $2",
                &[&s.op, &s.hash],
            )
            .await
            .quiet("read back operation");
        assert_eq!(op.get::<_, String>("generation_id"), s.generation);
        assert_eq!(op.get::<_, String>("job_id"), c.job);
        assert_eq!(op.get::<_, i64>("attempt"), 1);
        assert_eq!(op.get::<_, String>("state"), "pending");
        assert_eq!(op.get::<_, i64>("registry_revision"), 1);
        assert_eq!(op.get::<_, Option<String>>("predecessor"), None);
        assert!(op.get::<_, bool>("lease_matches"));
        assert!(op.get::<_, bool>("token_matches"));

        // Clean health: unknown, no evidence, no article progress, no pin.
        let h = db
            .query_one(
                "SELECT state, revision, round, failure_kind, verified_at, retention_checked_at \
                 FROM posted_file_health WHERE generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("read back health");
        assert_eq!(h.get::<_, String>("state"), "unknown");
        assert_eq!(h.get::<_, i64>("revision"), 0);
        assert_eq!(h.get::<_, i64>("round"), 0);
        assert_eq!(h.get::<_, Option<String>>("failure_kind"), None);
        assert_eq!(h.get::<_, Option<i64>>("verified_at"), None);
        assert_eq!(h.get::<_, Option<i64>>("retention_checked_at"), None);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_article_progress WHERE generation_id = $1",
                &[&s.generation]
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references WHERE generation_id = $1 \
                 AND kind = 'in_flight' AND owner_id = $2",
                &[&s.generation, &s.op]
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references WHERE generation_id = $1",
                &[&s.generation]
            )
            .await,
            1,
            "an admitted generation carries only its in-flight reference"
        );
    });
}

#[test]
fn postgres_same_operation_recovers_exact_bundle_and_fresh_mint_is_rejected() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        admitted(&mut db, &attempt, s).await;

        match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle)) => {
                assert_eq!(handle.generation_id(), s.generation);
                assert_eq!(handle.operation_id(), s.op);
                assert_eq!(bundle.generation_id(), s.generation);
                assert_eq!(bundle.plan_sha256(), s.plan);
                assert!(bundle.custody().expect("stored custody") == s.custody());
            }
            _ => panic!("recovery did not return the exact admitted bundle"),
        }
        // Unknown operation and unknown hash are never converted to a bundle.
        let other_op = format!("{}-x", s.op);
        assert!(matches!(
            recover(&mut db, &attempt, &s.hash, &other_op).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        let other_hash = digest(&db, "unregistered", &c.uuid).await;
        assert!(matches!(
            recover(&mut db, &attempt, &other_hash, &s.op).await,
            Ok(AdmissionOutcome::InconsistentOrMissing)
        ));

        // A fresh mint on the same operation is rejected, never merged.
        let fresh = observe(&db, &s.hash).await.expect("observe pending hash");
        let mut other_generation = Sealed::new(&c.uuid, &s.hash, &s.plan, 0x40);
        other_generation.generation = format!("pg-gen-{}-x", c.uuid);
        let mut other_plan = s.clone();
        other_plan.plan = digest(&db, "other-plan", &c.uuid).await;
        let other_bytes = Sealed::new(&c.uuid, &s.hash, &s.plan, 0x40);
        for (label, minted) in [
            ("different generation id", &other_generation),
            ("different plan", &other_plan),
            ("different sealed bytes", &other_bytes),
        ] {
            assert!(
                matches!(
                    admit(&mut db, &attempt, &fresh, &minted.mint()).await,
                    Ok(AdmissionOutcome::StaleOrDiverged)
                ),
                "{label} must not be accepted for an existing operation"
            );
        }
        // A different operation for the same hash is busy, never a second candidate.
        let mut other_operation = Sealed::new(&c.uuid, &s.hash, &s.plan, 0x40);
        other_operation.op = other_op;
        other_operation.generation = format!("pg-gen-{}-y", c.uuid);
        assert!(matches!(
            admit(&mut db, &attempt, &fresh, &other_operation.mint()).await,
            Ok(AdmissionOutcome::ClaimBusy)
        ));
        // The exact original mint is idempotent.
        assert!(matches!(
            admit(&mut db, &attempt, &fresh, &s.mint()).await,
            Ok(AdmissionOutcome::ResumedSameCandidate(..))
        ));

        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
        for rejected in [&other_generation, &other_operation] {
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) FROM posted_files WHERE id = $1",
                    &[&rejected.generation]
                )
                .await,
                0
            );
        }
        match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(_, bundle)) => {
                assert!(bundle.custody().expect("stored custody") == s.custody());
            }
            _ => panic!("original bundle changed after rejected mints"),
        }
    });
}

#[test]
fn postgres_competing_jobs_for_one_absent_hash_produce_one_candidate() {
    run_test(async {
        let mut db = connect().await;
        let a = case(&db).await;
        let (job_b, owner_b) = seed_job(&db, &format!("{}-b", a.uuid)).await;
        let mut sealed_b = Sealed::new(&a.uuid, &a.sealed.hash, &a.sealed.plan, 0x70);
        sealed_b.op = format!("pg-op-{}-b", a.uuid);
        sealed_b.generation = format!("pg-gen-{}-b", a.uuid);
        sealed_b.plan = digest(&db, "plan-b", &a.uuid).await;
        let hash = a.sealed.hash.clone();

        let attempt_a = acquire(&mut db, &a.job, &a.owner, 0).await;
        let attempt_b = acquire(&mut db, &job_b, &owner_b, 0).await;
        // Both observe the same absent hash before either writes.
        let obs_a = observe(&db, &hash).await.expect("observe");
        let obs_b = observe(&db, &hash).await.expect("observe");

        let mut client_a = connect().await;
        let mut client_b = connect().await;
        let pids: Vec<i32> = vec![
            client_a
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .quiet("backend pid")
                .get(0),
            client_b
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .quiet("backend pid")
                .get(0),
        ];

        // Deterministic barrier: an uncommitted registry insert for this hash
        // holds both contenders at their conflicting registry insert.
        let blocker = connect().await;
        blocker.batch_execute("BEGIN").await.quiet("begin barrier");
        blocker
            .execute(
                "INSERT INTO posted_file_registry (plaintext_sha256) VALUES ($1)",
                &[&hash],
            )
            .await
            .quiet("hold barrier row");

        let seal_a = a.sealed.clone();
        let task_a =
            tokio::spawn(
                async move { admit(&mut client_a, &attempt_a, &obs_a, &seal_a.mint()).await },
            );
        let task_b = tokio::spawn(async move {
            admit(&mut client_b, &attempt_b, &obs_b, &sealed_b.mint()).await
        });

        let mut polls = 0u32;
        loop {
            let waiting = count(
                &db,
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE pid = ANY($1) AND wait_event_type = 'Lock'",
                &[&pids],
            )
            .await;
            if waiting == 2 {
                break;
            }
            polls += 1;
            assert!(polls < 5000, "contenders never reached the barrier");
            tokio::task::yield_now().await;
        }
        blocker
            .batch_execute("COMMIT")
            .await
            .quiet("release barrier");

        let out_a = task_a.await.expect("contender a task");
        let out_b = task_b.await.expect("contender b task");
        let admitted_count = [&out_a, &out_b]
            .iter()
            .filter(|o| matches!(o, Ok(AdmissionOutcome::Admitted(..))))
            .count();
        let busy_count = [&out_a, &out_b]
            .iter()
            .filter(|o| matches!(o, Ok(AdmissionOutcome::ClaimBusy)))
            .count();
        assert_eq!(admitted_count, 1, "exactly one candidate may be admitted");
        assert_eq!(busy_count, 1, "the competing job must observe a busy claim");

        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&hash]
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_operations \
                 WHERE plaintext_sha256 = $1 AND state = 'pending'",
                &[&hash]
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references r JOIN posted_files f \
                 ON f.id = r.generation_id WHERE f.plaintext_sha256 = $1 AND r.kind = 'in_flight'",
                &[&hash]
            )
            .await,
            1
        );
    });
}

#[test]
fn postgres_stale_observation_is_rejected_before_any_custody_insert() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let stale = observe(&db, &s.hash).await.expect("observe absent hash");

        // Another actor advances the registry after the observation.
        db.execute(
            "INSERT INTO posted_file_registry (plaintext_sha256, revision) VALUES ($1, 3)",
            &[&s.hash],
        )
        .await
        .quiet("advance registry");
        assert!(matches!(
            admit(&mut db, &attempt, &stale, &s.mint()).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(custody_rows(&db, s).await, 0);
        assert_eq!(operation_rows(&db, s).await, 0);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry WHERE plaintext_sha256 = $1 \
                 AND revision = 3 AND pending_operation IS NULL AND selected_generation IS NULL",
                &[&s.hash]
            )
            .await,
            1,
            "a stale admission must leave the registry untouched"
        );

        // A fresh observation of the exact current value is accepted (CAS).
        admitted(&mut db, &attempt, s).await;
        let advanced = db
            .query_one(
                "SELECT r.revision, o.registry_revision FROM posted_file_registry r \
                 JOIN posted_file_operations o ON o.id = r.pending_operation \
                 WHERE r.plaintext_sha256 = $1",
                &[&s.hash],
            )
            .await
            .quiet("read advanced registry");
        assert_eq!(advanced.get::<_, i64>(0), 4);
        assert_eq!(advanced.get::<_, i64>(1), 4);
    });
}

#[test]
fn postgres_attempt_lease_owner_and_status_fence_every_mutation() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;

        // Acquisition preconditions.
        let missing = format!("pg-job-missing-{}", c.uuid);
        let wrong_owner = format!("pg-owner-other-{}", c.uuid);
        for (job, owner, expected) in [
            (missing.as_str(), c.owner.as_str(), 0),
            (c.job.as_str(), wrong_owner.as_str(), 0),
            (c.job.as_str(), c.owner.as_str(), 1),
        ] {
            assert!(matches!(
                acquire_attempt(&mut db, job, owner, expected).await,
                Ok(AttemptOutcome::StaleOrMissing)
            ));
        }
        let first = acquire(&mut db, &c.job, &c.owner, 0).await;
        // Active attempt: busy for the observed value, stale for any other.
        assert!(matches!(
            acquire_attempt(&mut db, &c.job, &c.owner, 1).await,
            Ok(AttemptOutcome::Busy)
        ));
        assert!(matches!(
            acquire_attempt(&mut db, &c.job, &c.owner, 0).await,
            Ok(AttemptOutcome::StaleOrMissing)
        ));

        // Expired lease: old handle cannot write; only the exact observed
        // attempt can be advanced, and advancing rotates the token.
        db.execute(
            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
            &[&c.job],
        )
        .await
        .quiet("expire attempt");
        assert!(matches!(
            try_admit(&mut db, &first, s).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(custody_rows(&db, s).await, 0);
        assert!(matches!(
            acquire_attempt(&mut db, &c.job, &c.owner, 0).await,
            Ok(AttemptOutcome::StaleOrMissing)
        ));
        let second = acquire(&mut db, &c.job, &c.owner, 1).await;
        assert!(matches!(
            try_admit(&mut db, &first, s).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(
            count(
                &db,
                "SELECT attempt FROM posted_file_job_attempts WHERE job_id = $1",
                &[&c.job]
            )
            .await,
            2
        );
        admitted(&mut db, &second, s).await;

        // Takeover is not implemented: an expired owner, or a newer attempt,
        // cannot recover or resume an operation it does not hold.
        db.execute(
            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
            &[&c.job],
        )
        .await
        .quiet("expire attempt");
        db.execute(
            "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
            &[&s.op],
        )
        .await
        .quiet("expire operation");
        assert!(matches!(
            recover(&mut db, &second, &s.hash, &s.op).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        let third = acquire(&mut db, &c.job, &c.owner, 2).await;
        assert!(matches!(
            recover(&mut db, &third, &s.hash, &s.op).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(custody_rows(&db, s).await, 1);

        // Owner and status fences on a second job.
        let d = case(&db).await;
        let other_owner = seed_user(&db, &format!("{}-o", d.uuid)).await;
        let attempt = acquire(&mut db, &d.job, &d.owner, 0).await;
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&d.job, &other_owner],
        )
        .await
        .quiet("move job owner");
        assert!(matches!(
            try_admit(&mut db, &attempt, &d.sealed).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&d.job, &d.owner],
        )
        .await
        .quiet("restore job owner");
        for status in ["queued", "failed", "done"] {
            db.execute(
                "UPDATE ingest_jobs SET status = $2 WHERE id = $1",
                &[&d.job, &status],
            )
            .await
            .quiet("change job status");
            assert!(matches!(
                try_admit(&mut db, &attempt, &d.sealed).await,
                Ok(AdmissionOutcome::StaleOrDiverged)
            ));
            assert!(matches!(
                acquire_attempt(&mut db, &d.job, &d.owner, 1).await,
                Ok(AttemptOutcome::StaleOrMissing)
            ));
        }
        assert_eq!(custody_rows(&db, &d.sealed).await, 0);
        assert_eq!(operation_rows(&db, &d.sealed).await, 0);
    });
}

async fn reject(
    db: &mut Client,
    attempt: &AttemptHandle,
    observation: &Observation,
    label: &'static str,
    minted: CandidateMint<'_>,
) {
    assert!(
        matches!(
            admit(db, attempt, observation, &minted).await,
            Err(StoreError::InvalidInput)
        ),
        "{label}: invalid candidate must fail before insertion"
    );
}

#[test]
fn postgres_invalid_partial_parameters_fail_before_any_insertion() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let obs = observe(&db, &s.hash).await.expect("observe");

        let long_op = "a".repeat(65);
        let short_manifest = [7u8; 15];
        let short_key = [7u8; 47];
        let short_header = [7u8; 23];
        reject(
            &mut db,
            &attempt,
            &obs,
            "empty operation",
            CandidateMint {
                operation_id: "",
                ..s.mint()
            },
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "long operation",
            CandidateMint {
                operation_id: &long_op,
                ..s.mint()
            },
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "spaced operation",
            CandidateMint {
                operation_id: "bad op",
                ..s.mint()
            },
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "bad generation",
            CandidateMint {
                generation_id: "bad/generation",
                ..s.mint()
            },
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "short plan",
            CandidateMint {
                plan_sha256: "abc",
                ..s.mint()
            },
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "nonhex plan",
            CandidateMint {
                plan_sha256: &"z".repeat(64),
                ..s.mint()
            },
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "verified count",
            mint_with(s, |p| p.verified_count = 1),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "post copy",
            mint_with(s, |p| p.post_copy = SpoolCopyState::Present),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "verify copy",
            mint_with(s, |p| p.verify_copy = SpoolCopyState::Verified),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "two spool flag",
            mint_with(s, |p| p.two_spool_verified = true),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "repost flag",
            mint_with(s, |p| p.repost_exhausted = true),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "short manifest",
            mint_with(s, |p| p.manifest_encrypted = &short_manifest),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "short file key",
            mint_with(s, |p| p.file_key_encrypted = &short_key),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "short header nonce",
            mint_with(s, |p| p.header_nonce = &short_header),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "same backbones",
            mint_with(s, |p| p.backbone_verify = "pool-a"),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "kek version",
            mint_with(s, |p| p.file_key_kek_version = 0),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "payload format",
            mint_with(s, |p| p.manifest_payload_format = 2),
        )
        .await;
        reject(
            &mut db,
            &attempt,
            &obs,
            "article count",
            mint_with(s, |p| p.article_count = 0),
        )
        .await;
        // An observation for another hash is never usable for this mint.
        let other_hash = digest(&db, "other-hash", &c.uuid).await;
        let other_obs = observe(&db, &other_hash).await.expect("observe");
        reject(&mut db, &attempt, &other_obs, "observation hash", s.mint()).await;

        // Malformed identities and observations are refused up front too.
        assert!(matches!(
            acquire_attempt(&mut db, "", &c.owner, 0).await,
            Err(StoreError::InvalidInput)
        ));
        assert!(matches!(
            acquire_attempt(&mut db, "bad job", &c.owner, 0).await,
            Err(StoreError::InvalidInput)
        ));
        assert!(matches!(
            acquire_attempt(&mut db, &c.job, &c.owner, -1).await,
            Err(StoreError::InvalidInput)
        ));
        assert!(matches!(
            observe(&db, "short").await,
            Err(StoreError::InvalidInput)
        ));
        assert!(matches!(
            recover(&mut db, &attempt, &s.hash, "bad op").await,
            Err(StoreError::InvalidInput)
        ));

        assert_eq!(custody_rows(&db, s).await, 0);
        assert_eq!(operation_rows(&db, s).await, 0);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry WHERE plaintext_sha256 = $1",
                &[&s.hash]
            )
            .await,
            0
        );
        // The refusals consumed nothing: the same attempt still admits.
        admitted(&mut db, &attempt, s).await;
    });
}

#[test]
fn postgres_managed_custody_is_immutable_and_legacy_id_health_writes_are_rejected() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        admitted(&mut db, &attempt, s).await;

        let fenced = Some("managed generation requires fenced health writes");
        let immutable = Some("posted generation custody is immutable");
        let id = &s.generation;
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET post_copy = 'verified' WHERE id = $1",
                &[id],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            fenced,
            "managed copy-state write",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET verified_count = 1 WHERE id = $1",
                &[id],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            fenced,
            "managed verified-count write",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET two_spool_verified = TRUE WHERE id = $1",
                &[id],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            fenced,
            "managed verification flag write",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET post_verified_at = 1 WHERE id = $1",
                &[id],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            fenced,
            "managed verified timestamp write",
        );
        let replacement = vec![9u8; 32];
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET manifest_encrypted = $2 WHERE id = $1",
                &[id, &replacement],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            immutable,
            "managed manifest rewrite",
        );
        let other_plan = digest(&db, "rewrite-plan", &c.uuid).await;
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET plan_sha256 = $2 WHERE id = $1",
                &[id, &other_plan],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            immutable,
            "managed plan rewrite",
        );
        expect_db_error(
            db.execute("DELETE FROM posted_files WHERE id = $1", &[id])
                .await,
            &SqlState::RAISE_EXCEPTION,
            Some("posted generation custody is retained"),
            "managed custody delete",
        );
        let r = db
            .query_one(
                "SELECT post_copy, verified_count, two_spool_verified, post_verified_at, plan_sha256 \
                 FROM posted_files WHERE id = $1",
                &[id],
            )
            .await
            .quiet("reread managed custody");
        assert_eq!(r.get::<_, String>(0), "unknown");
        assert_eq!(r.get::<_, i32>(1), 0);
        assert!(!r.get::<_, bool>(2));
        assert_eq!(r.get::<_, Option<i64>>(3), None);
        assert_eq!(r.get::<_, String>(4), s.plan);

        // Contrast: an unmanaged legacy row keeps its ID-only health writes,
        // but its custody columns and existence are equally protected.
        let legacy_hash = digest(&db, "legacy", &c.uuid).await;
        let legacy = Sealed::new(&c.uuid, &legacy_hash, &s.plan, 0x55);
        let p = legacy.custody();
        let post_copy = spool_copy_state_db_str(p.post_copy);
        let verify_copy = spool_copy_state_db_str(p.verify_copy);
        let params: [&(dyn ToSql + Sync); 25] = [
            &p.plaintext_sha256,
            &p.backbone_post,
            &p.backbone_verify,
            &p.article_count,
            &p.verified_count,
            &p.original_nzb_sha256,
            &post_copy,
            &verify_copy,
            &p.two_spool_verified,
            &p.repost_exhausted,
            &p.plaintext_size,
            &p.article_payload_size,
            &p.header_nonce,
            &p.file_key_encrypted,
            &p.file_key_kek_version,
            &p.file_key_dek_wrapped_nonce,
            &p.file_key_dek_wrapped,
            &p.file_key_payload_nonce,
            &p.file_key_payload_format,
            &p.manifest_encrypted,
            &p.manifest_kek_version,
            &p.manifest_dek_wrapped_nonce,
            &p.manifest_dek_wrapped,
            &p.manifest_payload_nonce,
            &p.manifest_payload_format,
        ];
        let inserted = db
            .execute(insert_posted_file_sql(), &params)
            .await
            .quiet("insert legacy-shaped row");
        assert_eq!(inserted, 1);
        let legacy_id: String = db
            .query_one(select_posted_file_id_by_sha_sql(), &[&legacy_hash])
            .await
            .quiet("read legacy id")
            .get(0);
        assert_eq!(
            db.execute(
                "UPDATE posted_files SET post_copy = 'present' WHERE id = $1",
                &[&legacy_id]
            )
            .await
            .quiet("legacy health write"),
            1
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET plaintext_size = 5 WHERE id = $1",
                &[&legacy_id],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            immutable,
            "legacy custody rewrite",
        );
        expect_db_error(
            db.execute("DELETE FROM posted_files WHERE id = $1", &[&legacy_id])
                .await,
            &SqlState::RAISE_EXCEPTION,
            Some("posted generation custody is retained"),
            "legacy custody delete",
        );
    });
}

#[test]
fn postgres_schema_replay_preserves_admitted_bundle_and_foreign_keys_reject_wrong_association() {
    run_blocking(true, async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        admitted(&mut db, &attempt, s).await;

        // Wrong generation/hash/operation association is a foreign-key error.
        let other_hash = digest(&db, "other-association", &c.uuid).await;
        db.execute(
            "INSERT INTO posted_file_registry (plaintext_sha256) VALUES ($1)",
            &[&other_hash],
        )
        .await
        .quiet("seed second registry row");
        let owner_ref = format!("pg-ref-{}", c.uuid);
        let fk = &SqlState::FOREIGN_KEY_VIOLATION;
        expect_db_error(
            db.execute(
                "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
                 VALUES ('rollback', $1, $2, $3)",
                &[&owner_ref, &s.generation, &other_hash],
            )
            .await,
            fk,
            None,
            "reference with wrong hash",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_file_registry SET selected_generation = $1 WHERE plaintext_sha256 = $2",
                &[&s.generation, &other_hash],
            )
            .await,
            fk,
            None,
            "selected generation with wrong hash",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_file_registry SET pending_operation = $1 WHERE plaintext_sha256 = $2",
                &[&s.op, &other_hash],
            )
            .await,
            fk,
            None,
            "pending operation with wrong hash",
        );
        let unknown_generation = format!("pg-gen-{}-none", c.uuid);
        expect_db_error(
            db.execute(
                "INSERT INTO posted_file_health (generation_id) VALUES ($1)",
                &[&unknown_generation],
            )
            .await,
            fk,
            None,
            "health for unknown generation",
        );
        // Positive control: the correct association is accepted.
        assert_eq!(
            db.execute(
                "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
                 VALUES ('rollback', $1, $2, $3)",
                &[&owner_ref, &s.generation, &s.hash],
            )
            .await
            .quiet("correct association"),
            1
        );

        let before = snapshot(&db, s).await;
        assert!(before.iter().all(Option::is_some));

        // Replay the exact migration on a separate session, twice.
        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0037)
            .await
            .quiet("replay migration 0037");
        ddl.batch_execute(MIGRATION_0037)
            .await
            .quiet("replay migration 0037 again");

        let after = snapshot(&db, s).await;
        assert!(before == after, "replay changed the admitted bundle");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references \
                 WHERE generation_id = $1 AND kind = 'legacy_pin'",
                &[&s.generation]
            )
            .await,
            0,
            "replay must never pin a managed generation as legacy"
        );
        match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(_, bundle)) => {
                assert!(bundle.custody().expect("stored custody") == s.custody());
            }
            _ => panic!("admitted bundle was not recoverable after replay"),
        }
    });
}
