//! Exact ingest freeze SQL against disposable PostgreSQL, including real locks.
use nntune_ingest::audio_manifest::AcceptedAudioManifest;
use nntune_ingest::audio_manifest_store::FREEZE_SQL;
use serde_json::json;
use tokio_postgres::{Client, Config, NoTls};

async fn client() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '10s'; SET search_path = public, pg_catalog")
        .await
        .unwrap();
    client
}

async fn fixture(client: &Client) -> (String, String) {
    let job = format!("manifest-race-{:x}", rand::random::<u64>());
    let owner = format!("{job}-owner");
    client
        .execute(
            "INSERT INTO users (id,email,created_at) VALUES ($1,$2,1700000000)",
            &[&owner, &format!("{owner}@example.invalid")],
        )
        .await
        .unwrap();
    client.execute("INSERT INTO ingest_jobs (id,user_id,idempotency_key,accepted_at,status,content_sha256) VALUES ($1,$2,'ingest:manifest:race',1700000000,'queued',repeat('a',64))", &[&job, &owner]).await.unwrap();
    client.execute("INSERT INTO ingest_originals (job_id,content_sha256,nzb_bytes,nzb_ref_encrypted,nzb_ref_envelope_version,nzb_ref_kek_version,nzb_ref_dek_wrapped_nonce,nzb_ref_dek_wrapped,nzb_ref_payload_nonce,nzb_ref_payload_format,created_at) VALUES ($1,repeat('a',64),1,'\\x00',2,1,decode(repeat('00',12),'hex'),decode(repeat('00',48),'hex'),decode(repeat('00',12),'hex'),1,1700000000)", &[&job]).await.unwrap();
    (job, owner)
}

fn manifest(job: &str, owner: &str, title: &str) -> String {
    let raw = json!({"version": 1, "job_id": job, "user_id": owner,
        "original_content_sha256": "a".repeat(64), "entries": [{
            "audio_ordinal": 1, "source_selector": {"nzb_file_ordinal": 0,
                "archive_member": null, "audio_selection_index": 0},
            "artist_group_ref": 1, "release_group_ref": 1, "hints": {"title": title}
        }], "art_blobs": []})
    .to_string();
    serde_json::to_string(&AcceptedAudioManifest::parse(&raw).unwrap()).unwrap()
}

async fn store(client: &Client, job: &str, owner: &str, raw: &str) -> Option<String> {
    client
        .query_opt(FREEZE_SQL, &[&job, &owner, &raw, &"a".repeat(64)])
        .await
        .unwrap()
        .map(|row| row.get(0))
}

async fn wait_blocked(observer: &Client, blocked: i32, blocker: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let pids: Vec<i32> = observer
                .query_one("SELECT pg_blocking_pids($1)", &[&blocked])
                .await
                .unwrap()
                .get(0);
            if pids.contains(&blocker) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("contender must actually block on the first transaction");
}

async fn pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}

async fn cleanup(client: &Client, job: &str, owner: &str) {
    client
        .execute("DELETE FROM ingest_originals WHERE job_id=$1", &[&job])
        .await
        .unwrap();
    client
        .execute("DELETE FROM ingest_jobs WHERE id=$1", &[&job])
        .await
        .unwrap();
    client
        .execute("DELETE FROM users WHERE id=$1", &[&owner])
        .await
        .unwrap();
}

#[tokio::test]
async fn first_manifest_wins_after_commit_but_not_after_rollback() {
    for commit in [true, false] {
        let first = client().await;
        let second = client().await;
        let observer = client().await;
        let (job, owner) = fixture(&first).await;
        let winner = manifest(&job, &owner, "First proposal");
        let contender = manifest(&job, &owner, "Changed tags");
        first.batch_execute("BEGIN").await.unwrap();
        assert_eq!(
            store(&first, &job, &owner, &winner).await,
            Some(winner.clone())
        );
        let (first_pid, second_pid) = (pid(&first).await, pid(&second).await);
        let (j, u, proposal) = (job.clone(), owner.clone(), contender.clone());
        let pending = tokio::spawn(async move { store(&second, &j, &u, &proposal).await });
        wait_blocked(&observer, second_pid, first_pid).await;
        first
            .batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
            .await
            .unwrap();
        let expected = if commit { winner } else { contender };
        assert_eq!(pending.await.unwrap(), Some(expected.clone()));
        let stored: String = first
            .query_one(
                "SELECT accepted_audio_manifest_v1 FROM ingest_originals WHERE job_id=$1",
                &[&job],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(stored, expected);
        cleanup(&first, &job, &owner).await;
    }
}

#[tokio::test]
async fn deletion_committed_before_freeze_refuses_the_waiting_writer() {
    let deleting = client().await;
    let freezing = client().await;
    let observer = client().await;
    let (job, owner) = fixture(&deleting).await;
    let proposal = manifest(&job, &owner, "Must not freeze");
    deleting.batch_execute("BEGIN").await.unwrap();
    deleting
        .execute("UPDATE users SET deleting_at=now() WHERE id=$1", &[&owner])
        .await
        .unwrap();
    let (deleting_pid, freezing_pid) = (pid(&deleting).await, pid(&freezing).await);
    let (j, u) = (job.clone(), owner.clone());
    let pending = tokio::spawn(async move { store(&freezing, &j, &u, &proposal).await });
    wait_blocked(&observer, freezing_pid, deleting_pid).await;
    deleting.batch_execute("COMMIT").await.unwrap();
    assert_eq!(pending.await.unwrap(), None);
    let stored: Option<String> = deleting
        .query_one(
            "SELECT accepted_audio_manifest_v1 FROM ingest_originals WHERE job_id=$1",
            &[&job],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(stored, None);
    cleanup(&deleting, &job, &owner).await;
}

#[tokio::test]
async fn frozen_writer_holds_the_owner_fence_until_commit() {
    let freezing = client().await;
    let deleting = client().await;
    let observer = client().await;
    let (job, owner) = fixture(&freezing).await;
    let proposal = manifest(&job, &owner, "Frozen first");
    freezing.batch_execute("BEGIN").await.unwrap();
    assert_eq!(
        store(&freezing, &job, &owner, &proposal).await,
        Some(proposal.clone())
    );
    let (freezing_pid, deleting_pid) = (pid(&freezing).await, pid(&deleting).await);
    let u = owner.clone();
    let pending = tokio::spawn(async move {
        deleting
            .execute("UPDATE users SET deleting_at=now() WHERE id=$1", &[&u])
            .await
            .unwrap()
    });
    wait_blocked(&observer, deleting_pid, freezing_pid).await;
    freezing.batch_execute("COMMIT").await.unwrap();
    assert_eq!(pending.await.unwrap(), 1);
    assert_eq!(store(&freezing, &job, &owner, &proposal).await, None);
    cleanup(&freezing, &job, &owner).await;
}
