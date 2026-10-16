use nntune_api::vault::{decrypt_vault, encrypt_vault, rewrap_dek, VaultBlob};
use rand::RngCore;
use rusqlite::{params, Connection};
use std::path::PathBuf;

const CURRENT_VERSION: u32 = 2;
const PER_VERSION_PROGRESS_QUERY: &str = "SELECT kek_version, COUNT(*) AS row_count\n  FROM provider_vaults\n GROUP BY kek_version\n ORDER BY kek_version;";

fn random_test_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn test_only_kek_by_version(
    old_kek: &[u8; 32],
    current_kek: &[u8; 32],
    user_id: &str,
    blob: &VaultBlob,
) -> Result<Vec<u8>, nntune_api::vault::VaultError> {
    match blob.kek_version {
        1 => decrypt_vault(old_kek, user_id, blob),
        CURRENT_VERSION => decrypt_vault(current_kek, user_id, blob),
        _ => panic!("unexpected fixture KEK version {}", blob.kek_version),
    }
}

fn migration_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../migrations")
        .join(name)
}

fn fixture_db() -> Connection {
    // Start at auth/vault schema; migration 0001 needs the PostgreSQL-only pg_trgm extension.
    let db = Connection::open_in_memory().expect("open fixture DB");
    let initial = std::fs::read_to_string(migration_path("0002_auth_vault.sql"))
        .expect("read initial vault migration");
    db.execute_batch(&initial)
        .expect("apply initial vault migration");

    let follow_up = std::fs::read_to_string(migration_path("0004_kek_rotation.sql"))
        .expect("read KEK rotation follow-up migration");
    db.execute_batch(&follow_up)
        .expect("apply KEK rotation follow-up migration");
    db
}

fn column_info(db: &Connection, name: &str) -> (String, bool) {
    let mut statement = db
        .prepare("PRAGMA table_info(provider_vaults)")
        .expect("inspect provider_vaults columns");
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .expect("query provider_vaults columns");
    for column in columns {
        let (column_name, column_type, not_null) = column.expect("read provider_vaults column");
        if column_name == name {
            return (column_type, not_null == 0);
        }
    }
    panic!("provider_vaults is missing column {name}");
}

fn vault_row(db: &Connection, user_id: &str) -> (VaultBlob, i64, Option<i64>) {
    db.query_row(
        "SELECT kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at, rewrapped_at
           FROM provider_vaults WHERE user_id = ?1",
        [user_id],
        |row| {
            Ok((
                VaultBlob {
                    kek_version: row.get::<_, i64>(0)? as u32,
                    dek_wrapped_nonce_b64url: row.get(1)?,
                    dek_wrapped_b64url: row.get(2)?,
                    payload_nonce_b64url: row.get(3)?,
                    payload_b64url: row.get(4)?,
                },
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )
    .expect("load vault fixture row")
}

#[test]
fn follow_up_migration_adds_nullable_timestamp_and_version_index() {
    let db = fixture_db();

    assert_eq!(column_info(&db, "rewrapped_at"), ("BIGINT".into(), true));

    let indexed_columns = {
        let mut statement = db
            .prepare("PRAGMA index_info(idx_vault_kek_version)")
            .expect("inspect KEK-version index");
        statement
            .query_map([], |row| row.get::<_, String>(2))
            .expect("query KEK-version index columns")
            .map(|column| column.expect("read KEK-version index column"))
            .collect::<Vec<_>>()
    };
    assert_eq!(indexed_columns, vec!["kek_version".to_string()]);
}

#[test]
fn rewrap_records_timestamp_and_mixed_versions_test_only_kek_by_versions() {
    let db = fixture_db();
    let old_kek = random_test_key();
    let current_kek = random_test_key();
    db.execute(
        "INSERT INTO users (id, email, created_at)
         VALUES (?1, ?2, ?3), (?4, ?5, ?6), (?7, ?8, ?9)",
        params![
            "legacy-user",
            "legacy@example.test",
            10,
            "retained-user",
            "retained@example.test",
            11,
            "current-user",
            "current@example.test",
            12,
        ],
    )
    .expect("seed users");

    let legacy_payload = b"legacy synthetic credentials";
    let retained_payload = b"retained synthetic credentials";
    let current_payload = b"current synthetic credentials";
    let legacy = encrypt_vault(&old_kek, 1, "legacy-user", legacy_payload).unwrap();
    let retained = encrypt_vault(&old_kek, 1, "retained-user", retained_payload).unwrap();
    let current = encrypt_vault(
        &current_kek,
        CURRENT_VERSION,
        "current-user",
        current_payload,
    )
    .unwrap();

    for (user_id, blob, updated_at) in [
        ("legacy-user", &legacy, 20_i64),
        ("retained-user", &retained, 21_i64),
        ("current-user", &current, 22_i64),
    ] {
        db.execute(
            "INSERT INTO provider_vaults
                (user_id, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, payload, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                user_id,
                blob.kek_version as i64,
                blob.dek_wrapped_nonce_b64url,
                blob.dek_wrapped_b64url,
                blob.payload_nonce_b64url,
                blob.payload_b64url,
                updated_at,
            ],
        )
        .expect("seed encrypted vault row");
    }

    let rewrapped = rewrap_dek(
        &old_kek,
        &current_kek,
        CURRENT_VERSION,
        "legacy-user",
        &legacy,
    )
    .expect("re-wrap legacy DEK");
    assert_eq!(rewrapped.payload_nonce_b64url, legacy.payload_nonce_b64url);
    assert_eq!(rewrapped.payload_b64url, legacy.payload_b64url);
    // Distinct values catch writing the rotation marker into the wrong column.
    let updated_at = 1_800_000_001_i64;
    let rewrapped_at = 1_800_000_000_i64;
    let changed = db
        .execute(
            "UPDATE provider_vaults
                SET kek_version = ?1, dek_wrapped_nonce = ?2, dek_wrapped = ?3,
                    updated_at = ?4, rewrapped_at = ?5
              WHERE user_id = ?6 AND kek_version = ?7",
            params![
                rewrapped.kek_version as i64,
                rewrapped.dek_wrapped_nonce_b64url,
                rewrapped.dek_wrapped_b64url,
                updated_at,
                rewrapped_at,
                "legacy-user",
                1,
            ],
        )
        .expect("persist re-wrapped row");
    assert_eq!(changed, 1, "the legacy version row should be updated once");

    let (legacy_after, updated_at, recorded_rewrapped_at) = vault_row(&db, "legacy-user");
    assert_eq!(legacy_after.kek_version, CURRENT_VERSION);
    assert_eq!(updated_at, 1_800_000_001_i64);
    assert_eq!(recorded_rewrapped_at, Some(rewrapped_at));
    assert_eq!(
        test_only_kek_by_version(&old_kek, &current_kek, "legacy-user", &legacy_after).unwrap(),
        legacy_payload
    );
    assert!(
        decrypt_vault(&old_kek, "legacy-user", &legacy_after).is_err(),
        "the row must decrypt only through its stamped KEK version"
    );

    let (retained_after, _, retained_rewrapped_at) = vault_row(&db, "retained-user");
    assert_eq!(retained_after.kek_version, 1);
    assert_eq!(retained_rewrapped_at, None);
    assert_eq!(
        test_only_kek_by_version(&old_kek, &current_kek, "retained-user", &retained_after).unwrap(),
        retained_payload
    );
    assert!(decrypt_vault(&current_kek, "retained-user", &retained_after).is_err());

    let (current_after, _, untouched_rewrapped_at) = vault_row(&db, "current-user");
    assert_eq!(current_after.kek_version, CURRENT_VERSION);
    assert_eq!(untouched_rewrapped_at, None);
    assert_eq!(
        test_only_kek_by_version(&old_kek, &current_kek, "current-user", &current_after).unwrap(),
        current_payload
    );
    assert!(decrypt_vault(&old_kek, "current-user", &current_after).is_err());

    let version_counts = {
        let mut statement = db
            .prepare(PER_VERSION_PROGRESS_QUERY)
            .expect("prepare per-version progress query");
        statement
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .expect("query per-version progress")
            .map(|row| row.expect("read per-version count"))
            .collect::<Vec<_>>()
    };
    assert_eq!(version_counts, vec![(1, 1), (CURRENT_VERSION as i64, 2)]);
}

#[test]
fn rotation_runbook_documents_per_version_progress_counts() {
    let runbook = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/security/kek-rotation.md"),
    )
    .expect("read KEK rotation runbook");

    assert!(
        runbook.contains(PER_VERSION_PROGRESS_QUERY),
        "the runbook must show an executable count grouped by kek_version"
    );
}
