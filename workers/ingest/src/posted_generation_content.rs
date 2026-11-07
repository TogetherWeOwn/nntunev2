//! Fenced persistence of the verifier's authenticated-content observations.
//! This module does not read/decrypt articles or prove provider topology. The
//! verifier must read the exact stored plan from the canonical pool-config
//! backbone, authenticate/decrypt it and check the full plaintext digest/size.
//! Account aliases must resolve to that canonical identity before admission.
//! Two unequal labels alone do not prove genuinely independent backbones.
//!
//! No content observation grants retention/reuse/promotion/publication or R2
//! deletion authority. Those consumers remain unwired. Each callback uses the
//! ORIGINAL permit; a newer handle cannot rebind an old network observation.

use super::*;
use std::future::Future;

/// Trusted full-set reader for ONE canonical backbone. Implementations must open
/// this exact stored bundle, resolve its configured side to a canonical spool,
/// read all required data/recovery articles and authenticate/check the complete
/// plaintext. A report must describe bytes actually read, never intended input.
/// Missing topology, unavailable credentials or partial reads are nonaffirmative.
/// The core crypto helper alone cannot construct a complete-set report.
///
/// This injection seam does not implement the production pool/envelope binding.
/// It is not an untrusted queue/HTTP result interface. Errors must be redacted.
pub trait GenerationContentTransport {
    fn read_content<'a>(
        &'a mut self,
        bundle: &'a StoredGeneration,
        side: ArticleSide,
    ) -> impl Future<Output = ContentCheckResult<'a>>;
}

#[derive(Debug)]
pub enum ContentReadStep {
    /// An observation (including a failure) was committed. This is NOT a claim
    /// that either side is intact, or reuse/publication/cleanup permission.
    ObservationRecorded(Box<GenerationHandle>),
    StaleOrDiverged,
}

/// Read one complete stored generation on one side and persist its result with
/// the ORIGINAL pre-read permit. No database transaction spans the read, and no
/// network retry or local revision/cursor advancement is performed. A stale
/// callback stops this delivery; a lost commit acknowledgment requires recovery.
/// Explicit rechecks never skip a prior Intact observation: failures must be able
/// to invalidate it. Renew both still-live candidate leases before the read;
/// preparation alone stays read-only. Renewal never refreshes the original
/// permit or keeps an expired callback alive. Callers own bounded scheduling and
/// read deadlines, not this one-read operation. Cancellation may retain only the
/// renewed deadlines, never an observation.
pub async fn drive_content_read<T: GenerationContentTransport>(
    client: &mut Client,
    handle: &GenerationHandle,
    side: ArticleSide,
    transport: &mut T,
) -> Result<ContentReadStep, StoreError> {
    let Some((permit, bundle)) = prepare_content_read(client, handle, side).await? else {
        return Ok(ContentReadStep::StaleOrDiverged);
    };
    if !renew_candidate_lease(client, handle).await? {
        return Ok(ContentReadStep::StaleOrDiverged);
    }
    let result = transport.read_content(&bundle, side).await;
    match record_content_check(client, &permit, result).await? {
        MutationOutcome::Applied(next) => Ok(ContentReadStep::ObservationRecorded(next)),
        MutationOutcome::StaleOrDiverged => Ok(ContentReadStep::StaleOrDiverged),
        MutationOutcome::Incomplete => Err(StoreError::Inconsistent),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentCheckState {
    Intact,
    Unavailable,
    AuthenticationFailed,
    IntegrityFailed,
}

impl ContentCheckState {
    fn db_value(self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::Unavailable => "unavailable",
            Self::AuthenticationFailed => "authentication_failed",
            Self::IntegrityFailed => "integrity_failed",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "intact" => Ok(Self::Intact),
            "unavailable" => Ok(Self::Unavailable),
            "authentication_failed" => Ok(Self::AuthenticationFailed),
            "integrity_failed" => Ok(Self::IntegrityFailed),
            _ => Err(StoreError::Inconsistent),
        }
    }
}

/// Complete-read report from the trusted verifier, not STAT/POST counts or a
/// caller's intended input. `checked_articles` covers the whole required sealed
/// plan, including recovery articles. The digest/size describe the authenticated
/// plaintext actually read. A malformed/mismatched report is not accepted proof.
pub struct ContentCheckReport<'a> {
    pub backbone: &'a str,
    pub plan_sha256: &'a str,
    pub plaintext_sha256: &'a str,
    pub plaintext_size: i64,
    pub checked_articles: i32,
}

impl fmt::Debug for ContentCheckReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentCheckReport([REDACTED])")
    }
}

/// Nonaffirmative observations invalidate a prior intact observation on this
/// side/round. They never declare terminal loss, spend a repost budget, or grant
/// fresh-generation/cleanup permission. Retry/repair policy stays with verifier.
pub enum ContentCheckResult<'a> {
    Intact(ContentCheckReport<'a>),
    Unavailable,
    AuthenticationFailed,
    IntegrityFailed,
}

impl fmt::Debug for ContentCheckResult<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentCheckResult([REDACTED])")
    }
}

#[derive(Clone)]
pub struct ContentCheckPermit {
    handle: GenerationHandle,
    side: ArticleSide,
    backbone: String,
    plan: String,
    hash: String,
    size: i64,
    articles: i32,
}

impl fmt::Debug for ContentCheckPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentCheckPermit([REDACTED])")
    }
}

fn backbone<'a>(custody: &PostedFileParams<'a>, side: ArticleSide) -> &'a str {
    match side {
        ArticleSide::Post => custody.backbone_post,
        ArticleSide::Verify => custody.backbone_verify,
    }
}

/// Prepare before reading content, with no transaction over the read. A permit
/// is bound to the entire original mutation fence AND immutable stored bundle.
/// Presence verification may already be finished; it is not content proof.
/// Parallel reads may obtain the same fence, but only one changing callback can
/// commit; the other must reselect/recheck, never substitute a refreshed handle.
pub async fn prepare_content_check(
    client: &mut Client,
    handle: &GenerationHandle,
    side: ArticleSide,
) -> Result<Option<ContentCheckPermit>, StoreError> {
    Ok(prepare_content_read(client, handle, side)
        .await?
        .map(|(permit, _)| permit))
}

async fn prepare_content_read(
    client: &mut Client,
    handle: &GenerationHandle,
    side: ArticleSide,
) -> Result<Option<(ContentCheckPermit, StoredGeneration)>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(None);
    };
    if current.get::<_, &str>("state") != "verifying" || handle.round == 0 {
        return Ok(None);
    }
    let bundle = stored(&tx, &handle.generation).await?;
    let custody = bundle.custody()?;
    let permit = ContentCheckPermit {
        handle: handle.clone(),
        side,
        backbone: backbone(&custody, side).to_owned(),
        plan: bundle.plan_sha256().to_owned(),
        hash: custody.plaintext_sha256.to_owned(),
        size: custody.plaintext_size,
        articles: custody.article_count,
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some((permit, bundle)))
}

/// Persist an observation under its original generation/attempt/claim/registry/
/// round/revision fence. Invalid successful reports write nothing. An ambiguous
/// commit requires recover/load_checkpoint before any retry. A repeated callback
/// cannot erase or restore a newer observation by acquiring a newer revision.
pub async fn record_content_check(
    client: &mut Client,
    permit: &ContentCheckPermit,
    result: ContentCheckResult<'_>,
) -> Result<MutationOutcome, StoreError> {
    let state = checked_state(
        result,
        &permit.backbone,
        &permit.plan,
        &permit.hash,
        permit.size,
        permit.articles,
    )?;
    let handle = &permit.handle;
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "verifying" || handle.round == 0 {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let changed = store_observation(
        &tx,
        &handle.generation,
        handle.round,
        permit.side,
        &permit.backbone,
        state,
        permit.articles,
    )
    .await?;
    // A conflicting/inconsistent stored identity must not look like idempotence.
    // Loading also validates complete-article counts against immutable custody.
    load_checks(&tx, handle).await?;
    if changed == 1 {
        tx.execute(
            "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            &[&handle.generation],
        )
        .await
        .map_err(database_error)?;
    }
    commit_mutation(tx, handle).await
}

pub(super) fn checked_state(
    result: ContentCheckResult<'_>,
    backbone: &str,
    plan: &str,
    hash: &str,
    size: i64,
    articles: i32,
) -> Result<ContentCheckState, StoreError> {
    Ok(match result {
        ContentCheckResult::Intact(report) => {
            if report.backbone != backbone
                || report.plan_sha256 != plan
                || report.plaintext_sha256 != hash
                || report.plaintext_size != size
                || report.checked_articles != articles
            {
                return Err(StoreError::InvalidInput);
            }
            ContentCheckState::Intact
        }
        ContentCheckResult::Unavailable => ContentCheckState::Unavailable,
        ContentCheckResult::AuthenticationFailed => ContentCheckState::AuthenticationFailed,
        ContentCheckResult::IntegrityFailed => ContentCheckState::IntegrityFailed,
    })
}

// Both candidate and retained callbacks already hold their original health fence.
// This helper grants no mutation authority of its own. Identical reports do not
// refresh checked_at; retrying a callback cannot manufacture a fresher check.
pub(super) async fn store_observation(
    tx: &Transaction<'_>,
    generation: &str,
    round: i64,
    side: ArticleSide,
    backbone: &str,
    state: ContentCheckState,
    articles: i32,
) -> Result<u64, StoreError> {
    let checked_articles = if state == ContentCheckState::Intact {
        articles
    } else {
        0
    };
    let changed = tx
        .execute(
            "INSERT INTO posted_file_content_checks AS c \
             (generation_id, round, side, backbone, state, checked_articles, checked_at) \
             VALUES ($1, $2, $3, $4, $5, $6, floor(extract(epoch FROM clock_timestamp()))::bigint) \
             ON CONFLICT (generation_id, round, side) DO UPDATE \
             SET state = EXCLUDED.state, checked_articles = EXCLUDED.checked_articles, \
                 checked_at = EXCLUDED.checked_at \
             WHERE c.backbone = EXCLUDED.backbone \
             AND (c.state, c.checked_articles) IS DISTINCT FROM \
                 (EXCLUDED.state, EXCLUDED.checked_articles)",
            &[
                &generation,
                &round,
                &side.db_value(),
                &backbone,
                &state.db_value(),
                &checked_articles,
            ],
        )
        .await
        .map_err(database_error)?;
    let recorded = tx
        .query_one(
            "SELECT backbone, state, checked_articles FROM posted_file_content_checks \
             WHERE generation_id = $1 AND round = $2 AND side = $3",
            &[&generation, &round, &side.db_value()],
        )
        .await
        .map_err(database_error)?;
    if recorded.get::<_, &str>("backbone") != backbone
        || recorded.get::<_, &str>("state") != state.db_value()
        || recorded.get::<_, i32>("checked_articles") != checked_articles
    {
        return Err(StoreError::Inconsistent);
    }
    Ok(changed)
}

pub(super) async fn load_checks(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
) -> Result<[Option<ContentCheckState>; 2], StoreError> {
    let bundle = stored(tx, &handle.generation).await?;
    let custody = bundle.custody()?;
    let mut checks = [None; 2];
    for row in tx
        .query(
            "SELECT side, backbone, state, checked_articles FROM posted_file_content_checks \
             WHERE generation_id = $1 AND round = $2",
            &[&handle.generation, &handle.round],
        )
        .await
        .map_err(database_error)?
    {
        let side: i16 = row.get("side");
        let side = match side {
            0 => ArticleSide::Post,
            1 => ArticleSide::Verify,
            _ => return Err(StoreError::Inconsistent),
        };
        let state = ContentCheckState::parse(row.get("state"))?;
        let expected_count = if state == ContentCheckState::Intact {
            custody.article_count
        } else {
            0
        };
        if row.get::<_, &str>("backbone") != backbone(&custody, side)
            || row.get::<_, i32>("checked_articles") != expected_count
        {
            return Err(StoreError::Inconsistent);
        }
        checks[side.db_value() as usize] = Some(state);
    }
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_reports_and_outcomes_redact_all_identity() {
        let report = ContentCheckReport {
            backbone: "private-backbone",
            plan_sha256: "private-plan",
            plaintext_sha256: "private-digest",
            plaintext_size: 1024,
            checked_articles: 3,
        };
        assert_eq!(format!("{report:?}"), "ContentCheckReport([REDACTED])");
        assert_eq!(
            format!("{:?}", ContentCheckResult::Intact(report)),
            "ContentCheckResult([REDACTED])"
        );
        for state in [
            ContentCheckState::Intact,
            ContentCheckState::Unavailable,
            ContentCheckState::AuthenticationFailed,
            ContentCheckState::IntegrityFailed,
        ] {
            assert_eq!(ContentCheckState::parse(state.db_value()), Ok(state));
        }
        assert_eq!(
            ContentCheckState::parse("usable"),
            Err(StoreError::Inconsistent)
        );
    }
}
