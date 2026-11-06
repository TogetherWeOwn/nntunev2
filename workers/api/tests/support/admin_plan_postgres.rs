//! Real private admission + concrete API sealed-plan persistence. Synthetic
//! opaque payloads only; no HTTP/planner, actual commit-error or deployed proof.

use super::{admin_ingest_upload_on, attempt, connect, owner, run, stored, Quiet};
use crate::admin_ingest::AdminIngestAttempt;
use crate::ingest_plan::{
    freeze_plan, read_plan, OpenedPlan, PlanError, PlanMode, PlanRequest,
    MAX_PREPARATION_PLAN_BYTES,
};
use crate::rotation::KekSource;
use tokio_postgres::Client;
use zeroize::Zeroizing;

const MODE: PlanMode = PlanMode::PreparationOnly;

struct Keys(Zeroizing<[u8; 32]>);
impl Keys {
    fn new() -> Self {
        Self(Zeroizing::new(
            crate::random::secure_random_bytes::<32>().expect("key fixture randomness"),
        ))
    }
}
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        (version == 1).then_some(&*self.0)
    }
}

fn ok<T>(result: Result<T, PlanError>) -> T {
    result.unwrap_or_else(|_| panic!("sealed-plan adapter error"))
}
fn present(value: Option<OpenedPlan>) -> OpenedPlan {
    value.unwrap_or_else(|| panic!("expected admitted plan"))
}
fn request<'a>(input: &'a AdminIngestAttempt, operation: &'a str) -> PlanRequest<'a> {
    PlanRequest {
        owner_id: &input.actor_user_id,
        job_id: &input.job_id,
        operation_key: operation,
        file_index: 0,
        original_sha256: &input.content_sha256_hex,
        original_bytes: input.nzb_bytes as i64,
        file_count: input.file_count.expect("fixture file count") as i32,
    }
}
async fn fixture() -> (Client, AdminIngestAttempt, Keys) {
    let mut db = connect().await;
    let actor = owner(&db).await;
    let input = attempt(&actor);
    stored(admin_ingest_upload_on(&mut db, &input).await, false);
    (db, input, Keys::new())
}
async fn snapshot(db: &Client, job: &str) -> String {
    db.query_one(
        "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(j) FROM ingest_jobs j WHERE j.id = $1), \
         (SELECT to_jsonb(o) FROM ingest_originals o WHERE o.job_id = $1), \
         (SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM ingest_preparation_plans p WHERE p.job_id = $1), \
         (SELECT jsonb_agg(to_jsonb(d) ORDER BY id) FROM ingest_file_dispatches d WHERE d.job_id = $1), \
         (SELECT to_jsonb(a) FROM posted_file_job_attempts a WHERE a.job_id = $1))::text)",
        &[&job],
    ).await.quiet("snapshot plan custody").get(0)
}
fn payload() -> Zeroizing<Vec<u8>> {
    Zeroizing::new(
        crate::random::secure_random_bytes::<79>()
            .expect("plan fixture randomness")
            .to_vec(),
    )
}

#[test]
fn postgres_sealed_plan_restart_replays_exact_envelope_and_prior_key() {
    run(async {
        let (mut db, input, keys) = fixture().await;
        let bytes = payload();
        let input_plan = request(&input, "plan-one");
        let first = present(ok(freeze_plan(
            &mut db,
            MODE,
            &keys,
            1,
            &input_plan,
            &bytes,
        )
        .await));
        assert!(first.bytes() == bytes.as_slice());
        assert_eq!(format!("{first:?}"), "OpenedPlan([REDACTED])");
        let id = first.id().to_owned();
        let version = first.version().to_owned();
        let before = snapshot(&db, &input.job_id).await;
        drop(first);
        drop(db);
        let mut db = connect().await;
        // Requested key 2 is absent, but replay must use the existing version-1
        // envelope, not reseal or require the newly selected minting key.
        let replay = present(ok(freeze_plan(
            &mut db,
            MODE,
            &keys,
            2,
            &input_plan,
            &bytes,
        )
        .await));
        assert!(
            replay.id() == id && replay.version() == version && replay.bytes() == bytes.as_slice()
        );
        let read = present(ok(read_plan(
            &mut db,
            MODE,
            &keys,
            &input_plan,
            &id,
            &version,
        )
        .await));
        assert!(read.bytes() == bytes.as_slice());
        assert!(before == snapshot(&db, &input.job_id).await);
        // Only ciphertext and bounded scalar metadata survive in the DB.
        let row = db
            .query_one(
                "SELECT encrypted, plaintext_bytes FROM ingest_preparation_plans WHERE id = $1",
                &[&id],
            )
            .await
            .quiet("inspect encrypted plan shape");
        assert!(row.get::<_, Vec<u8>>(0).as_slice() != bytes.as_slice());
        assert_eq!(row.get::<_, i32>(1), bytes.len() as i32);
        // Compose the concrete opened plan with the existing real DB outbox.
        // Tagged bytes remain synthetic here; this is not a live producer.
        use nntune_ingest::posted_generations::dispatch::{
            prepare_file_dispatch, DispatchMode, DispatchObject, PreparedDispatchFile,
        };
        let file = PreparedDispatchFile::from_bytes(
            0,
            DispatchObject::new("synthetic-tagged", "version-one").expect("file identity"),
            &bytes,
            DispatchObject::new(read.id(), read.version()).expect("plan identity"),
            read.bytes(),
        )
        .expect("byte-derived preparation");
        assert!(super::ok(
            prepare_file_dispatch(
                &mut db,
                DispatchMode::PreparationOnly,
                &input.job_id,
                "plan-one",
                0,
                &file,
            )
            .await
        )
        .is_some());
        let dispatched = db.query_one(
            "SELECT plan_id, plan_version, plan_bytes, plan_sha256 FROM ingest_file_dispatches WHERE job_id = $1",
            &[&input.job_id],
        ).await.quiet("inspect frozen dispatch binding");
        assert!(dispatched.get::<_, String>(0) == read.id());
        assert!(dispatched.get::<_, String>(1) == read.version());
        assert_eq!(dispatched.get::<_, i64>(2), read.bytes().len() as i64);
        assert!(
            dispatched.get::<_, String>(3) == crate::admin_ingest::content_sha256_hex(read.bytes())
        );
    });
}

#[test]
fn postgres_sealed_plan_disabled_invalid_and_unadmitted_write_nothing() {
    run(async {
        let (mut db, input, keys) = fixture().await;
        let bytes = payload();
        let mut plan = request(&input, "plan-one");
        let before = snapshot(&db, &input.job_id).await;
        assert!(
            ok(freeze_plan(&mut db, PlanMode::Disabled, &keys, 1, &plan, &bytes).await).is_none()
        );
        assert!(ok(read_plan(&mut db, PlanMode::Disabled, &keys, &plan, "", "").await).is_none());
        assert!(matches!(
            freeze_plan(&mut db, MODE, &keys, 1, &plan, &[]).await,
            Err(PlanError::Invalid)
        ));
        let oversized = Zeroizing::new(vec![0_u8; MAX_PREPARATION_PLAN_BYTES + 1]);
        assert!(matches!(
            freeze_plan(&mut db, MODE, &keys, 1, &plan, &oversized).await,
            Err(PlanError::Invalid)
        ));
        plan.file_index = -1;
        assert!(matches!(
            freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await,
            Err(PlanError::Invalid)
        ));
        plan.file_index = 0;
        plan.job_id = "absent-plan-job";
        assert!(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await).is_none());
        assert!(before == snapshot(&db, &input.job_id).await);
    });
}

#[test]
fn postgres_sealed_plan_divergent_replay_provenance_and_version_refuse() {
    run(async {
        let (mut db, input, keys) = fixture().await;
        let bytes = payload();
        let mut plan = request(&input, "plan-one");
        let frozen = present(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await));
        let before = snapshot(&db, &input.job_id).await;
        let mut changed = Zeroizing::new(bytes.to_vec());
        changed[0] ^= 1;
        assert!(matches!(
            freeze_plan(&mut db, MODE, &keys, 1, &plan, &changed).await,
            Err(PlanError::Conflict)
        ));
        assert!(
            ok(read_plan(&mut db, MODE, &keys, &plan, frozen.id(), "wrong-version").await)
                .is_none()
        );
        assert!(matches!(
            read_plan(&mut db, MODE, &keys, &plan, "wrong-id", frozen.version()).await,
            Err(PlanError::Invalid)
        ));
        plan.original_bytes += 1;
        assert!(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await).is_none());
        assert!(
            ok(read_plan(&mut db, MODE, &keys, &plan, frozen.id(), frozen.version()).await)
                .is_none()
        );
        plan.original_bytes -= 1;
        plan.owner_id = "another-owner";
        assert!(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await).is_none());
        assert!(before == snapshot(&db, &input.job_id).await);
    });
}

#[test]
fn postgres_sealed_plan_current_admission_fences_both_entries() {
    run(async {
        for mutation in [
            "UPDATE users SET role = 'member' WHERE id = $1",
            "UPDATE users SET deleting_at = to_timestamp(1700000001) WHERE id = $1",
            "UPDATE ingest_jobs SET status = 'failed', failure_reason = 'empty' WHERE user_id = $1",
            "UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1 WHERE job_id IN (SELECT id FROM ingest_jobs WHERE user_id = $1)",
        ] {
            let (mut db, input, keys) = fixture().await;
            let bytes = payload();
            let plan = request(&input, "plan-one");
            let frozen = present(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await));
            assert_eq!(
                db.execute(mutation, &[&input.actor_user_id])
                    .await
                    .quiet("change current admission fence"),
                1,
                "each control must change exactly one admitted row"
            );
            let before = snapshot(&db, &input.job_id).await;
            assert!(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await).is_none());
            assert!(ok(read_plan(&mut db, MODE, &keys, &plan, frozen.id(), frozen.version()).await).is_none());
            assert!(before == snapshot(&db, &input.job_id).await);
        }
    });
}

#[test]
fn postgres_sealed_plan_concurrent_writers_converge_without_resealing() {
    run(async {
        let (mut db, input, keys) = fixture().await;
        let mut other = connect().await;
        let bytes = payload();
        let plan = request(&input, "plan-one");
        let (a, b) = tokio::join!(
            freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes),
            freeze_plan(&mut other, MODE, &keys, 1, &plan, &bytes),
        );
        let a = present(ok(a));
        let b = present(ok(b));
        assert!(a.id() == b.id() && a.version() == b.version() && a.bytes() == b.bytes());
        let second = request(&input, "plan-two");
        let changed = payload();
        let (a, b) = tokio::join!(
            freeze_plan(&mut db, MODE, &keys, 1, &second, &bytes),
            freeze_plan(&mut other, MODE, &keys, 1, &second, &changed),
        );
        assert!(matches!(
            (&a, &b),
            (Ok(Some(_)), Err(PlanError::Conflict)) | (Err(PlanError::Conflict), Ok(Some(_)))
        ));
        let row = db
            .query_one(
                "SELECT count(*) FROM ingest_preparation_plans WHERE job_id = $1",
                &[&input.job_id],
            )
            .await
            .quiet("count frozen plans");
        assert_eq!(row.get::<_, i64>(0), 2);
    });
}

#[test]
fn postgres_sealed_plan_immutability_and_migration_replay_preserve_custody() {
    run(async {
        let (mut db, input, keys) = fixture().await;
        let bytes = payload();
        let plan = request(&input, "plan-one");
        let frozen = present(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await));
        let before = snapshot(&db, &input.job_id).await;
        for sql in [
            "DELETE FROM ingest_preparation_plans WHERE id = $1",
            "UPDATE ingest_preparation_plans SET encrypted = encrypted || decode('00','hex') WHERE id = $1",
            "UPDATE ingest_preparation_plans SET version = repeat('0',32) WHERE id = $1",
            "UPDATE ingest_preparation_plans SET owner_id = 'changed' WHERE id = $1",
        ] {
            let error = db.execute(sql, &[&frozen.id()]).await.err().expect("immutable plan mutation must fail");
            assert!(error.as_db_error().is_some_and(|e| e.code().code() == "23514"));
            assert!(before == snapshot(&db, &input.job_id).await);
        }
        db.execute(
            "UPDATE ingest_preparation_plans SET version = version WHERE id = $1",
            &[&frozen.id()],
        )
        .await
        .quiet("allow true no-op plan update");
        db.batch_execute(include_str!(
            "../../../../migrations/0048_ingest_preparation_plans.sql"
        ))
        .await
        .quiet("replay plan migration");
        let read = present(ok(read_plan(
            &mut db,
            MODE,
            &keys,
            &plan,
            frozen.id(),
            frozen.version(),
        )
        .await));
        assert!(read.bytes() == bytes.as_slice() && before == snapshot(&db, &input.job_id).await);
    });
}

#[test]
fn postgres_sealed_plan_unknown_wrong_key_and_cross_row_envelopes_fail_closed() {
    run(async {
        let (mut db, input, keys) = fixture().await;
        let bytes = payload();
        let plan = request(&input, "plan-one");
        let frozen = present(ok(freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await));
        let other_keys = Keys::new();
        assert!(matches!(
            read_plan(
                &mut db,
                MODE,
                &other_keys,
                &plan,
                frozen.id(),
                frozen.version()
            )
            .await,
            Err(PlanError::Crypto)
        ));
        struct Empty;
        impl KekSource for Empty {
            fn kek(&self, _: u32) -> Option<&[u8; 32]> {
                None
            }
        }
        assert!(matches!(
            read_plan(&mut db, MODE, &Empty, &plan, frozen.id(), frozen.version()).await,
            Err(PlanError::UnknownKey)
        ));
        let other_plan = request(&input, "plan-two");
        let other = present(ok(freeze_plan(
            &mut db,
            MODE,
            &keys,
            1,
            &other_plan,
            &bytes,
        )
        .await));
        // Disposable superuser corruption probe, not a supported mutation API.
        // Only this session bypasses the immutable trigger; immediately restore
        // normal enforcement before calling the real reader.
        db.batch_execute("SET session_replication_role = replica")
            .await
            .quiet("begin corruption probe");
        db.execute(
            "UPDATE ingest_preparation_plans dst SET dek_wrapped_nonce = src.dek_wrapped_nonce, \
             dek_wrapped = src.dek_wrapped, payload_nonce = src.payload_nonce, encrypted = src.encrypted \
             FROM ingest_preparation_plans src WHERE dst.id = $1 AND src.id = $2",
            &[&frozen.id(), &other.id()],
        ).await.quiet("substitute bounded encrypted row");
        db.batch_execute("SET session_replication_role = origin")
            .await
            .quiet("end corruption probe");
        let before = snapshot(&db, &input.job_id).await;
        assert!(matches!(
            read_plan(&mut db, MODE, &keys, &plan, frozen.id(), frozen.version()).await,
            Err(PlanError::Crypto)
        ));
        assert!(matches!(
            freeze_plan(&mut db, MODE, &keys, 1, &plan, &bytes).await,
            Err(PlanError::Crypto)
        ));
        assert!(before == snapshot(&db, &input.job_id).await);
    });
}
