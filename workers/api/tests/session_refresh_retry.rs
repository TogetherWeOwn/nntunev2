//! Execute the production CAS statement, not a reimplementation of its
//! predicate. PostgreSQL migration replay is also covered by the SQL runner.
use nntune_api::session::{SessionClaims, REFRESH_RETRY_SECS, ROTATE_SESSION_WITH_RETRY_SQL};
use rusqlite::{named_params, params, Connection, OptionalExtension};

const NOW: u64 = 1_800_000_000;

fn fixture() -> (Connection, SessionClaims) {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../../../migrations/0002_auth_vault.sql"))
        .unwrap();
    // SQLite lacks ADD COLUMN IF NOT EXISTS; only adapt that DDL syntax.
    db.execute_batch(
        &include_str!("../../../migrations/0024_session_refresh_retry.sql")
            .replace("ADD COLUMN IF NOT EXISTS", "ADD COLUMN"),
    )
    .unwrap();
    db.execute(
        "INSERT INTO users (id,email,created_at) VALUES ('u1','synthetic@example.test',?1)",
        [NOW as i64],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sessions (id,user_id,created_at,expires_at) VALUES ('s1','u1',?1,?2)",
        params![NOW as i64, (NOW + 3600) as i64],
    )
    .unwrap();
    (
        db,
        SessionClaims {
            session_id: "s1".into(),
            user_id: "u1".into(),
            created_at: NOW,
            issued_at: NOW,
            expires_at: NOW + 3600,
            rotation_counter: 0,
        },
    )
}

fn rotate(db: &Connection, old: &SessionClaims, hash: &str, now: u64) -> Option<SessionClaims> {
    db.query_row(
        ROTATE_SESSION_WITH_RETRY_SQL,
        // `$N` is a named parameter in SQLite, so bind by name rather than
        // encounter order (the production PostgreSQL statement starts at $6).
        named_params! {
            "$1": old.session_id,
            "$2": old.user_id,
            "$3": old.created_at as i64,
            "$4": old.rotation_counter,
            "$5": old.expires_at as i64,
            "$6": old.rotation_counter + 1,
            "$7": (now + 3600) as i64,
            "$8": hash,
            "$9": now as i64,
            "$10": now.saturating_sub(REFRESH_RETRY_SECS) as i64,
        },
        |r| {
            Ok(SessionClaims {
                session_id: r.get(0)?,
                user_id: r.get(1)?,
                created_at: r.get::<_, i64>(2)? as u64,
                expires_at: r.get::<_, i64>(3)? as u64,
                rotation_counter: r.get(4)?,
                issued_at: r.get::<_, i64>(5)? as u64,
            })
        },
    )
    .optional()
    .unwrap()
}

#[test]
fn production_cas_recovers_same_successor_without_extending_any_bound() {
    let (db, old) = fixture();
    let successor = rotate(&db, &old, "synthetic-bound-digest", NOW + 10).unwrap();
    assert_eq!(successor.rotation_counter, 1);
    assert_eq!(successor.issued_at, NOW + 10);
    assert_eq!(successor.expires_at, NOW + 3610);
    assert_eq!(successor.created_at, old.created_at);
    for now in [NOW + 10, NOW + 11, NOW + 129] {
        assert_eq!(
            rotate(&db, &old, "synthetic-bound-digest", now),
            Some(successor.clone())
        );
    }
    assert!(rotate(&db, &old, "different-digest", NOW + 11).is_none());
    assert!(rotate(&db, &old, "synthetic-bound-digest", NOW + 130).is_none());
    assert!(rotate(&db, &old, "synthetic-bound-digest", NOW + 9).is_none());
    let next = rotate(&db, &successor, "next-bound-digest", NOW + 20).unwrap();
    assert_eq!(next.rotation_counter, 2);
    assert!(rotate(&db, &old, "synthetic-bound-digest", NOW + 21).is_none());
}

#[test]
fn production_cas_rejects_mismatched_row_identity_and_old_expiry() {
    let (db, old) = fixture();
    let mut wrong_owner = old.clone();
    wrong_owner.user_id = "other-user".into();
    let mut wrong_id = old.clone();
    wrong_id.session_id = "other-session".into();
    let mut wrong_created = old.clone();
    wrong_created.created_at += 1;
    let mut wrong_expiry = old.clone();
    wrong_expiry.expires_at += 1;
    let mut wrong_counter = old.clone();
    wrong_counter.rotation_counter = 5;
    for altered in [
        wrong_owner,
        wrong_id,
        wrong_created,
        wrong_expiry,
        wrong_counter,
    ] {
        assert!(rotate(&db, &altered, "synthetic-bound-digest", NOW).is_none());
    }
    assert!(rotate(&db, &old, "synthetic-bound-digest", NOW).is_some());
}

#[test]
fn production_cas_never_recovers_revoked_deleted_or_expired_sessions() {
    for mutation in [
        "UPDATE sessions SET revoked=TRUE",
        "DELETE FROM sessions",
        "UPDATE sessions SET expires_at=1800000001",
    ] {
        let (db, old) = fixture();
        rotate(&db, &old, "synthetic-bound-digest", NOW).unwrap();
        db.execute(mutation, []).unwrap();
        assert!(rotate(&db, &old, "synthetic-bound-digest", NOW + 1).is_none());
    }
}

#[test]
fn production_cas_concurrent_identical_retries_converge_and_other_keys_lose() {
    use std::sync::{Arc, Barrier, Mutex};
    let (db, old) = fixture();
    let db = Arc::new(Mutex::new(db));
    let barrier = Arc::new(Barrier::new(8));
    let joins: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            let old = old.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                rotate(&db.lock().unwrap(), &old, "same-client-digest", NOW).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = joins.into_iter().map(|j| j.join().unwrap()).collect();
    assert!(results.iter().all(|r| r == &results[0]));
    assert_eq!(results[0].rotation_counter, 1);
    assert!(rotate(&db.lock().unwrap(), &old, "other-client-digest", NOW).is_none());
}
