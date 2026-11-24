use super::*;
use private_playlists::{route, Command::*};
use serde_json::{json, Value};

#[path = "../support/blocker_observer.rs"]
mod blocker;

pub(super) fn assert_denied(response: Response) {
    let denied = not_found();
    assert_eq!(
        (response.status, response.body),
        (denied.status, denied.body)
    );
}

fn body(response: Response, status: u16) -> Value {
    assert_eq!(response.status, status, "{}", response.body);
    serde_json::from_str(&response.body).unwrap()
}

pub(super) async fn exercise(
    db: &mut Client,
    ctx: &Ctx<'_, impl Store + SingleUseStore, impl Mailer, impl Dialer, impl AsyncDns>,
    request: &Request,
    unrelated: &Request,
    track: &str,
) -> String {
    db.batch_execute(include_str!("private_playlist_tables.sql"))
        .await
        .unwrap();
    let owner = Request {
        origin: Some(ORIGIN.into()),
        ..request.clone()
    };
    let foreign = Request {
        origin: owner.origin.clone(),
        ..unrelated.clone()
    };
    let created = body(route(ctx, &owner, db, Create("Private".into())).await, 201);
    let playlist = created["id"].as_str().unwrap();
    let attempt = || Append(playlist, track.to_owned());

    // The browser already returns this exact track. The owned Library starts empty.
    let active: String = db
        .query_one_scalar(
            "INSERT INTO public.libraries (user_id, name) VALUES ($1, 'Active') RETURNING id::text",
            &[&RECIPIENT],
        )
        .await
        .unwrap();
    assert_denied(route(ctx, &owner, db, attempt()).await);

    // Model independent normalized memberships, including a foreign and a fenced Library.
    db.execute(
        "WITH seeded AS (
            INSERT INTO public.libraries (user_id, name, deleting_at)
            VALUES ($1, 'Foreign', NULL), ($2, 'Deleting', now()) RETURNING id
         )
         INSERT INTO public.library_tracks (library_id, track_id)
         SELECT id, $3::text::uuid FROM seeded",
        &[&OTHER_MEMBER, &RECIPIENT, &track],
    )
    .await
    .unwrap();
    assert_denied(route(ctx, &owner, db, attempt()).await);
    assert_denied(route(ctx, &foreign, db, attempt()).await);
    assert_denied(route(ctx, &foreign, db, Read(playlist)).await);
    let detail = body(route(ctx, &owner, db, Read(playlist)).await, 200);
    assert_eq!(detail["entries"], json!([]));

    db.execute(
        "INSERT INTO public.library_tracks (library_id, track_id)
         VALUES ($1::text::uuid, $2::text::uuid)",
        &[&active, &track],
    )
    .await
    .unwrap();
    let appended = body(route(ctx, &owner, db, attempt()).await, 201);
    let detail = body(route(ctx, &owner, db, Read(playlist)).await, 200);
    assert_eq!(
        detail["entries"],
        json!([{"id": appended["id"], "position": 0, "track_id": track}])
    );
    assert_denied(route(ctx, &owner, db, attempt()).await);
    assert_eq!(
        body(route(ctx, &owner, db, Read(playlist)).await, 200),
        detail
    );

    // Hold each kind of membership loss open while another append starts.
    let pending = body(route(ctx, &owner, db, Create("Pending".into())).await, 201);
    let pending_id = pending["id"].as_str().unwrap();
    let mut peer = connect().await;
    let mut remover = connect().await;
    let pid = "SELECT pg_backend_pid()";
    let holder = remover.query_one_scalar(pid, &[]).await.unwrap();
    let waiter = peer.query_one_scalar(pid, &[]).await.unwrap();
    let fence = "UPDATE public.libraries SET deleting_at = now() WHERE id = $1::text::uuid";
    let remove = "DELETE FROM public.library_tracks WHERE library_id = $1::text::uuid";
    for change in [fence, remove] {
        if change == remove {
            // Restore eligibility before exercising removal of the membership row.
            let clear = "UPDATE public.libraries SET deleting_at = NULL WHERE id = $1::text::uuid";
            assert_eq!(db.execute(clear, &[&active]).await.unwrap(), 1);
            assert_eq!(
                body(route(ctx, &owner, db, Read(playlist)).await, 200),
                detail
            );
        }
        let tx = remover.transaction().await.unwrap();
        assert_eq!(tx.execute(change, &[&active]).await.unwrap(), 1);
        let late = route(ctx, &owner, &mut peer, Append(pending_id, track.to_owned()));
        tokio::pin!(late);
        blocker::observe_blocked(db, holder, waiter, late.as_mut())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_denied(late.await);
        let pending = body(route(ctx, &owner, db, Read(pending_id)).await, 200);
        assert_eq!(pending["entries"], json!([]));
        let dormant = body(route(ctx, &owner, db, Read(playlist)).await, 200);
        assert_eq!(
            dormant["entries"],
            json!([{"id": appended["id"], "position": 0, "dormant": true}])
        );
    }

    let deleted = body(route(ctx, &owner, db, Delete(playlist)).await, 202);
    assert!(deleted["operation_id"].is_string());
    assert_eq!(
        body(route(ctx, &owner, db, Delete(playlist)).await, 202),
        deleted
    );
    assert_denied(route(ctx, &foreign, db, Delete(playlist)).await);
    pending_id.to_owned()
}
