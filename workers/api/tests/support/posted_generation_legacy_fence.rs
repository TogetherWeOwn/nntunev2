//! Real-PostgreSQL tests for migration 0042: the registered-hash fence on the
//! old ID/hash-scoped `posted_files` health writers.
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Sealed`, `Case`, `connect`, `count`, `digest`,
//! `expect_db_error`, `run_test`, `run_blocking`, `MIGRATION_0037`,
//! `MIGRATION_0042`). Fixtures are synthetic sealed byte buffers keyed by
//! database UUIDs. Failures print only static labels: no connection detail,
//! query text, row data or sealed bytes.
//!
//! Scope: the separate BEFORE UPDATE trigger that rejects changed legacy
//! mutable health columns on UNMANAGED (`generation_operation IS NULL`)
//! rows whose hash exists in `posted_file_registry`. Managed rows stay under
//! the 0037 protector; these tests only require a P0001 refusal there, never
//! the 0042 message. Registration below is plain test SQL (registry, health,
//! `legacy_pin`), a SEEDED ROLLOUT FIXTURE: it is not proof of the real
//! migration backfill or of any drain. Custody columns, SQL and AAD are not
//! touched, the unique hash stays, unregistered legacy rows remain
//! intentionally writable, and nothing here grants topology, selection or
//! replacement authority.

use super::*;

#[path = "posted_generation_registration.rs"]
mod registration;
use nntune_ingest::posted_files::{touch_post_verified_sql, touch_retention_check_sql};
use nntune_ingest::two_spool::{mark_repost_exhausted_sql, mark_two_spool_verified_sql};

/// Static message raised by the 0042 trigger.
const FENCE_MESSAGE: &str = "registered generation requires fenced health writes";

/// The seven legacy mutable health columns, each with a value that differs
/// from the fixture default, as (column, by-id SQL, by-hash SQL). Binds: `$1`
/// is the id or the hash respectively.
pub(super) const FIELD_UPDATES: [(&str, &str, &str); 7] = [
    (
        "post_copy",
        "UPDATE posted_files SET post_copy = 'present' WHERE id = $1",
        "UPDATE posted_files SET post_copy = 'present' WHERE plaintext_sha256 = $1",
    ),
    (
        "verify_copy",
        "UPDATE posted_files SET verify_copy = 'present' WHERE id = $1",
        "UPDATE posted_files SET verify_copy = 'present' WHERE plaintext_sha256 = $1",
    ),
    (
        "verified_count",
        "UPDATE posted_files SET verified_count = 1 WHERE id = $1",
        "UPDATE posted_files SET verified_count = 1 WHERE plaintext_sha256 = $1",
    ),
    (
        "two_spool_verified",
        "UPDATE posted_files SET two_spool_verified = TRUE WHERE id = $1",
        "UPDATE posted_files SET two_spool_verified = TRUE WHERE plaintext_sha256 = $1",
    ),
    (
        "repost_exhausted",
        "UPDATE posted_files SET repost_exhausted = TRUE WHERE id = $1",
        "UPDATE posted_files SET repost_exhausted = TRUE WHERE plaintext_sha256 = $1",
    ),
    (
        "post_verified_at",
        "UPDATE posted_files SET post_verified_at = 1700000000 WHERE id = $1",
        "UPDATE posted_files SET post_verified_at = 1700000000 WHERE plaintext_sha256 = $1",
    ),
    (
        "last_retention_check",
        "UPDATE posted_files SET last_retention_check = 1700000000 WHERE id = $1",
        "UPDATE posted_files SET last_retention_check = 1700000000 WHERE plaintext_sha256 = $1",
    ),
];

/// Synthetic unmanaged legacy row inserted with the real full-row insert SQL.
/// Returns the DB-owned id and the sealed fixture (its `generation` field is
/// a label only; the legacy id is the returned value).
pub(super) async fn legacy_fixture(db: &Client, fill: u8) -> (String, Sealed) {
    let uuid = db_uuid(db).await;
    let hash = digest(db, "legacy-fence-content", &uuid).await;
    let plan = digest(db, "legacy-fence-plan", &uuid).await;
    let sealed = Sealed::new(&uuid, &hash, &plan, fill);
    assert_eq!(
        insert_legacy(db, &sealed)
            .await
            .quiet("insert legacy fence fixture"),
        1
    );
    let id: String = db
        .query_one(select_posted_file_id_by_sha_sql(), &[&sealed.hash])
        .await
        .quiet("read legacy fence fixture id")
        .get(0);
    (id, sealed)
}

pub(super) async fn insert_legacy(
    db: &Client,
    sealed: &Sealed,
) -> Result<u64, tokio_postgres::Error> {
    let p = sealed.custody();
    let post_copy = spool_copy_state_db_str(p.post_copy);
    let verify_copy = spool_copy_state_db_str(p.verify_copy);
    let params: [&(dyn ToSql + Sync); 28] = [
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
        &p.media.legacy_nonce(),
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
        &p.media.format(),
        &p.media.key_mode(),
        &p.media.header(),
    ];
    db.execute(insert_posted_file_sql(), &params).await
}

/// Seeded rollout fixture: plain test SQL standing in for registration.
async fn register(db: &Client, id: &str, hash: &str) {
    db.execute(
        "INSERT INTO posted_file_registry (plaintext_sha256, selected_generation) \
         VALUES ($1, $2) ON CONFLICT (plaintext_sha256) DO NOTHING",
        &[&hash, &id],
    )
    .await
    .quiet("seed fixture registry");
    db.execute(
        "INSERT INTO posted_file_health (generation_id) VALUES ($1) \
         ON CONFLICT (generation_id) DO NOTHING",
        &[&id],
    )
    .await
    .quiet("seed fixture health");
    db.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         VALUES ('legacy_pin', $1, $1, $2) ON CONFLICT DO NOTHING",
        &[&id, &hash],
    )
    .await
    .quiet("seed fixture legacy pin");
}

async fn registered_legacy(db: &Client, fill: u8) -> (String, Sealed) {
    let (id, sealed) = legacy_fixture(db, fill).await;
    register(db, &id, &sealed.hash).await;
    (id, sealed)
}

/// Opaque digest of the stored legacy row (never printed).
async fn row_digest(db: &Client, id: &str) -> Option<String> {
    db.query_one(
        "SELECT md5(t::text) FROM posted_files t WHERE t.id = $1",
        &[&id],
    )
    .await
    .quiet("digest legacy row")
    .get(0)
}

async fn legacy_snapshot(db: &Client, id: &str, hash: &str) -> Vec<Option<String>> {
    let row = db
        .query_one(
            "SELECT (SELECT md5(t::text) FROM posted_files t WHERE t.id = $1), \
             (SELECT md5(t::text) FROM posted_file_registry t WHERE t.plaintext_sha256 = $2), \
             (SELECT md5(t::text) FROM posted_file_health t WHERE t.generation_id = $1), \
             (SELECT md5(string_agg(t::text, ',' ORDER BY t.kind, t.owner_id)) \
                FROM posted_file_references t WHERE t.generation_id = $1)",
            &[&id, &hash],
        )
        .await
        .quiet("snapshot legacy bundle");
    (0..4).map(|i| row.get(i)).collect()
}

async fn expect_fenced(result: Result<u64, tokio_postgres::Error>, ctx: &'static str) {
    expect_db_error(result, &SqlState::RAISE_EXCEPTION, Some(FENCE_MESSAGE), ctx);
}

/// True when an old-style health write is refused with the 0042 error; false
/// when it was accepted. Any other failure is a test failure.
async fn is_fenced(db: &Client, id: &str) -> bool {
    match db
        .execute(
            "UPDATE posted_files SET last_retention_check = 1700000001 WHERE id = $1",
            &[&id],
        )
        .await
    {
        Ok(_) => false,
        Err(e) => {
            let Some(d) = e.as_db_error() else {
                panic!("fence probe: not a database error")
            };
            assert!(
                d.code() == &SqlState::RAISE_EXCEPTION,
                "fence probe: SQLSTATE"
            );
            d.message() == FENCE_MESSAGE
        }
    }
}

#[test]
fn postgres_legacy_fence_refuses_each_mutable_health_field_by_id_and_hash() {
    run_test(async {
        let db = connect().await;
        let (id, s) = registered_legacy(&db, 0x61).await;
        let before = legacy_snapshot(&db, &id, &s.hash).await;
        assert!(before.iter().all(Option::is_some));

        for (_column, by_id, by_hash) in FIELD_UPDATES {
            expect_fenced(
                db.execute(by_id, &[&id]).await,
                "registered by-id health write",
            )
            .await;
            expect_fenced(
                db.execute(by_hash, &[&s.hash]).await,
                "registered by-hash health write",
            )
            .await;
            assert!(
                legacy_snapshot(&db, &id, &s.hash).await == before,
                "a refused legacy health write must leave the bundle unchanged"
            );
        }
        // Several columns in one statement are refused as well.
        expect_fenced(
            db.execute(
                "UPDATE posted_files SET post_copy = 'present', verified_count = 2 \
                 WHERE id = $1",
                &[&id],
            )
            .await,
            "registered multi-column health write",
        )
        .await;
        assert!(legacy_snapshot(&db, &id, &s.hash).await == before);
    });
}

#[test]
fn postgres_legacy_fence_refuses_the_actual_old_sql_helpers() {
    run_test(async {
        let db = connect().await;
        let (id, s) = registered_legacy(&db, 0x62).await;
        let before = legacy_snapshot(&db, &id, &s.hash).await;
        let at: i64 = 1_700_000_000;
        let count_value: i32 = 3;

        expect_fenced(
            db.execute(touch_post_verified_sql(), &[&id, &at]).await,
            "old post-verified touch",
        )
        .await;
        expect_fenced(
            db.execute(touch_retention_check_sql(), &[&id, &at]).await,
            "old retention touch",
        )
        .await;
        expect_fenced(
            db.execute(mark_two_spool_verified_sql(), &[&count_value, &s.hash])
                .await,
            "old two-spool verified mark",
        )
        .await;
        expect_fenced(
            db.execute(mark_repost_exhausted_sql(), &[&count_value, &s.hash])
                .await,
            "old repost-exhausted mark",
        )
        .await;
        assert!(
            legacy_snapshot(&db, &id, &s.hash).await == before,
            "refused old helpers must leave the bundle unchanged"
        );
    });
}

#[test]
fn postgres_legacy_fence_allows_registered_no_op_updates() {
    run_test(async {
        let db = connect().await;
        let (id, s) = registered_legacy(&db, 0x63).await;
        let before = legacy_snapshot(&db, &id, &s.hash).await;

        for sql in [
            "UPDATE posted_files SET post_copy = post_copy, verify_copy = verify_copy, \
             verified_count = verified_count, two_spool_verified = two_spool_verified, \
             repost_exhausted = repost_exhausted, post_verified_at = post_verified_at, \
             last_retention_check = last_retention_check WHERE id = $1",
            "UPDATE posted_files SET post_copy = 'unknown', verified_count = 0, \
             two_spool_verified = FALSE, post_verified_at = NULL WHERE id = $1",
        ] {
            assert_eq!(
                db.execute(sql, &[&id])
                    .await
                    .quiet("registered no-op by id"),
                1
            );
        }
        assert_eq!(
            db.execute(
                "UPDATE posted_files SET verify_copy = 'unknown' WHERE plaintext_sha256 = $1",
                &[&s.hash]
            )
            .await
            .quiet("registered no-op by hash"),
            1
        );
        assert!(
            legacy_snapshot(&db, &id, &s.hash).await == before,
            "no-op updates must not change the bundle"
        );
        // Still fenced afterwards.
        assert!(is_fenced(&db, &id).await);
    });
}

#[test]
fn postgres_legacy_fence_keeps_unregistered_compatibility_until_registered() {
    run_test(async {
        let db = connect().await;
        let (id, s) = legacy_fixture(&db, 0x64).await;
        let at: i64 = 1_700_000_000;
        let count_value: i32 = 3;

        // Unregistered synthetic row: the old writers keep working.
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry WHERE plaintext_sha256 = $1",
                &[&s.hash]
            )
            .await,
            0,
            "fixture must start unregistered"
        );
        assert_eq!(
            db.execute(
                "UPDATE posted_files SET post_copy = 'present' WHERE id = $1",
                &[&id]
            )
            .await
            .quiet("unregistered by-id write"),
            1
        );
        assert_eq!(
            db.execute(touch_post_verified_sql(), &[&id, &at])
                .await
                .quiet("unregistered post-verified touch"),
            1
        );
        assert_eq!(
            db.execute(mark_two_spool_verified_sql(), &[&count_value, &s.hash])
                .await
                .quiet("unregistered verified mark"),
            1
        );
        let after_legacy = row_digest(&db, &id).await;

        // The verification touch also set last_retention_check to `at`.
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE id = $1 \
                 AND post_verified_at = $2 AND last_retention_check = $2",
                &[&id, &at],
            )
            .await,
            1,
            "legacy verification touch must seed both timestamps"
        );

        // Seeded registration preserves true no-ops, but refuses changes.
        register(&db, &id, &s.hash).await;
        assert!(row_digest(&db, &id).await == after_legacy);
        let registered = legacy_snapshot(&db, &id, &s.hash).await;
        assert!(registered.iter().all(Option::is_some));
        for sql in [touch_post_verified_sql(), touch_retention_check_sql()] {
            assert_eq!(
                db.execute(sql, &[&id, &at])
                    .await
                    .quiet("newly registered no-op timestamp helper"),
                1
            );
        }
        assert!(legacy_snapshot(&db, &id, &s.hash).await == registered);
        assert!(is_fenced(&db, &id).await);
        let later = at + 1;
        expect_fenced(
            db.execute(touch_retention_check_sql(), &[&id, &later])
                .await,
            "newly registered changed retention touch",
        )
        .await;
        expect_fenced(
            db.execute(mark_repost_exhausted_sql(), &[&count_value, &s.hash])
                .await,
            "newly registered exhausted mark",
        )
        .await;
        assert!(
            legacy_snapshot(&db, &id, &s.hash).await == registered,
            "refused writes after registration must not change the bundle"
        );
    });
}

#[test]
fn postgres_legacy_fence_bulk_update_rolls_back_registered_and_unregistered() {
    run_test(async {
        let db = connect().await;
        let (reg_id, reg) = registered_legacy(&db, 0x65).await;
        let (open_id, open) = legacy_fixture(&db, 0x66).await;
        let reg_before = row_digest(&db, &reg_id).await;
        let open_before = row_digest(&db, &open_id).await;
        let ids = vec![open_id.clone(), reg_id.clone()];
        let hashes = vec![open.hash.clone(), reg.hash.clone()];

        expect_fenced(
            db.execute(
                "UPDATE posted_files SET last_retention_check = 1700000000 WHERE id = ANY($1)",
                &[&ids],
            )
            .await,
            "bulk by-id update",
        )
        .await;
        expect_fenced(
            db.execute(
                "UPDATE posted_files SET post_copy = 'present' \
                 WHERE plaintext_sha256 = ANY($1)",
                &[&hashes],
            )
            .await,
            "bulk by-hash update",
        )
        .await;
        assert!(row_digest(&db, &reg_id).await == reg_before);
        assert!(
            row_digest(&db, &open_id).await == open_before,
            "the unregistered row must roll back with the refused statement"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE id = $1 \
                 AND post_copy = 'unknown' AND last_retention_check IS NULL",
                &[&open_id]
            )
            .await,
            1
        );
    });
}

#[test]
fn postgres_legacy_fence_survives_0042_replay_with_bundle_unchanged() {
    run_blocking(true, async {
        let db = connect().await;
        let (id, s) = registered_legacy(&db, 0x67).await;
        assert!(is_fenced(&db, &id).await);
        let before = legacy_snapshot(&db, &id, &s.hash).await;
        assert!(before.iter().all(Option::is_some));

        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0042)
            .await
            .quiet("replay migration 0042");
        ddl.batch_execute(MIGRATION_0042)
            .await
            .quiet("replay migration 0042 again");

        assert!(
            legacy_snapshot(&db, &id, &s.hash).await == before,
            "0042 replay changed identity, custody, health or references"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references \
                 WHERE generation_id = $1 AND kind = 'legacy_pin'",
                &[&id]
            )
            .await,
            1,
            "0042 replay must not add or drop legacy pins"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry \
                 WHERE plaintext_sha256 = $1 AND selected_generation = $2",
                &[&s.hash, &id]
            )
            .await,
            1
        );
        assert!(is_fenced(&db, &id).await, "fence lost after 0042 replay");
        // Exercise the constraint, not merely the current one-row count.
        expect_db_error(
            db.execute(
                "INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count) \
                 SELECT plaintext_sha256, backbone_post, backbone_verify, article_count \
                 FROM posted_files WHERE id = $1",
                &[&id],
            )
            .await,
            &SqlState::UNIQUE_VIOLATION,
            None,
            "duplicate legacy hash after replay",
        );
        assert_eq!(custody_rows(&db, &s).await, 1);
    });
}

#[test]
fn postgres_legacy_fence_survives_exclusive_historical_0037_replay() {
    run_blocking(true, async {
        let db = connect().await;
        let (id, s) = registered_legacy(&db, 0x68).await;
        assert!(
            is_fenced(&db, &id).await,
            "fence missing before 0037 replay"
        );
        let before = legacy_snapshot(&db, &id, &s.hash).await;
        let (other_id, other) = legacy_fixture(&db, 0x69).await;
        let at: i64 = 1_700_000_000;
        db.execute(touch_post_verified_sql(), &[&other_id, &at])
            .await
            .quiet("legacy-shaped pre-backfill timestamp");
        let other_before = row_digest(&db, &other_id).await;

        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0037)
            .await
            .quiet("replay historical migration 0037");

        // Legacy-shaped rows after schema apply plus historical replay, not an
        // actual old-version upgrade/restore or a deployed-writer drain.
        assert!(
            is_fenced(&db, &id).await,
            "historical replay weakened fence"
        );
        assert!(legacy_snapshot(&db, &id, &s.hash).await == before);
        assert!(is_fenced(&db, &other_id).await);
        assert!(row_digest(&db, &other_id).await == other_before);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_health WHERE generation_id = $1 \
                 AND state = 'unknown' AND verified_at IS NULL AND retention_checked_at IS NULL",
                &[&other_id],
            )
            .await,
            1,
            "backfill must not promote an old timestamp to health evidence"
        );
        assert!(legacy_snapshot(&db, &other_id, &other.hash)
            .await
            .iter()
            .all(Option::is_some));
    });
}

#[test]
fn postgres_legacy_fence_leaves_new_managed_candidate_on_generation_api() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = admitted(&mut db, &attempt, s).await;
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("current posting fence"));
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
        let before = snapshot(&db, s).await;
        assert!(before.iter().all(Option::is_some));

        // The managed row stays under the 0037 protector: only a P0001
        // refusal is required, never the 0042 message.
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET post_copy = 'present' WHERE id = $1",
                &[&s.generation],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            None,
            "managed by-id health write",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_files SET verified_count = 1 WHERE plaintext_sha256 = $1",
                &[&s.hash],
            )
            .await,
            &SqlState::RAISE_EXCEPTION,
            None,
            "managed by-hash health write",
        );
        assert!(
            snapshot(&db, s).await == before,
            "refused legacy writes must not touch a managed bundle"
        );
    });
}
