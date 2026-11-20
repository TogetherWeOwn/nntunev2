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

fn grant_play_id(number: u8) -> String {
    crate::b64::encode(&[
        0x40, 0xc3, 0x25, 0xc0, 0x2c, 0xee, 0x48, 0x9d, 0xa8, 0xb6, 0xfb, 0xe9, 0x37, 0x15, 0x56,
        number,
    ])
}

async fn seed_owner(client: &Client, owner: &str) {
    client
        .execute(
            "INSERT INTO public.users (id,email,created_at) VALUES ($1,$2,1000)",
            &[&owner, &format!("{owner}@example.test")],
        )
        .await
        .unwrap();
}

async fn seed_lease(client: &Client, owner: &str, number: u8) {
    client
        .execute(
            "INSERT INTO public.playback_leases (user_id,session_id,track_id,play_id,epoch,expires_at,updated_at)
             VALUES ($1,'tab','catalogue-42',$2,1,1045,1000)
             ON CONFLICT (user_id) DO UPDATE SET play_id = EXCLUDED.play_id",
            &[&owner, &grant_play_id(number)],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn history_uses_the_grant_privacy_decision() {
    use crate::playback::LeaseAcquire;
    use crate::worker_entry::private_aware_lease_for_postgres_test;
    use ListenEvent::{Complete, Skip, Start};
    use WriteOutcome::{Conflict, Written};

    let mut client = connect().await;
    let owner = format!("listen-privacy-{:x}", rand::random::<u64>());
    seed_owner(&client, &owner).await;
    let owner = owner.as_str();
    let connection = &client;
    let set_private = |on| async move {
        if on {
            connection
                .execute(
                    "INSERT INTO public.private_sessions (user_id,session_id,expires_at)
                 VALUES ($1,'tab',EXTRACT(EPOCH FROM now())::bigint + 300)",
                    &[&owner],
                )
                .await
                .unwrap();
        } else {
            connection
                .execute(
                    "DELETE FROM public.private_sessions WHERE user_id = $1",
                    &[&owner],
                )
                .await
                .unwrap();
        }
    };
    let acquire = |number| async move {
        let result = private_aware_lease_for_postgres_test(
            connect().await,
            owner,
            "tab",
            "catalogue-42",
            &grant_play_id(number),
            1000,
            1045,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            LeaseAcquire::Acquired {
                epoch: u64::from(number)
            }
        );
    };
    let mut writer = connect().await;
    assert_eq!(write(&mut writer, owner, event(1, Start)).await, Conflict);
    set_private(true).await;
    acquire(1).await;
    assert_eq!(write(&mut writer, owner, event(1, Start)).await, Conflict);
    set_private(false).await;
    for phase in [Start, Skip, Complete] {
        assert_eq!(write(&mut writer, owner, event(1, phase)).await, Conflict);
    }
    assert!(export(&client, owner, None).await.unwrap().is_empty());

    acquire(2).await;
    assert_eq!(write(&mut writer, owner, event(2, Start)).await, Written);
    set_private(true).await;
    acquire(3).await;
    assert_eq!(write(&mut writer, owner, event(2, Start)).await, Written);
    assert_eq!(write(&mut writer, owner, event(2, Complete)).await, Written);
    assert_eq!(write(&mut writer, owner, event(3, Start)).await, Conflict);
    let rows = export(&client, owner, None).await.unwrap();
    assert_eq!(
        rows.iter().map(|row| row.event).collect::<Vec<_>>(),
        [Complete, Start]
    );
    assert!(rows
        .iter()
        .all(|row| row.playback_id == event(2, Start).playback_id));
    let lease = client
        .query_one(
            "SELECT play_id, epoch, expires_at FROM public.playback_leases WHERE user_id = $1",
            &[&owner],
        )
        .await
        .unwrap();
    assert_eq!(lease.get::<_, &str>(0), grant_play_id(3));
    assert_eq!(lease.get::<_, i64>(1), 3);
    assert_eq!(lease.get::<_, i64>(2), 1045);
    client
        .execute("DELETE FROM public.users WHERE id = $1", &[&owner])
        .await
        .unwrap();
    first_admission_requires_each_lease_predicate().await;
}

async fn pid(client: &Client) -> i32 {
    let row = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap();
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
    seed_owner(&clearer, &owner).await;
    seed_owner(&clearer, &other).await;
    seed_lease(&clearer, &owner, 1).await;
    seed_lease(&clearer, &other, 2).await;
    assert_eq!(write(&mut writer, &owner, event(1, Start)).await, Written);
    let before = export(&clearer, &owner, None).await.unwrap();
    assert_eq!(write(&mut writer, &other, event(2, Start)).await, Written);
    assert_eq!(export(&clearer, &owner, None).await.unwrap(), before);
    let other_rows = export(&clearer, &other, None).await.unwrap();
    let other_cursor = other_rows[0].cursor();
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
    assert_eq!(export(&clearer, &other, None).await.unwrap(), other_rows);
    seed_lease(&clearer, &owner, 3).await;
    for phase in [Start, Complete] {
        let body = event(3, phase);
        assert_eq!(write(&mut writer, &owner, body.clone()).await, Written);
        assert_eq!(
            record(&mut writer, &owner, &body, 2000).await.unwrap(),
            Written
        );
    }
    assert_eq!(
        write(&mut writer, &owner, event(3, ListenEvent::Skip)).await,
        Conflict
    );
    let replayed_rows = export(&clearer, &owner, None).await.unwrap();
    assert_eq!(
        replayed_rows
            .iter()
            .map(|row| (row.event, row.recorded_at))
            .collect::<Vec<_>>(),
        [(Complete, 1000), (Start, 1000)]
    );

    for number in 4..=104 {
        seed_lease(&clearer, &owner, number).await;
        assert_eq!(
            write(&mut writer, &owner, event(number, Start)).await,
            Written
        );
    }
    let rows = export(&clearer, &owner, None).await.unwrap();
    assert_eq!(rows.len(), EXPORT_PAGE_SIZE + 1);
    let cursor = ExportCursor::parse(Some(&rows[EXPORT_PAGE_SIZE - 1].cursor())).unwrap();
    let rest = export(&clearer, &owner, cursor.as_ref()).await.unwrap();
    assert_eq!(
        rows[EXPORT_PAGE_SIZE].playback_id,
        event(4, Start).playback_id
    );
    assert_eq!(
        rest,
        [&rows[EXPORT_PAGE_SIZE..], &replayed_rows[..]].concat()
    );

    seed_lease(&clearer, &owner, 105).await;
    clearer
        .execute(
            "UPDATE public.users SET deleting_at = now() WHERE id = $1",
            &[&owner],
        )
        .await
        .unwrap();
    let result = write(&mut writer, &owner, event(105, Start)).await;
    assert_eq!(result, AccountDeleting);
    let cleanup = "DELETE FROM public.users WHERE id = $1";
    clearer.execute(cleanup, &[&owner]).await.unwrap();
    assert!(export(&clearer, &owner, None).await.unwrap().is_empty());
    assert_eq!(export(&clearer, &other, None).await.unwrap(), other_rows);
    clearer.execute(cleanup, &[&other]).await.unwrap();
}

async fn first_admission_requires_each_lease_predicate() {
    use ListenEvent::Start;
    use WriteOutcome::{Conflict, Written};
    let mut client = connect().await;
    let suffix = rand::random::<u64>();
    let owner = format!("listen-{suffix:x}-nolease");
    let other = format!("listen-{suffix:x}-lease");
    seed_owner(&client, &owner).await;
    seed_owner(&client, &other).await;
    let other = other.as_str();

    // Owner: only another account holds the lease.
    seed_lease(&client, other, 1).await;
    assert_eq!(write(&mut client, &owner, event(1, Start)).await, Conflict);
    assert_eq!(write(&mut client, other, event(1, Start)).await, Written);
    // Track: same lease and play id, different catalogue track.
    seed_lease(&client, other, 2).await;
    let mut wrong_track = event(2, Start);
    wrong_track.track_id = "catalogue-43".into();
    assert_eq!(write(&mut client, other, wrong_track).await, Conflict);
    assert_eq!(write(&mut client, other, event(2, Start)).await, Written);
    // Play id: lease still names playback 2.
    assert_eq!(write(&mut client, other, event(3, Start)).await, Conflict);
    seed_lease(&client, other, 3).await;
    assert_eq!(write(&mut client, other, event(3, Start)).await, Written);
    // Expiry: the lease expires at 1045 and must be strictly in the future.
    seed_lease(&client, other, 4).await;
    let expired = record(&mut client, other, &event(4, Start), 1045).await;
    assert_eq!(expired.unwrap(), Conflict);
    let live = record(&mut client, other, &event(4, Start), 1044).await;
    assert_eq!(live.unwrap(), Written);

    assert!(export(&client, &owner, None).await.unwrap().is_empty());
    assert_eq!(export(&client, other, None).await.unwrap().len(), 4);
    let cleanup = "DELETE FROM public.users WHERE id IN ($1, $2)";
    client.execute(cleanup, &[&owner, &other]).await.unwrap();
}
