use super::*;
use crate::listen_history::{valid_playback_id, ExportCursor, ListenRow, EXPORT_PAGE_SIZE};
use std::collections::HashSet;

pub(super) const EXPORT_PATH: &str = "/v1/plays/export";
pub(super) const CLEAR_PATH: &str = "/v1/plays/events";
const EXPORTS_PER_MINUTE: u64 = 120;
const CLEARS_PER_MINUTE: u64 = 6;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    before_id: Option<String>,
}

fn export_page(mut rows: Vec<ListenRow>, before_id: Option<&str>) -> Response {
    if rows.len() > EXPORT_PAGE_SIZE + 1 {
        return internal();
    }
    let before = before_id.and_then(|cursor| cursor.split_once(':'));
    let mut seen = HashSet::with_capacity(rows.len());
    let valid = rows.iter().all(|row| {
        let phase = if row.event.phase() == 0 { "0" } else { "1" };
        let cursor = (row.playback_id.as_str(), phase);
        valid_playback_id(cursor.0)
            && (1..=128).contains(&row.track_id.len())
            && !row.track_id.chars().any(char::is_control)
            && row.recorded_at >= 0
            && before != Some(cursor)
            && seen.insert(cursor)
    });
    if !valid {
        return internal();
    }
    let has_more = rows.len() > EXPORT_PAGE_SIZE;
    rows.truncate(EXPORT_PAGE_SIZE);
    let next_cursor = rows.last().filter(|_| has_more).map(|row| row.cursor());
    json_response(
        200,
        serde_json::json!({
            "events": rows, "next_cursor": next_cursor,
            "retention": "account_lifetime",
        }),
    )
}

pub(super) async fn export<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    // Keep cursor values out of URLs and require the cookie's exact Origin.
    if req.authorization.is_some() {
        return not_found();
    }
    let (claims, _) = match ctx.authenticate(req, true).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    // Serde structs also accept positional arrays without this object check.
    if req.body.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'{') {
        return validation_failed("invalid request");
    }
    let Some(body) = parse_body::<ExportRequest>(req) else {
        return validation_failed("invalid request");
    };
    let before = match ExportCursor::parse(body.before_id.as_deref()) {
        Ok(cursor) => cursor,
        Err(message) => return validation_failed(message),
    };
    match ctx
        .store
        .reserve_rate_limit(
            &claims.user_id,
            "plays.export",
            EXPORTS_PER_MINUTE,
            ctx.now,
            60,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return throttled(60),
        Err(_) => return internal(),
    }
    match ctx
        .store
        .export_listen_events(&claims.user_id, before.as_ref())
        .await
    {
        Ok(rows) => export_page(rows, body.before_id.as_deref()),
        Err(_) => internal(),
    }
}

pub(super) async fn clear<S, M, D, N>(ctx: &Ctx<'_, S, M, D, N>, req: &Request) -> Response
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    if req.authorization.is_some() {
        return not_found();
    }
    let (claims, _) = match ctx.authenticate_privileged(req).await {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    match ctx
        .store
        .reserve_rate_limit(
            &claims.user_id,
            "plays.clear",
            CLEARS_PER_MINUTE,
            ctx.now,
            60,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return throttled(60),
        Err(_) => return internal(),
    }
    match ctx.store.clear_listen_events(&claims.user_id).await {
        Ok(()) => json_response(200, serde_json::json!({"ok": true})),
        Err(_) => internal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listen_history::ListenEvent;

    #[test]
    fn export_refuses_invalid_rows_and_nonadvancing_pages() {
        let row = ListenRow {
            playback_id: "00000000-0000-4000-8000-000000000001".into(),
            track_id: "synthetic-track".into(),
            event: ListenEvent::Start,
            recorded_at: 0,
        };
        let anchor = row.cursor();
        let refused = |rows, cursor: Option<&str>| {
            let response = export_page(rows, cursor);
            assert_eq!(response.status, 500);
            assert!(!response.body.contains("synthetic-track"));
        };
        assert_eq!(export_page(vec![row.clone()], None).status, 200);
        refused(vec![row.clone()], Some(&anchor));
        refused(vec![row.clone(), row.clone()], None);
        for (playback_id, track_id, recorded_at) in [
            ("bad-id", "track", 0),
            (row.playback_id.as_str(), "track\n", 0),
            (row.playback_id.as_str(), "track", -1),
        ] {
            let invalid = ListenRow {
                playback_id: playback_id.into(),
                track_id: track_id.into(),
                recorded_at,
                ..row.clone()
            };
            refused(vec![invalid], None);
        }
    }
}
