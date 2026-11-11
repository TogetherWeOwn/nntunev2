//! Real-PostgreSQL / fake-Queue tests for the posting-disabled preparation and
//! outbox adapter `nntune_ingest::posted_generations::dispatch` (migration 0047).
//!
//! Included as a submodule of `posted_generations_postgres.rs` and reuses its
//! harness. Fixtures are synthetic: arbitrary fill-pattern byte buffers behind
//! opaque `DispatchObject` ids and versions, never NZB bytes, article
//! identifiers, file names or segment lists. Readers and Queues are bounded
//! in-memory fakes; no network, object store or Queue binding is touched.
//!
//! Admission fixtures mirror the exact `ingest_jobs` + `ingest_originals`
//! inserts of the private `worker_entry::admin_ingest_upload_on`, whose
//! visibility keeps it out of reach of an integration-test crate. They are
//! seeded with the production column set, `admin:<sha256>` idempotency key
//! and v2 envelope shape, but not through that function.
//!
//! Dropped clients simulate restarts and lost replies; they are not process
//! crashes or commit-error injection. Trigger injection exercises pre-commit
//! rollback only. Assertions compare full logical snapshots and public
//! outcomes; handles and pointers are compared, never formatted.

use super::*;
use futures_util::future::join_all;
use nntune_ingest::posted_generations::dispatch::{
    pending_file_dispatches, prepare_file_dispatch, recover_file_dispatch, send_file_dispatch,
    DispatchMode, DispatchObject, DispatchObjectReader, DispatchPointer, DispatchQueue,
    DispatchSend, PreparedDispatchFile,
};
use std::collections::{HashMap, VecDeque};

#[path = "posted_generation_tagged_dispatch.rs"]
mod tagged;

const ON: DispatchMode = DispatchMode::PreparationOnly;
const OFF: DispatchMode = DispatchMode::Disabled;
const SMALL: usize = 2_600;
const LARGE: usize = 70_000;
const MIGRATION_0047: &str = include_str!("../../../../migrations/0049_ingest_file_dispatch.sql");

// ---- synthetic objects, bounded fakes -------------------------------------

fn synthetic(len: usize, salt: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(salt) % 251) as u8)
        .collect()
}

struct Objects {
    file_bytes: Vec<u8>,
    plan_bytes: Vec<u8>,
    file: DispatchObject,
    plan: DispatchObject,
}

fn objects(fx: &Fx, file_len: usize) -> Objects {
    Objects {
        file_bytes: synthetic(file_len, 3),
        plan_bytes: synthetic(700, 9),
        file: DispatchObject::new(&format!("pg-obj-{}", fx.uuid), "ver-a").expect("object id"),
        plan: DispatchObject::new(&format!("pg-plan-{}", fx.uuid), "ver-a").expect("plan id"),
    }
}

fn prepared(o: &Objects, index: i32) -> PreparedDispatchFile {
    PreparedDispatchFile::from_bytes(
        index,
        o.file.clone(),
        &o.file_bytes,
        o.plan.clone(),
        &o.plan_bytes,
    )
    .expect("synthetic prepared file")
}

/// Range reader over immutable (id, version) pairs. Counts reads and records
/// the largest requested range so tests can prove IO stayed bounded.
struct MemReader {
    objects: HashMap<(String, String), Vec<u8>>,
    chunk: usize,
    reads: usize,
    max_limit: usize,
    ignore_limit: bool,
    broken: bool,
}

impl MemReader {
    fn exact(o: &Objects) -> Self {
        let mut reader = Self {
            objects: HashMap::new(),
            chunk: 1_000,
            reads: 0,
            max_limit: 0,
            ignore_limit: false,
            broken: false,
        };
        reader.put(&o.file, o.file_bytes.clone());
        reader.put(&o.plan, o.plan_bytes.clone());
        reader
    }

    fn put(&mut self, object: &DispatchObject, bytes: Vec<u8>) {
        self.objects
            .insert((object.id().to_owned(), object.version().to_owned()), bytes);
    }

    fn remove(&mut self, object: &DispatchObject) {
        self.objects
            .remove(&(object.id().to_owned(), object.version().to_owned()));
    }
}

impl DispatchObjectReader for MemReader {
    async fn read(
        &mut self,
        object: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        self.reads += 1;
        self.max_limit = self.max_limit.max(limit);
        if self.broken {
            return Err(StoreError::Database);
        }
        let data = self
            .objects
            .get(&(object.id().to_owned(), object.version().to_owned()))
            .ok_or(StoreError::Database)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let rest = data.len() - start;
        let take = if self.ignore_limit {
            rest
        } else {
            limit.min(self.chunk).min(rest)
        };
        Ok(data[start..start + take].to_vec())
    }
}

#[derive(Clone, Copy)]
enum Send {
    Accept,
    /// Queue never accepted the message and reports an error.
    Reject,
    /// Queue accepted the message but the reply was lost: ambiguous.
    AcceptedReplyLost,
}

#[derive(Default)]
struct FakeQueue {
    script: VecDeque<Send>,
    sent: Vec<DispatchPointer>,
    calls: usize,
}

impl FakeQueue {
    fn scripted(script: &[Send]) -> Self {
        Self {
            script: script.iter().copied().collect(),
            ..Self::default()
        }
    }
}

impl DispatchQueue for FakeQueue {
    async fn send(&mut self, pointer: DispatchPointer) -> Result<(), StoreError> {
        self.calls += 1;
        match self.script.pop_front().unwrap_or(Send::Accept) {
            Send::Accept => {
                self.sent.push(pointer);
                Ok(())
            }
            Send::Reject => Err(StoreError::Database),
            Send::AcceptedReplyLost => {
                self.sent.push(pointer);
                Err(StoreError::Database)
            }
        }
    }
}

// ---- admission fixtures ----------------------------------------------------

struct Fx {
    uuid: String,
    owner: String,
    job: String,
    hash: String,
}

#[derive(Clone, Copy)]
struct Shape {
    role: &'static str,
    source: &'static str,
    status: &'static str,
    failure: Option<&'static str>,
    original: bool,
    files: Option<i32>,
}

impl Shape {
    fn admin() -> Self {
        Self {
            role: "admin",
            source: "admin",
            status: "queued",
            failure: None,
            original: true,
            files: Some(2),
        }
    }
}

async fn seed(db: &Client, shape: Shape) -> Fx {
    let uuid = db_uuid(db).await;
    let hash = digest(db, "dispatch-content", &uuid).await;
    let owner = format!("pg-owner-{uuid}");
    let job = format!("pg-dsp-{uuid}");
    let key = format!("admin:{hash}");
    db.execute(
        "INSERT INTO users (id, email, created_at, role) VALUES ($1, $2, 1700000000, $3)",
        &[&owner, &format!("{owner}@example.invalid"), &shape.role],
    )
    .await
    .quiet("seed dispatch user");
    let nzb_bytes: i64 = 1_048_576;
    let segments = shape.files.map(|files| files * 2);
    db.execute(
        "INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status, source, \
         content_sha256, nzb_bytes, file_count, segment_count, failure_reason) \
         VALUES ($1, $2, $3, 1700000000, $4, $5, $6, $7, $8, $9, $10)",
        &[
            &job,
            &owner,
            &key,
            &shape.status,
            &shape.source,
            &hash,
            &nzb_bytes,
            &shape.files,
            &segments,
            &shape.failure,
        ],
    )
    .await
    .quiet("seed dispatch job");
    if shape.original {
        db.execute(
            "INSERT INTO ingest_originals (job_id, content_sha256, nzb_bytes, nzb_ref_encrypted, \
             nzb_ref_envelope_version, nzb_ref_kek_version, nzb_ref_dek_wrapped_nonce, \
             nzb_ref_dek_wrapped, nzb_ref_payload_nonce, nzb_ref_payload_format, created_at) \
             VALUES ($1, $2, $3, $4, 2, 1, $5, $6, $7, 1, 1700000000)",
            &[
                &job,
                &hash,
                &nzb_bytes,
                &vec![0x77_u8; 64],
                &vec![0x9e_u8; 12],
                &vec![0x5a_u8; 48],
                &vec![0x3c_u8; 12],
            ],
        )
        .await
        .quiet("seed dispatch original");
    }
    Fx {
        uuid,
        owner,
        job,
        hash,
    }
}

async fn admin_fx(db: &Client) -> Fx {
    seed(db, Shape::admin()).await
}

// ---- database observers ----------------------------------------------------

async fn state(db: &impl tokio_postgres::GenericClient, fx: &Fx) -> String {
    db.query_one(
        "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(u) FROM users u WHERE u.id = $1), \
         (SELECT to_jsonb(j) FROM ingest_jobs j WHERE j.id = $2), \
         (SELECT to_jsonb(o) FROM ingest_originals o WHERE o.job_id = $2), \
         (SELECT jsonb_agg(to_jsonb(d) ORDER BY d.expected_attempt, d.operation_key) \
            FROM ingest_file_dispatches d WHERE d.job_id = $2), \
         (SELECT to_jsonb(a) FROM posted_file_job_attempts a WHERE a.job_id = $2), \
         (SELECT jsonb_agg(to_jsonb(r) ORDER BY r.request_id) \
            FROM posted_file_attempt_receipts r WHERE r.job_id = $2), \
         (SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $3) \
         )::text)",
        &[&fx.owner, &fx.job, &fx.hash],
    )
    .await
    .quiet("snapshot dispatch state")
    .get(0)
}

async fn attempt_row(db: &Client, fx: &Fx) -> Option<(i64, String, i64)> {
    db.query_opt(
        "SELECT attempt, token, lease_until FROM posted_file_job_attempts WHERE job_id = $1",
        &[&fx.job],
    )
    .await
    .quiet("read dispatch attempt")
    .map(|row| (row.get(0), row.get(1), row.get(2)))
}

async fn rows_of(db: &Client, table: &'static str, fx: &Fx) -> i64 {
    let sql = format!("SELECT count(*) FROM {table} WHERE job_id = $1");
    count(db, &sql, &[&fx.job]).await
}

async fn dispatch_rows(db: &Client, fx: &Fx) -> i64 {
    rows_of(db, "ingest_file_dispatches", fx).await
}

async fn attempts(db: &Client, fx: &Fx) -> i64 {
    rows_of(db, "posted_file_job_attempts", fx).await
}

async fn receipts(db: &Client, fx: &Fx) -> i64 {
    rows_of(db, "posted_file_attempt_receipts", fx).await
}

async fn status(db: &Client, fx: &Fx) -> String {
    db.query_one("SELECT status FROM ingest_jobs WHERE id = $1", &[&fx.job])
        .await
        .quiet("read dispatch job status")
        .get(0)
}

async fn acknowledged(db: &Client, p: &DispatchPointer) -> bool {
    db.query_one(
        "SELECT send_acknowledged FROM ingest_file_dispatches WHERE id = $1",
        &[&id_of(p)],
    )
    .await
    .quiet("read dispatch acknowledgement")
    .get(0)
}

async fn set_lease(db: &Client, fx: &Fx, seconds_from_now: i64) {
    db.execute(
        "UPDATE posted_file_job_attempts \
         SET lease_until = extract(epoch FROM clock_timestamp())::bigint + $2::bigint \
         WHERE job_id = $1",
        &[&fx.job, &seconds_from_now],
    )
    .await
    .quiet("set dispatch fixture lease");
}

async fn expire(db: &Client, fx: &Fx) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
        &[&fx.job],
    )
    .await
    .quiet("expire dispatch fixture lease");
}

// ---- adapter shorthands ----------------------------------------------------

fn id_of(p: &DispatchPointer) -> String {
    serde_json::to_value(p).expect("serialize pointer")["id"]
        .as_str()
        .expect("pointer id")
        .to_owned()
}

fn same(a: &[DispatchPointer], b: &[DispatchPointer]) -> bool {
    a == b
}

fn is_stale(outcome: &Result<AttemptOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(AttemptOutcome::StaleOrMissing))
}

async fn prep(
    db: &mut Client,
    fx: &Fx,
    op: &str,
    expected: i64,
    file: &PreparedDispatchFile,
) -> Option<DispatchPointer> {
    prepare_file_dispatch(db, ON, &fx.job, op, expected, file)
        .await
        .expect("prepare dispatch")
}

async fn must_prep(
    db: &mut Client,
    fx: &Fx,
    op: &str,
    expected: i64,
    file: &PreparedDispatchFile,
) -> DispatchPointer {
    prep(db, fx, op, expected, file)
        .await
        .expect("admitted job prepares a dispatch")
}

async fn send(
    db: &mut Client,
    p: &DispatchPointer,
    reader: &mut MemReader,
    queue: &mut FakeQueue,
) -> DispatchSend {
    send_file_dispatch(db, ON, p, reader, queue)
        .await
        .expect("send dispatch")
}

async fn recover(
    db: &mut Client,
    p: &DispatchPointer,
    reader: &mut MemReader,
) -> Result<AttemptOutcome, StoreError> {
    recover_file_dispatch(db, ON, p, reader).await
}

fn handle_of(outcome: Result<AttemptOutcome, StoreError>, ctx: &'static str) -> AttemptHandle {
    match outcome {
        Ok(AttemptOutcome::Acquired(handle)) => handle,
        _ => panic!("{ctx}: expected an exact recovered attempt"),
    }
}

// ---- tests -------------------------------------------------------------------

#[test]
fn postgres_dispatch_default_disabled_has_no_database_or_io_effect() {
    run_test(async {
        assert!(DispatchMode::default() == OFF);
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let file = prepared(&o, 0);
        let before = state(&db, &fx).await;
        // Disabled returns before input validation: not even a malformed
        // coordinate reaches the database.
        for (job, op, expected) in [
            (fx.job.as_str(), "op-off", 0_i64),
            ("", "", -1),
            ("a b", "a b", i64::MAX),
        ] {
            assert!(matches!(
                prepare_file_dispatch(&mut db, OFF, job, op, expected, &file).await,
                Ok(None)
            ));
        }
        assert_eq!(dispatch_rows(&db, &fx).await, 0);
        assert!(before == state(&db, &fx).await);

        // A real enabled pointer is inert under Disabled: no startup, read or send.
        let real = admin_fx(&db).await;
        let ro = objects(&real, SMALL);
        let p = must_prep(&mut db, &real, "op-real", 0, &prepared(&ro, 0)).await;
        let real_before = state(&db, &real).await;
        let forged: DispatchPointer =
            serde_json::from_value(serde_json::json!({"version": 9, "id": "a b"}))
                .expect("shape-valid forged pointer");
        let mut reader = MemReader::exact(&ro);
        let mut queue = FakeQueue::default();
        for pointer in [&p, &forged] {
            assert!(matches!(
                send_file_dispatch(&mut db, OFF, pointer, &mut reader, &mut queue).await,
                Ok(DispatchSend::Disabled)
            ));
            assert!(is_stale(
                &recover_file_dispatch(&mut db, OFF, pointer, &mut reader).await
            ));
        }
        assert_eq!(reader.reads, 0);
        assert_eq!(queue.calls, 0);
        assert!(real_before == state(&db, &real).await);
        assert_eq!(status(&db, &real).await, "queued");
        assert_eq!(attempts(&db, &real).await, 0);
        assert_eq!(receipts(&db, &real).await, 0);

        // Control: the same pointer works once enabled, so the zero effects
        // above are the mode and not a broken fixture.
        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert_eq!(attempts(&db, &real).await, 1);
    });
}

#[test]
fn postgres_dispatch_admin_admission_prepares_and_pipeline_or_drifted_jobs_refuse() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let accepted = must_prep(&mut db, &fx, "op-ok", 0, &prepared(&o, 0)).await;
        assert!(!acknowledged(&db, &accepted).await);
        assert_eq!(dispatch_rows(&db, &fx).await, 1);
        // Accepted queued and running admin jobs both prepare.
        let running = seed(
            &db,
            Shape {
                status: "running",
                ..Shape::admin()
            },
        )
        .await;
        let ro = objects(&running, SMALL);
        let _ = must_prep(&mut db, &running, "op-run", 0, &prepared(&ro, 1)).await;

        let refused = [
            (
                "member role",
                Shape {
                    role: "member",
                    ..Shape::admin()
                },
            ),
            (
                "pipeline source with admin-shaped columns",
                Shape {
                    source: "pipeline",
                    ..Shape::admin()
                },
            ),
            (
                "terminal done job",
                Shape {
                    status: "done",
                    ..Shape::admin()
                },
            ),
            (
                "rejected failed upload",
                Shape {
                    status: "failed",
                    failure: Some("empty"),
                    original: false,
                    files: None,
                    ..Shape::admin()
                },
            ),
            (
                "missing original",
                Shape {
                    original: false,
                    ..Shape::admin()
                },
            ),
        ];
        for (label, shape) in refused {
            let other = seed(&db, shape).await;
            let oo = objects(&other, SMALL);
            let before = state(&db, &other).await;
            assert!(
                prep(&mut db, &other, "op-no", 0, &prepared(&oo, 0))
                    .await
                    .is_none(),
                "{label}"
            );
            assert!(before == state(&db, &other).await, "{label}");
            assert_eq!(dispatch_rows(&db, &other).await, 0, "{label}");
        }

        // Row-level drift of an otherwise admitted job.
        let drifts = [
            "UPDATE users SET deleting_at = clock_timestamp() WHERE id = '{owner}'",
            "UPDATE ingest_jobs SET idempotency_key = 'admin:' || repeat('0', 64) WHERE id = '{job}'",
            "UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1 WHERE job_id = '{job}'",
        ];
        for drift in drifts {
            let other = admin_fx(&db).await;
            let oo = objects(&other, SMALL);
            db.batch_execute(
                &drift
                    .replace("{owner}", &other.owner)
                    .replace("{job}", &other.job),
            )
            .await
            .quiet("drift admission fixture");
            let before = state(&db, &other).await;
            assert!(prep(&mut db, &other, "op-no", 0, &prepared(&oo, 0))
                .await
                .is_none());
            assert!(before == state(&db, &other).await);
            assert_eq!(dispatch_rows(&db, &other).await, 0);
        }

        // Missing job, file index outside the admitted count, malformed input.
        let before = state(&db, &fx).await;
        let ghost = Fx {
            uuid: fx.uuid.clone(),
            owner: fx.owner.clone(),
            job: format!("pg-ghost-{}", fx.uuid),
            hash: fx.hash.clone(),
        };
        assert!(prep(&mut db, &ghost, "op-ghost", 0, &prepared(&o, 0))
            .await
            .is_none());
        assert!(prep(&mut db, &fx, "op-far", 0, &prepared(&o, 2))
            .await
            .is_none());
        let file = prepared(&o, 0);
        let long = "a".repeat(65);
        for (job, op, expected) in [
            ("a b", "op-bad", 0_i64),
            (fx.job.as_str(), "", 0),
            (fx.job.as_str(), long.as_str(), 0),
            (fx.job.as_str(), "op-bad", -1),
            (fx.job.as_str(), "op-bad", i64::MAX),
        ] {
            assert!(matches!(
                prepare_file_dispatch(&mut db, ON, job, op, expected, &file).await,
                Err(StoreError::InvalidInput)
            ));
        }
        assert!(before == state(&db, &fx).await);
        assert_eq!(dispatch_rows(&db, &fx).await, 1);
    });
}

#[test]
fn postgres_dispatch_prepare_reuses_pointer_across_restart_and_changed_inputs_refuse() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let file = prepared(&o, 0);
        let p = must_prep(&mut db, &fx, "op-a", 0, &file).await;
        let before = state(&db, &fx).await;
        assert!(p == must_prep(&mut db, &fx, "op-a", 0, &file).await);
        drop(db);
        let mut db = connect().await;
        assert!(p == must_prep(&mut db, &fx, "op-a", 0, &file).await);
        assert!(before == state(&db, &fx).await);
        assert_eq!(dispatch_rows(&db, &fx).await, 1);
        assert_eq!(attempts(&db, &fx).await, 0);
        assert_eq!(receipts(&db, &fx).await, 0);
        assert_eq!(status(&db, &fx).await, "queued");

        let build =
            |index: i32, file: &DispatchObject, bytes: &[u8], plan: &DispatchObject, p: &[u8]| {
                PreparedDispatchFile::from_bytes(index, file.clone(), bytes, plan.clone(), p)
                    .expect("changed fixture")
            };
        let mut flipped = o.file_bytes.clone();
        flipped[0] ^= 1;
        let mut plan_flipped = o.plan_bytes.clone();
        plan_flipped[0] ^= 1;
        let other_version =
            |object: &DispatchObject| DispatchObject::new(object.id(), "ver-b").unwrap();
        let other_id = |object: &DispatchObject| {
            DispatchObject::new(&format!("{}x", object.id()), object.version()).unwrap()
        };
        let changed = [
            (
                "file bytes",
                build(0, &o.file, &flipped, &o.plan, &o.plan_bytes),
                0,
            ),
            (
                "file length",
                build(0, &o.file, &o.file_bytes[1..], &o.plan, &o.plan_bytes),
                0,
            ),
            (
                "plan bytes",
                build(0, &o.file, &o.file_bytes, &o.plan, &plan_flipped),
                0,
            ),
            (
                "file version",
                build(
                    0,
                    &other_version(&o.file),
                    &o.file_bytes,
                    &o.plan,
                    &o.plan_bytes,
                ),
                0,
            ),
            (
                "plan version",
                build(
                    0,
                    &o.file,
                    &o.file_bytes,
                    &other_version(&o.plan),
                    &o.plan_bytes,
                ),
                0,
            ),
            (
                "file id",
                build(0, &other_id(&o.file), &o.file_bytes, &o.plan, &o.plan_bytes),
                0,
            ),
            ("file index", prepared(&o, 1), 0),
            // Same operation key, different predecessor.
            ("operation predecessor", prepared(&o, 0), 1),
        ];
        for (label, input, expected) in changed {
            assert!(
                prep(&mut db, &fx, "op-a", expected, &input).await.is_none(),
                "{label}"
            );
            assert!(before == state(&db, &fx).await, "{label}");
        }
        // One request per explicit predecessor, and no future predecessor.
        for (op, expected) in [("op-b", 0_i64), ("op-future", 1), ("op-far", 2)] {
            assert!(prep(&mut db, &fx, op, expected, &file).await.is_none());
            assert!(before == state(&db, &fx).await);
        }
        assert_eq!(dispatch_rows(&db, &fx).await, 1);

        // Invalid frozen-format inputs are typed refusals before any database use.
        for (index, bytes, plan) in [
            (-1, o.file_bytes.as_slice(), o.plan_bytes.as_slice()),
            (0, &[][..], o.plan_bytes.as_slice()),
            (0, o.file_bytes.as_slice(), &[][..]),
        ] {
            assert!(matches!(
                PreparedDispatchFile::from_bytes(
                    index,
                    o.file.clone(),
                    bytes,
                    o.plan.clone(),
                    plan
                ),
                Err(StoreError::InvalidInput)
            ));
        }
        for (id, version) in [("", "v"), ("a b", "v"), ("id", ""), ("id", "v/1")] {
            assert!(matches!(
                DispatchObject::new(id, version),
                Err(StoreError::InvalidInput)
            ));
        }
        assert!(matches!(
            DispatchObject::new(&"a".repeat(65), "v"),
            Err(StoreError::InvalidInput)
        ));

        // The successor predecessor is only open once the live attempt exists.
        let mut reader = MemReader::exact(&o);
        let mut queue = FakeQueue::default();
        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        let started = attempt_row(&db, &fx).await;
        assert!(started.as_ref().is_some_and(|row| row.0 == 1));
        assert!(prep(&mut db, &fx, "op-far", 2, &file).await.is_none());
        let next = must_prep(&mut db, &fx, "op-b", 1, &prepared(&o, 1)).await;
        assert!(next != p);
        assert!(started == attempt_row(&db, &fx).await);
        assert_eq!(receipts(&db, &fx).await, 1);
    });
}

#[test]
fn postgres_dispatch_pointer_wire_form_rejects_unknown_forged_and_missing_fields() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let p = must_prep(&mut db, &fx, "op-a", 0, &prepared(&o, 0)).await;
        assert_eq!(format!("{p:?}"), "DispatchPointer([REDACTED])");
        assert_eq!(
            format!("{:?}", prepared(&o, 0)),
            "PreparedDispatchFile([REDACTED])"
        );
        assert_eq!(format!("{:?}", o.file), "DispatchObject([REDACTED])");

        // Exactly two fields cross the Queue boundary.
        let wire = serde_json::to_value(&p).expect("serialize pointer");
        let object = wire.as_object().expect("pointer is an object");
        assert_eq!(object.len(), 2);
        assert_eq!(object["version"], 1);
        assert!(object["id"].as_str().is_some_and(|id| !id.is_empty()));
        let round: DispatchPointer = serde_json::from_value(wire.clone()).expect("round trip");
        assert!(round == p);

        let id = id_of(&p);
        for (index, rejected) in [
            serde_json::json!({"version": 1, "id": id, "job_id": fx.job}),
            serde_json::json!({"version": 1, "id": id, "attempt": 1}),
            serde_json::json!({"version": 1, "id": id, "token": "x"}),
            serde_json::json!({"version": 1}),
            serde_json::json!({"id": id}),
            serde_json::json!({}),
            serde_json::json!({"version": "1", "id": id}),
            serde_json::json!({"version": 1, "id": null}),
            serde_json::json!({"version": 1, "id": 7}),
            serde_json::json!({"version": 300, "id": id}),
            serde_json::json!([1, id]),
            serde_json::json!([]),
            serde_json::json!([1, id, "extra"]),
            serde_json::json!(true),
            serde_json::json!(id),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                serde_json::from_value::<DispatchPointer>(rejected).is_err(),
                "malformed pointer case {index} was accepted"
            );
        }
        // Raw JSON preserves duplicate keys, unlike serde_json::Value.
        for duplicate in [
            r#"{"version":1,"version":1,"id":"opaque"}"#,
            r#"{"version":1,"id":"opaque","id":"other"}"#,
        ] {
            assert!(serde_json::from_str::<DispatchPointer>(duplicate).is_err());
        }

        // Shape-valid forgeries deserialize but carry no authority.
        let before = state(&db, &fx).await;
        let mut reader = MemReader::exact(&o);
        let mut queue = FakeQueue::default();
        let unknown = db_uuid(&db).await;
        let stale = serde_json::from_value::<DispatchPointer>(
            serde_json::json!({"version": 1, "id": unknown}),
        )
        .expect("shape-valid unknown id");
        assert!(matches!(
            send_file_dispatch(&mut db, ON, &stale, &mut reader, &mut queue).await,
            Ok(DispatchSend::StaleOrMissing)
        ));
        assert!(is_stale(&recover(&mut db, &stale, &mut reader).await));
        let long = "a".repeat(65);
        for forged in [
            serde_json::json!({"version": 2, "id": id}),
            serde_json::json!({"version": 0, "id": id}),
            serde_json::json!({"version": 1, "id": ""}),
            serde_json::json!({"version": 1, "id": "a b"}),
            serde_json::json!({"version": 1, "id": long}),
        ] {
            let forged: DispatchPointer =
                serde_json::from_value(forged).expect("shape-valid forged pointer");
            assert!(matches!(
                send_file_dispatch(&mut db, ON, &forged, &mut reader, &mut queue).await,
                Err(StoreError::InvalidInput)
            ));
            assert!(matches!(
                recover(&mut db, &forged, &mut reader).await,
                Err(StoreError::InvalidInput)
            ));
        }
        assert!(matches!(
            pending_file_dispatches(&db, "a b").await,
            Err(StoreError::InvalidInput)
        ));
        assert_eq!(reader.reads, 0);
        assert_eq!(queue.calls, 0);
        assert!(before == state(&db, &fx).await);
        assert_eq!(status(&db, &fx).await, "queued");
    });
}

#[test]
fn postgres_dispatch_recovery_before_start_never_creates_receipt_or_attempt() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let p = must_prep(&mut db, &fx, "op-a", 0, &prepared(&o, 0)).await;
        let mut reader = MemReader::exact(&o);
        let before = state(&db, &fx).await;
        assert!(is_stale(&recover(&mut db, &p, &mut reader).await));
        assert!(is_stale(&recover(&mut db, &p, &mut reader).await));
        assert_eq!(reader.reads, 0);
        assert!(before == state(&db, &fx).await);
        assert_eq!(status(&db, &fx).await, "queued");
        assert_eq!(receipts(&db, &fx).await, 0);
        assert_eq!(attempts(&db, &fx).await, 0);

        // A job moved to running without the explicit producer startup still
        // has no receipt, so recovery cannot fall back to starting it.
        db.execute(
            "UPDATE ingest_jobs SET status = 'running' WHERE id = $1",
            &[&fx.job],
        )
        .await
        .quiet("run dispatch job without startup");
        let running = state(&db, &fx).await;
        assert!(is_stale(&recover(&mut db, &p, &mut reader).await));
        assert!(running == state(&db, &fx).await);
        assert_eq!(reader.reads, 0);
        assert_eq!(receipts(&db, &fx).await, 0);
        assert_eq!(attempts(&db, &fx).await, 0);

        // Control: explicit startup is what makes the same pointer recoverable.
        let mut queue = FakeQueue::default();
        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        let _ = handle_of(recover(&mut db, &p, &mut reader).await, "after startup");
    });
}

#[test]
fn postgres_dispatch_send_starts_atomically_and_queue_recovery_after_drop_returns_same_handle() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let p = must_prep(&mut db, &fx, "op-a", 0, &prepared(&o, 0)).await;
        let mut reader = MemReader::exact(&o);
        let mut queue = FakeQueue::default();
        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert!(same(&queue.sent, std::slice::from_ref(&p)));
        assert_eq!(status(&db, &fx).await, "running");
        assert_eq!(receipts(&db, &fx).await, 1);
        assert_eq!(attempts(&db, &fx).await, 1);
        assert!(acknowledged(&db, &p).await);
        assert!(pending_file_dispatches(&db, &fx.job)
            .await
            .expect("pending")
            .is_empty());
        assert!(reader.reads > 2, "ranged reads are chunked");
        assert!(reader.max_limit <= 65_536);

        set_lease(&db, &fx, 100).await;
        let leased = attempt_row(&db, &fx).await;
        assert!(leased.as_ref().is_some_and(|row| row.0 == 1));
        let before = state(&db, &fx).await;
        drop(db);
        drop(queue);

        // Restart: a Queue consumer holds only the pointer and a fresh client.
        let mut consumer = connect().await;
        let mut fresh = MemReader::exact(&o);
        let first = handle_of(
            recover(&mut consumer, &p, &mut fresh).await,
            "first recovery",
        );
        drop(consumer);
        let mut consumer = connect().await;
        let second = handle_of(
            recover(&mut consumer, &p, &mut fresh).await,
            "second recovery",
        );
        assert!(first == second);
        assert!(fresh.max_limit <= 65_536);
        assert!(before == state(&consumer, &fx).await);
        assert!(leased == attempt_row(&consumer, &fx).await);

        // A repeated producer send is at-least-once but never renews or mints.
        let mut queue = FakeQueue::default();
        assert!(matches!(
            send(&mut consumer, &p, &mut fresh, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert!(same(&queue.sent, std::slice::from_ref(&p)));
        assert!(leased == attempt_row(&consumer, &fx).await);
        assert!(before == state(&consumer, &fx).await);
        let third = handle_of(recover(&mut consumer, &p, &mut fresh).await, "after repeat");
        assert!(first == third);
    });
}

#[test]
fn postgres_dispatch_ambiguous_queue_ack_retains_pending_pointer_without_token_or_budget_reset() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let p = must_prep(&mut db, &fx, "op-a", 0, &prepared(&o, 0)).await;
        let mut reader = MemReader::exact(&o);
        let mut queue = FakeQueue::scripted(&[Send::AcceptedReplyLost, Send::Reject, Send::Accept]);

        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Pending
        ));
        assert!(!acknowledged(&db, &p).await);
        assert!(same(
            &pending_file_dispatches(&db, &fx.job)
                .await
                .expect("pending"),
            std::slice::from_ref(&p)
        ));
        // The Queue did accept the first message: only a retry of the SAME pointer
        // can reconcile it, and it must not be an implied delivery id.
        assert!(same(&queue.sent, std::slice::from_ref(&p)));
        set_lease(&db, &fx, 100).await;
        let leased = attempt_row(&db, &fx).await;
        let pending = state(&db, &fx).await;
        drop(db);

        let mut db = connect().await;
        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Pending
        ));
        assert_eq!(queue.calls, 2);
        assert!(pending == state(&db, &fx).await);
        assert!(leased == attempt_row(&db, &fx).await);
        assert!(same(
            &pending_file_dispatches(&db, &fx.job)
                .await
                .expect("pending"),
            std::slice::from_ref(&p)
        ));

        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert_eq!(queue.calls, 3);
        assert!(same(&queue.sent, &[p.clone(), p.clone()]));
        assert!(acknowledged(&db, &p).await);
        assert!(pending_file_dispatches(&db, &fx.job)
            .await
            .expect("pending")
            .is_empty());
        assert!(leased == attempt_row(&db, &fx).await);
        assert_eq!(receipts(&db, &fx).await, 1);
        assert_eq!(attempts(&db, &fx).await, 1);
        assert_eq!(dispatch_rows(&db, &fx).await, 1);
        // No candidate, progress or repost budget exists to reset: the send path
        // never creates custody.
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&fx.hash]
            )
            .await,
            0
        );
    });
}

#[test]
fn postgres_dispatch_wrong_immutable_bytes_or_versions_fail_before_send() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, LARGE);
        let p = must_prep(&mut db, &fx, "op-a", 0, &prepared(&o, 0)).await;

        let mut variants: Vec<(&'static str, MemReader)> = Vec::new();
        let mut add = |label: &'static str, change: &dyn Fn(&mut MemReader)| {
            let mut reader = MemReader::exact(&o);
            change(&mut reader);
            variants.push((label, reader));
        };
        add("file bit flip", &|r| {
            let mut bytes = o.file_bytes.clone();
            bytes[LARGE - 1] ^= 1;
            r.put(&o.file, bytes);
        });
        add("file truncated", &|r| {
            let mut bytes = o.file_bytes.clone();
            bytes.pop();
            r.put(&o.file, bytes);
        });
        add("file extended", &|r| {
            let mut bytes = o.file_bytes.clone();
            bytes.push(0);
            r.put(&o.file, bytes);
        });
        add("plan bit flip", &|r| {
            let mut bytes = o.plan_bytes.clone();
            bytes[0] ^= 1;
            r.put(&o.plan, bytes);
        });
        add("plan truncated", &|r| {
            let mut bytes = o.plan_bytes.clone();
            bytes.pop();
            r.put(&o.plan, bytes);
        });
        add("objects swapped", &|r| {
            r.put(&o.file, o.plan_bytes.clone());
            r.put(&o.plan, o.file_bytes.clone());
        });
        add("file only under another version", &|r| {
            r.remove(&o.file);
            r.put(
                &DispatchObject::new(o.file.id(), "ver-z").unwrap(),
                o.file_bytes.clone(),
            );
        });
        add("plan only under another version", &|r| {
            r.remove(&o.plan);
            r.put(
                &DispatchObject::new(o.plan.id(), "ver-z").unwrap(),
                o.plan_bytes.clone(),
            );
        });
        add("plan missing", &|r| r.remove(&o.plan));
        add("reader failure", &|r| r.broken = true);
        add("reader ignores range limit", &|r| r.ignore_limit = true);

        let mut queue = FakeQueue::default();
        let mut after_first: Option<(String, Option<(i64, String, i64)>)> = None;
        for (label, mut reader) in variants {
            assert!(
                matches!(
                    send(&mut db, &p, &mut reader, &mut queue).await,
                    DispatchSend::ObjectsUnavailable
                ),
                "{label}"
            );
            assert!(
                is_stale(&recover(&mut db, &p, &mut reader).await),
                "{label}"
            );
            assert!(reader.max_limit <= 65_536, "{label}");
            assert_eq!(queue.calls, 0, "{label}");
            assert!(!acknowledged(&db, &p).await, "{label}");
            assert!(
                same(
                    &pending_file_dispatches(&db, &fx.job)
                        .await
                        .expect("pending"),
                    std::slice::from_ref(&p)
                ),
                "{label}"
            );
            let now = (state(&db, &fx).await, attempt_row(&db, &fx).await);
            // Startup is explicit and happens once; later failures change nothing.
            match &after_first {
                None => after_first = Some(now),
                Some(first) => assert!(*first == now, "{label}"),
            }
        }

        let mut good = MemReader::exact(&o);
        assert!(matches!(
            send(&mut db, &p, &mut good, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert!(good.max_limit <= 65_536);
        assert!(same(&queue.sent, std::slice::from_ref(&p)));
        let after = attempt_row(&db, &fx).await;
        assert!(after_first.is_some_and(|(_, attempt)| attempt == after));
    });
}

#[test]
fn postgres_dispatch_current_role_deletion_job_and_original_drift_refuse_without_writes() {
    run_test(async {
        let mut db = connect().await;
        // `{owner}`, `{job}`, `{other}` and `{uuid}` are controlled fixture ids.
        let drifts = [
            "UPDATE users SET role = 'member' WHERE id = '{owner}'",
            "UPDATE users SET deleting_at = clock_timestamp() WHERE id = '{owner}'",
            "UPDATE ingest_jobs SET status = 'done' WHERE id = '{job}'",
            "UPDATE ingest_jobs SET status = 'failed', failure_reason = 'empty' WHERE id = '{job}'",
            "UPDATE ingest_jobs SET idempotency_key = 'admin:' || repeat('0', 64) WHERE id = '{job}'",
            "UPDATE ingest_jobs SET content_sha256 = md5('{uuid}') || md5('x{uuid}') WHERE id = '{job}'",
            "UPDATE ingest_jobs SET file_count = 1, segment_count = 2 WHERE id = '{job}'",
            "UPDATE ingest_jobs SET user_id = '{other}' WHERE id = '{job}'",
            "UPDATE ingest_jobs SET source = 'pipeline' WHERE id = '{job}'",
            "DELETE FROM ingest_originals WHERE job_id = '{job}'",
            "UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1 WHERE job_id = '{job}'",
        ];
        for started in [false, true] {
            for drift in drifts {
                let fx = admin_fx(&db).await;
                let other = admin_fx(&db).await;
                let o = objects(&fx, SMALL);
                // File index 1 so the shrunken file_count drift is observable.
                let p = must_prep(&mut db, &fx, "op-a", 0, &prepared(&o, 1)).await;
                if started {
                    let mut reader = MemReader::exact(&o);
                    let mut queue = FakeQueue::scripted(&[Send::Reject]);
                    assert!(matches!(
                        send(&mut db, &p, &mut reader, &mut queue).await,
                        DispatchSend::Pending
                    ));
                }
                let sql = drift
                    .replace("{owner}", &fx.owner)
                    .replace("{job}", &fx.job)
                    .replace("{other}", &other.owner)
                    .replace("{uuid}", &fx.uuid);
                db.batch_execute(&sql).await.quiet("drift admitted job");
                let drifted = state(&db, &fx).await;
                let attempt = attempt_row(&db, &fx).await;
                let mut reader = MemReader::exact(&o);
                let mut queue = FakeQueue::default();
                assert!(
                    matches!(
                        send_file_dispatch(&mut db, ON, &p, &mut reader, &mut queue).await,
                        Ok(DispatchSend::StaleOrMissing)
                    ),
                    "{drift}"
                );
                assert!(
                    is_stale(&recover(&mut db, &p, &mut reader).await),
                    "{drift}"
                );
                assert!(
                    prep(
                        &mut db,
                        &fx,
                        "op-next",
                        i64::from(started),
                        &prepared(&o, 1)
                    )
                    .await
                    .is_none(),
                    "{drift}"
                );
                assert_eq!(reader.reads, 0, "{drift}");
                assert_eq!(queue.calls, 0, "{drift}");
                assert!(drifted == state(&db, &fx).await, "{drift}");
                assert!(attempt == attempt_row(&db, &fx).await, "{drift}");
                assert_eq!(dispatch_rows(&db, &fx).await, 1, "{drift}");
                assert!(!acknowledged(&db, &p).await, "{drift}");
                if !started {
                    assert_eq!(attempts(&db, &fx).await, 0, "{drift}");
                    assert_eq!(receipts(&db, &fx).await, 0, "{drift}");
                }
            }
        }
    });
}

#[test]
fn postgres_dispatch_expired_and_superseded_requests_recover_and_send_refuse_without_startup() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let mut reader = MemReader::exact(&o);
        let mut queue = FakeQueue::scripted(&[Send::Reject]);

        let p0 = must_prep(&mut db, &fx, "op-0", 0, &prepared(&o, 0)).await;
        assert!(matches!(
            send(&mut db, &p0, &mut reader, &mut queue).await,
            DispatchSend::Pending
        ));
        let p1 = must_prep(&mut db, &fx, "op-1", 1, &prepared(&o, 1)).await;
        assert!(p0 != p1);
        // Newest predecessor first, so an old pending intent cannot starve it.
        assert!(same(
            &pending_file_dispatches(&db, &fx.job)
                .await
                .expect("pending"),
            &[p1.clone(), p0.clone()]
        ));
        assert!(matches!(
            send(&mut db, &p0, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert!(same(
            &pending_file_dispatches(&db, &fx.job)
                .await
                .expect("pending"),
            std::slice::from_ref(&p1)
        ));

        // The successor never borrows or replaces a live predecessor.
        let live = state(&db, &fx).await;
        let calls = queue.calls;
        assert!(matches!(
            send(&mut db, &p1, &mut reader, &mut queue).await,
            DispatchSend::StaleOrMissing
        ));
        assert!(is_stale(&recover(&mut db, &p1, &mut reader).await));
        assert!(live == state(&db, &fx).await);
        assert_eq!(queue.calls, calls);

        expire(&db, &fx).await;
        let expired = state(&db, &fx).await;
        for pointer in [&p0, &p1] {
            assert!(is_stale(&recover(&mut db, pointer, &mut reader).await));
        }
        assert!(matches!(
            send(&mut db, &p0, &mut reader, &mut queue).await,
            DispatchSend::StaleOrMissing
        ));
        assert!(expired == state(&db, &fx).await);
        assert_eq!(receipts(&db, &fx).await, 1);

        // Only the explicit producer send for the successor starts attempt two.
        assert!(matches!(
            send(&mut db, &p1, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert!(attempt_row(&db, &fx).await.is_some_and(|row| row.0 == 2));
        assert_eq!(receipts(&db, &fx).await, 2);
        let current = handle_of(recover(&mut db, &p1, &mut reader).await, "successor");

        let superseded = state(&db, &fx).await;
        let calls = queue.calls;
        assert!(is_stale(&recover(&mut db, &p0, &mut reader).await));
        assert!(matches!(
            send(&mut db, &p0, &mut reader, &mut queue).await,
            DispatchSend::StaleOrMissing
        ));
        assert!(superseded == state(&db, &fx).await);
        assert_eq!(queue.calls, calls);
        assert!(current == handle_of(recover(&mut db, &p1, &mut reader).await, "still current"));
    });
}

#[test]
fn postgres_dispatch_concurrent_same_and_distinct_preparation_and_startup_callers() {
    run_test(async {
        let db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let mut clients = Vec::new();
        for _ in 0..6 {
            clients.push(connect().await);
        }

        // Two callers each repeat three distinct operation keys for one predecessor.
        let ops = ["op-x", "op-x", "op-y", "op-y", "op-z", "op-z"];
        let file0 = prepared(&o, 0);
        let results = join_all(clients.iter_mut().zip(ops).map(|(client, op)| {
            let job = fx.job.clone();
            let file = file0.clone();
            async move { prepare_file_dispatch(client, ON, &job, op, 0, &file).await }
        }))
        .await;
        let winners: Vec<DispatchPointer> = results
            .into_iter()
            .filter_map(|result| result.expect("concurrent prepare"))
            .collect();
        assert_eq!(winners.len(), 2);
        assert!(winners[0] == winners[1]);
        assert_eq!(dispatch_rows(&db, &fx).await, 1);
        let p0 = winners[0].clone();

        // Concurrent identical startups: one receipt, one attempt, one pointer sent.
        let results = join_all(clients.iter_mut().map(|client| {
            let p = p0.clone();
            let mut reader = MemReader::exact(&o);
            async move {
                let mut queue = FakeQueue::default();
                let sent = send_file_dispatch(client, ON, &p, &mut reader, &mut queue).await;
                (sent, queue.sent)
            }
        }))
        .await;
        for (sent, queued) in results {
            assert!(matches!(sent, Ok(DispatchSend::Acknowledged)));
            assert!(same(&queued, std::slice::from_ref(&p0)));
        }
        assert_eq!(receipts(&db, &fx).await, 1);
        assert_eq!(attempts(&db, &fx).await, 1);
        assert_eq!(status(&db, &fx).await, "running");
        let first = attempt_row(&db, &fx).await;

        // Distinct successors for the next predecessor: one wins.
        let file1 = prepared(&o, 1);
        let ops = ["op-s", "op-t", "op-s", "op-t", "op-s", "op-t"];
        let results = join_all(clients.iter_mut().zip(ops).map(|(client, op)| {
            let job = fx.job.clone();
            let file = file1.clone();
            async move { prepare_file_dispatch(client, ON, &job, op, 1, &file).await }
        }))
        .await;
        let successors: Vec<DispatchPointer> = results
            .into_iter()
            .filter_map(|result| result.expect("concurrent successor prepare"))
            .collect();
        assert_eq!(successors.len(), 3);
        assert!(successors.iter().all(|p| *p == successors[0]));
        assert_eq!(dispatch_rows(&db, &fx).await, 2);
        let p1 = successors[0].clone();
        assert!(first == attempt_row(&db, &fx).await);

        // Old and new startups race after expiry: the successor wins once and the
        // predecessor is stale in either interleaving.
        expire(&db, &fx).await;
        let pointers = [&p0, &p1, &p0, &p1, &p0, &p1];
        let results = join_all(clients.iter_mut().zip(pointers).map(|(client, pointer)| {
            let p = pointer.clone();
            let mut reader = MemReader::exact(&o);
            async move {
                let mut queue = FakeQueue::default();
                send_file_dispatch(client, ON, &p, &mut reader, &mut queue).await
            }
        }))
        .await;
        for (index, result) in results.into_iter().enumerate() {
            if index % 2 == 0 {
                assert!(matches!(result, Ok(DispatchSend::StaleOrMissing)));
            } else {
                assert!(matches!(result, Ok(DispatchSend::Acknowledged)));
            }
        }
        assert!(attempt_row(&db, &fx).await.is_some_and(|row| row.0 == 2));
        assert_eq!(receipts(&db, &fx).await, 2);
        assert_eq!(attempts(&db, &fx).await, 1);
    });
}

struct MutatingReader {
    inner: MemReader,
    db: Client,
    sql: &'static str,
    target: String,
    fired: bool,
}

impl DispatchObjectReader for MutatingReader {
    async fn read(
        &mut self,
        object: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        if !self.fired {
            self.db
                .execute(self.sql, &[&self.target])
                .await
                .quiet("mutate dispatch fence during object IO");
            self.fired = true;
        }
        self.inner.read(object, offset, limit).await
    }
}

#[test]
fn postgres_dispatch_rechecks_role_job_deletion_and_lease_after_unlocked_object_io() {
    run_test(async {
        let mut db = connect().await;
        for recovery in [false, true] {
            for (sql, owner) in [
                ("UPDATE users SET role = 'member' WHERE id = $1", true),
                (
                    "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
                    true,
                ),
                (
                    "UPDATE ingest_jobs SET status = 'done' WHERE id = $1",
                    false,
                ),
                (
                    "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                    false,
                ),
            ] {
                let fx = admin_fx(&db).await;
                let o = objects(&fx, SMALL);
                let p = must_prep(&mut db, &fx, "op-io", 0, &prepared(&o, 0)).await;
                if recovery {
                    let mut reader = MemReader::exact(&o);
                    let mut queue = FakeQueue::scripted(&[Send::Reject]);
                    assert!(matches!(
                        send(&mut db, &p, &mut reader, &mut queue).await,
                        DispatchSend::Pending
                    ));
                }
                let mut reader = MutatingReader {
                    inner: MemReader::exact(&o),
                    db: connect().await,
                    sql,
                    target: if owner {
                        fx.owner.clone()
                    } else {
                        fx.job.clone()
                    },
                    fired: false,
                };
                let mut queue = FakeQueue::default();
                if recovery {
                    assert!(is_stale(
                        &recover_file_dispatch(&mut db, ON, &p, &mut reader).await
                    ));
                } else {
                    assert!(matches!(
                        send_file_dispatch(&mut db, ON, &p, &mut reader, &mut queue).await,
                        Ok(DispatchSend::StaleOrMissing)
                    ));
                }
                // The second connection wrote during the awaited range read: a
                // retained provenance/job/attempt transaction would deadlock here.
                assert!(reader.fired);
                assert_eq!(queue.calls, 0);
                assert!(!acknowledged(&db, &p).await);
                assert_eq!(attempts(&db, &fx).await, 1);
                assert_eq!(receipts(&db, &fx).await, 1);
                let after = state(&db, &fx).await;
                assert!(is_stale(
                    &recover_file_dispatch(&mut db, ON, &p, &mut reader).await
                ));
                assert!(after == state(&db, &fx).await);
            }
        }
    });
}

#[test]
fn postgres_dispatch_trigger_injection_rolls_back_and_immutability_survives_migration_replay() {
    run_blocking(true, async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let file = prepared(&o, 0);

        // Preparation insert failure leaves no row and the exact retry succeeds.
        let before = state(&db, &fx).await;
        db.batch_execute(
            "CREATE FUNCTION pg_temp.reject_dispatch_insert() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected dispatch refusal'; END; $$; \
             CREATE TRIGGER reject_dispatch_insert BEFORE INSERT ON ingest_file_dispatches \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_dispatch_insert();",
        )
        .await
        .quiet("install dispatch insert injection");
        let failed = prepare_file_dispatch(&mut db, ON, &fx.job, "op-a", 0, &file).await;
        db.batch_execute("DROP TRIGGER reject_dispatch_insert ON ingest_file_dispatches")
            .await
            .quiet("remove dispatch insert injection");
        assert!(matches!(failed, Err(StoreError::Database)));
        assert!(before == state(&db, &fx).await);
        assert_eq!(dispatch_rows(&db, &fx).await, 0);
        let p = must_prep(&mut db, &fx, "op-a", 0, &file).await;

        // Startup failure rolls back job, receipt and attempt; nothing is sent.
        let before = state(&db, &fx).await;
        let mut reader = MemReader::exact(&o);
        let mut queue = FakeQueue::default();
        db.batch_execute(
            "CREATE FUNCTION pg_temp.reject_dispatch_attempt() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected attempt refusal'; END; $$; \
             CREATE TRIGGER reject_dispatch_attempt BEFORE INSERT ON posted_file_job_attempts \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_dispatch_attempt();",
        )
        .await
        .quiet("install dispatch startup injection");
        let failed = send_file_dispatch(&mut db, ON, &p, &mut reader, &mut queue).await;
        db.batch_execute("DROP TRIGGER reject_dispatch_attempt ON posted_file_job_attempts")
            .await
            .quiet("remove dispatch startup injection");
        assert!(matches!(failed, Err(StoreError::Database)));
        assert!(before == state(&db, &fx).await);
        assert_eq!(status(&db, &fx).await, "queued");
        assert_eq!(receipts(&db, &fx).await, 0);
        assert_eq!(attempts(&db, &fx).await, 0);
        assert_eq!(reader.reads, 0);
        assert_eq!(queue.calls, 0);
        assert!(matches!(
            send(&mut db, &p, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));

        // Acknowledgement failure after Queue acceptance keeps the pointer pending.
        let ack = admin_fx(&db).await;
        let ao = objects(&ack, SMALL);
        let ap = must_prep(&mut db, &ack, "op-a", 0, &prepared(&ao, 0)).await;
        let mut reader = MemReader::exact(&ao);
        let mut queue = FakeQueue::default();
        db.batch_execute(
            "CREATE FUNCTION pg_temp.reject_dispatch_ack() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected acknowledgement refusal'; END; $$; \
             CREATE TRIGGER reject_dispatch_ack BEFORE UPDATE ON ingest_file_dispatches \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_dispatch_ack();",
        )
        .await
        .quiet("install dispatch acknowledgement injection");
        let failed = send_file_dispatch(&mut db, ON, &ap, &mut reader, &mut queue).await;
        db.batch_execute("DROP TRIGGER reject_dispatch_ack ON ingest_file_dispatches")
            .await
            .quiet("remove dispatch acknowledgement injection");
        assert!(matches!(failed, Err(StoreError::Database)));
        assert!(same(&queue.sent, std::slice::from_ref(&ap)));
        assert!(!acknowledged(&db, &ap).await);
        assert!(same(
            &pending_file_dispatches(&db, &ack.job)
                .await
                .expect("pending"),
            std::slice::from_ref(&ap)
        ));
        let started = attempt_row(&db, &ack).await;
        assert!(started.as_ref().is_some_and(|row| row.0 == 1));
        assert!(matches!(
            send(&mut db, &ap, &mut reader, &mut queue).await,
            DispatchSend::Acknowledged
        ));
        assert!(started == attempt_row(&db, &ack).await);
        assert!(acknowledged(&db, &ap).await);

        // Native immutability and constraints, then replay of migration 0047.
        let id = id_of(&p);
        let immutable = Some("file dispatch is immutable");
        for sql in [
            "UPDATE ingest_file_dispatches SET plan_sha256 = repeat('0', 64) WHERE id = $1",
            "UPDATE ingest_file_dispatches SET object_version = 'ver-b' WHERE id = $1",
            "UPDATE ingest_file_dispatches SET operation_key = 'other' WHERE id = $1",
            "UPDATE ingest_file_dispatches SET send_acknowledged = FALSE WHERE id = $1",
            "DELETE FROM ingest_file_dispatches WHERE id = $1",
        ] {
            expect_db_error(
                db.execute(sql, &[&id]).await,
                &SqlState::CHECK_VIOLATION,
                immutable,
                "dispatch mutation",
            );
        }
        db.execute(
            "UPDATE ingest_file_dispatches SET send_acknowledged = send_acknowledged WHERE id = $1",
            &[&id],
        )
        .await
        .quiet("dispatch true no-op");
        // Isolate the new FK: no receipt/attempt exists, and the original's
        // older RESTRICT FK must not mask the dispatch constraint. Roll back
        // the temporary original removal along with the refused job deletion.
        let retained = admin_fx(&db).await;
        let ro = objects(&retained, SMALL);
        let rp = must_prep(&mut db, &retained, "op-a", 0, &prepared(&ro, 0)).await;
        assert_eq!(receipts(&db, &retained).await, 0);
        assert_eq!(attempts(&db, &retained).await, 0);
        let retained_before = state(&db, &retained).await;
        let tx = db
            .transaction()
            .await
            .quiet("begin isolated dispatch FK check");
        assert_eq!(
            tx.execute(
                "DELETE FROM ingest_originals WHERE job_id = $1",
                &[&retained.job]
            )
            .await
            .quiet("temporarily remove competing original FK"),
            1
        );
        let error = tx
            .execute("DELETE FROM ingest_jobs WHERE id = $1", &[&retained.job])
            .await
            .expect_err("dispatch FK must retain the job");
        let error = error.as_db_error().expect("dispatch FK database error");
        assert!(error.code() == &SqlState::RESTRICT_VIOLATION);
        assert!(error.constraint() == Some("ingest_file_dispatches_job_id_fkey"));
        tx.rollback()
            .await
            .quiet("restore isolated dispatch FK fixture");
        assert!(retained_before == state(&db, &retained).await);
        assert!(rp == must_prep(&mut db, &retained, "op-a", 0, &prepared(&ro, 0)).await);
        let key = format!("admin:{}", fx.hash);
        let insert = "INSERT INTO ingest_file_dispatches (job_id, operation_key, owner_id, \
             admission_key, original_sha256, file_index, expected_attempt, object_id, \
             object_version, plaintext_bytes, plaintext_sha256, plan_id, plan_version, \
             plan_format, plan_bytes, plan_sha256) \
             VALUES ($1, $2, $3, $4, $5, 0, $6, 'o', 'v', $7, $5, 'p', 'v', $8, 10, $5)";
        for (op, expected, bytes, format, state_code) in [
            ("a b", 5_i64, 1_i64, 1_i16, &SqlState::CHECK_VIOLATION),
            ("op-new", 5, 0, 1, &SqlState::CHECK_VIOLATION),
            ("op-new", 5, 1, 2, &SqlState::CHECK_VIOLATION),
            ("op-new", i64::MAX, 1, 1, &SqlState::CHECK_VIOLATION),
            ("op-new", 0, 1, 1, &SqlState::UNIQUE_VIOLATION),
            ("op-a", 5, 1, 1, &SqlState::UNIQUE_VIOLATION),
        ] {
            expect_db_error(
                db.execute(
                    insert,
                    &[
                        &fx.job, &op, &fx.owner, &key, &fx.hash, &expected, &bytes, &format,
                    ],
                )
                .await,
                state_code,
                None,
                "invalid dispatch shape",
            );
        }
        let before = state(&db, &fx).await;
        for _ in 0..2 {
            db.batch_execute(MIGRATION_0047)
                .await
                .quiet("replay dispatch migration");
        }
        assert!(before == state(&db, &fx).await);
        assert!(p == must_prep(&mut db, &fx, "op-a", 0, &file).await);
        assert!(before == state(&db, &fx).await);
        expect_db_error(
            db.execute(
                "UPDATE ingest_file_dispatches SET plan_sha256 = repeat('0', 64) WHERE id = $1",
                &[&id],
            )
            .await,
            &SqlState::CHECK_VIOLATION,
            immutable,
            "dispatch mutation after replay",
        );
    });
}
