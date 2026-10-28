//! Required native-visible PRODUCTION parameterized SQL plans, not copies of
//! application queries in test fixtures. Missing exports are compile-RED.
//!
//! Expected `subsonic_auth` exports:
//! SubsonicSqlQuery { sql: String, params: Vec<SubsonicSqlParam> }, where params
//! are Integer(i32) or Text(String); constructors below return Result<Query,
//! StoreError>. Worker storage must execute THESE plans, not a parallel query.
//! A fenced write uses a MATERIALIZED owner CTE with FOR SHARE in its single
//! SQL statement, holding the owner lock through the update/transaction. A
//! separate preflight SELECT or process-local mutex is not a substitute.
//!
//! These executable plan inspections check real SQL text/parameter boundaries
//! and projection/fence shape, NOT PostgreSQL execution or concurrent locking.
//! The separate SQL regression fixture is deliberately NOT run in this task.
//! No DB, migration, credential, rotation or runtime operation is performed.

use nntune_api::subsonic_auth::{
    subsonic_last_used_query, subsonic_lookup_query, subsonic_rehmac_query,
    subsonic_unrevoked_pepper_query, SubsonicClientRow, SubsonicOwnerRow, SubsonicSqlParam,
    SubsonicSqlQuery,
};

const ID: &str = "00000000-0000-4000-8000-000000000001";
const USER: &str = "synthetic-sql-owner' OR true --";
const OLD: &str = "524f316146ebc41db98bf8e3881dd7038659624f1061d24b30d8ab1a1d576a05";
const NEW: &str = "5c886144a4a0aa546256e6c76c05f0c37ebf37bb53bd7b0d56f08b7bf486f57e";

fn old_row() -> SubsonicClientRow {
    SubsonicClientRow {
        client_id: ID.into(),
        user_id: USER.into(),
        subsonic_username: "synthetic-generated-name".into(),
        api_key_hmac: OLD.into(),
        pepper_version: 1,
        auth_mode: "apikey".into(),
        scope: "subsonic".into(),
        revoked_at: None,
        owner: Some(SubsonicOwnerRow {
            user_id: USER.into(),
            email: "synthetic-sql-owner@example.test".into(),
            deleting_at: None,
            subsonic_disabled_at: None,
        }),
    }
}

fn text(value: &str) -> SubsonicSqlParam {
    SubsonicSqlParam::Text(value.into())
}
fn version(value: i32) -> SubsonicSqlParam {
    SubsonicSqlParam::Integer(value)
}
fn normalized(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}
fn assert_parameterized(query: &SubsonicSqlQuery) -> String {
    for value in [
        ID,
        USER,
        OLD,
        NEW,
        "synthetic-generated-name",
        "synthetic-sql-owner@example.test",
    ] {
        assert!(
            !query.sql.contains(value),
            "row material was interpolated in SQL"
        );
    }
    for diagnostic in [
        format!("{query:?}"),
        format!("{query:#?}"),
        format!("{:#?}", query.params),
    ] {
        for canary in [ID, USER, OLD, NEW] {
            assert!(
                !diagnostic.contains(canary),
                "SQL diagnostics disclosed bound values"
            );
        }
    }
    let sql = normalized(&query.sql);
    for forbidden in [
        "subsonic_clients",
        "native_rest_credentials",
        "legacy_seal",
        "key_version",
    ] {
        assert!(
            !sql.contains(forbidden),
            "legacy/cross-surface authority in SQL"
        );
    }
    sql
}

#[test]
fn lookup_binds_current_previous_pairs_and_projects_the_actual_joined_owner() {
    let query = subsonic_lookup_query(&[(2, NEW.into()), (1, OLD.into())]).unwrap();
    assert_eq!(query.params, [version(2), text(NEW), version(1), text(OLD)]);
    let sql = assert_parameterized(&query);
    // Aliases c/u and projection aliases are part of the small driver seam.
    // Identity must retain owner_user_id, never an email or credential name.
    for required in [
        "from subsonic_credentials c",
        "join users u on u.id = c.user_id",
        "u.id as owner_user_id",
        "u.email as owner_email",
        "u.deleting_at",
        "u.subsonic_disabled_at",
        "c.id",
        "c.user_id",
        "c.pepper_version",
        "c.api_key_hmac",
        "c.auth_mode",
        "c.scope",
        "c.revoked_at",
        "c.pepper_version = $1",
        "c.api_key_hmac = $2",
        "c.pepper_version = $3",
        "c.api_key_hmac = $4",
    ] {
        assert!(sql.contains(required), "missing SQL contract: {required}");
    }
    assert!(!sql.contains("false as owner_disabled"));
    assert!(!sql.contains("false as subsonic_disabled"));
    assert!(
        !sql.contains("now() < u.deleting_at"),
        "scheduled deletion is already ineligible"
    );
    assert!(
        !sql.contains("limit 1"),
        "must observe ambiguous distinct matches"
    );
    assert!(
        !sql.contains("in ($1, $3)"),
        "versions and digests must not be cross-product matched"
    );
    assert!(sql.contains(" or ") || sql.contains("values ($1, $2), ($3, $4)"));
}

#[test]
fn lookup_rejects_empty_too_many_invalid_or_duplicate_candidate_versions() {
    for pairs in [
        vec![],
        vec![(0, NEW.into())],
        vec![(2, NEW.to_ascii_uppercase())],
        vec![(2, "short".into())],
        vec![(2, NEW.into()), (2, OLD.into())],
        vec![(1, OLD.into()), (2, NEW.into()), (3, NEW.into())],
    ] {
        assert!(subsonic_lookup_query(&pairs).is_err());
    }
    let query = subsonic_lookup_query(&[(2, NEW.into())]).unwrap();
    assert_eq!(query.params, [version(2), text(NEW)]);
    assert!(!query.sql.contains("$3"));
}

fn assert_fenced_update(query: &SubsonicSqlQuery) -> String {
    let sql = assert_parameterized(query);
    for required in [
        "with owner_fence as materialized",
        "from users",
        "id = $2",
        "deleting_at is null",
        "subsonic_disabled_at is null",
        "for share",
        "update subsonic_credentials",
        "from owner_fence",
        "revoked_at is null",
        "auth_mode = 'apikey'",
        "scope = 'subsonic'",
        "id = $1",
        "user_id = $2",
        "pepper_version = $3",
        "api_key_hmac = $4",
        "returning",
    ] {
        assert!(
            sql.contains(required),
            "missing same-statement fence/predicate: {required}"
        );
    }
    let update = sql.find("update subsonic_credentials").unwrap();
    assert!(sql.find("for share").unwrap() < update);
    assert!(
        !sql.contains("pg_advisory"),
        "pool-unsafe session lock is not the owner fence"
    );
    assert!(
        !sql.contains("commit;"),
        "query must not release its own fence before its write"
    );
    sql
}

#[test]
fn cas_fences_owner_and_binds_id_old_pair_unrevoked_state_and_new_pair() {
    let query = subsonic_rehmac_query(&old_row(), 2, NEW).unwrap();
    assert_eq!(
        query.params,
        [
            text(ID),
            text(USER),
            version(1),
            text(OLD),
            version(2),
            text(NEW)
        ]
    );
    let sql = assert_fenced_update(&query);
    assert!(sql.contains("pepper_version = $5"));
    assert!(sql.contains("api_key_hmac = $6"));
    assert!(
        !sql.contains("last_used_at ="),
        "CAS must not masquerade as successful use"
    );
}

#[test]
fn last_used_has_the_owner_and_exact_current_row_fence_without_rewriting_material() {
    let mut row = old_row();
    row.pepper_version = 2;
    row.api_key_hmac = NEW.into();
    let query = subsonic_last_used_query(&row).unwrap();
    assert_eq!(query.params, [text(ID), text(USER), version(2), text(NEW)]);
    let sql = assert_fenced_update(&query);
    let (_, update) = sql.split_once("update subsonic_credentials").unwrap();
    let (assignment, _) = update.split_once(" from owner_fence").unwrap();
    assert!(assignment.contains("last_used_at ="));
    assert!(
        !assignment.contains("api_key_hmac ="),
        "stale projection overwrote credential material"
    );
    assert!(!assignment.contains("pepper_version ="));
    assert!(!assignment.contains("revoked_at ="));
}

#[test]
fn retirement_inventory_counts_every_unrevoked_row_without_owner_or_recency_filters() {
    let query = subsonic_unrevoked_pepper_query(1).unwrap();
    assert_eq!(query.params, [version(1)]);
    let sql = assert_parameterized(&query);
    for required in [
        "count(",
        "from subsonic_credentials",
        "pepper_version = $1",
        "revoked_at is null",
    ] {
        assert!(sql.contains(required));
    }
    for forbidden in [
        "join users",
        "last_used_at",
        "deleting_at",
        "subsonic_disabled_at",
        "interval",
        "created_at",
    ] {
        assert!(
            !sql.contains(forbidden),
            "dormant or ineligible owner hid an unrevoked credential"
        );
    }
}
