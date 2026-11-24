use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{begin, CleanupClaim, CleanupKind, CleanupScope, Client};
use crate::{digest_candidate_store::uuid, multipart::session_completion::valid_upload_id};

const TARGET: &str = include_str!("reconcile.sql");

/// Reuse the delivery lease to bound the provider attempt. A discovered ID is
/// committed before abort, so replay never has to infer success from an empty
/// listing. Only tombstones can acquire an ID here.
pub(super) async fn reconcile(
    db: &mut Client,
    scope: CleanupScope<'_>,
    claim: &CleanupClaim,
    bucket: &str,
    configuration: &[u8; 16],
    find: impl AsyncFnOnce(&str, SystemTime) -> Result<String, ()>,
    abort: impl AsyncFnOnce(&str, &str, SystemTime) -> Result<(), ()>,
) -> Result<(), ()> {
    if claim.kind != CleanupKind::TerminalReconcile {
        return Ok(());
    }
    if !claim.session_id.eq_ignore_ascii_case(scope.session_id) {
        return Err(());
    }
    let [environment, library, principal, session] = scope.params();
    let configuration = uuid(configuration);
    let target = async |db: &mut Client, discovered: Option<&str>| {
        let tx = begin(db).await?;
        let row = tx
            .query_opt(
                TARGET,
                &[
                    environment,
                    library,
                    principal,
                    session,
                    &claim.claim_id,
                    &bucket,
                    &configuration,
                    &discovered,
                ],
            )
            .await
            .map_err(|_| ())?
            .ok_or(())?;
        tx.commit().await.map_err(|_| ())?;
        let key: Option<String> = row.try_get(0).map_err(|_| ())?;
        let id: Option<String> = row.try_get(1).map_err(|_| ())?;
        let millis: i64 = row.try_get(2).map_err(|_| ())?;
        let until = UNIX_EPOCH
            .checked_add(Duration::from_millis(millis.try_into().map_err(|_| ())?))
            .ok_or(())?;
        Ok((key, id, until))
    };

    let (Some(key), id, until) = target(db, None).await? else {
        return Ok(());
    };
    let (id, until) = if let Some(id) = id {
        (id, until)
    } else {
        let found = find(&key, until).await?;
        if !valid_upload_id(&found) {
            return Err(());
        }
        let (pinned_key, pinned_id, until) = target(db, Some(&found)).await?;
        if pinned_key.as_deref() != Some(&key) || pinned_id.as_deref() != Some(&found) {
            return Err(());
        }
        (found, until)
    };
    if !valid_upload_id(&id) {
        return Err(());
    }
    abort(&key, &id, until).await
}
