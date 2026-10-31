//! Behavioral acceptance below the production pre-artifact identity helper.
//!
//! Only `WriterDb` is scripted: the supplied SQL, typed ordered binds and
//! selected proof rows cross the real helper boundary. Expectations below are
//! hand-derived from the declared publication contract, not helper source text.
//! In particular, a successful no-op helper cannot satisfy a script.
//!
//! This bank does NOT prove PostgreSQL execution, lock behavior under a live
//! deletion race, driver UUID compatibility, artifact or bucket ordering, or
//! queue and checkpoint lifecycle. The edge adapters must consume this same
//! helper before runtime acceptance. It also does not change the existing
//! grant publication convergence: a different first-writer provider remains
//! the stored grant proof there, never rewritten.

use std::cell::RefCell;
use std::fmt::Debug;

use nntune_ingest::jobs::{JobCheckpoint, JobSource, JobState};
use nntune_ingest::queue::QueueMsg;
use nntune_ingest::retry::JobError;
use nntune_ingest::transcode::TranscodeInput;
use nntune_ingest::writer_execution::{publication_context, DbFailure, Value, WriterDb};

const USER: &str = "publication_owner";
const OTHER_USER: &str = "other_publication_owner";
const JOB: &str = "job_publication_0123456789";
const OTHER_JOB: &str = "job_other_9876543210";
const KEY: &str =
    "ingest:publication_owner:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:upload";
const OTHER_KEY: &str =
    "ingest:publication_owner:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:upload";
const TRACK: &str = "cccccccc-3333-4333-8333-333333333333";
const OTHER_TRACK: &str = "dddddddd-4444-4444-8444-444444444444";
const PROVIDER: &str = "eeeeeeee-5555-4555-8555-555555555555";
const OTHER_PROVIDER: &str = "ffffffff-6666-4666-8666-666666666666";
const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const KEY_SUFFIX_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const KEY_SUFFIX_D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

// Bounded proof-query semantics, not a SQL engine. Supports one conjunctive
// inner-join SELECT with balanced grouping; aliases, order, numbering, CASTs,
// harmless parens, extra ANDs, extra projection and plain share lock are
// allowed. Rejects OR/NOT, UNION, outer joins, subqueries, expressions,
// FALSE/TRUE bypass and false predicates. No execution or live-race proof.

fn tokens(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i..].starts_with(b"--") {
            i += sql[i..].find('\n').unwrap_or(bytes.len() - i);
        } else if bytes[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'\'' {
            // A quoted diagnostic cannot supply a missing predicate or lock.
            i += 1;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"''") {
                    i += 2;
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            out.push("literal".into());
        } else if bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$') {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'$' | b'.'))
            {
                i += 1;
            }
            out.push(sql[start..i].to_ascii_lowercase());
        } else {
            out.push(char::from(bytes[i]).to_string());
            i += 1;
        }
    }
    out
}

fn strip_casts(sql: &str) -> String {
    sql.replace("::text", "")
        .replace("::TEXT", "")
        .replace("::Text", "")
        .replace("::uuid", "")
        .replace("::UUID", "")
        .replace("::Uuid", "")
}

fn normalize_aliases(words: &mut [String]) -> bool {
    for table in ["ingest_jobs", "users", "providers", "tracks"] {
        let Some(index) = words
            .windows(2)
            .position(|pair| matches!(pair[0].as_str(), "from" | "join") && pair[1] == table)
        else {
            return false;
        };
        let next = index + 2;
        let alias = if words.get(next).is_some_and(|word| word == "as") {
            words.get(next + 1).cloned()
        } else {
            words
                .get(next)
                .filter(|word| !matches!(word.as_str(), "join" | "inner" | "on" | "where" | ","))
                .cloned()
        };
        if let Some(alias) = alias {
            // A bare keyword is never an alias; the filter above already
            // excluded the join plumbing, but guard the lock tail too.
            if matches!(
                alias.as_str(),
                "for" | "share" | "of" | "select" | "from" | "and" | "or"
            ) {
                continue;
            }
            let prefix = format!("{alias}.");
            for word in words.iter_mut() {
                if *word == alias {
                    *word = table.to_string();
                } else if let Some(column) = word.strip_prefix(&prefix) {
                    *word = format!("{table}.{column}");
                }
            }
        }
    }
    true
}

fn balanced_parens(words: &[String]) -> bool {
    let mut depth = 0;
    for w in words {
        if w == "(" {
            depth += 1;
        } else if w == ")" {
            if depth == 0 {
                return false;
            }
            depth -= 1;
        }
    }
    depth == 0
}

fn true_only_as_on_true(words: &[String]) -> bool {
    let at: Vec<usize> = words
        .iter()
        .enumerate()
        .filter(|(_, w)| *w == "true")
        .map(|(i, _)| i)
        .collect();
    if at.is_empty() {
        return true;
    }
    if at.len() != 1 {
        return false;
    }
    let i = at[0];
    if i == 0 || words[i - 1] != "on" {
        return false;
    }
    words.get(i + 1).map_or(true, |w| {
        matches!(w.as_str(), "join" | "where" | "for" | "and")
    })
}

fn strip_outer_owned(mut v: Vec<String>) -> Vec<String> {
    loop {
        if v.len() < 2 || v[0] != "(" || v[v.len() - 1] != ")" {
            break;
        }
        let mut depth = 0;
        let mut wraps = true;
        for (i, w) in v.iter().enumerate() {
            if w == "(" {
                depth += 1;
            } else if w == ")" {
                depth -= 1;
                if depth == 0 && i + 1 < v.len() {
                    wraps = false;
                    break;
                }
            }
        }
        if wraps && depth == 0 {
            v = v[1..v.len() - 1].to_vec();
        } else {
            break;
        }
    }
    v
}

fn and_conjuncts(words: &[String]) -> Vec<Vec<String>> {
    let Some(from_at) = words.iter().position(|w| w == "from") else {
        return Vec::new();
    };
    let for_at = words
        .iter()
        .skip(from_at + 1)
        .position(|w| w == "for")
        .map(|i| i + from_at + 1)
        .unwrap_or(words.len());
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut depth = 0;
    for tok in &words[from_at + 1..for_at] {
        if tok == "(" {
            depth += 1;
            cur.push(tok.clone());
        } else if tok == ")" {
            if depth > 0 {
                depth -= 1;
            }
            cur.push(tok.clone());
        } else if depth == 0
            && matches!(
                tok.as_str(),
                "on" | "and" | "where" | "join" | "inner" | "cross"
            )
        {
            if !cur.is_empty() {
                out.push(strip_outer_owned(std::mem::take(&mut cur)));
            }
        } else {
            cur.push(tok.clone());
        }
    }
    if !cur.is_empty() {
        out.push(strip_outer_owned(cur));
    }
    out
}

fn is_placeholder(tok: &str) -> bool {
    tok.strip_prefix('$')
        .and_then(|r| r.parse::<usize>().ok())
        .is_some_and(|n| n > 0)
}

fn has_exact_equals(conjuncts: &[Vec<String>], left: &str, right: &str) -> bool {
    conjuncts.iter().any(|c| {
        c.len() == 3
            && c[1] == "="
            && ((c[0] == left && c[2] == right) || (c[0] == right && c[2] == left))
    })
}

fn has_bind_conjunct(conjuncts: &[Vec<String>], column: &str) -> bool {
    conjuncts.iter().any(|c| {
        c.len() == 3
            && c[1] == "="
            && ((c[0] == column && is_placeholder(&c[2]))
                || (c[2] == column && is_placeholder(&c[0])))
    })
}

fn has_fence_conjunct(conjuncts: &[Vec<String>]) -> bool {
    conjuncts
        .iter()
        .any(|c| c == &["users.deleting_at", "is", "null"])
}

fn has_false_predicate(conjuncts: &[Vec<String>]) -> bool {
    for c in conjuncts {
        if c == &["false"] {
            return true;
        }
        if c.len() == 3 && c[1] == "=" {
            let ident = |s: &str| s.contains('.') || is_placeholder(s);
            if !ident(&c[0]) && !ident(&c[2]) {
                return true;
            }
        }
    }
    false
}

fn lock_ok(words: &[String]) -> bool {
    let Some(for_at) = words.iter().position(|w| w == "for") else {
        return false;
    };
    if words.get(for_at + 1).map(String::as_str) != Some("share") {
        return false;
    }
    let tail = &words[for_at..];
    if tail
        .iter()
        .any(|w| matches!(w.as_str(), "nowait" | "skip" | "locked" | "update"))
    {
        return false;
    }
    if !tail.iter().any(|w| w == "of") {
        return true;
    }
    let Some(of_rel) = tail.iter().position(|w| w == "of") else {
        return false;
    };
    tail[of_rel + 1..].iter().any(|w| w == "users")
}

fn select_column_order(tokens: &[String]) -> Vec<String> {
    let Some(select_at) = tokens.iter().position(|w| w == "select") else {
        return Vec::new();
    };
    let Some(from_at) = tokens
        .iter()
        .enumerate()
        .skip(select_at + 1)
        .find(|(_, w)| *w == "from")
        .map(|(i, _)| i)
    else {
        return Vec::new();
    };
    let mut order = Vec::new();
    let mut current: Option<String> = None;
    for token in &tokens[select_at + 1..from_at] {
        if token == "," {
            if let Some(col) = current.take() {
                order.push(col);
            }
        } else if current.is_none() && token.contains('.') && !token.starts_with('$') {
            current = Some(token.clone());
        }
    }
    if let Some(col) = current.take() {
        order.push(col);
    }
    order
}

fn live_publication_identity_query(sql: &str) -> bool {
    let cleaned = strip_casts(sql);
    let words = tokens(&cleaned);
    // Conjunctive single-SELECT subset only; grouping must balance.
    if words.iter().filter(|w| *w == "select").count() != 1 {
        return false;
    }
    if words.iter().any(|w| {
        matches!(
            w.as_str(),
            "or" | "not"
                | "union"
                | "except"
                | "intersect"
                | "left"
                | "right"
                | "full"
                | "coalesce"
                | "nullif"
                | "case"
                | "when"
                | "then"
                | "else"
                | "end"
                | "false"
                | ";"
                | "delete"
                | "insert"
        )
    }) {
        return false;
    }
    if !balanced_parens(&words) || !true_only_as_on_true(&words) {
        return false;
    }
    let mut norm = words;
    if !normalize_aliases(&mut norm) {
        return false;
    }
    // Proof must return at least the owned identities; extras allowed.
    let selected = select_column_order(&norm);
    for req in [
        "ingest_jobs.idempotency_key",
        "ingest_jobs.user_id",
        "providers.id",
        "providers.user_id",
        "tracks.id",
    ] {
        if !selected.iter().any(|c| c == req) {
            return false;
        }
    }
    if selected.len() != {
        let mut dedup = selected.clone();
        dedup.sort();
        dedup.dedup();
        dedup.len()
    } {
        return false;
    }
    if !norm.iter().any(|w| w == "where") || !lock_ok(&norm) {
        return false;
    }
    let conjuncts = and_conjuncts(&norm);
    if has_false_predicate(&conjuncts) {
        return false;
    }
    has_exact_equals(&conjuncts, "users.id", "ingest_jobs.user_id")
        && has_exact_equals(&conjuncts, "users.id", "providers.user_id")
        && has_bind_conjunct(&conjuncts, "ingest_jobs.id")
        && has_bind_conjunct(&conjuncts, "tracks.id")
        && has_bind_conjunct(&conjuncts, "providers.id")
        && has_fence_conjunct(&conjuncts)
}

fn placeholder_numbers(tokens: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    for token in tokens {
        if let Some(rest) = token.strip_prefix('$') {
            if let Ok(n) = rest.parse::<usize>() {
                if n > 0 {
                    out.push(n);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn binds_for_placeholder(tokens: &[String], n: usize) -> Vec<String> {
    let marker = format!("${n}");
    let mut out = Vec::new();
    for w in tokens.windows(3) {
        if w[1] != "=" {
            continue;
        }
        if w[0] == marker && w[2].contains('.') {
            out.push(w[2].clone());
        } else if w[2] == marker && w[0].contains('.') {
            out.push(w[0].clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn uuid_bytes_equal(left: &str, right: &str) -> bool {
    match (uuid::Uuid::parse_str(left), uuid::Uuid::parse_str(right)) {
        (Ok(a), Ok(b)) => a.as_bytes() == b.as_bytes(),
        _ => false,
    }
}

#[test]
fn publication_sql_model_accepts_live_owner_join_with_aliases_and_reordered_predicates() {
    let canonical = "SELECT providers.user_id, providers.id, tracks.id, ingest_jobs.user_id, ingest_jobs.idempotency_key FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id JOIN providers ON providers.user_id = users.id JOIN tracks ON true WHERE ingest_jobs.id = $1 AND providers.id = $2 AND tracks.id = $3 AND users.deleting_at IS NULL FOR SHARE OF users";
    assert!(live_publication_identity_query(canonical));
    assert!(live_publication_identity_query(
        "select j.idempotency_key, j.user_id, p.id, p.user_id, t.id from ingest_jobs as j inner join users as u on j.user_id = u.id inner join providers as p on p.user_id = u.id inner join tracks as t on t.id = $3::text::uuid where $1 = j.id and p.id = $2::text::uuid and u.deleting_at is null and j.status = 'queued' for share of u"
    ));
    // Order and numbering are immaterial as long as each bind is conjunctive.
    assert!(live_publication_identity_query(
        "SELECT tracks.id, ingest_jobs.idempotency_key, providers.id, ingest_jobs.user_id, providers.user_id FROM tracks JOIN ingest_jobs ON ingest_jobs.id = $3 JOIN users ON users.id = ingest_jobs.user_id JOIN providers ON providers.user_id = users.id AND providers.id = $2 WHERE tracks.id = $1 AND users.deleting_at IS NULL FOR SHARE OF users"
    ));
    // Harmless grouping, extra projection and plain share lock stay valid.
    assert!(live_publication_identity_query(&canonical.replace(
        "AND users.deleting_at IS NULL",
        "AND (users.deleting_at IS NULL)"
    )));
    assert!(live_publication_identity_query(&canonical.replace(
        "SELECT providers.user_id,",
        "SELECT ingest_jobs.id, providers.user_id,"
    )));
    assert!(live_publication_identity_query(
        &canonical.replace("FOR SHARE OF users", "FOR SHARE")
    ));
}

#[test]
fn publication_sql_model_rejects_missing_reversed_unrelated_and_comment_only_fences() {
    let canonical = "SELECT providers.user_id, providers.id, tracks.id, ingest_jobs.user_id, ingest_jobs.idempotency_key FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id JOIN providers ON providers.user_id = users.id JOIN tracks ON true WHERE ingest_jobs.id = $1 AND providers.id = $2 AND tracks.id = $3 AND users.deleting_at IS NULL FOR SHARE OF users";
    let mutants = [
        canonical.replace("AND users.deleting_at IS NULL", ""),
        canonical.replace("IS NULL", "IS NOT NULL"),
        canonical.replace("users.id = ingest_jobs.user_id", "users.id = ingest_jobs.id"),
        canonical.replace("providers.user_id = users.id", "providers.id = users.id"),
        canonical.replace("ingest_jobs.id = $1", "ingest_jobs.user_id = $1"),
        canonical.replace("providers.id = $2", "providers.user_id = $2"),
        canonical.replace("tracks.id = $3", "tracks.mbid = $3"),
        canonical.replace(" FOR SHARE OF users", ""),
        canonical.replace("FOR SHARE OF users", "FOR SHARE OF ingest_jobs"),
        canonical.replace("FOR SHARE OF users", "FOR UPDATE OF users"),
        format!("{canonical} NOWAIT"),
        format!("{canonical} SKIP LOCKED"),
        canonical.replace("AND users.deleting_at IS NULL", "OR users.deleting_at IS NULL"),
        canonical.replace("JOIN users", "LEFT JOIN users"),
        canonical.replace("JOIN providers", "LEFT JOIN providers"),
        canonical.replace(
            "AND users.deleting_at IS NULL",
            "/* AND users.deleting_at IS NULL */",
        ),
        canonical.replace(
            "AND users.deleting_at IS NULL",
            "AND 'users.deleting_at IS NULL' = 'users.deleting_at IS NULL'",
        ),
        format!("SELECT user_id FROM ingest_jobs WHERE id = $1 -- {canonical}"),
        canonical.replace(
            "SELECT providers.user_id, providers.id, tracks.id,",
            "SELECT providers.user_id, tracks.id,",
        ),
        canonical.replace("providers.id,", "providers.label,"),
        canonical.replace("JOIN providers ON providers.user_id = users.id ", ""),
        canonical.replace(
            "SELECT providers.user_id, providers.id, tracks.id, ingest_jobs.user_id, ingest_jobs.idempotency_key",
            "SELECT providers.user_id, providers.id, tracks.id, ingest_jobs.user_id",
        ),
        format!("{canonical} UNION SELECT 1"),
        // Negated, suffixed and expression-hidden predicates.
        canonical.replace(
            "AND users.deleting_at IS NULL",
            "AND NOT (users.deleting_at IS NULL)",
        ),
        canonical.replace(
            "AND users.deleting_at IS NULL",
            "AND (users.deleting_at IS NULL) = FALSE",
        ),
        canonical.replace(
            "AND users.deleting_at IS NULL",
            "AND (users.deleting_at IS NULL) = TRUE",
        ),
        canonical.replace(
            "users.id = ingest_jobs.user_id",
            "COALESCE(users.id, '') = ingest_jobs.user_id",
        ),
        canonical.replace(
            "users.id = ingest_jobs.user_id",
            "(users.id = ingest_jobs.user_id) = FALSE",
        ),
        canonical.replace("tracks.id = $3", "COALESCE(tracks.id, '') = $3"),
        canonical.replace("providers.id = $2", "COALESCE(providers.id, '') = $2"),
        canonical.replace("JOIN tracks ON true", "JOIN tracks ON false"),
        canonical.replace("FOR SHARE OF users", "AND 1 = 0 FOR SHARE OF users"),
        canonical.replace("FOR SHARE OF users", "AND FALSE FOR SHARE OF users"),
        canonical.replace("WHERE", "WHERE ("),
    ];
    for mutant in mutants {
        assert!(
            !live_publication_identity_query(&mutant),
            "unsafe SQL accepted: {mutant}"
        );
    }
}

// ---------------------------------------------------------------------------
// Fixtures. The durable track map rides the checkpoint as unknown JSON so
// this bank compiles before the field lands; older checkpoints without it
// still decode, and the helper must fail those closed without database I/O.
// ---------------------------------------------------------------------------

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn scratch_prefix_for(job: &str) -> String {
    format!("ingest-scratch/{job}/r1/")
}

fn input_for(file_index: usize) -> TranscodeInput {
    let (suffix, sha) = match file_index {
        2 => (KEY_SUFFIX_C, SHA_A),
        7 => (KEY_SUFFIX_D, SHA_B),
        _ => panic!("test helper only models sparse ordinals 2 and 7"),
    };
    let prefix = scratch_prefix_for(JOB);
    TranscodeInput {
        file_index,
        key: format!("{prefix}in/{file_index:04}-{suffix}"),
        sha256: sha.to_owned(),
        size: 512,
        scratch_prefix: prefix,
    }
}

fn checkpoint_json_with_tracks(
    track_ids: Option<serde_json::Value>,
    provider: Option<&str>,
) -> serde_json::Value {
    // Seed from production initial() so future fields keep decoding; all
    // overrides are JSON-only, never a struct literal.
    let seed = message_with_provider(provider);
    let base = nntune_ingest::checkpoint::initial(&seed);
    let mut value = serde_json::to_value(&base).expect("checkpoint serializes");
    let obj = value.as_object_mut().expect("checkpoint is an object");
    obj.insert("state".to_string(), serde_json::json!("transcoding"));
    obj.insert(
        "transcode_inputs".to_string(),
        serde_json::to_value(vec![input_for(2), input_for(7)]).expect("inputs serialize"),
    );
    obj.insert("transcode_next".to_string(), serde_json::json!(0));
    if let Some(map) = track_ids {
        obj.insert("track_ids".to_string(), map);
    }
    value
}

fn checkpoint_with_tracks(
    track_ids: Option<serde_json::Value>,
    provider: Option<&str>,
) -> JobCheckpoint {
    serde_json::from_value(checkpoint_json_with_tracks(track_ids, provider))
        .expect("checkpoint with unknown track map still decodes")
}

fn valid_track_map() -> serde_json::Value {
    serde_json::json!({ "2": TRACK, "7": OTHER_TRACK })
}

fn message_with_provider(provider: Option<&str>) -> QueueMsg {
    QueueMsg {
        job_id: JOB.to_owned(),
        user_id: USER.to_owned(),
        idempotency_key: KEY.to_owned(),
        source: JobSource::NzbUpload,
        state: JobState::Transcoding,
        attempt: 1,
        repair_rounds: 0,
        query: None,
        indexer_ref: None,
        categories: Vec::new(),
        limit: None,
        page: None,
        trace_id: None,
        payload_ref: None,
        provider_id: provider.map(str::to_owned),
    }
}

#[track_caller]
fn error<T: Debug>(result: Result<T, JobError>, transient: bool) -> JobError {
    let failure = result.expect_err("invalid or unverified publication identity must fail");
    assert_eq!(
        failure.transient, transient,
        "wrong retry policy: {failure:?}"
    );
    assert!(!failure.made_progress, "a refusal is not a continuation");
    assert_eq!(failure.retry_after_secs, None);
    failure
}

struct NoDb;

impl WriterDb for NoDb {
    async fn execute(&self, sql: &'static str, params: &[Value]) -> Result<u64, DbFailure> {
        panic!("refused request must not write: {sql} {params:?}")
    }

    async fn query(
        &self,
        sql: &'static str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure> {
        panic!("refused request must not query: {sql} {params:?}")
    }
}

struct ExpectedProof {
    user: String,
    provider: String,
    track: String,
    job: String,
    key: String,
}

struct ProofDb {
    expected: ExpectedProof,
    stored_provider_user: String,
    stored_provider_id: String,
    stored_track_id: String,
    stored_job_user: String,
    stored_job_key: String,
    rows_override: Option<Vec<Vec<Value>>>,
    failure: Option<DbFailure>,
    calls: RefCell<usize>,
}

impl ProofDb {
    fn valid(track: &str) -> Self {
        Self {
            expected: ExpectedProof {
                user: USER.to_owned(),
                provider: PROVIDER.to_owned(),
                track: track.to_owned(),
                job: JOB.to_owned(),
                key: KEY.to_owned(),
            },
            stored_provider_user: USER.to_owned(),
            stored_provider_id: PROVIDER.to_owned(),
            stored_track_id: track.to_owned(),
            stored_job_user: USER.to_owned(),
            stored_job_key: KEY.to_owned(),
            rows_override: None,
            failure: None,
            calls: RefCell::new(0),
        }
    }

    #[track_caller]
    fn assert_queried_once(&self) {
        assert_eq!(
            *self.calls.borrow(),
            1,
            "the helper owes exactly one fenced proof query"
        );
    }

    fn expected_for_column(&self, column: &str) -> Option<(&str, bool)> {
        match column {
            "users.id" | "ingest_jobs.user_id" | "providers.user_id" => {
                Some((&self.expected.user, false))
            }
            "ingest_jobs.id" => Some((&self.expected.job, false)),
            "ingest_jobs.idempotency_key" => Some((&self.expected.key, false)),
            "providers.id" => Some((&self.expected.provider, true)),
            "tracks.id" => Some((&self.expected.track, true)),
            _ => None,
        }
    }

    #[track_caller]
    fn check_binds(&self, sql: &str, params: &[Value]) {
        let cleaned = strip_casts(sql);
        let mut words = tokens(&cleaned);
        assert!(
            normalize_aliases(&mut words),
            "proof query must join all fenced tables: {sql}"
        );
        let numbers = placeholder_numbers(&words);
        let max = numbers.iter().max().copied().unwrap_or(0);
        assert_eq!(
            params.len(),
            max,
            "wrong typed bind count for {sql}: {params:?}"
        );
        for n in 1..=max {
            assert!(
                numbers.contains(&n),
                "placeholder ${n} is missing from {sql}"
            );
            let bound = binds_for_placeholder(&words, n);
            assert!(
                !bound.is_empty(),
                "placeholder ${n} binds no publication column in {sql}"
            );
            let Value::Text(actual) = &params[n - 1] else {
                panic!("placeholder ${n} must bind text identity: {params:?}");
            };
            for column in bound {
                let Some((expected, is_uuid)) = self.expected_for_column(&column) else {
                    panic!("placeholder ${n} binds unexpected column {column} in {sql}");
                };
                if is_uuid {
                    assert!(
                        uuid_bytes_equal(actual, expected),
                        "placeholder ${n} for {column} carries another UUID: {actual} vs {expected}"
                    );
                } else {
                    assert_eq!(
                        actual, expected,
                        "placeholder ${n} for {column} carries another identity"
                    );
                }
            }
        }
    }

    #[track_caller]
    fn proof_row(&self, sql: &str) -> Vec<Value> {
        let cleaned = strip_casts(sql);
        let mut words = tokens(&cleaned);
        assert!(normalize_aliases(&mut words));
        let mut row = Vec::new();
        for column in select_column_order(&words) {
            match column.as_str() {
                "providers.user_id" => row.push(text(&self.stored_provider_user)),
                "providers.id" => row.push(text(&self.stored_provider_id)),
                "tracks.id" => row.push(text(&self.stored_track_id)),
                "ingest_jobs.user_id" => row.push(text(&self.stored_job_user)),
                "ingest_jobs.idempotency_key" => row.push(text(&self.stored_job_key)),
                // Extra projection beyond the contract; the helper must ignore it.
                _ => row.push(text("extra-proof-column")),
            }
        }
        row
    }
}

impl WriterDb for ProofDb {
    async fn execute(&self, sql: &'static str, params: &[Value]) -> Result<u64, DbFailure> {
        panic!("publication identity must never write: {sql} {params:?}")
    }

    async fn query(
        &self,
        sql: &'static str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure> {
        *self.calls.borrow_mut() += 1;
        assert!(
            live_publication_identity_query(sql),
            "wrong ownership or fence semantics: {sql}"
        );
        self.check_binds(sql, params);
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if let Some(rows) = &self.rows_override {
            return Ok(rows.clone());
        }
        Ok(vec![self.proof_row(sql)])
    }
}

// ---------------------------------------------------------------------------
// Refusals before any database effect.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publication_refuses_message_checkpoint_mismatch_without_database_effects() {
    let db = NoDb;
    let input = input_for(2);
    let base_msg = message_with_provider(Some(OTHER_PROVIDER));
    let base_cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    // User, job and key must agree; any mismatch fails before I/O.
    let mut wrong_user_msg = base_msg.clone();
    wrong_user_msg.user_id = OTHER_USER.to_owned();
    let mut wrong_job_msg = base_msg.clone();
    wrong_job_msg.job_id = OTHER_JOB.to_owned();
    let mut wrong_key_msg = base_msg.clone();
    wrong_key_msg.idempotency_key = OTHER_KEY.to_owned();
    let mut wrong_user_cp_value =
        checkpoint_json_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    wrong_user_cp_value["user_id"] = serde_json::json!(OTHER_USER);
    let wrong_user_cp: JobCheckpoint = serde_json::from_value(wrong_user_cp_value).unwrap();
    for (msg, cp) in [
        (wrong_user_msg, base_cp.clone()),
        (wrong_job_msg, base_cp.clone()),
        (wrong_key_msg, base_cp.clone()),
        (base_msg.clone(), wrong_user_cp),
    ] {
        let failure = error(publication_context(&db, &msg, &cp, &input).await, false);
        assert_eq!(failure.code, "write_failed");
    }
}

#[tokio::test]
async fn publication_refuses_missing_null_or_empty_track_map_without_database_effects() {
    let db = NoDb;
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let input = input_for(2);
    // A missing map is not a completed empty map: without the recorded
    // selection the helper cannot name a track.
    for track_ids in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!({})),
    ] {
        let cp = checkpoint_with_tracks(track_ids, Some(PROVIDER));
        let failure = error(publication_context(&db, &msg, &cp, &input).await, false);
        assert_eq!(failure.code, "write_failed");
    }
}

#[tokio::test]
async fn publication_refuses_unknown_ordinal_or_key_mismatch_without_database_effects() {
    let db = NoDb;
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    // Ordinal 99 was never recorded; ordinal 2 with another key adopts
    // another file's bytes.
    let unknown = TranscodeInput {
        file_index: 99,
        ..input_for(2)
    };
    let mut wrong_key = input_for(2);
    wrong_key.key = input_for(7).key.clone();
    let mut wrong_sha = input_for(2);
    wrong_sha.sha256 = SHA_B.to_owned();
    for input in [unknown, wrong_key, wrong_sha] {
        let failure = error(publication_context(&db, &msg, &cp, &input).await, false);
        assert_eq!(failure.code, "write_failed");
    }
}

#[tokio::test]
async fn publication_refuses_malformed_track_or_provider_uuid_without_database_effects() {
    let db = NoDb;
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    for track_ids in [
        serde_json::json!({ "2": "not-a-uuid", "7": OTHER_TRACK }),
        serde_json::json!({ "2": "", "7": OTHER_TRACK }),
        serde_json::json!({ "2": "zzzzzzzz-3333-4333-8333-333333333333", "7": OTHER_TRACK }),
    ] {
        let cp = checkpoint_with_tracks(Some(track_ids), Some(PROVIDER));
        let failure = error(
            publication_context(&db, &msg, &cp, &input_for(2)).await,
            false,
        );
        assert_eq!(failure.code, "write_failed");
    }
    for provider in ["", "not-a-uuid", "eeeeeeee-5555-4555-8555-55555555555"] {
        let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(provider));
        let failure = error(
            publication_context(&db, &msg, &cp, &input_for(2)).await,
            false,
        );
        assert_eq!(failure.code, "write_failed");
    }
}

#[tokio::test]
async fn publication_refuses_missing_checkpoint_provider_without_stale_fallback() {
    let db = NoDb;
    // Even a well-formed stale message provider cannot supply attribution
    // once the live checkpoint is unattributed.
    for msg_provider in [None, Some(PROVIDER), Some(OTHER_PROVIDER)] {
        let msg = message_with_provider(msg_provider);
        let cp = checkpoint_with_tracks(Some(valid_track_map()), None);
        let failure = error(
            publication_context(&db, &msg, &cp, &input_for(2)).await,
            false,
        );
        assert_eq!(failure.code, "write_failed");
    }
}

#[tokio::test]
async fn publication_refuses_malformed_job_user_or_key_without_database_effects() {
    let db = NoDb;
    let input = input_for(2);
    let long_job = "j".repeat(65);
    let long_user = "u".repeat(129);
    let long_key = format!("ingest:publication_owner:{}", "a".repeat(768));
    for (job, user, key) in [
        ("", USER, KEY),
        ("bad job!", USER, KEY),
        (long_job.as_str(), USER, KEY),
        (JOB, "", KEY),
        (JOB, "owner:confused", KEY),
        (JOB, long_user.as_str(), KEY),
        (JOB, USER, ""),
        (JOB, USER, "not-an-ingest-key"),
        (JOB, USER, "ingest:missing-second-delimiter"),
        (JOB, USER, long_key.as_str()),
    ] {
        let mut msg = message_with_provider(Some(OTHER_PROVIDER));
        msg.job_id = job.to_owned();
        msg.user_id = user.to_owned();
        msg.idempotency_key = key.to_owned();
        let mut cp_value = checkpoint_json_with_tracks(Some(valid_track_map()), Some(PROVIDER));
        cp_value["job_id"] = serde_json::json!(job);
        cp_value["user_id"] = serde_json::json!(user);
        cp_value["idempotency_key"] = serde_json::json!(key);
        let cp: JobCheckpoint = serde_json::from_value(cp_value).unwrap();
        let failure = error(publication_context(&db, &msg, &cp, &input).await, false);
        assert_eq!(failure.code, "write_failed");
    }
}

// ---------------------------------------------------------------------------
// Valid proof through the real helper.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publication_uses_current_checkpoint_provider_despite_stale_message() {
    // The checkpoint is the live copy; a redelivered stale message (other
    // provider or unattributed) must never override it.
    for msg_provider in [None, Some(OTHER_PROVIDER), Some(PROVIDER)] {
        let db = ProofDb::valid(TRACK);
        let msg = message_with_provider(msg_provider);
        let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
        let context = publication_context(&db, &msg, &cp, &input_for(2))
            .await
            .expect("live owner plus owned provider, track and job must resolve");
        assert_eq!(context.track_id, TRACK);
        assert_eq!(context.provider_id, PROVIDER);
        db.assert_queried_once();
    }
}

#[tokio::test]
async fn publication_resolves_sparse_original_index_not_vector_position() {
    // Ordinals 2 and 7 share one checkpoint; vector position 1 is ordinal
    // 7, never ordinal 1. Requesting the second input must select the
    // second track, not the first.
    let db = ProofDb::valid(OTHER_TRACK);
    let msg = message_with_provider(None);
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    let context = publication_context(&db, &msg, &cp, &input_for(7))
        .await
        .expect("sparse ordinal 7 must resolve to its own recorded track");
    assert_eq!(context.track_id, OTHER_TRACK);
    assert_eq!(context.provider_id, PROVIDER);
    db.assert_queried_once();
}

#[tokio::test]
async fn publication_accepts_alternate_uuid_renderings_via_bytes() {
    let track_upper = TRACK.to_ascii_uppercase();
    let provider_upper = PROVIDER.to_ascii_uppercase();
    let track_mixed = "cCcCcCcC-3333-4333-8333-333333333333";
    let provider_mixed = "eEeEeEeE-5555-4555-8555-555555555555";
    assert!(uuid_bytes_equal(&track_upper, TRACK));
    assert!(uuid_bytes_equal(&provider_upper, PROVIDER));
    assert!(uuid_bytes_equal(track_mixed, TRACK));
    assert!(uuid_bytes_equal(provider_mixed, PROVIDER));
    for (stored_track, stored_provider) in [
        (track_upper.as_str(), provider_upper.as_str()),
        (track_mixed, provider_mixed),
        (TRACK, provider_upper.as_str()),
        (track_upper.as_str(), PROVIDER),
    ] {
        let mut db = ProofDb::valid(TRACK);
        db.stored_track_id = stored_track.to_owned();
        db.stored_provider_id = stored_provider.to_owned();
        let msg = message_with_provider(Some(OTHER_PROVIDER));
        let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
        let context = publication_context(&db, &msg, &cp, &input_for(2))
            .await
            .expect("same UUID bytes in another rendering are the same identity");
        // The helper returns the checkpoint identities it validated, not a
        // driver-cased echo.
        assert!(uuid_bytes_equal(&context.track_id, TRACK));
        assert!(uuid_bytes_equal(&context.provider_id, PROVIDER));
        db.assert_queried_once();
    }
}

// ---------------------------------------------------------------------------
// Database proof must agree; foreign, missing, malformed or ambiguous proof
// fails closed through the shared policy.
// ---------------------------------------------------------------------------

type ProofMutate = fn(&mut ProofDb);

fn mutate_provider_owner(db: &mut ProofDb) {
    db.stored_provider_user = OTHER_USER.to_owned();
}

fn mutate_provider_id(db: &mut ProofDb) {
    db.stored_provider_id = OTHER_PROVIDER.to_owned();
}

fn mutate_track_id(db: &mut ProofDb) {
    db.stored_track_id = OTHER_TRACK.to_owned();
}

fn mutate_job_owner(db: &mut ProofDb) {
    db.stored_job_user = OTHER_USER.to_owned();
}

fn mutate_job_key(db: &mut ProofDb) {
    db.stored_job_key = OTHER_KEY.to_owned();
}

fn mutate_provider_case(db: &mut ProofDb) {
    db.stored_provider_id = OTHER_PROVIDER.to_ascii_uppercase();
}

fn mutate_track_case(db: &mut ProofDb) {
    db.stored_track_id = OTHER_TRACK.to_ascii_uppercase();
}

#[tokio::test]
async fn publication_rejects_foreign_proof_identities() {
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    let input = input_for(2);
    // Each stored identity is replaced while the binds stay honest.
    let cases: &[(&str, ProofMutate)] = &[
        ("provider owner", mutate_provider_owner),
        ("provider id", mutate_provider_id),
        ("track id", mutate_track_id),
        ("job owner", mutate_job_owner),
        ("job key", mutate_job_key),
        ("provider other-case foreign bytes", mutate_provider_case),
        ("track other-case foreign bytes", mutate_track_case),
    ];
    for &(label, mutate) in cases {
        let mut db = ProofDb::valid(TRACK);
        mutate(&mut db);
        let failure = error(publication_context(&db, &msg, &cp, &input).await, false);
        assert_eq!(failure.code, "write_failed", "foreign {label}");
        db.assert_queried_once();
    }
}

#[tokio::test]
async fn publication_rejects_missing_proof_row_without_converging() {
    let mut db = ProofDb::valid(TRACK);
    db.rows_override = Some(Vec::new());
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    // Missing proof is permanent; the code depends on whether the fence
    // or the identities were absent, so only the retry policy is pinned.
    error(
        publication_context(&db, &msg, &cp, &input_for(2)).await,
        false,
    );
    db.assert_queried_once();
}

#[tokio::test]
async fn publication_rejects_ambiguous_proof_rows() {
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    let input = input_for(2);
    // Two identical proof rows are still ambiguous: the helper must see
    // exactly one live owner, provider, track and job.
    let single = ProofDb::valid(TRACK);
    // Build the honest row through the same SELECT ordering the helper
    // will request, then duplicate it.
    let honest_sql = "SELECT providers.user_id, providers.id, tracks.id, ingest_jobs.user_id, ingest_jobs.idempotency_key FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id JOIN providers ON providers.user_id = users.id JOIN tracks ON true WHERE ingest_jobs.id = $1 AND providers.id = $2 AND tracks.id = $3 AND users.deleting_at IS NULL FOR SHARE OF users";
    let row = single.proof_row(honest_sql);
    let mut db = ProofDb::valid(TRACK);
    db.rows_override = Some(vec![row.clone(), row]);
    let failure = error(publication_context(&db, &msg, &cp, &input).await, false);
    assert_eq!(failure.code, "write_failed");
    db.assert_queried_once();
}

fn malformed_rows(index: usize) -> Vec<Vec<Value>> {
    let honest = vec![
        text(USER),
        text(PROVIDER),
        text(TRACK),
        text(USER),
        text(KEY),
    ];
    let mut too_short = honest.clone();
    too_short.pop();
    let mut too_long = honest.clone();
    too_long.push(text("extra"));
    let mut wrong_type = honest;
    wrong_type[2] = Value::I64(7);
    match index {
        0 => vec![Vec::new()],
        1 => vec![vec![text(USER)]],
        2 => vec![too_short],
        3 => vec![too_long],
        4 => vec![wrong_type],
        5 => vec![vec![
            text(USER),
            text("not-a-uuid"),
            text(TRACK),
            text(USER),
            text(KEY),
        ]],
        _ => vec![vec![
            text(USER),
            text(PROVIDER),
            text("not-a-uuid"),
            text(USER),
            text(KEY),
        ]],
    }
}

async fn malformed_case(index: usize) {
    let mut db = ProofDb::valid(TRACK);
    db.rows_override = Some(malformed_rows(index));
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    let failure = error(
        publication_context(&db, &msg, &cp, &input_for(2)).await,
        false,
    );
    assert_eq!(failure.code, "write_failed");
    db.assert_queried_once();
}

macro_rules! malformed_tests {
    ($($name:ident, $idx:expr;)*) => {
        $(#[tokio::test]
        async fn $name() {
            malformed_case($idx).await;
        })*
    };
}

malformed_tests! {
    publication_rejects_empty_proof_row, 0;
    publication_rejects_single_column_proof_row, 1;
    publication_rejects_short_proof_row, 2;
    publication_rejects_long_proof_row, 3;
    publication_rejects_wrong_type_proof_row, 4;
    publication_rejects_malformed_provider_proof_row, 5;
    publication_rejects_malformed_track_proof_row, 6;
}

async fn sqlstate_case(sqlstate: Option<&str>, transient: bool) {
    let mut db = ProofDb::valid(TRACK);
    db.failure = Some(DbFailure {
        sqlstate: sqlstate.map(str::to_owned),
    });
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    let failure = error(
        publication_context(&db, &msg, &cp, &input_for(2)).await,
        transient,
    );
    assert_eq!(failure.code, "write_failed", "{sqlstate:?}");
    db.assert_queried_once();
}

macro_rules! sqlstate_tests {
    ($($name:ident, $state:expr, $transient:expr;)*) => {
        $(#[tokio::test]
        async fn $name() {
            sqlstate_case($state, $transient).await;
        })*
    };
}

// Literal policy, not computed with the production classifier.
sqlstate_tests! {
    publication_sqlstate_42p01_is_permanent, Some("42P01"), false;
    publication_sqlstate_42703_is_permanent, Some("42703"), false;
    publication_sqlstate_42p10_is_permanent, Some("42P10"), false;
    publication_sqlstate_22003_is_permanent, Some("22003"), false;
    publication_sqlstate_22p02_is_permanent, Some("22P02"), false;
    publication_sqlstate_23503_is_permanent, Some("23503"), false;
    publication_sqlstate_23505_is_permanent, Some("23505"), false;
    publication_sqlstate_23514_is_permanent, Some("23514"), false;
    publication_sqlstate_3f000_is_permanent, Some("3F000"), false;
    publication_sqlstate_none_is_transient, None, true;
    publication_sqlstate_08001_is_transient, Some("08001"), true;
    publication_sqlstate_08006_is_transient, Some("08006"), true;
    publication_sqlstate_40001_is_transient, Some("40001"), true;
    publication_sqlstate_40p01_is_transient, Some("40P01"), true;
    publication_sqlstate_53300_is_transient, Some("53300"), true;
    publication_sqlstate_57p03_is_transient, Some("57P03"), true;
    publication_sqlstate_57014_is_transient, Some("57014"), true;
}

#[tokio::test]
async fn publication_failures_never_echo_driver_diagnostics() {
    // A malformed transport field must not become a raw driver detail
    // through Debug formatting. This marker is synthetic, not a credential.
    let raw = "publication_detail_canary: synthetic driver diagnostic, not a SQLSTATE";
    let mut db = ProofDb::valid(TRACK);
    db.failure = Some(DbFailure {
        sqlstate: Some(raw.to_owned()),
    });
    let msg = message_with_provider(Some(OTHER_PROVIDER));
    let cp = checkpoint_with_tracks(Some(valid_track_map()), Some(PROVIDER));
    let failure = error(
        publication_context(&db, &msg, &cp, &input_for(2)).await,
        true,
    );
    assert_eq!(failure.code, "write_failed");
    assert!(
        !failure.detail.contains("publication_detail_canary"),
        "{failure:?}"
    );
    db.assert_queried_once();
}
