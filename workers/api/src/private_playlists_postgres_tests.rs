use super::*;
use Command::*;

async fn connect() -> Client {
    let url = std::env::var("NNTUNE_TEST_PRIVATE_PLAYLIST_DATABASE_URL").expect("rehearsal target");
    let (db, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    assert_eq!(
        scalar(&db, "SELECT current_database()::text", &[]).await,
        "nntune_playlist_ci"
    );
    db.batch_execute("SET statement_timeout = '5s'")
        .await
        .unwrap();
    db
}

async fn scalar(db: &Client, sql: &str, args: &[&(dyn ToSql + Sync)]) -> String {
    db.query_one(sql, args).await.unwrap().get(0)
}

async fn run(db: &mut Client, owner: &str, cmd: Command<'_>) -> Option<Value> {
    execute(db, owner, cmd).await.unwrap()
}

#[tokio::test]
#[ignore = "run by prepare_migrated_stores.sh against its disposable clone"]
async fn postgres_store_rehearsal() {
    let mut db = connect().await;
    let mut peer = connect().await;
    let owner = "fixture-user-owner";
    let other = "fixture-user-other";
    // Reset the second account after the preceding SQL-only fence scenario.
    db.batch_execute("UPDATE users SET deleting_at = NULL WHERE id = 'fixture-user-other'")
        .await
        .unwrap();
    let created = run(&mut db, owner, Create("Draft playlist".into()))
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();
    assert!(handle_shape(id));
    let renamed = run(&mut db, owner, Rename(id, "Renamed".into())).await;
    assert_eq!(renamed, Some(json!({"id": id, "name": "Renamed"})));
    let track_id = "SELECT id::text FROM tracks WHERE title = $1";
    let track = scalar(&db, track_id, &[&"new-track"]).await;
    let foreign = scalar(&db, track_id, &[&"trkRpMiddle00000000000"]).await;
    for cmd in [
        Read(id),
        Rename(id, "Denied".into()),
        Append(id, track.clone()),
        Delete(id),
        Read("noSuchPlaylist00000000"),
    ] {
        assert!(run(&mut db, other, cmd).await.is_none());
    }
    for denied in [foreign, "00000000-0000-0000-0000-000000000000".into()] {
        assert!(run(&mut db, owner, Append(id, denied)).await.is_none());
    }
    let page = run(&mut db, owner, List(None, 1)).await.unwrap();
    let after = parse_page_cursor(page["next_cursor"].as_str().unwrap()).unwrap();
    let next = run(&mut db, owner, List(Some(after), 1)).await.unwrap();
    assert_ne!(page["items"][0]["id"], next["items"][0]["id"]);
    let legacy = run(&mut db, owner, Read("plRepeatedRefs00000000"))
        .await
        .unwrap();
    assert_eq!(legacy["legacy_items"].as_array().unwrap().len(), 4);
    assert_eq!(legacy["entries"][1]["dormant"], true);
    assert!(legacy["entries"][1].get("track_id").is_none());
    let entries = legacy["entries"].as_array().unwrap();
    assert_eq!(entries[0]["track_id"], entries[3]["track_id"]);

    let (first, second) = tokio::join!(
        run(&mut db, owner, Append(id, track.clone())),
        run(&mut peer, owner, Append(id, track.clone()))
    );
    assert_ne!(first.is_some(), second.is_some());
    assert!(run(&mut db, owner, Append(id, track)).await.is_none());
    let detail = run(&mut db, owner, Read(id)).await.unwrap();
    assert_eq!(detail["entries"].as_array().unwrap().len(), 1);
    let deleted = run(&mut db, owner, Delete(id)).await.unwrap();
    assert!(deleted["operation_id"].is_string());
    assert_eq!(run(&mut db, owner, Delete(id)).await, Some(deleted));
    assert!(run(&mut db, owner, Read(id)).await.is_none());

    let clean_id = "plRepeatedRefs00000000";
    db.batch_execute("ALTER TABLE delete_operations ADD CHECK (scope <> 'playlist') NOT VALID")
        .await
        .unwrap();
    assert!(execute(&mut db, owner, Delete(clean_id)).await.is_err());
    assert!(run(&mut db, owner, Read(clean_id)).await.is_some());
}
