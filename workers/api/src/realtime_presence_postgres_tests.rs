//! Off-route reference transaction on the disposable Actions database.
use super::*;
use crate::listen_history::postgres_tests::connect;
use crate::postgres::{PgConnect, PgQuery, PgTransaction};
use crate::session::{
    issue_session, rotate_session, verify_session_dual, SessionRevokeClaim, MAX_SESSION_SECS,
};
use tokio_postgres::{error::SqlState, types::Type, Client, Row};

const START: u64 = 100;
const KEYS: SessionKeys<'static> = SessionKeys {
    primary: &[0x51; 32],
    previous: None,
};

enum Operation {
    Heartbeat(u64),
    Snapshot,
}

fn session_row(row: Row) -> SessionRecord {
    SessionRecord {
        id: row.get("id"),
        user_id: row.get("user_id"),
        created_at: u64::try_from(row.get::<_, i64>("created_at")).unwrap(),
        expires_at: u64::try_from(row.get::<_, i64>("expires_at")).unwrap(),
        rotation_counter: u32::try_from(row.get::<_, i32>("rotation_counter")).unwrap(),
        revoked: row.get("revoked"),
    }
}

// Fixed server time makes the expiry edges deterministic. Wiring must sample
// its authoritative clock after lock acquisition, before verifying the row.
async fn apply(
    tx: &PgTransaction<'_>,
    token: &str,
    now: u64,
    operation: Operation,
) -> Result<Vec<OnlineDevice>, PresenceError> {
    let locator =
        verify_session_dual(&KEYS, token, now).map_err(|_| PresenceError::InvalidSession)?;
    let owner = &locator.user_id;
    // User before session, consistent with the credential and erasure paths.
    tx.query_typed_opt(
        "SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR NO KEY UPDATE",
        &[(owner, Type::TEXT)],
    )
    .await
    .unwrap()
    .ok_or(PresenceError::InvalidSession)?;
    let persisted = tx
        .query_typed_opt(
            "SELECT watermark, payload FROM presence_fixture WHERE owner = $1",
            &[(owner, Type::TEXT)],
        )
        .await
        .unwrap();
    let at = i64::try_from(now).unwrap();
    let prior: Option<Presence> = match persisted {
        Some(row) => {
            if at < row.get::<_, i64>("watermark") {
                return Err(PresenceError::InvalidSession);
            }
            let state: Presence = serde_json::from_str(row.get("payload")).unwrap();
            assert_eq!(&state.owner, owner);
            assert!(state.devices.len() <= MAX_DEVICES);
            Some(state)
        }
        None => None,
    };

    let mut ids = vec![locator.session_id.as_str()];
    if let (Operation::Snapshot, Some(state)) = (&operation, &prior) {
        ids.extend(
            state
                .devices
                .iter()
                .filter(|(id, entry)| id.as_str() != locator.session_id && now < entry.online_until)
                .map(|(id, _)| id.as_str()),
        );
    }
    assert!(ids.len() <= MAX_DEVICES + 1);
    let rows: BTreeMap<_, _> = tx
        .query_typed(
            "SELECT id, user_id, created_at, expires_at, rotation_counter, revoked
             FROM sessions WHERE user_id = $1 AND id = ANY($2) ORDER BY id FOR SHARE",
            &[(owner, Type::TEXT), (&ids, Type::TEXT_ARRAY)],
        )
        .await
        .unwrap()
        .into_iter()
        .map(session_row)
        .map(|row| (row.id.clone(), row))
        .collect();
    let auth = || PresenceSession {
        keys: KEYS,
        token,
        now,
        current_row: rows.get(&locator.session_id),
    };
    let mut state = match prior {
        Some(state) => state,
        None => Presence::new(auth())?,
    };
    let result = match operation {
        Operation::Heartbeat(sequence) => {
            state.heartbeat(auth(), sequence)?;
            Vec::new()
        }
        Operation::Snapshot => state.online(auth(), |id| rows.get(id))?,
    };
    tx.execute_typed(
        "INSERT INTO presence_fixture (owner, watermark, payload) VALUES ($1, $2, $3)
         ON CONFLICT (owner) DO UPDATE SET watermark = EXCLUDED.watermark, payload = EXCLUDED.payload",
        &[
            (owner, Type::TEXT),
            (&at, Type::INT8),
            (&serde_json::to_string(&state).unwrap(), Type::TEXT),
        ],
    )
    .await
    .unwrap();
    Ok(result)
}

async fn run(
    db: &mut Client,
    token: &str,
    now: u64,
    operation: Operation,
) -> Result<Vec<OnlineDevice>, PresenceError> {
    let tx = db.read_committed().await.unwrap();
    let result = apply(&tx, token, now, operation).await;
    if result.is_ok() {
        tx.commit().await.unwrap();
    } else {
        tx.rollback().await.unwrap();
    }
    result
}

async fn login(db: &Client, owner: &str, now: u64, ttl: u64) -> (String, SessionClaims) {
    let token = issue_session(KEYS.primary, owner, now, ttl).unwrap();
    let claims = verify_session_dual(&KEYS, &token, now).unwrap();
    db.execute(
        "INSERT INTO sessions (id, user_id, created_at, expires_at) VALUES ($1, $2, $3, $4)",
        &[
            &claims.session_id,
            &claims.user_id,
            &i64::try_from(claims.created_at).unwrap(),
            &i64::try_from(claims.expires_at).unwrap(),
        ],
    )
    .await
    .unwrap();
    (token, claims)
}

#[tokio::test]
async fn postgres_presence_keeps_authority_and_replay_state_together() {
    use Operation::{Heartbeat, Snapshot};

    let schema = format!("presence_fixture_{}", std::process::id());
    let search_path = format!("SET search_path TO {schema}");
    let mut db = connect().await;
    db.batch_execute(&format!("CREATE SCHEMA {schema}; {search_path}"))
        .await
        .unwrap();
    db.batch_execute(include_str!("../../../migrations/0002_auth_vault.sql"))
        .await
        .unwrap();
    db.batch_execute(include_str!(
        "../../../migrations/0006_account_deletion.sql"
    ))
    .await
    .unwrap();
    db.batch_execute(
        "CREATE TABLE presence_fixture (
            owner text PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
            watermark bigint NOT NULL,
            payload text NOT NULL CHECK (octet_length(payload) <= 8192)
         );
         INSERT INTO users (id, email, created_at)
         VALUES ('alice', 'alice@example.test', 100), ('bob', 'bob@example.test', 100)",
    )
    .await
    .unwrap();
    let mut writer = connect().await;
    writer.batch_execute(&search_path).await.unwrap();
    let (first, claims) = login(&db, "alice", START, 10).await;
    let (second, peer) = login(&db, "alice", START, MAX_SESSION_SECS).await;
    let (foreign, _) = login(&db, "bob", START, MAX_SESSION_SECS).await;
    run(&mut db, &first, START, Heartbeat(2)).await.unwrap();
    run(&mut db, &second, START, Heartbeat(9)).await.unwrap();
    let snapshot = |order| {
        vec![OnlineDevice {
            order,
            is_current: true,
        }]
    };
    assert_eq!(run(&mut db, &foreign, START, Snapshot).await, Ok(vec![]));
    run(&mut db, &foreign, START, Heartbeat(2)).await.unwrap();
    let tx = db.read_committed().await.unwrap();
    apply(&tx, &second, START + 1, Heartbeat(10)).await.unwrap();
    assert_eq!(
        writer
            .query_one(
                "SELECT id FROM sessions WHERE id = $1 FOR UPDATE NOWAIT",
                &[&peer.session_id]
            )
            .await
            .unwrap_err()
            .code(),
        Some(&SqlState::LOCK_NOT_AVAILABLE)
    );
    tx.rollback().await.unwrap();
    run(&mut writer, &second, START + 1, Heartbeat(10))
        .await
        .unwrap();

    let refreshed = rotate_session(&KEYS, &first, START + 9, MAX_SESSION_SECS).unwrap();
    let affected = writer
        .execute(
            "UPDATE sessions SET rotation_counter = 1, expires_at = $2
         WHERE id = $1 AND rotation_counter = 0 AND NOT revoked",
            &[
                &claims.session_id,
                &i64::try_from(START + MAX_SESSION_SECS).unwrap(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(affected, 1);
    assert_eq!(
        run(&mut db, &first, START + 9, Heartbeat(3)).await,
        Err(PresenceError::InvalidSession)
    );
    for sequence in [2, 1] {
        assert_eq!(
            run(&mut db, &refreshed, START + 9, Heartbeat(sequence)).await,
            Err(PresenceError::StaleSequence)
        );
    }
    assert_eq!(
        run(&mut db, &second, START + 10, Snapshot).await.unwrap(),
        snapshot(2)
    );
    run(&mut db, &refreshed, START + 10, Heartbeat(3))
        .await
        .unwrap();

    let revoke = SessionRevokeClaim::Single {
        session_id: &claims.session_id,
        user_id: &claims.user_id,
    };
    let tx = writer.transaction().await.unwrap();
    tx.execute(revoke.statement(), &[&claims.session_id, &claims.user_id])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let still_online = run(&mut db, &second, START + 10, Snapshot).await.unwrap();
    assert_eq!(still_online.len(), 2);
    writer
        .execute(revoke.statement(), &[&claims.session_id, &claims.user_id])
        .await
        .unwrap();
    drop(db);
    assert_eq!(
        run(&mut writer, &refreshed, START + 11, Snapshot).await,
        Err(PresenceError::InvalidSession)
    );
    assert_eq!(
        run(&mut writer, &second, START + 11, Snapshot)
            .await
            .unwrap(),
        snapshot(2)
    );
    assert_eq!(
        run(&mut writer, &second, START + 11, Heartbeat(10)).await,
        Err(PresenceError::StaleSequence)
    );

    for slot in 2..MAX_DEVICES {
        let (token, device) = login(&writer, "alice", START, MAX_SESSION_SECS).await;
        run(&mut writer, &token, START + 11, Heartbeat(1))
            .await
            .unwrap();
        if slot == 2 {
            writer
                .execute("DELETE FROM sessions WHERE id = $1", &[&device.session_id])
                .await
                .unwrap();
            assert_eq!(
                run(&mut writer, &second, START + 11, Snapshot)
                    .await
                    .unwrap(),
                snapshot(2)
            );
        }
    }
    let (extra, _) = login(&writer, "alice", START + 11, MAX_SESSION_SECS).await;
    let deadline = START + MAX_SESSION_SECS;
    let all_offline = START + 11 + ONLINE_SECS;
    assert_eq!(
        run(&mut writer, &extra, all_offline, Snapshot).await,
        Ok(vec![])
    );
    assert_eq!(
        run(&mut writer, &extra, deadline - 1, Heartbeat(1)).await,
        Err(PresenceError::Capacity)
    );
    run(&mut writer, &second, deadline - 1, Heartbeat(11))
        .await
        .unwrap();
    run(&mut writer, &extra, deadline, Heartbeat(1))
        .await
        .unwrap();
    assert_eq!(
        run(&mut writer, &extra, deadline, Snapshot).await.unwrap(),
        snapshot(MAX_DEVICES as u64 + 1)
    );
    assert_eq!(
        run(&mut writer, &second, deadline - 1, Heartbeat(12)).await,
        Err(PresenceError::InvalidSession)
    );

    writer
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
