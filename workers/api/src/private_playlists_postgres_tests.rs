use super::*;
use crate::library::parse_page_cursor;
use private_playlists::{execute, route, Command};
use serde_json::{json, Value};
use tokio_postgres::types::ToSql;
use tokio_postgres::{types::FromSqlOwned, Client};
use Command::*;

async fn connect() -> Client {
    let port = std::env::var("NNTUNE_TEST_PRIVATE_PLAYLIST_PORT").expect("fixture port");
    let port: u16 = port.parse().expect("numeric fixture port");
    assert_ne!(port, 0);
    let url = format!(
        "host=127.0.0.1 port={port} user=postgres password=ci-throwaway \
        dbname=nntune_playlist_ci connect_timeout=5 options='-c statement_timeout=5000'"
    );
    let (db, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    db
}

async fn scalar<T: FromSqlOwned>(db: &Client, sql: &str, args: &[&(dyn ToSql + Sync)]) -> T {
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
    let clean_id = "plRepeatedRefs00000000";
    let unresolved = "plAllUnresolved0000000";
    let create = Create("Draft playlist".into());
    let created = run(&mut db, owner, create).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let renamed = run(&mut db, owner, Rename(id, "Renamed".into())).await;
    assert_eq!(renamed, Some(json!({"id": id, "name": "Renamed"})));
    let track_id = "SELECT id::text FROM tracks WHERE title = $1";
    let track: String = scalar(&db, track_id, &[&"new-track"]).await;
    let foreign = scalar(&db, track_id, &[&"trkRpMiddle00000000000"]).await;
    let another = scalar(&db, track_id, &[&"trkFrAlpha000000000000"]).await;
    let repeat: String = scalar(&db, track_id, &[&"trkRpRepeat00000000000"]).await;
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
    let set_fence =
        "UPDATE libraries SET deleting_at = CASE WHEN $2 THEN now() END WHERE user_id = $1";
    assert_eq!(db.execute(set_fence, &[&owner, &true]).await.unwrap(), 1);
    let inactive = Append(id, track.clone());
    assert!(run(&mut db, owner, inactive).await.is_none());
    assert_eq!(db.execute(set_fence, &[&owner, &false]).await.unwrap(), 1);
    let page = run(&mut db, owner, List(None, 1)).await.unwrap();
    let after = parse_page_cursor(page["next_cursor"].as_str().unwrap()).unwrap();
    let next = run(&mut db, owner, List(Some(after), 50)).await.unwrap();
    let mut ids = [id, clean_id, unresolved, "plFullyResolved0000000"];
    ids.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(page["items"][0]["id"], ids[0]);
    let tail = next["items"].as_array().unwrap();
    assert_eq!(tail.len(), ids.len() - 1);
    for (item, id) in tail.iter().zip(&ids[1..]) {
        assert_eq!(item["id"], *id);
    }
    let legacy = run(&mut db, owner, Read(clean_id)).await.unwrap();
    assert_eq!(legacy["legacy_items"].as_array().unwrap().len(), 4);
    assert_eq!(legacy["entries"][1]["dormant"], true);
    assert!(legacy["entries"][1].get("track_id").is_none());
    let entries = legacy["entries"].as_array().unwrap();
    assert_eq!(entries[0]["track_id"], repeat);
    assert_eq!(entries[3]["track_id"], repeat);
    assert_eq!(entries[1]["position"], 1);
    assert!(entries[1]["id"].is_string());
    let (first, second) = tokio::join!(
        run(&mut db, owner, Append(id, track.clone())),
        run(&mut peer, owner, Append(id, track.clone()))
    );
    assert_ne!(first.is_some(), second.is_some());
    assert_eq!(first.or(second).unwrap()["position"], 0);
    let (first, second) = tokio::join!(
        run(&mut db, owner, Append(unresolved, track.clone())),
        run(&mut peer, owner, Append(unresolved, another))
    );
    assert!(first.is_some() && second.is_some());
    let detail = run(&mut db, owner, Read(unresolved)).await.unwrap();
    assert_eq!(
        detail["legacy_items"],
        json!(["legacy-ref-au-unknown-1", "legacy-ref-au-unknown-2"])
    );
    let entries = detail["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["position"], 2);
    assert_eq!(entries[1]["position"], 3);
    let deleted = run(&mut db, owner, Delete(id)).await.unwrap();
    assert!(deleted["operation_id"].is_string());
    assert_eq!(run(&mut db, owner, Delete(id)).await, Some(deleted.clone()));
    assert!(run(&mut db, owner, Read(id)).await.is_none());
    let complete = "SELECT private_playlist_rehearsal.complete_playlist_delete($1, $2)";
    db.query_one(complete, &[&owner, &id]).await.unwrap();
    assert_eq!(run(&mut db, owner, Delete(id)).await, Some(deleted));
    assert!(run(&mut db, other, Delete(id)).await.is_none());

    db.batch_execute("ALTER TABLE delete_operations ADD CHECK (scope <> 'playlist') NOT VALID")
        .await
        .unwrap();
    let h = harness();
    let email = "playlist-owner@example.com";
    seed_user(&h, owner, email);
    let cookie = magic_login(&h, email);
    for (session, origin, target, expected) in [
        (None, ORIGIN, clean_id, generic_auth()),
        (Some(cookie.clone()), "https://wrong.example", clean_id, generic_auth()),
        (Some(cookie.clone()), ORIGIN, "noSuchPlaylist00000000", not_found()),
        (Some(cookie), ORIGIN, clean_id, unavailable()),
    ] {
        let req = Request {
            method: "DELETE".into(), path: format!("/v1/playlists/{target}"), cookie: session,
            origin: Some(origin.into()), authorization: None, stripe_signature: None, body: Vec::new(),
        };
        let result = route(&h.ctx(), &req, &mut db, Delete(target)).await;
        assert_eq!((result.status, result.body), (expected.status, expected.body));
    }
    assert!(run(&mut db, owner, Read(clean_id)).await.is_some());

    let writer_pid: i32 = scalar(&peer, "SELECT pg_backend_pid()", &[]).await;
    let playlist_fence = "UPDATE playlists SET deleting_at = now() WHERE public_id = $1";
    let account_fence = "UPDATE users SET deleting_at = now() WHERE id = $1";
    for (sql, key, target) in [(playlist_fence, clean_id, clean_id),
        (account_fence, owner, "plFullyResolved0000000")] {
        let tx = db.transaction().await.unwrap();
        tx.execute(sql, &[&key]).await.unwrap();
        let finish_fence = async {
            let wait = "SELECT private_playlist_rehearsal.await_blocked($1)";
            tx.query_one(wait, &[&writer_pid]).await.unwrap();
            tx.commit().await.unwrap();
        };
        let append = Append(target, track.clone());
        let ((), late) = tokio::join!(finish_fence, run(&mut peer, owner, append));
        assert!(late.is_none());
    }
}
