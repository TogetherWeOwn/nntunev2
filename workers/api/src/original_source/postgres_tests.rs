use super::super::postgres::PostgresSourceRepository;
use tokio_postgres::{Client, NoTls};

async fn connection() -> Client {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("disposable PostgreSQL URL");
    let config: tokio_postgres::Config = url.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(!config.get_hosts().is_empty());
    assert!(config.get_hosts().iter().all(|host| match host {
        tokio_postgres::config::Host::Tcp(host) =>
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()),
        _ => false,
    }));
    assert!(config.get_hostaddrs().iter().all(|ip| ip.is_loopback()));
    let (client, driver) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { driver.await.expect("disposable source connection") });
    // Exact production column types, but deliberately no constraints, to
    // exercise corrupt metadata. TEMP rows disappear with this connection.
    // This is query/codec conformance, not a migration or Hyperdrive proof.
    // Dropping a TEMP shadow must never expose an existing persistent table.
    client
        .batch_execute(
            "SET search_path=pg_temp,pg_catalog;
        CREATE TEMP TABLE track_sources (
        track_id UUID, source_job_id TEXT, nzb_ref_envelope_version SMALLINT,
        nzb_ref_payload_format SMALLINT, nzb_ref_kek_version INTEGER,
        nzb_ref_dek_wrapped_nonce BYTEA, nzb_ref_dek_wrapped BYTEA,
        nzb_ref_payload_nonce BYTEA, nzb_ref_encrypted BYTEA)",
        )
        .await
        .unwrap();
    client
}

async fn insert(client: &Client, source: &SealedSource) {
    client
        .execute(
            "INSERT INTO track_sources VALUES (
        $1::text::uuid, $2, $3, $4, $5, $6, $7, $8, $9)",
            &[
                &source.metadata.track_id,
                &source.metadata.source_job_id,
                &source.metadata.envelope_version,
                &source.metadata.payload_format,
                &source.metadata.kek_version,
                &source.envelope.dek_wrapped_nonce.as_slice(),
                &source.envelope.dek_wrapped,
                &source.envelope.payload_nonce.as_slice(),
                &source.envelope.ciphertext,
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_source_real_queries_and_int4_parameters_round_trip() {
    let mut client = connection().await;
    let source = prepare(A, 1, JOB);
    insert(&client, &seal_original_source(&Keys, 7, &source).unwrap()).await;
    let mut repository = PostgresSourceRepository::new(&client);
    let read = read_original_source_async(&Allow, &mut repository, &Keys, A)
        .await
        .unwrap();
    assert_eq!(read.encode().unwrap(), source.encode().unwrap());
    assert!(matches!(
        read_original_source_async(&Allow, &mut repository, &Keys, B).await,
        Err(SourceServiceError::Missing)
    ));
    // The same executor works inside an API-owned transaction too.
    let transaction = client.transaction().await.unwrap();
    let mut repository = PostgresSourceRepository::new(&transaction);
    assert!(
        read_original_source_async(&Allow, &mut repository, &Keys, A)
            .await
            .is_ok()
    );
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn postgres_source_snapshot_changes_are_not_retried() {
    let client = connection().await;
    insert(
        &client,
        &seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap(),
    )
    .await;
    let mut repository = PostgresSourceRepository::new(&client);
    let [Some(selected), None] = repository.metadata(A).await.unwrap() else {
        panic!("one row")
    };
    for change in [
        "nzb_ref_kek_version = 8",
        "source_job_id = 'job_other'",
        "nzb_ref_encrypted = nzb_ref_encrypted || decode('00','hex')",
    ] {
        client.batch_execute("BEGIN").await.unwrap();
        client
            .batch_execute(&format!("UPDATE track_sources SET {change}"))
            .await
            .unwrap();
        assert!(repository.envelope(&selected).await.unwrap().is_none());
        client.batch_execute("ROLLBACK").await.unwrap();
    }
}

#[tokio::test]
async fn postgres_source_sql_guards_never_materialize_corrupt_blobs() {
    let client = connection().await;
    insert(
        &client,
        &seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap(),
    )
    .await;
    let mut repository = PostgresSourceRepository::new(&client);
    let [Some(selected), None] = repository.metadata(A).await.unwrap() else {
        panic!("one row")
    };
    for change in [
        "nzb_ref_encrypted = decode(repeat('00',8388625),'hex')",
        "nzb_ref_encrypted = decode(repeat('00',16),'hex')",
        "nzb_ref_dek_wrapped_nonce = decode(repeat('00',13),'hex')",
        "nzb_ref_dek_wrapped = decode(repeat('00',49),'hex')",
        "nzb_ref_payload_nonce = decode(repeat('00',11),'hex')",
        "nzb_ref_payload_format = 2",
        "nzb_ref_envelope_version = 1",
        "source_job_id = repeat('x',65)",
        "source_job_id = NULL",
        "nzb_ref_kek_version = NULL",
    ] {
        client.batch_execute("BEGIN").await.unwrap();
        client
            .batch_execute(&format!("UPDATE track_sources SET {change}"))
            .await
            .unwrap();
        assert!(
            read_original_source_async(&Allow, &mut repository, &Keys, A)
                .await
                .is_err(),
            "{change}"
        );
        assert!(
            repository.envelope(&selected).await.unwrap().is_none(),
            "{change}"
        );
        client.batch_execute("ROLLBACK").await.unwrap();
    }
    let invalid = SourceMetadata {
        ciphertext_len: i32::MAX,
        ..selected
    };
    assert!(matches!(
        repository.envelope(&invalid).await,
        Err(SourceServiceError::Invalid)
    ));
}

#[tokio::test]
async fn postgres_source_denial_duplicate_unknown_key_and_tamper_fail_closed() {
    let client = connection().await;
    let sealed = seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap();
    insert(&client, &sealed).await;
    let mut repository = PostgresSourceRepository::new(&client);
    insert(&client, &sealed).await;
    assert!(matches!(
        read_original_source_async(&Allow, &mut repository, &Keys, A).await,
        Err(SourceServiceError::Invalid)
    ));
    client
        .batch_execute("DELETE FROM track_sources")
        .await
        .unwrap();
    insert(&client, &sealed).await;
    client
        .batch_execute("UPDATE track_sources SET nzb_ref_kek_version=8")
        .await
        .unwrap();
    assert!(matches!(
        read_original_source_async(&Allow, &mut repository, &Keys, A).await,
        Err(SourceServiceError::UnknownKey)
    ));
    client.batch_execute("UPDATE track_sources SET nzb_ref_kek_version=7, nzb_ref_encrypted=set_byte(nzb_ref_encrypted,0,1-get_byte(nzb_ref_encrypted,0)%2)").await.unwrap();
    assert!(matches!(
        read_original_source_async(&Allow, &mut repository, &Keys, A).await,
        Err(SourceServiceError::Crypto)
    ));
    client
        .batch_execute("DROP TABLE track_sources")
        .await
        .unwrap();
    assert!(matches!(
        read_original_source_async(&Deny, &mut repository, &Keys, A).await,
        Err(SourceServiceError::Denied)
    ));
    assert!(matches!(
        read_original_source_async(&Allow, &mut repository, &Keys, A).await,
        Err(SourceServiceError::Store)
    ));
    // Malformed identity is rejected before PostgreSQL's UUID cast.
    assert!(matches!(
        repository.metadata("invalid").await,
        Err(SourceServiceError::Invalid)
    ));
}
