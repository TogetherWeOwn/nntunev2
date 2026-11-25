use super::*;
use private_playlists::{route, Command::*};
use serde_json::{json, Value};
use tokio_postgres::{Error, Transaction};

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

// Fixture-only removal contract. Take the account fence before the owned
// Library fence, then take the membership row's delete lock. Append holds
// a share lock on that same membership until it commits.
async fn stage_removal(
    tx: &Transaction<'_>,
    owner: &str,
    library: &str,
    track: &str,
) -> Result<u64, Error> {
    let account = tx
        .query_opt(
            "SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE",
            &[&owner],
        )
        .await?;
    if account.is_none() {
        return Ok(0);
    }
    let library_lock = tx
        .query_opt(
            "SELECT id FROM public.libraries
             WHERE user_id = $1 AND id = $2::text::uuid AND deleting_at IS NULL FOR SHARE",
            &[&owner, &library],
        )
        .await?;
    if library_lock.is_none() {
        return Ok(0);
    }
    tx.execute(
        "DELETE FROM public.library_tracks WHERE library_id = $1::text::uuid AND track_id = $2::text::uuid",
        &[&library, &track],
    )
    .await
}

async fn seed_membership(db: &Client, library: &str, track: &str) {
    db.execute(
        "INSERT INTO public.library_tracks (library_id, track_id)
         VALUES ($1::text::uuid, $2::text::uuid)",
        &[&library, &track],
    )
    .await
    .unwrap();
}

async fn remove_membership(db: &mut Client, owner: &str, library: &str, track: &str) -> u64 {
    let tx = db.transaction().await.unwrap();
    let removed = stage_removal(&tx, owner, library, track).await.unwrap();
    tx.commit().await.unwrap();
    removed
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

    seed_membership(db, &active, track).await;
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

    let alternate: String = db
        .query_one_scalar(
            "INSERT INTO public.libraries (user_id, name) VALUES ($1, 'Alternate') RETURNING id::text",
            &[&RECIPIENT],
        )
        .await
        .unwrap();
    seed_membership(db, &alternate, track).await;
    assert_eq!(
        remove_membership(db, OTHER_MEMBER, &alternate, track).await,
        0
    );
    assert_eq!(remove_membership(db, RECIPIENT, &alternate, track).await, 1);
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
    for removing in [false, true] {
        if removing {
            // Restore eligibility before exercising removal of the membership row.
            let clear = "UPDATE public.libraries SET deleting_at = NULL WHERE id = $1::text::uuid";
            assert_eq!(db.execute(clear, &[&active]).await.unwrap(), 1);
            assert_eq!(
                body(route(ctx, &owner, db, Read(playlist)).await, 200),
                detail
            );
        }
        let tx = remover.transaction().await.unwrap();
        let changed = if removing {
            stage_removal(&tx, RECIPIENT, &active, track).await.unwrap()
        } else {
            tx.execute(fence, &[&active]).await.unwrap()
        };
        assert_eq!(changed, 1);
        let late = route(ctx, &owner, &mut peer, Append(pending_id, track.to_owned()));
        tokio::pin!(late);
        blocker::observe_blocked(db, holder, waiter, late.as_mut())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_denied(late.await);
        assert_denied(route(ctx, &owner, db, Append(pending_id, track.to_owned())).await);
        let pending = body(route(ctx, &owner, db, Read(pending_id)).await, 200);
        assert_eq!(pending["entries"], json!([]));
        let dormant = body(route(ctx, &owner, db, Read(playlist)).await, 200);
        assert_eq!(
            dormant["entries"],
            json!([{"id": appended["id"], "position": 0, "dormant": true}])
        );
    }

    // Seed a fresh case with one qualifying membership. The track row's
    // foreign-key lock stalls the insert after append locks that membership.
    assert_eq!(remove_membership(db, RECIPIENT, &active, track).await, 0);
    seed_membership(db, &active, track).await;
    let first = body(
        route(ctx, &owner, db, Create("Append first".into())).await,
        201,
    );
    let first_id = first["id"].as_str().unwrap();
    let mut track_locker = connect().await;
    let track_locker_pid = track_locker.query_one_scalar(pid, &[]).await.unwrap();
    let track_lock = track_locker.transaction().await.unwrap();
    track_lock
        .query_one(
            "SELECT id FROM public.tracks WHERE id = $1::text::uuid FOR UPDATE",
            &[&track],
        )
        .await
        .unwrap();
    let first_append = route(ctx, &owner, &mut peer, Append(first_id, track.to_owned()));
    tokio::pin!(first_append);
    blocker::observe_blocked(db, track_locker_pid, waiter, first_append.as_mut())
        .await
        .unwrap();

    let late_remove = remove_membership(&mut remover, RECIPIENT, &active, track);
    tokio::pin!(late_remove);
    blocker::observe_blocked(db, waiter, holder, late_remove.as_mut())
        .await
        .unwrap();
    track_lock.commit().await.unwrap();
    let appended_first = body(first_append.await, 201);
    assert_eq!(appended_first["position"], 0);
    // The removal future has yet to send COMMIT, so readers still see membership.
    let before_removal = body(route(ctx, &owner, db, Read(first_id)).await, 200);
    assert_eq!(
        before_removal["entries"],
        json!([{"id": appended_first["id"], "position": 0, "track_id": track}])
    );
    assert_eq!(late_remove.await, 1);

    assert_eq!(remove_membership(db, RECIPIENT, &active, track).await, 0);
    assert_denied(route(ctx, &owner, db, Append(first_id, track.to_owned())).await);
    let dormant = body(route(ctx, &owner, db, Read(first_id)).await, 200);
    assert_eq!(
        dormant["entries"],
        json!([{"id": appended_first["id"], "position": 0, "dormant": true}])
    );
    let surviving_memberships: Vec<String> = db
        .query_one_scalar(
            "SELECT array_agg(l.name ORDER BY l.name)
             FROM public.library_tracks lt JOIN public.libraries l ON lt.library_id = l.id
             WHERE lt.track_id = $1::text::uuid",
            &[&track],
        )
        .await
        .unwrap();
    assert_eq!(surviving_memberships, ["Deleting", "Foreign"]);

    let deleted = body(route(ctx, &owner, db, Delete(playlist)).await, 202);
    assert!(deleted["operation_id"].is_string());
    assert_eq!(
        body(route(ctx, &owner, db, Delete(playlist)).await, 202),
        deleted
    );
    assert_denied(route(ctx, &foreign, db, Delete(playlist)).await);
    pending_id.to_owned()
}
