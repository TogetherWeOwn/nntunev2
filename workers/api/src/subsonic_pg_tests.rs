//! Disposable PostgreSQL only, invoked by the required Actions migration job.
use super::*;
use tokio_postgres::{Config, NoTls};

const USER: &str = "subsonic-test-owner";
const ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

async fn database() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty());
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move {
        connection.await.unwrap();
    });
    client
        .batch_execute(
            "SET search_path = pg_temp;
        CREATE TEMP TABLE users (id TEXT PRIMARY KEY, deleting_at TIMESTAMPTZ);",
        )
        .await
        .unwrap();
    // The required migration job has applied/replayed the actual manifest.
    // Clone its columns, checks and indexes; per-session rows stay isolated.
    client
        .batch_execute("CREATE TEMP TABLE subsonic_credentials (LIKE public.subsonic_credentials INCLUDING ALL)")
        .await
        .unwrap();
    client
        .execute("INSERT INTO users (id) VALUES ($1)", &[&USER])
        .await
        .unwrap();
    client.execute("INSERT INTO subsonic_credentials (id,user_id,subsonic_username,api_key_hmac,pepper_version)
        VALUES ($1::text::uuid,$2,'generated-client',repeat('a',64),1)", &[&ID,&USER]).await.unwrap();
    client
}

#[tokio::test]
async fn exact_lookup_completion_and_reindex_are_committed() {
    let mut client = database().await;
    assert!(lookup(&mut client, Some(2), &"a".repeat(64))
        .await
        .unwrap()
        .is_none());
    assert!(lookup(&mut client, None, "email@example.invalid")
        .await
        .unwrap()
        .is_none());
    let selected = lookup(&mut client, Some(1), &"a".repeat(64))
        .await
        .unwrap()
        .unwrap();
    assert!(mutate(&mut client, &selected, None, 120).await.unwrap());
    let reindex = Reindex {
        pepper_version: 2,
        api_key_hmac: "b".repeat(64),
    };
    assert!(mutate(&mut client, &selected, Some(Some(&reindex)), 121)
        .await
        .unwrap());
    assert!(lookup(&mut client, Some(1), &"a".repeat(64))
        .await
        .unwrap()
        .is_none());
    let current = lookup(&mut client, Some(2), &"b".repeat(64))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.user_id, USER);
    let row = client.query_one("SELECT extract(epoch FROM last_used_at)::bigint,attempt_count FROM subsonic_credentials", &[]).await.unwrap();
    assert_eq!(row.get::<_, i64>(0), 121);
    assert_eq!(row.get::<_, i32>(1), 1);
    // The stale reindex can never overwrite a completed concurrent rotation.
    assert!(!mutate(&mut client, &selected, Some(Some(&reindex)), 122)
        .await
        .unwrap());
}

#[tokio::test]
async fn rate_limit_is_persistent_and_clock_regression_never_resets_it() {
    let mut client = database().await;
    let selected = lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .unwrap();
    for _ in 0..60 {
        assert!(mutate(&mut client, &selected, None, 120).await.unwrap());
    }
    assert!(!mutate(&mut client, &selected, None, 120).await.unwrap());
    assert!(!mutate(&mut client, &selected, None, 60).await.unwrap());
    assert!(mutate(&mut client, &selected, None, 180).await.unwrap());
    let count: i32 = client
        .query_one("SELECT attempt_count FROM subsonic_credentials", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn revocation_and_account_deletion_fence_both_mutations() {
    let mut client = database().await;
    let selected = lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .unwrap();
    client
        .batch_execute("UPDATE subsonic_credentials SET revoked_at=now()")
        .await
        .unwrap();
    assert!(!mutate(&mut client, &selected, None, 120).await.unwrap());
    assert!(!mutate(&mut client, &selected, Some(None), 120)
        .await
        .unwrap());
    assert!(lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .is_none());
    client
        .batch_execute(
            "UPDATE subsonic_credentials SET revoked_at=NULL; UPDATE users SET deleting_at=now()",
        )
        .await
        .unwrap();
    assert!(!mutate(&mut client, &selected, None, 120).await.unwrap());
    assert!(!mutate(&mut client, &selected, Some(None), 120)
        .await
        .unwrap());
    assert!(lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn changed_username_owner_or_material_never_completes_a_stale_snapshot() {
    let mut client = database().await;
    let selected = lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .unwrap();
    for change in [
        "UPDATE subsonic_credentials SET subsonic_username='different-client'",
        "UPDATE subsonic_credentials SET subsonic_username='generated-client',api_key_hmac=repeat('b',64)",
        "INSERT INTO users(id) VALUES ('other'); UPDATE subsonic_credentials SET api_key_hmac=repeat('a',64),user_id='other'",
    ] {
        client.batch_execute(change).await.unwrap();
        assert!(!mutate(&mut client, &selected, None, 120).await.unwrap());
        assert!(!mutate(&mut client, &selected, Some(None), 120).await.unwrap());
    }
}

#[tokio::test]
async fn token_envelope_decodes_and_every_field_participates_in_cas() {
    let mut client = database().await;
    client.batch_execute("UPDATE subsonic_credentials SET auth_mode='both',
        token_secret_encrypted=decode(repeat('aa',48),'hex'),token_envelope_version=2,
        token_payload_format=1,token_kek_version=1,token_dek_wrapped_nonce=decode(repeat('bb',12),'hex'),
        token_dek_wrapped=decode(repeat('cc',48),'hex'),token_payload_nonce=decode(repeat('dd',12),'hex')").await.unwrap();
    let selected = lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        selected.token.as_ref().unwrap().envelope.dek_wrapped_nonce,
        [0xbb; 12]
    );
    assert!(mutate(&mut client, &selected, None, 120).await.unwrap());
    assert!(mutate(&mut client, &selected, Some(None), 120)
        .await
        .unwrap());
    for change in [
        "token_secret_encrypted=decode(repeat('ee',48),'hex')",
        "token_kek_version=2",
        "token_dek_wrapped_nonce=decode(repeat('ee',12),'hex')",
        "token_dek_wrapped=decode(repeat('ee',48),'hex')",
        "token_payload_nonce=decode(repeat('ee',12),'hex')",
    ] {
        let current = lookup(&mut client, None, "generated-client")
            .await
            .unwrap()
            .unwrap();
        client
            .batch_execute(&format!("UPDATE subsonic_credentials SET {change}"))
            .await
            .unwrap();
        assert!(!mutate(&mut client, &current, Some(None), 121)
            .await
            .unwrap());
    }
}

#[tokio::test]
async fn failed_commit_does_not_report_success_or_persist_last_used() {
    let mut client = database().await;
    let selected = lookup(&mut client, None, "generated-client")
        .await
        .unwrap()
        .unwrap();
    client
        .batch_execute(
            "CREATE FUNCTION pg_temp.reject_completion() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'test commit failure'; END $$;
        CREATE CONSTRAINT TRIGGER reject_completion AFTER UPDATE ON subsonic_credentials
        DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.last_used_at IS NOT NULL)
        EXECUTE FUNCTION pg_temp.reject_completion()",
        )
        .await
        .unwrap();
    assert!(mutate(&mut client, &selected, Some(None), 120)
        .await
        .is_err());
    let untouched: bool = client
        .query_one("SELECT last_used_at IS NULL FROM subsonic_credentials", &[])
        .await
        .unwrap()
        .get(0);
    assert!(untouched);
}

#[tokio::test]
async fn schema_refuses_partial_extra_and_unscoped_material() {
    let client = database().await;
    for change in [
        "scope='native-rest'",
        "pepper_version=0",
        "api_key_hmac=NULL",
        "auth_mode='token'",
        "token_kek_version=1",
        "token_secret_encrypted=decode('aa','hex')",
        "auth_mode='invalid'",
        "subsonic_username='user@example.invalid'",
    ] {
        assert!(
            client
                .batch_execute(&format!("UPDATE subsonic_credentials SET {change}"))
                .await
                .is_err(),
            "{change}"
        );
    }
}
