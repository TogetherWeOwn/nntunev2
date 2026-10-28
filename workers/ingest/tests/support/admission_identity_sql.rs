//! Bounded SQL semantics for the admission replay fixture, not a SQL engine.
//! Formatting, aliases, predicate order and unrelated extra AND predicates are
//! immaterial. The selected identity must belong to the current, share-locked
//! owner, rather than merely a previously admitted job. This does not prove
//! PostgreSQL execution or lock behavior under a live deletion race.

pub const FENCED_ADMISSION_SELECT: &str = "SELECT ingest_jobs.user_id, ingest_jobs.idempotency_key FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id WHERE ingest_jobs.id = $1 AND users.deleting_at IS NULL FOR SHARE OF users";

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

fn contains(tokens: &[String], expected: &[&str]) -> bool {
    tokens
        .windows(expected.len())
        .any(|part| part.iter().map(String::as_str).eq(expected.iter().copied()))
}

pub fn live_owner_identity_query(sql: &str) -> bool {
    let mut words = tokens(sql);
    // Model one inner-join SELECT. Reject OR/UNION and nested-query decoys,
    // rather than treating a predicate's mere textual presence as a fence.
    if words.iter().filter(|word| *word == "select").count() != 1
        || words.iter().any(|word| {
            matches!(
                word.as_str(),
                "or" | "union" | "except" | "intersect" | "left" | "right" | "full"
            )
        })
    {
        return false;
    }
    for table in ["ingest_jobs", "users"] {
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
                .filter(|word| !matches!(word.as_str(), "join" | "inner" | "on" | "where"))
                .cloned()
        };
        if let Some(alias) = alias {
            let prefix = format!("{alias}.");
            for word in &mut words {
                if *word == alias {
                    *word = table.into();
                } else if let Some(column) = word.strip_prefix(&prefix) {
                    *word = format!("{table}.{column}");
                }
            }
        }
    }
    let owner_link = contains(&words, &["users.id", "=", "ingest_jobs.user_id"])
        || contains(&words, &["ingest_jobs.user_id", "=", "users.id"]);
    let job_key = contains(&words, &["ingest_jobs.id", "=", "$1"])
        || contains(&words, &["$1", "=", "ingest_jobs.id"]);
    contains(
        &words,
        &[
            "select",
            "ingest_jobs.user_id",
            ",",
            "ingest_jobs.idempotency_key",
            "from",
        ],
    ) && contains(&words, &["join", "users"])
        && words.iter().any(|word| word == "where")
        && owner_link
        && job_key
        && contains(&words, &["users.deleting_at", "is", "null"])
        && contains(&words, &["for", "share", "of", "users"])
}

#[test]
fn sql_model_accepts_live_owner_join_with_aliases_and_reordered_predicates() {
    assert!(live_owner_identity_query(FENCED_ADMISSION_SELECT));
    assert!(live_owner_identity_query(
        "select j.user_id, j.idempotency_key from ingest_jobs as j inner join users as u on j.user_id = u.id where u.deleting_at is null and $1 = j.id and j.status = 'queued' for share of u"
    ));
}

#[test]
fn sql_model_rejects_missing_reversed_unrelated_and_comment_only_fences() {
    let mutants = [
        FENCED_ADMISSION_SELECT.replace("AND users.deleting_at IS NULL", ""),
        FENCED_ADMISSION_SELECT.replace("IS NULL", "IS NOT NULL"),
        FENCED_ADMISSION_SELECT.replace("users.id = ingest_jobs.user_id", "users.id = ingest_jobs.id"),
        FENCED_ADMISSION_SELECT.replace("ingest_jobs.id = $1", "ingest_jobs.user_id = $1"),
        FENCED_ADMISSION_SELECT.replace(" FOR SHARE OF users", ""),
        FENCED_ADMISSION_SELECT.replace("FOR SHARE OF users", "FOR SHARE OF ingest_jobs"),
        FENCED_ADMISSION_SELECT.replace("AND users.deleting_at IS NULL", "OR users.deleting_at IS NULL"),
        FENCED_ADMISSION_SELECT.replace("JOIN users", "LEFT JOIN users"),
        FENCED_ADMISSION_SELECT.replace("AND users.deleting_at IS NULL", "/* AND users.deleting_at IS NULL */"),
        FENCED_ADMISSION_SELECT.replace("AND users.deleting_at IS NULL", "AND 'users.deleting_at IS NULL' = 'users.deleting_at IS NULL'"),
        format!("SELECT user_id, idempotency_key FROM ingest_jobs WHERE id = $1 -- {FENCED_ADMISSION_SELECT}"),
    ];
    for mutant in mutants {
        assert!(
            !live_owner_identity_query(&mutant),
            "unsafe SQL accepted: {mutant}"
        );
    }
}

#[test]
fn emitted_admission_identity_select_requires_the_current_live_owner_fence() {
    assert!(
        live_owner_identity_query(crate::ingest_jobs::select_ingest_job_identity_sql()),
        "admission identity reselect must join the stored owner to live users and share-lock that owner"
    );
}
