use super::*;
use std::time::Duration;
use tokio_postgres::{config::Host, Config, NoTls};

async fn connect() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("requires the disposable Actions database")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty());
    assert!(matches!(config.get_hosts(), [Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")));
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '10s'")
        .await
        .unwrap();
    client
}

fn event(playback_number: u8, event: ListenEvent) -> ListenEventBody {
    ListenEventBody {
        playback_id: format!("40c325c0-2cee-489d-a8b6-fbe9371556{playback_number:02x}"),
        track_id: "catalogue-42".into(),
        event,
    }
}

async fn pid(client: &Client) -> i32 {
    let row = client.query_one("SELECT pg_backend_pid()", &[]).await.unwrap();
    row.get(0)
}

async fn write(client: &mut Client, owner: &str, body: ListenEventBody) -> WriteOutcome {
    record(client, owner, &body, 1000).await.unwrap()
}

#[tokio::test]
async fn clear_and_terminal_write_share_owner_fence() {
    use ListenEvent::{Complete, Start};
    use WriteOutcome::{AccountDeleting, Conflict, Written};
    let mut clearer = connect().await;
    let mut writer = connect().await;
    let (writer_pid, clearer_pid) = (pid(&writer).await, pid(&clearer).await);
    let suffix = rand::random::<u64>();
    let owner = format!("listen-{suffix:x}-owner");
    let other = format!("listen-{suffix:x}-other");
    for id in [&owner, &other] {
        clearer
            .execute(
                "INSERT INTO public.users (id,email,created_at) VALUES ($1,$2,1000)",
                &[id, &format!("{id}@example.test")],
            )
            .await
            .unwrap();
    }
    assert_eq!(write(&mut writer, &owner, event(1, Start)).await, Written);
    let before = export(&clearer, &owner, None).await.unwrap();
    assert_eq!(write(&mut writer, &other, event(2, Start)).await, Written);
    assert_eq!(export(&clearer, &owner, None).await.unwrap(), before);
    let other_cursor = export(&clearer, &other, None).await.unwrap()[0].cursor();
    let cursor = ExportCursor::parse(Some(&other_cursor)).unwrap();
    let foreign_page = export(&clearer, &owner, cursor.as_ref()).await.unwrap();
    assert!(foreign_page.is_empty());

    let tx = begin_write(&mut clearer).await.unwrap();
    clear_in_transaction(&tx, &owner).await.unwrap();
    let write_owner = owner.clone();
    let terminal = tokio::spawn(async move {
        let result = write(&mut writer, &write_owner, event(1, Complete)).await;
        (writer, result)
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blockers: Vec<i32> = tx
                .query_one("SELECT pg_blocking_pids($1)", &[&writer_pid])
                .await
                .unwrap()
                .get(0);
            if blockers.contains(&clearer_pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("terminal must wait on the clearing transaction");
    tx.commit().await.unwrap();
    let (mut writer, result) = terminal.await.unwrap();
    assert_eq!(result, Conflict);
    assert!(export(&clearer, &owner, None).await.unwrap().is_empty());
    for phase in [Start, Complete] {
        assert_eq!(write(&mut writer, &owner, event(3, phase)).await, Written);
    }
    let rows = export(&clearer, &owner, None).await.unwrap();
    let cursor = ExportCursor::parse(Some(&rows[0].cursor())).unwrap();
    assert_eq!(
        export(&clearer, &owner, cursor.as_ref()).await.unwrap(),
        rows[1..]
    );

    clearer
        .execute(
            "UPDATE public.users SET deleting_at = now() WHERE id = $1",
            &[&owner],
        )
        .await
        .unwrap();
    let result = write(&mut writer, &owner, event(4, Start)).await;
    assert_eq!(result, AccountDeleting);
    let cleanup = "DELETE FROM public.users WHERE id IN ($1, $2)";
    clearer.execute(cleanup, &[&owner, &other]).await.unwrap();
}
