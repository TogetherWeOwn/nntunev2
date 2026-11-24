//! One node and one keyset page of the recorded invite tree.

use serde::Serialize;
use tokio_postgres::types::Type;

use crate::handlers::StoreError;
use crate::postgres::PgQuery;

const PAGE_SIZE: usize = 50;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct InviteTreePage {
    pub user_id: String,
    pub inviter_id: Option<String>,
    pub invitee_ids: Vec<String>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InviteTreeRead {
    NotAdmin,
    Missing,
    AccountDeleting,
    InvalidCursor,
    Page(InviteTreePage),
}

pub(crate) async fn read_on(
    client: &impl PgQuery,
    actor_id: &str,
    user_id: &str,
    cursor: Option<&str>,
) -> Result<InviteTreeRead, StoreError> {
    // Recheck the actor in the same snapshot as the graph projection. The
    // cursor is an existing direct-child ID, not a free-floating sort key.
    // A NULL child from the lateral join preserves empty nodes and pages.
    let rows = client
        .query_typed(
            r#"SELECT actor.deleting_at IS NOT NULL OR subject.deleting_at IS NOT NULL AS deleting,
                      subject.id, subject.invited_by, anchor.id AS anchor, page.id AS child
               FROM users actor
               LEFT JOIN users subject ON subject.id = $2 AND actor.deleting_at IS NULL
               LEFT JOIN users anchor ON anchor.id = $3 AND anchor.invited_by = subject.id
               LEFT JOIN LATERAL (
                   SELECT child.id FROM users child
                   WHERE child.invited_by = subject.id AND subject.deleting_at IS NULL
                     AND ($3 IS NULL OR child.id COLLATE "C" > anchor.id COLLATE "C")
                   ORDER BY child.id COLLATE "C" LIMIT $4
               ) page ON TRUE
               WHERE actor.id = $1 AND actor.role = 'admin'
               ORDER BY page.id COLLATE "C""#,
            &[
                (&actor_id, Type::TEXT),
                (&user_id, Type::TEXT),
                (&cursor, Type::TEXT),
                (&((PAGE_SIZE + 1) as i64), Type::INT8),
            ],
        )
        .await?;
    use InviteTreeRead::*;
    let Some(first) = rows.first() else {
        return Ok(NotAdmin);
    };
    if first.try_get::<_, bool>("deleting")? {
        return Ok(AccountDeleting);
    }
    let Some(user_id) = first.try_get("id")? else {
        return Ok(Missing);
    };
    if cursor.is_some() && first.try_get::<_, Option<&str>>("anchor")?.is_none() {
        return Ok(InvalidCursor);
    }
    let mut invitee_ids = Vec::with_capacity(rows.len().min(PAGE_SIZE));
    for row in rows.iter().take(PAGE_SIZE) {
        if let Some(id) = row.try_get::<_, Option<String>>("child")? {
            invitee_ids.push(id);
        }
    }
    let next_cursor = (rows.len() > PAGE_SIZE).then(|| invitee_ids[PAGE_SIZE - 1].clone());
    Ok(Page(InviteTreePage {
        user_id,
        inviter_id: first.try_get("invited_by")?,
        invitee_ids,
        next_cursor,
    }))
}
