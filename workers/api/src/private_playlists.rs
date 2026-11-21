//! Unmounted session-only handlers for the normalized Library contract (ADR 0002).
//! Register only as part of the Library and playlist cutovers.

use super::*;
use crate::library::{check_name, clamp_page_size, new_public_id, page_cursor, parse_page_cursor};
use serde_json::{json, Value};
use tokio_postgres::{types::ToSql, Client, GenericClient, IsolationLevel};

const LIST: &str = "
SELECT p.public_id, p.name FROM playlists p JOIN users u ON u.id = p.owner_user_id
WHERE u.id = $1 AND u.deleting_at IS NULL AND NOT p.is_public AND p.deleting_at IS NULL
  AND ($2::text IS NULL OR p.public_id COLLATE \"C\" < $2 COLLATE \"C\")
ORDER BY p.public_id COLLATE \"C\" DESC LIMIT $3";

const READ: &str = "
SELECT jsonb_build_object('id', p.public_id, 'name', p.name, 'legacy_items', p.items,
  'entries', COALESCE((SELECT jsonb_agg(
    jsonb_build_object('id', e.id, 'position', e.position) ||
    CASE WHEN EXISTS (SELECT FROM libraries l JOIN library_tracks lt ON lt.library_id = l.id
        WHERE l.user_id = u.id AND l.deleting_at IS NULL AND lt.track_id = e.track_id)
      THEN jsonb_build_object('track_id', e.track_id) ELSE '{\"dormant\":true}'::jsonb END
    ORDER BY e.position) FROM playlist_tracks e WHERE e.playlist_id = p.id), '[]'::jsonb))::text
FROM playlists p JOIN users u ON u.id = p.owner_user_id
WHERE u.id = $1 AND u.deleting_at IS NULL AND NOT p.is_public AND p.deleting_at IS NULL
  AND p.public_id = $2";

const LOCK_ACCOUNT: &str = "SELECT 1 FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE";
const LOCK_PLAYLIST: &str = "
SELECT id FROM playlists WHERE owner_user_id = $1 AND public_id = $2 AND NOT is_public
  AND ($3::boolean OR deleting_at IS NULL) FOR UPDATE";

const APPEND: &str = "
WITH next AS (
  SELECT greatest(jsonb_array_length(p.items), (SELECT max(e.position) + 1
    FROM playlist_tracks e WHERE e.playlist_id = p.id)) AS position
  FROM playlists p WHERE p.id = $2
)
INSERT INTO playlist_tracks (playlist_id, track_id, position)
SELECT $2, $3::text::uuid, next.position FROM next WHERE next.position < $4
  AND EXISTS (SELECT FROM libraries l JOIN library_tracks lt ON lt.library_id = l.id
    WHERE l.user_id = $1 AND l.deleting_at IS NULL AND lt.track_id = $3::text::uuid)
  AND NOT EXISTS (SELECT FROM playlist_tracks e WHERE e.playlist_id = $2 AND e.track_id = $3::text::uuid)
RETURNING jsonb_build_object('id', id, 'position', position)::text";

const DELETE_REQUEST: &str = "
WITH existing AS (
  SELECT id FROM delete_operations WHERE user_id = $1 AND scope = 'playlist' AND container_id = $2
), fenced AS (
  UPDATE playlists SET deleting_at = now() WHERE id = $3 AND NOT EXISTS (SELECT FROM existing)
  RETURNING public_id
), requested AS (
  INSERT INTO delete_operations (user_id, scope, container_id, started_at)
  SELECT $1, 'playlist', public_id, now() FROM fenced RETURNING id
)
SELECT jsonb_build_object('operation_id', id)::text
FROM (SELECT id FROM existing UNION ALL SELECT id FROM requested) operation";

enum Command<'a> {
    List(Option<String>, usize),
    Read(&'a str),
    Create(String),
    Rename(&'a str, String),
    Append(&'a str, String),
    Delete(&'a str),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Title {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    track_id: String,
}

fn handle_shape(id: &str) -> bool {
    id.len() == 22
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn command(req: &Request) -> Result<(u16, Command<'_>), Response> {
    let (path, query) = req.path.split_once('?').unwrap_or((&req.path, ""));
    let title = || {
        let body = parse_body::<Title>(req).ok_or_else(invalid_request)?;
        if body.name.contains('\0') {
            return Err(invalid_request());
        }
        check_name(&body.name).map_err(|_| invalid_request())
    };
    match (req.method.as_str(), path) {
        ("GET", "/v1/playlists") => {
            let mut after = None;
            let mut limit = crate::library::MAX_PAGE_SIZE;
            for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
                match key.as_ref() {
                    "limit" => limit = value.parse().map_err(|_| invalid_request())?,
                    "after" => {
                        after = parse_page_cursor(&value).ok().filter(|id| handle_shape(id));
                        if after.is_none() {
                            return Err(invalid_request());
                        }
                    }
                    _ => return Err(invalid_request()),
                }
            }
            Ok((200, Command::List(after, limit)))
        }
        ("POST", "/v1/playlists") => Ok((201, Command::Create(title()?))),
        (method, path) => {
            let rest = path.strip_prefix("/v1/playlists/").ok_or_else(not_found)?;
            let id = rest.strip_suffix("/entries").unwrap_or(rest);
            if !handle_shape(id) {
                return Err(not_found());
            }
            match (method, id != rest) {
                ("POST", true) => {
                    let body = parse_body::<Track>(req).ok_or_else(invalid_request)?;
                    if !crate::meter_sink::is_uuid_shaped(&body.track_id) {
                        return Err(invalid_request());
                    }
                    Ok((201, Command::Append(id, body.track_id)))
                }
                ("GET", false) => Ok((200, Command::Read(id))),
                ("PATCH", false) => Ok((200, Command::Rename(id, title()?))),
                ("DELETE", false) => Ok((202, Command::Delete(id))),
                _ => Err(not_found()),
            }
        }
    }
}

pub(super) async fn route(
    ctx: &Ctx<'_, impl Store + SingleUseStore, impl Mailer, impl Dialer, impl AsyncDns>,
    req: &Request,
    db: &mut Client,
) -> Response {
    if req.authorization.is_some() {
        return not_found();
    }
    let (claims, _) = match ctx.authenticate(req, req.method != "GET").await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if req.body.len() > MAX_BODY_BYTES {
        return invalid_request();
    }
    let (status, cmd) = match command(req) {
        Ok(command) => command,
        Err(response) => return response,
    };
    match execute(db, &claims.user_id, cmd).await {
        Ok(Some(body)) => json_response(status, body),
        Ok(None) => not_found(),
        Err(_) => unavailable(),
    }
}

async fn value(
    db: &impl GenericClient,
    sql: &str,
    args: &[&(dyn ToSql + Sync)],
) -> Result<Option<Value>, StoreError> {
    let Some(row) = db.query_opt(sql, args).await.map_err(|_| StoreError)? else {
        return Ok(None);
    };
    let text = row.try_get(0).map_err(|_| StoreError)?;
    serde_json::from_str(text).map(Some).map_err(|_| StoreError)
}

async fn execute(
    db: &mut Client,
    owner: &str,
    cmd: Command<'_>,
) -> Result<Option<Value>, StoreError> {
    let handle = match cmd {
        Command::List(after, limit) => {
            let limit = clamp_page_size(limit);
            let rows = db
                .query(LIST, &[&owner, &after, &(limit as i64 + 1)])
                .await
                .map_err(|_| StoreError)?;
            let mut items = Vec::new();
            let mut next = None;
            for row in rows.iter().take(limit) {
                let id: &str = row.try_get(0).map_err(|_| StoreError)?;
                let name: &str = row.try_get(1).map_err(|_| StoreError)?;
                items.push(json!({"id": id, "name": name}));
                if rows.len() > limit && items.len() == limit {
                    next = Some(page_cursor(id));
                }
            }
            return Ok(Some(json!({"items": items, "next_cursor": next})));
        }
        Command::Read(id) => return value(db, READ, &[&owner, &id]).await,
        Command::Create(_) => None,
        Command::Rename(id, _) | Command::Append(id, _) | Command::Delete(id) => Some(id),
    };
    // Wait for both locks before taking a fresh snapshot for duplicate checks.
    let tx = db
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| StoreError)?;
    if tx
        .query_opt(LOCK_ACCOUNT, &[&owner])
        .await
        .map_err(|_| StoreError)?
        .is_none()
    {
        return Ok(None);
    }
    let deleting = matches!(cmd, Command::Delete(_));
    let target: Option<i64> = if let Some(id) = handle {
        tx.query_opt(LOCK_PLAYLIST, &[&owner, &id, &deleting])
            .await
            .map_err(|_| StoreError)?
            .map(|row| row.try_get(0))
            .transpose()
            .map_err(|_| StoreError)?
    } else {
        None
    };
    let result = match cmd {
        Command::Create(name) => {
            let id = new_public_id().map_err(|_| StoreError)?;
            value(
                &tx,
                "INSERT INTO playlists (owner_user_id, public_id, name, created_at)
                VALUES ($1, $2, $3, extract(epoch FROM now())::bigint)
                RETURNING jsonb_build_object('id', public_id, 'name', name)::text",
                &[&owner, &id, &name],
            )
            .await?
        }
        Command::Rename(_, name) => {
            value(
                &tx,
                "UPDATE playlists SET name = $2 WHERE id = $1
            RETURNING jsonb_build_object('id', public_id, 'name', name)::text",
                &[&target, &name],
            )
            .await?
        }
        Command::Append(_, track) => {
            value(
                &tx,
                APPEND,
                &[&owner, &target, &track, &(crate::library::MAX_ITEMS as i32)],
            )
            .await?
        }
        Command::Delete(id) => value(&tx, DELETE_REQUEST, &[&owner, &id, &target]).await?,
        Command::Read(_) | Command::List(_, _) => unreachable!(),
    };
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(result)
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "private_playlists_postgres_tests.rs"]
mod postgres_tests;
