//! Unmounted session-only handlers for the normalized Library contract (ADR 0002).
//! Register only as part of the Library and playlist cutovers.

use super::*;
use crate::library::{check_name, clamp_page_size, new_public_id, page_cursor, MAX_ITEMS};
use crate::meter_sink::is_uuid_shaped;
use crate::postgres::{PgConnect, PgQuery};
use serde_json::{json, Value};
use tokio_postgres::types::{ToSql, Type};

const LIST: &str = "
SELECT p.public_id, p.name FROM playlists p JOIN users u ON u.id = p.owner_user_id
WHERE u.id = $1 AND u.deleting_at IS NULL AND NOT p.is_public AND p.deleting_at IS NULL
  AND ($2::text IS NULL OR p.public_id COLLATE \"C\" < $2 COLLATE \"C\")
ORDER BY p.public_id COLLATE \"C\" DESC LIMIT $3";

// Keep the canonical track gate shared by the read projection and append.
// Invite-scoped browser Library admissions have their own access contract.
macro_rules! owned_membership {
    () => {
        "SELECT 1 FROM public.libraries l
        JOIN public.library_tracks lt ON lt.library_id = l.id
        WHERE l.user_id = $1 AND l.deleting_at IS NULL"
    };
}

const READ: &str = concat!(
    "
SELECT jsonb_build_object('id', p.public_id, 'name', p.name, 'legacy_items', p.items,
  'entries', COALESCE((SELECT jsonb_agg(
    jsonb_build_object('id', e.id, 'position', e.position) ||
    CASE WHEN EXISTS (",
    owned_membership!(),
    " AND lt.track_id = e.track_id)
      THEN jsonb_build_object('track_id', e.track_id) ELSE '{\"dormant\":true}'::jsonb END
    ORDER BY e.position) FROM playlist_tracks e WHERE e.playlist_id = p.id), '[]'::jsonb))::text
FROM playlists p JOIN users u ON u.id = p.owner_user_id
WHERE u.id = $1 AND u.deleting_at IS NULL AND NOT p.is_public AND p.deleting_at IS NULL
  AND p.public_id = $2"
);

const LOCK_ACCOUNT: &str = "SELECT 1 FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE";
const LOCK_PLAYLIST: &str = "
SELECT id FROM playlists WHERE owner_user_id = $1 AND public_id = $2 AND NOT is_public
  AND ($3::boolean OR deleting_at IS NULL) FOR UPDATE";

const CREATE: &str = "INSERT INTO playlists (owner_user_id, public_id, name, created_at)
VALUES ($1, $2, $3, extract(epoch FROM now())::bigint)
RETURNING jsonb_build_object('id', public_id, 'name', name)::text";
const RENAME: &str = "UPDATE playlists SET name = $2 WHERE id = $1
RETURNING jsonb_build_object('id', public_id, 'name', name)::text";

// Hold one qualifying Library and membership through commit. PostgreSQL
// rechecks the predicate if an in-flight fence or removal won the row lock.
const APPEND: &str = concat!(
    "
WITH next AS (
  SELECT greatest(jsonb_array_length(p.items), (SELECT max(e.position) + 1
    FROM playlist_tracks e WHERE e.playlist_id = p.id)) AS position
  FROM playlists p WHERE p.id = $2
)
INSERT INTO playlist_tracks (playlist_id, track_id, position)
SELECT $2, $3::text::uuid, next.position FROM next WHERE next.position < $4
  AND EXISTS (",
    owned_membership!(),
    " AND lt.track_id = $3::text::uuid LIMIT 1 FOR SHARE OF l, lt)
  AND NOT EXISTS (SELECT FROM playlist_tracks e WHERE e.playlist_id = $2 AND e.track_id = $3::text::uuid)
RETURNING jsonb_build_object('id', id, 'position', position)::text"
);

// The legacy array is read-only. New entries occupy the suffix after its
// reserved slots. Reorder requires a complete permutation of that suffix and
// reuses its occupied positions, including when there are gaps.
const REORDER: &str = concat!(
    "
WITH entries AS (
  SELECT e.id, e.track_id, e.position, row_number() OVER (ORDER BY e.position) AS ordinal
  FROM playlist_tracks e JOIN playlists p ON p.id = e.playlist_id
  WHERE p.id = $2 AND NOT e.is_legacy
    AND e.position >= jsonb_array_length(p.items) AND e.position < $4
), moves AS (
  SELECT e.id, e.track_id, e.position, slot.position AS destination
  FROM unnest($3::text[]) WITH ORDINALITY AS requested(id, ordinal)
  JOIN entries e ON e.id = requested.id::uuid
  JOIN entries slot ON slot.ordinal = requested.ordinal
), valid AS (
  SELECT 1 WHERE cardinality($3) > 0
    AND (SELECT count(*) FROM entries) = cardinality($3)
    AND (SELECT count(DISTINCT id) FROM moves) = cardinality($3)
    AND NOT EXISTS (SELECT FROM moves m WHERE m.position <> m.destination AND NOT EXISTS (",
    owned_membership!(),
    " AND lt.track_id = m.track_id LIMIT 1 FOR SHARE OF l, lt))
), reordered AS (
  UPDATE playlist_tracks e SET position = m.destination FROM moves m, valid
  WHERE e.playlist_id = $2 AND e.id = m.id AND m.position <> m.destination
)
SELECT '{\"reordered\":true}' FROM valid"
);

// Leave the other positions stable, including dormant tombstones. Append
// keeps using the next tail position, so deleting an interior row leaves a gap.
const REMOVE: &str = concat!(
    "
DELETE FROM playlist_tracks e USING playlists p
WHERE p.id = $2 AND e.playlist_id = p.id AND e.id = $3::text::uuid AND NOT e.is_legacy
  AND e.position >= jsonb_array_length(p.items) AND e.position < $4
  AND EXISTS (",
    owned_membership!(),
    " AND lt.track_id = e.track_id LIMIT 1 FOR SHARE OF l, lt)
RETURNING jsonb_build_object('id', e.id)::text"
);

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

pub(super) enum Command<'a> {
    List(Option<String>, usize),
    Read(&'a str),
    Create(String),
    Rename(&'a str, String),
    Append(&'a str, String),
    Reorder(&'a str, Vec<String>),
    Remove(&'a str, String),
    Delete(&'a str),
}

impl<'a> Command<'a> {
    fn handle(&self) -> Option<&'a str> {
        match self {
            Self::Read(id)
            | Self::Rename(id, _)
            | Self::Append(id, _)
            | Self::Reorder(id, _)
            | Self::Remove(id, _)
            | Self::Delete(id) => Some(id),
            Self::Create(_) | Self::List(_, _) => None,
        }
    }
}

fn handle_shape(id: &str) -> bool {
    let valid_byte = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-');
    id.len() == 22 && id.bytes().all(valid_byte)
}

fn validate(cmd: Command<'_>) -> Result<(u16, Command<'_>), Response> {
    if cmd.handle().is_some_and(|id| !handle_shape(id)) {
        return Err(not_found());
    }
    let title = |name: String| {
        check_name(&name)
            .ok()
            .filter(|name| !name.contains('\0'))
            .ok_or_else(invalid_request)
    };
    match cmd {
        Command::List(ref after, _) if after.as_deref().is_some_and(|id| !handle_shape(id)) => {
            Err(invalid_request())
        }
        Command::Append(_, ref id) | Command::Remove(_, ref id) if !is_uuid_shaped(id) => {
            Err(invalid_request())
        }
        Command::Reorder(_, ref ids)
            if ids.is_empty()
                || ids.len() > MAX_ITEMS
                || ids.iter().any(|id| !is_uuid_shaped(id)) =>
        {
            Err(invalid_request())
        }
        Command::Create(name) => Ok((201, Command::Create(title(name)?))),
        Command::Rename(id, name) => Ok((200, Command::Rename(id, title(name)?))),
        Command::Append(..) => Ok((201, cmd)),
        Command::Delete(_) => Ok((202, cmd)),
        Command::List(..) | Command::Read(_) | Command::Reorder(..) | Command::Remove(..) => {
            Ok((200, cmd))
        }
    }
}

/// Accepts the operation selected by a future HTTP dispatcher.
pub(super) async fn route(
    ctx: &Ctx<'_, impl Store + SingleUseStore, impl Mailer, impl Dialer, impl AsyncDns>,
    req: &Request,
    db: &mut impl PgConnect,
    cmd: Command<'_>,
) -> Response {
    if req.authorization.is_some() {
        return not_found();
    }
    let mutation = !matches!(cmd, Command::Read(_) | Command::List(..));
    let (claims, _) = match ctx.authenticate(req, mutation).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if req.body.len() > MAX_BODY_BYTES {
        return invalid_request();
    }
    let (status, cmd) = match validate(cmd) {
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
    db: &impl PgQuery,
    sql: &str,
    args: &[(&(dyn ToSql + Sync), Type)],
) -> Result<Option<Value>, StoreError> {
    let found = db.query_typed_opt(sql, args).await;
    let Some(row) = found.map_err(|_| StoreError)? else {
        return Ok(None);
    };
    let text = row.try_get(0).map_err(|_| StoreError)?;
    serde_json::from_str(text).map(Some).map_err(|_| StoreError)
}

pub(super) async fn execute(
    db: &mut impl PgConnect,
    owner: &str,
    cmd: Command<'_>,
) -> Result<Option<Value>, StoreError> {
    let owner = || (&owner as &(dyn ToSql + Sync), Type::TEXT);
    let handle = cmd.handle();
    match cmd {
        Command::List(after, limit) => {
            let limit = clamp_page_size(limit);
            let fetch = limit as i64 + 1;
            let rows = db
                .query_typed(LIST, &[owner(), (&after, Type::TEXT), (&fetch, Type::INT8)])
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
        Command::Read(id) => return value(db, READ, &[owner(), (&id, Type::TEXT)]).await,
        _ => {}
    }
    // Wait for both locks before taking a fresh snapshot for duplicate checks.
    let tx = db.read_committed().await.map_err(|_| StoreError)?;
    let account = tx.query_typed_opt(LOCK_ACCOUNT, &[owner()]).await;
    if account.map_err(|_| StoreError)?.is_none() {
        return Ok(None);
    }
    let deleting = matches!(cmd, Command::Delete(_));
    let target: Option<i64> = if let Some(id) = handle {
        tx.query_typed_opt(
            LOCK_PLAYLIST,
            &[owner(), (&id, Type::TEXT), (&deleting, Type::BOOL)],
        )
        .await
        .map_err(|_| StoreError)?
        .map(|row| row.try_get(0))
        .transpose()
        .map_err(|_| StoreError)?
    } else {
        None
    };
    let target = (&target as &(dyn ToSql + Sync), Type::INT8);
    let max_items = MAX_ITEMS as i32;
    let cap = (&max_items as &(dyn ToSql + Sync), Type::INT4);
    let result = match cmd {
        Command::Create(name) => {
            let id = new_public_id().map_err(|_| StoreError)?;
            let params = [owner(), (&id, Type::TEXT), (&name, Type::TEXT)];
            value(&tx, CREATE, &params).await?
        }
        Command::Rename(_, name) => value(&tx, RENAME, &[target, (&name, Type::TEXT)]).await?,
        Command::Append(_, track) => {
            let params = [owner(), target, (&track, Type::TEXT), cap];
            value(&tx, APPEND, &params).await?
        }
        Command::Reorder(_, entries) => {
            value(
                &tx,
                REORDER,
                &[owner(), target, (&entries, Type::TEXT_ARRAY), cap],
            )
            .await?
        }
        Command::Remove(_, entry) => {
            value(&tx, REMOVE, &[owner(), target, (&entry, Type::TEXT), cap]).await?
        }
        Command::Delete(id) => {
            value(&tx, DELETE_REQUEST, &[owner(), (&id, Type::TEXT), target]).await?
        }
        Command::Read(_) | Command::List(_, _) => unreachable!(),
    };
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(result)
}
