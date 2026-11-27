use std::{
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{
    begin, dispatch_claims, CleanupClaim, CleanupKind, CleanupMessage, CleanupScope, Client,
    DispatchError,
};
use crate::{
    digest_candidate_store::uuid,
    multipart::{
        session_completion::valid_upload_id,
        session_outcome::{uuid_text, valid_media_key},
    },
};
use nntune_core::s3_locator::valid_bucket;

const TARGET: &str = include_str!("reconcile.sql");

pub(super) async fn dispatch_multipart_claims<
    S: Future<Output = Result<(), ()>>,
    D: Future<Output = ()>,
>(
    db: &mut Client,
    scope: CleanupScope<'_>,
    environment: [u8; 16],
    (configuration, bucket): ([u8; 16], &str),
    (mut find, mut abort): (
        impl AsyncFnMut(&str, SystemTime) -> Result<String, ()>,
        impl AsyncFnMut(&str, &str, SystemTime) -> Result<(), ()>,
    ),
    send: impl FnMut(CleanupMessage, u32) -> S,
    deadline: impl Fn() -> D,
) -> Result<(), DispatchError> {
    if !scope
        .environment_id
        .eq_ignore_ascii_case(&uuid_text(environment))
        || !valid_bucket(bucket)
    {
        return Err(DispatchError::MultipartNotConfirmed);
    }
    dispatch_claims(
        db,
        scope,
        async |db, claim| {
            reconcile(
                db,
                scope,
                claim,
                bucket,
                &configuration,
                &mut find,
                &mut abort,
            )
            .await
        },
        send,
        &deadline,
    )
    .await
}

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
    let target = async |db: &mut Client, discovered: Option<(&str, &str)>| {
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
                    &discovered.map(|(_, id)| id),
                ],
            )
            .await
            .map_err(|_| ())?
            .ok_or(())?;
        let key: Option<String> = row.try_get(0).map_err(|_| ())?;
        let id: Option<String> = row.try_get(1).map_err(|_| ())?;
        let millis: i64 = row.try_get(2).map_err(|_| ())?;
        let until = UNIX_EPOCH
            .checked_add(Duration::from_millis(millis.try_into().map_err(|_| ())?))
            .ok_or(())?;
        if !valid_target(key.as_deref(), id.as_deref())
            || discovered.is_some_and(|(want_key, want_id)| {
                key.as_deref() != Some(want_key) || id.as_deref() != Some(want_id)
            })
        {
            return Err(());
        }
        tx.commit().await.map_err(|_| ())?;
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
        let (_, _, until) = target(db, Some((&key, &found))).await?;
        (found, until)
    };
    abort(&key, &id, until).await
}

fn valid_target(key: Option<&str>, id: Option<&str>) -> bool {
    match key {
        Some(key) => valid_media_key(key) && id.is_none_or(valid_upload_id),
        None => id.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reconciliation_target_requires_a_canonical_key_and_an_opaque_upload_id() {
        let key = "media/03030303030340038003030303030303/04040404040440048004040404040404";
        assert!(valid_target(Some(key), Some("opaque+/=")));
        assert!(!valid_target(None, Some("opaque")));
        assert!(!valid_target(Some(key), Some("bad id")));
        assert!(!valid_target(Some("media/invalid"), Some("opaque")));
    }
}
