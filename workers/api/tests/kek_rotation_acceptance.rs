use nntune_api::vault::{decrypt_vault, encrypt_vault, rewrap_dek, VaultBlob};
use rand::RngCore;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection};
use std::fs;
use std::path::PathBuf;

const CURRENT_VERSION: u32 = 2;
const PER_VERSION_PROGRESS_QUERY: &str = "SELECT kek_version, COUNT(*) AS row_count\n  FROM provider_vaults\n GROUP BY kek_version\n ORDER BY kek_version;";
// The claim must be a compare-and-swap on the captured envelope identity
// (the wrapped DEK and its nonce read at claim time), not on
// user/version alone: a vault PUT replaces the whole envelope at the
// same KEK version with a fresh DEK, so a version-only guard still
// matches the replacement row and a rewrap computed from the captured
// envelope would install the old DEK over the new payload.
const CONDITIONAL_REWRAP_UPDATE: &str = "UPDATE provider_vaults\n      SET kek_version = $new, dek_wrapped = $w,\n          dek_wrapped_nonce = $n, updated_at = $now,\n          rewrapped_at = $now\n    WHERE user_id = $u AND kek_version = $old\n      AND dek_wrapped = $captured_wrapped\n      AND dek_wrapped_nonce = $captured_wrapped_nonce;";

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

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

fn migration_path(name: &str) -> PathBuf {
    migrations_dir().join(name)
}

fn kek_rotation_migration_sql() -> String {
    // Discover by suffix, not number, so a renumber (e.g. resolving a
    // duplicate migration number) does not break this file; the migration
    // order acceptance test pins numbering and manifest placement.
    let dir = migrations_dir();
    let mut matches = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", dir.display()))
        .map(|entry| entry.expect("migration directory entry").path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("_kek_rotation.sql"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one *_kek_rotation.sql migration in {}, found {matches:?}",
        dir.display()
    );
    fs::read_to_string(&matches[0]).expect("read KEK rotation follow-up migration")
}

fn fixture_db() -> Connection {
    // Start at auth/vault schema; migration 0001 needs the PostgreSQL-only pg_trgm extension.
    let db = Connection::open_in_memory().expect("open fixture DB");
    let initial = std::fs::read_to_string(migration_path("0002_auth_vault.sql"))
        .expect("read initial vault migration");
    db.execute_batch(&initial)
        .expect("apply initial vault migration");

    let follow_up = kek_rotation_migration_sql();
    assert!(
        follow_up.contains("ADD COLUMN IF NOT EXISTS rewrapped_at"),
        "the raw migration must use ADD COLUMN IF NOT EXISTS rewrapped_at \
         so Postgres replay of the migration is idempotent"
    );
    // SQLite rejects `ADD COLUMN IF NOT EXISTS`; strip it for this fixture
    // only. The assertion above keeps the raw Postgres text pinned.
    let sqlite_follow_up = follow_up.replace("ADD COLUMN IF NOT EXISTS ", "ADD COLUMN ");
    db.execute_batch(&sqlite_follow_up)
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

fn runbook_text() -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/security/kek-rotation.md"),
    )
    .expect("read KEK rotation runbook")
}

/// The normative §6 conditional UPDATE, extracted verbatim from the
/// runbook so these tests exercise the documented SQL contract itself
/// rather than a hand-written copy that could drift from it.
fn documented_rewrap_update_sql() -> String {
    let runbook = runbook_text();
    let section_start = runbook
        .find("## 6.")
        .expect("runbook must have a §6 re-wrap job section");
    let section = &runbook[section_start..];
    let start = section
        .find("UPDATE provider_vaults")
        .expect("runbook §6 must document the conditional rewrap UPDATE");
    let end = section[start..]
        .find(';')
        .expect("runbook §6 conditional UPDATE must be a terminated statement");
    section[start..start + end + 1].to_string()
}

/// Execute the documented §6 UPDATE against the SQLite fixture.
/// `$name` placeholders are bound positionally in order of appearance;
/// an unknown placeholder fails loudly so a renamed documented binding
/// cannot silently change what this test exercises.
fn execute_documented_rewrap(
    db: &Connection,
    user_id: &str,
    captured: &VaultBlob,
    rewrapped: &VaultBlob,
    now: i64,
) -> usize {
    let template = documented_rewrap_update_sql();
    let mut sql = String::new();
    let mut values: Vec<Value> = Vec::new();
    let mut rest = template.as_str();
    while let Some(pos) = rest.find('$') {
        sql.push_str(&rest[..pos]);
        let tail = &rest[pos + 1..];
        let name_len = tail
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
            .unwrap_or(tail.len());
        let name = &tail[..name_len];
        let value = match name {
            "new" => Value::Integer(rewrapped.kek_version as i64),
            "w" => Value::Text(rewrapped.dek_wrapped_b64url.clone()),
            "n" => Value::Text(rewrapped.dek_wrapped_nonce_b64url.clone()),
            "now" => Value::Integer(now),
            "u" => Value::Text(user_id.to_string()),
            "old" => Value::Integer(captured.kek_version as i64),
            "captured_wrapped" => Value::Text(captured.dek_wrapped_b64url.clone()),
            "captured_wrapped_nonce" => Value::Text(captured.dek_wrapped_nonce_b64url.clone()),
            other => panic!(
                "documented rewrap UPDATE uses unknown placeholder ${other}; \
                 bind it here explicitly instead of guessing"
            ),
        };
        sql.push('?');
        values.push(value);
        rest = &tail[name_len..];
    }
    sql.push_str(rest);
    db.execute(&sql, params_from_iter(values))
        .expect("execute documented rewrap UPDATE")
}

fn insert_vault_row(db: &Connection, user_id: &str, blob: &VaultBlob, updated_at: i64) {
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

/// A vault PUT replaces the whole envelope (fresh DEK, fresh payload)
/// at whatever KEK version is current for writes at that moment.
fn replace_vault_row_put(db: &Connection, user_id: &str, blob: &VaultBlob, updated_at: i64) {
    let changed = db
        .execute(
            "UPDATE provider_vaults
                SET kek_version = ?1, dek_wrapped_nonce = ?2, dek_wrapped = ?3,
                    payload_nonce = ?4, payload = ?5, updated_at = ?6
              WHERE user_id = ?7",
            params![
                blob.kek_version as i64,
                blob.dek_wrapped_nonce_b64url,
                blob.dek_wrapped_b64url,
                blob.payload_nonce_b64url,
                blob.payload_b64url,
                updated_at,
                user_id,
            ],
        )
        .expect("replace vault envelope (PUT)");
    assert_eq!(changed, 1, "the PUT must replace exactly one vault row");
}

fn assert_blob_eq(actual: &VaultBlob, expected: &VaultBlob) {
    assert_eq!(actual.kek_version, expected.kek_version, "kek_version");
    assert_eq!(
        actual.dek_wrapped_nonce_b64url, expected.dek_wrapped_nonce_b64url,
        "dek_wrapped_nonce"
    );
    assert_eq!(
        actual.dek_wrapped_b64url, expected.dek_wrapped_b64url,
        "dek_wrapped"
    );
    assert_eq!(
        actual.payload_nonce_b64url, expected.payload_nonce_b64url,
        "payload_nonce"
    );
    assert_eq!(actual.payload_b64url, expected.payload_b64url, "payload");
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
        insert_vault_row(&db, user_id, blob, updated_at);
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
    // Persist through the documented §6 conditional UPDATE itself, with
    // the still-current legacy envelope as the captured claim.
    let now = 1_800_000_000_i64;
    let changed = execute_documented_rewrap(&db, "legacy-user", &legacy, &rewrapped, now);
    assert_eq!(
        changed, 1,
        "a claim on the current envelope should update exactly once"
    );

    let (legacy_after, updated_at, recorded_rewrapped_at) = vault_row(&db, "legacy-user");
    assert_eq!(legacy_after.kek_version, CURRENT_VERSION);
    assert_eq!(updated_at, now);
    assert_eq!(recorded_rewrapped_at, Some(now));
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

/// §10 text only, whitespace-collapsed so the pin below keys on the
/// stated guard, not on markdown line wrapping.
fn section_10_normalized() -> String {
    let runbook = runbook_text();
    let section_start = runbook
        .find("## 10.")
        .expect("runbook must have a §10 compromise rotation section");
    let section = &runbook[section_start..];
    let section_end = section[1..]
        .find("\n## ")
        .map(|offset| offset + 1)
        .unwrap_or(section.len());
    section[..section_end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn rotation_runbook_documents_per_version_progress_counts() {
    let runbook = runbook_text();

    assert!(
        runbook.contains(PER_VERSION_PROGRESS_QUERY),
        "the runbook must show an executable count grouped by kek_version"
    );
    assert!(
        runbook.contains(CONDITIONAL_REWRAP_UPDATE),
        "the runbook §6 conditional UPDATE must set rewrapped_at = $now \
         alongside kek_version, guarded by a compare-and-swap on the \
         captured wrapped DEK and its nonce (dek_wrapped = \
         $captured_wrapped AND dek_wrapped_nonce = \
         $captured_wrapped_nonce), not by user/version alone"
    );
}

#[test]
fn compromise_rekey_uses_captured_envelope_cas_not_timestamp_guard() {
    let section_10 = section_10_normalized();

    // Production change that must fail this test: §10 reverting to the
    // old (`user_id`, `kek_version`, `updated_at`) guard. A same-version
    // replacement can land in the same `updated_at` tick (see the §6
    // race tests), so a timestamp-keyed guard would let a stale re-key
    // overwrite a newer envelope.
    assert!(
        section_10.contains(
            "ONE conditional UPDATE guarded by the same captured-envelope \
             compare-and-swap as §6 (`user_id`, captured `kek_version`, \
             captured `dek_wrapped` and `dek_wrapped_nonce` — never a \
             timestamp-only guard)"
        ),
        "§10 must guard the compromise re-key with the same §6 \
         captured-envelope compare-and-swap (captured `dek_wrapped` + \
         `dek_wrapped_nonce`)"
    );
    assert!(
        section_10.contains("Zero affected rows is a lost claim as in §6"),
        "§10 must define zero affected rows as a lost claim as in §6"
    );
    assert!(
        section_10.contains(
            "the stale re-key is discarded, the row is reloaded and \
             re-driven against the current envelope, and the newer \
             envelope is never overwritten"
        ),
        "§10 must discard a stale re-key, reload, and never overwrite \
         the newer envelope"
    );
    assert!(
        !section_10
            .contains("conditional UPDATE keyed on (`user_id`, `kek_version`, `updated_at`)"),
        "§10 must not key the compromise re-key guard on `updated_at`"
    );
    assert!(
        !section_10.contains("a concurrent vault update changes `updated_at` and loses the race"),
        "§10 must not treat an `updated_at` change as the race guard"
    );
}

struct SameVersionRace {
    db: Connection,
    old_kek: [u8; 32],
    current_kek: [u8; 32],
    captured_a: VaultBlob,
    replacement_b: VaultBlob,
    payload_b: &'static [u8],
}

/// Capture envelope A at KEK version 1 (the rewrap claim read), then a
/// PUT replaces the row with envelope B — a fresh DEK and payload, but
/// still KEK version 1, because writes have not flipped yet.
fn same_version_race_fixture() -> SameVersionRace {
    let db = fixture_db();
    let old_kek = random_test_key();
    let current_kek = random_test_key();
    db.execute(
        "INSERT INTO users (id, email, created_at) VALUES (?1, ?2, ?3)",
        params!["race-user", "race@example.test", 10],
    )
    .expect("seed user");

    let captured_a = encrypt_vault(&old_kek, 1, "race-user", b"captured synthetic credentials")
        .expect("encrypt envelope A");
    insert_vault_row(&db, "race-user", &captured_a, 20);

    let payload_b: &'static [u8] = b"replacement synthetic credentials";
    let replacement_b =
        encrypt_vault(&old_kek, 1, "race-user", payload_b).expect("encrypt envelope B");
    assert_ne!(
        replacement_b.dek_wrapped_b64url, captured_a.dek_wrapped_b64url,
        "fixture premise: the PUT must install a fresh wrapped DEK"
    );
    assert_eq!(
        replacement_b.kek_version, captured_a.kek_version,
        "fixture premise: the PUT stays at the same KEK version"
    );
    replace_vault_row_put(&db, "race-user", &replacement_b, 30);

    SameVersionRace {
        db,
        old_kek,
        current_kek,
        captured_a,
        replacement_b,
        payload_b,
    }
}

#[test]
fn stale_same_version_rewrap_claim_updates_zero_rows_and_preserves_replacement() {
    let race = same_version_race_fixture();

    // The rewrap is computed from captured envelope A after B replaced it.
    let stale_rewrap = rewrap_dek(
        &race.old_kek,
        &race.current_kek,
        CURRENT_VERSION,
        "race-user",
        &race.captured_a,
    )
    .expect("re-wrap captured DEK");
    let changed = execute_documented_rewrap(
        &race.db,
        "race-user",
        &race.captured_a,
        &stale_rewrap,
        1_800_000_100,
    );
    assert_eq!(
        changed, 0,
        "a rewrap computed from a stale captured envelope must lose the \
         claim: the row now holds a different envelope at the same KEK \
         version, so a version-only guard is not a valid claim"
    );

    let (after, updated_at, rewrapped_at) = vault_row(&race.db, "race-user");
    assert_blob_eq(&after, &race.replacement_b);
    assert_eq!(updated_at, 30, "the lost claim must not touch updated_at");
    assert_eq!(rewrapped_at, None, "the lost claim must not mark a rewrap");
    assert_eq!(
        decrypt_vault(&race.old_kek, "race-user", &after).expect("B stays decryptable"),
        race.payload_b,
        "the replacement payload must survive the stale rewrap attempt"
    );
}

#[test]
fn lost_claim_reload_and_retry_rewraps_current_envelope_without_losing_new_payload() {
    let race = same_version_race_fixture();

    let stale_rewrap = rewrap_dek(
        &race.old_kek,
        &race.current_kek,
        CURRENT_VERSION,
        "race-user",
        &race.captured_a,
    )
    .expect("re-wrap captured DEK");
    let stale_changed = execute_documented_rewrap(
        &race.db,
        "race-user",
        &race.captured_a,
        &stale_rewrap,
        1_800_000_100,
    );
    assert_eq!(stale_changed, 0, "premise: the stale claim loses");

    // Reload the current envelope and retry the rewrap against it.
    let (current_row, _, _) = vault_row(&race.db, "race-user");
    assert_blob_eq(&current_row, &race.replacement_b);
    let retry_rewrap = rewrap_dek(
        &race.old_kek,
        &race.current_kek,
        CURRENT_VERSION,
        "race-user",
        &current_row,
    )
    .expect("re-wrap current DEK");
    assert_eq!(retry_rewrap.payload_b64url, current_row.payload_b64url);
    let retry_now = 1_800_000_200_i64;
    let retry_changed = execute_documented_rewrap(
        &race.db,
        "race-user",
        &current_row,
        &retry_rewrap,
        retry_now,
    );
    assert_eq!(
        retry_changed, 1,
        "a claim on the reloaded current envelope should update exactly once"
    );

    let (after, updated_at, rewrapped_at) = vault_row(&race.db, "race-user");
    assert_eq!(after.kek_version, CURRENT_VERSION);
    assert_eq!(after.payload_b64url, race.replacement_b.payload_b64url);
    assert_eq!(
        after.payload_nonce_b64url, race.replacement_b.payload_nonce_b64url,
        "rewrap must never re-encrypt or replace the payload"
    );
    assert_eq!(updated_at, retry_now);
    assert_eq!(rewrapped_at, Some(retry_now));
    assert_eq!(
        decrypt_vault(&race.current_kek, "race-user", &after)
            .expect("retried rewrap stays decryptable under the new KEK"),
        race.payload_b,
        "the replacement payload must survive the reload-and-retry rewrap"
    );
    assert!(
        decrypt_vault(&race.old_kek, "race-user", &after).is_err(),
        "after the retry the row must decrypt only through its stamped KEK version"
    );
}
