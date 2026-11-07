//! Account selection/client composition (`account_selection::run`).
//!
//! Synthetic fakes only: a registry source and an `AccountSource` that record
//! every call. No network, credentials or Durable Object. These are native
//! composition tests, NOT evidence of real Worker service-binding behaviour.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use nntune_core::pool_channel::{self, Channel};
use nntune_stream::account_dispatch::{OperationReply, OperationRequest};
use nntune_stream::account_operation::{Operation, OperationError, Output, Served};
use nntune_stream::account_pool::{AccountConfig, WorkClass};
use nntune_stream::account_route::{
    CandidateRequest, ChannelKeys, RegistryError, RegistrySource, REGISTRY_BODY,
};
use nntune_stream::account_selection::{run, AccountSource, HINT_CONCURRENCY};
use nntune_stream::body::MAX_BODY_ARTICLE_BYTES;
use serde_json::{json, Value};

const ACCOUNT_KEY: [u8; 32] = [5; 32];
const RESOLVER_KEY: [u8; 32] = [9; 32];
const NOW_MS: u64 = 1_700_000_000_000;
const NOW_S: u64 = NOW_MS / 1000;
const ARTICLE: &str = "part1@example.invalid";

fn uid(n: u32) -> String {
    format!("{n:08x}-1234-4234-8234-123456789abc")
}

fn row(n: u32, tier: u32) -> AccountConfig {
    AccountConfig {
        id: uid(n),
        revision: 1,
        secret_ref: uid(0x1000 + n),
        can_read: true,
        max_connections: 4,
        stream_reserve: 1,
        ingest_limit: 2,
        tier,
        enabled: true,
    }
}

fn hint(n: u32, tier: u32, in_flight: u32, eligible: bool) -> Vec<u8> {
    serde_json::to_vec(&json!({"id": uid(n), "tier": tier,
        "in_flight": in_flight, "eligible": eligible}))
    .unwrap()
}

// ---------------------------------------------------------------- fakes

#[derive(Default)]
struct State {
    registry: RefCell<Option<Vec<u8>>>,
    registry_auth: RefCell<Vec<String>>,
    hints: RefCell<HashMap<String, Vec<u8>>>,
    candidate_calls: RefCell<Vec<(String, String, Vec<u8>)>>,
    execute_calls: RefCell<Vec<(String, String, Vec<u8>)>>,
    reply: RefCell<Option<OperationReply>>,
    /// Candidate reads hold until this opens (when `gated`).
    gated: Cell<bool>,
    gate: Cell<bool>,
    wakers: RefCell<Vec<Waker>>,
    active: Cell<usize>,
    peak: Cell<usize>,
    /// Execute never completes.
    execute_hangs: Cell<bool>,
    /// Candidate reads never complete.
    candidates_hang: Cell<bool>,
}

#[derive(Clone, Default)]
struct Fake(Rc<State>);

impl RegistrySource for Fake {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.0.registry_auth.borrow_mut().push(authorization.into());
        self.0.registry.borrow().clone().ok_or(RegistryError)
    }
}

impl AccountSource for Fake {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, RegistryError> {
        let s = &self.0;
        s.candidate_calls.borrow_mut().push((
            account_id.into(),
            authorization.into(),
            body.to_vec(),
        ));
        s.active.set(s.active.get() + 1);
        s.peak.set(s.peak.get().max(s.active.get()));
        if s.candidates_hang.get() {
            std::future::pending::<()>().await;
        }
        if s.gated.get() {
            poll_fn(|cx| {
                if s.gate.get() {
                    Poll::Ready(())
                } else {
                    // Combinator children are only re-polled when woken.
                    s.wakers.borrow_mut().push(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await;
        }
        s.active.set(s.active.get() - 1);
        s.hints
            .borrow()
            .get(account_id)
            .cloned()
            .ok_or(RegistryError)
    }

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply {
        self.0.execute_calls.borrow_mut().push((
            account_id.into(),
            authorization.into(),
            body.to_vec(),
        ));
        if self.0.execute_hangs.get() {
            std::future::pending::<()>().await;
        }
        self.0
            .reply
            .borrow_mut()
            .take()
            .unwrap_or(OperationReply::NotFound)
    }
}

impl Fake {
    fn new(rows: &[AccountConfig], hints: &[(u32, Vec<u8>)]) -> Self {
        let fake = Self::default();
        *fake.0.registry.borrow_mut() =
            Some(serde_json::to_vec(&json!({ "accounts": rows })).unwrap());
        for (n, raw) in hints {
            fake.0.hints.borrow_mut().insert(uid(*n), raw.clone());
        }
        fake
    }

    fn reply(self, reply: OperationReply) -> Self {
        *self.0.reply.borrow_mut() = Some(reply);
        self
    }

    fn executes(&self) -> usize {
        self.0.execute_calls.borrow().len()
    }

    fn candidates(&self) -> usize {
        self.0.candidate_calls.borrow().len()
    }

    fn reads(&self) -> usize {
        self.0.registry_auth.borrow().len() + self.candidates() + self.executes()
    }
}

fn served(n: u32, output: Output) -> OperationReply {
    OperationReply::Served(Served {
        provider_id: uid(n),
        output,
    })
}

fn body_op() -> Operation {
    Operation::Body(ARTICLE.into())
}

fn keys() -> ChannelKeys<'static> {
    ChannelKeys {
        account: Some(&ACCOUNT_KEY),
        resolver: Some(&RESOLVER_KEY),
    }
}

async fn select(
    fake: &Fake,
    class: WorkClass,
    operation: Operation,
) -> Result<Served, OperationError> {
    run(
        fake,
        fake,
        keys(),
        class,
        operation,
        || NOW_MS,
        std::future::pending::<()>(),
    )
    .await
}

fn executed_id(fake: &Fake) -> String {
    let calls = fake.0.execute_calls.borrow();
    assert_eq!(calls.len(), 1, "exactly one execute call");
    calls[0].0.clone()
}

// ------------------------------------------------------- happy / signing

#[tokio::test]
async fn selects_best_hint_and_signs_exact_candidate_and_operation_requests() {
    let rows = [row(1, 1), row(2, 0), row(3, 0)];
    let fake = Fake::new(
        &rows,
        &[
            (1, hint(1, 1, 0, true)),
            (2, hint(2, 0, 3, true)),
            (3, hint(3, 0, 1, true)),
        ],
    )
    .reply(served(3, Output::Body(b"x".to_vec())));
    let got = select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    assert_eq!(got.provider_id, uid(3));

    // Registry call: resolver channel, exact registry body.
    let reg = fake.0.registry_auth.borrow();
    assert_eq!(reg.len(), 1);
    assert_eq!(
        pool_channel::verify(
            Some(&RESOLVER_KEY),
            pool_channel::REGISTRY_PATH,
            Some(&reg[0]),
            REGISTRY_BODY,
            NOW_S
        ),
        Ok(())
    );

    // One candidate request per row; account namespace == body account_id.
    let calls = fake.0.candidate_calls.borrow();
    assert_eq!(calls.len(), 3);
    for (account, authorization, body) in calls.iter() {
        assert_eq!(
            Channel::Account.verify(
                Some(&ACCOUNT_KEY),
                pool_channel::CANDIDATE_PATH,
                Some(authorization),
                body,
                NOW_S
            ),
            Ok(())
        );
        // Domain separation: the resolver key cannot verify an account hint.
        assert!(Channel::Account
            .verify(
                Some(&RESOLVER_KEY),
                pool_channel::CANDIDATE_PATH,
                Some(authorization),
                body,
                NOW_S
            )
            .is_err());
        let value: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value, json!({"account_id": account, "class": "stream"}));
        assert_eq!(
            body,
            &serde_json::to_vec(&CandidateRequest {
                account_id: account.clone(),
                class: WorkClass::Stream
            })
            .unwrap()
        );
    }

    // Single operation call to the selected account, exact signed body.
    let (account, authorization, body) = fake.0.execute_calls.borrow()[0].clone();
    assert_eq!(account, uid(3));
    assert_eq!(
        Channel::Account.verify(
            Some(&ACCOUNT_KEY),
            pool_channel::OPERATION_PATH,
            Some(&authorization),
            &body,
            NOW_S
        ),
        Ok(())
    );
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        json!({"account_id": uid(3), "class": "stream",
               "operation": {"kind": "body", "ids": ARTICLE}})
    );
    let parsed: OperationRequest = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed.account_id, account);
}

#[tokio::test]
async fn ranking_is_tier_then_in_flight_then_id() {
    // (accounts as (n, tier, in_flight), expected winner)
    let cases = [
        (vec![(1, 2, 0), (2, 1, 9), (3, 3, 0)], 2), // lower tier beats load
        (vec![(1, 1, 5), (2, 1, 2), (3, 1, 7)], 2), // same tier: least in flight
        (vec![(3, 1, 2), (1, 1, 2), (2, 1, 2)], 1), // full tie: smallest id
        (vec![(1, 0, 4), (2, 1, 0)], 1),
    ];
    for (accounts, winner) in cases {
        let rows: Vec<_> = accounts.iter().map(|(n, t, _)| row(*n, *t)).collect();
        let hints: Vec<_> = accounts
            .iter()
            .map(|(n, t, f)| (*n, hint(*n, *t, *f, true)))
            .collect();
        let fake = Fake::new(&rows, &hints).reply(served(winner, Output::Body(vec![])));
        select(&fake, WorkClass::Stream, body_op()).await.unwrap();
        assert_eq!(executed_id(&fake), uid(winner), "{accounts:?}");
        assert_eq!(fake.candidates(), accounts.len());
    }
}

#[tokio::test]
async fn ineligible_hints_are_never_selected() {
    let rows = [row(1, 0), row(2, 5)];
    let fake = Fake::new(
        &rows,
        &[(1, hint(1, 0, 0, false)), (2, hint(2, 5, 9, true))],
    )
    .reply(served(2, Output::Body(vec![])));
    select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    assert_eq!(executed_id(&fake), uid(2));
}

// --------------------------------------------------------------- filter

#[tokio::test]
async fn enabled_read_and_ingest_filters_gate_candidate_reads() {
    let mut disabled = row(1, 0);
    disabled.enabled = false;
    let mut no_read = row(2, 0);
    no_read.can_read = false;
    no_read.stream_reserve = 0;
    no_read.ingest_limit = 0;
    let mut no_ingest = row(3, 0);
    no_ingest.ingest_limit = 0;
    let ok = row(4, 1);
    let rows = [disabled, no_read, no_ingest, ok];
    let all_hints: Vec<_> = (1..=4)
        .map(|n| (n, hint(n, u32::from(n == 4), 0, true)))
        .collect();

    // Stream: only the disabled and non-reading rows are skipped.
    let fake = Fake::new(&rows, &all_hints).reply(served(3, Output::Body(vec![])));
    select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    let mut asked: Vec<String> = fake
        .0
        .candidate_calls
        .borrow()
        .iter()
        .map(|c| c.0.clone())
        .collect();
    asked.sort();
    assert_eq!(asked, vec![uid(3), uid(4)]);

    // Ingest additionally requires a positive ingest limit.
    let fake = Fake::new(&rows, &all_hints).reply(served(4, Output::Body(vec![])));
    select(&fake, WorkClass::Ingest, body_op()).await.unwrap();
    let asked: Vec<String> = fake
        .0
        .candidate_calls
        .borrow()
        .iter()
        .map(|c| c.0.clone())
        .collect();
    assert_eq!(asked, vec![uid(4)]);
    let body = fake.0.candidate_calls.borrow()[0].2.clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["class"],
        "ingest"
    );
    assert_eq!(executed_id(&fake), uid(4));
}

#[tokio::test]
async fn no_candidate_rows_means_unavailable_without_execute() {
    let mut disabled = row(1, 0);
    disabled.enabled = false;
    for rows in [vec![], vec![disabled]] {
        let fake = Fake::new(&rows, &[]);
        assert_eq!(
            select(&fake, WorkClass::Stream, body_op())
                .await
                .unwrap_err(),
            OperationError::Unavailable
        );
        assert_eq!(fake.candidates(), 0);
        assert_eq!(fake.executes(), 0);
    }
}

#[tokio::test]
async fn single_account_pool_is_selected() {
    let fake = Fake::new(&[row(1, 7)], &[(1, hint(1, 7, 0, true))])
        .reply(served(1, Output::Body(b"ok".to_vec())));
    let got = select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    assert_eq!(got.provider_id, uid(1));
    assert_eq!(fake.candidates(), 1);
    assert_eq!(fake.executes(), 1);
}

// ------------------------------------------------- candidate failures

#[tokio::test]
async fn candidate_errors_and_malformed_hints_are_skipped() {
    let good = hint(9, 0, 5, true);
    let pad_over = {
        let mut raw = good.clone();
        raw.extend(std::iter::repeat_n(b' ', pool_channel::MAX_REQUEST_BYTES));
        raw
    };
    let spaced = [b" \n".as_slice(), &good].concat();
    let malformed: Vec<(&str, Vec<u8>)> = vec![
        (
            "array",
            serde_json::to_vec(&json!([uid(1), 0, 0, true])).unwrap(),
        ),
        ("empty", Vec::new()),
        ("not json", b"{".to_vec()),
        (
            "unknown field",
            serde_json::to_vec(&json!({"id": uid(1), "tier": 0, "in_flight": 0,
                "eligible": true, "x": 1}))
            .unwrap(),
        ),
        (
            "duplicate field",
            format!(
                r#"{{"id":"{}","tier":0,"tier":0,"in_flight":0,"eligible":true}}"#,
                uid(1)
            )
            .into_bytes(),
        ),
        (
            "other identity",
            serde_json::to_vec(&json!({"id": uid(9), "tier": 0, "in_flight": 0,
                "eligible": true}))
            .unwrap(),
        ),
        (
            "tier mismatch",
            serde_json::to_vec(&json!({"id": uid(1), "tier": 4, "in_flight": 0,
                "eligible": true}))
            .unwrap(),
        ),
        ("oversize", {
            let mut raw = hint(1, 0, 0, true);
            raw.extend(std::iter::repeat_n(b' ', pool_channel::MAX_REQUEST_BYTES));
            raw
        }),
    ];
    for (label, raw) in malformed {
        // Account 1 returns the bad hint; account 2 is healthy and must win.
        let fake = Fake::new(
            &[row(1, 0), row(2, 0)],
            &[(1, raw), (2, hint(2, 0, 9, true))],
        )
        .reply(served(2, Output::Body(vec![])));
        select(&fake, WorkClass::Stream, body_op()).await.unwrap();
        assert_eq!(executed_id(&fake), uid(2), "{label}");
    }

    // A transport error (no entry => Err) is skipped the same way.
    let fake = Fake::new(&[row(1, 0), row(2, 0)], &[(2, hint(2, 0, 9, true))])
        .reply(served(2, Output::Body(vec![])));
    select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    assert_eq!(executed_id(&fake), uid(2));

    // Whitespace-prefixed objects are fine; whitespace-padded oversize is not.
    let fake = Fake::new(&[row(9, 0)], &[(9, spaced)]).reply(served(9, Output::Body(vec![])));
    select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    let fake = Fake::new(&[row(9, 0)], &[(9, pad_over)]);
    assert_eq!(
        select(&fake, WorkClass::Stream, body_op())
            .await
            .unwrap_err(),
        OperationError::Unavailable
    );
    assert_eq!(fake.executes(), 0);
}

#[tokio::test]
async fn every_candidate_bad_means_unavailable_with_zero_execute() {
    let fake = Fake::new(&[row(1, 0), row(2, 0)], &[(1, b"[]".to_vec())]);
    assert_eq!(
        select(&fake, WorkClass::Stream, body_op())
            .await
            .unwrap_err(),
        OperationError::Unavailable
    );
    assert_eq!(fake.candidates(), 2);
    assert_eq!(fake.executes(), 0);
}

#[tokio::test]
async fn registry_failures_stop_before_any_candidate_read() {
    let invalid_rows = [
        serde_json::to_vec(&json!({"accounts": [], "x": 1})).unwrap(),
        b"[]".to_vec(),
        b"{".to_vec(),
        // duplicate account ids
        serde_json::to_vec(&json!({"accounts": [row(1, 0), row(1, 0)]})).unwrap(),
    ];
    for raw in invalid_rows {
        let fake = Fake::default();
        *fake.0.registry.borrow_mut() = Some(raw);
        assert_eq!(
            select(&fake, WorkClass::Stream, body_op())
                .await
                .unwrap_err(),
            OperationError::Unavailable
        );
        assert_eq!(fake.candidates() + fake.executes(), 0);
    }
    let fake = Fake::default(); // fetch error
    assert_eq!(
        select(&fake, WorkClass::Stream, body_op())
            .await
            .unwrap_err(),
        OperationError::Unavailable
    );
    assert_eq!(fake.candidates() + fake.executes(), 0);
}

// ----------------------------------------------- single execute call

#[tokio::test]
async fn every_failure_reply_makes_exactly_one_execute_call_and_no_retry() {
    type FailureCase = (&'static str, fn() -> OperationReply, OperationError);
    let replies: Vec<FailureCase> = vec![
        (
            "not found",
            || OperationReply::NotFound,
            OperationError::Unavailable,
        ),
        (
            "bad request",
            || OperationReply::BadRequest,
            OperationError::Unavailable,
        ),
        (
            "invalid",
            || OperationReply::Failed(OperationError::Invalid),
            OperationError::Invalid,
        ),
        (
            "unavailable",
            || OperationReply::Failed(OperationError::Unavailable),
            OperationError::Unavailable,
        ),
        (
            "deadline",
            || OperationReply::Failed(OperationError::Deadline),
            OperationError::Deadline,
        ),
        (
            "missing",
            || OperationReply::Failed(OperationError::Missing),
            OperationError::Missing,
        ),
        (
            "too large",
            || OperationReply::Failed(OperationError::TooLarge),
            OperationError::TooLarge,
        ),
        (
            "wrong serving id",
            || served(2, Output::Body(vec![])),
            OperationError::Unavailable,
        ),
        (
            "stat output for body",
            || {
                served(
                    1,
                    Output::Stat {
                        present: vec![],
                        missing: vec![],
                    },
                )
            },
            OperationError::Unavailable,
        ),
        (
            "oversize body",
            || served(1, Output::Body(vec![0; MAX_BODY_ARTICLE_BYTES + 1])),
            OperationError::Unavailable,
        ),
    ];
    for (label, make, expected) in replies {
        // Two healthy accounts: the second must never be tried after a failure.
        let fake = Fake::new(
            &[row(1, 0), row(2, 1)],
            &[(1, hint(1, 0, 0, true)), (2, hint(2, 1, 0, true))],
        )
        .reply(make());
        assert_eq!(
            select(&fake, WorkClass::Stream, body_op())
                .await
                .unwrap_err(),
            expected,
            "{label}"
        );
        assert_eq!(executed_id(&fake), uid(1), "{label}");
        assert_eq!(fake.candidates(), 2, "{label}: no re-hint");
        assert_eq!(fake.0.registry_auth.borrow().len(), 1, "{label}");
    }
}

// ----------------------------------------------- output validation

#[tokio::test]
async fn body_output_at_the_cap_is_accepted() {
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))])
        .reply(served(1, Output::Body(vec![0; MAX_BODY_ARTICLE_BYTES])));
    let got = select(&fake, WorkClass::Stream, body_op()).await.unwrap();
    assert!(matches!(got.output, Output::Body(b) if b.len() == MAX_BODY_ARTICLE_BYTES));
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

async fn stat_outcome(
    request: &[&str],
    present: &[&str],
    missing: &[&str],
) -> Result<Served, OperationError> {
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))]).reply(served(
        1,
        Output::Stat {
            present: ids(present),
            missing: ids(missing),
        },
    ));
    let result = select(&fake, WorkClass::Stream, Operation::Stat(ids(request))).await;
    assert_eq!(fake.executes(), 1);
    result
}

#[tokio::test]
async fn stat_partition_must_match_the_request_multiset() {
    let ok: &[(&[&str], &[&str], &[&str])] = &[
        (&["a@b", "c@d"], &["a@b", "c@d"], &[]),
        (&["a@b", "c@d"], &[], &["c@d", "a@b"]),
        (&["a@b", "c@d", "e@f"], &["e@f"], &["a@b", "c@d"]),
        // duplicates preserved
        (&["a@b", "a@b"], &["a@b", "a@b"], &[]),
        (&["a@b", "a@b"], &[], &["a@b", "a@b"]),
        // Each occurrence is probed separately; an article may expire between STATs.
        (&["a@b", "a@b"], &["a@b"], &["a@b"]),
    ];
    for (req, p, m) in ok {
        assert!(stat_outcome(req, p, m).await.is_ok(), "{req:?} {p:?} {m:?}");
    }
    let bad: &[(&[&str], &[&str], &[&str])] = &[
        (&["a@b", "c@d"], &["a@b"], &[]),        // missing id
        (&["a@b"], &["a@b"], &["a@b"]),          // extra (overlap)
        (&["a@b"], &["x@y"], &[]),               // unknown id
        (&["a@b", "c@d"], &["a@b"], &["x@y"]),   // right count, wrong id
        (&["a@b", "a@b"], &["a@b"], &[]),        // duplicate dropped
        (&["a@b", "c@d"], &["a@b", "a@b"], &[]), // duplicate substituted
        (&["a@b", "c@d"], &["a@b"], &["a@b"]),   // overlap replaces c@d
    ];
    for (req, p, m) in bad {
        assert_eq!(
            stat_outcome(req, p, m).await.unwrap_err(),
            OperationError::Unavailable,
            "{req:?} {p:?} {m:?}"
        );
    }
}

#[tokio::test]
async fn body_output_for_stat_operation_is_refused() {
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))])
        .reply(served(1, Output::Body(vec![1])));
    assert_eq!(
        select(&fake, WorkClass::Stream, Operation::Stat(ids(&["a@b"])))
            .await
            .unwrap_err(),
        OperationError::Unavailable
    );
    assert_eq!(fake.executes(), 1);
}

// ----------------------------------------- missing keys / invalid input

#[tokio::test]
async fn missing_keys_and_invalid_input_perform_no_reads() {
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))]);
    let short = [1u8; 31];
    let key_cases: Vec<ChannelKeys<'_>> = vec![
        ChannelKeys {
            account: None,
            resolver: Some(&RESOLVER_KEY),
        },
        ChannelKeys {
            account: Some(&short),
            resolver: Some(&RESOLVER_KEY),
        },
    ];
    for k in key_cases {
        let r = run(
            &fake,
            &fake,
            k,
            WorkClass::Stream,
            body_op(),
            || NOW_MS,
            std::future::pending::<()>(),
        )
        .await;
        assert_eq!(r.unwrap_err(), OperationError::Unavailable);
        assert_eq!(fake.reads(), 0);
    }
    // Missing/short resolver key: registry cannot even be signed.
    for resolver in [None, Some(&short[..])] {
        let r = run(
            &fake,
            &fake,
            ChannelKeys {
                account: Some(&ACCOUNT_KEY),
                resolver,
            },
            WorkClass::Stream,
            body_op(),
            || NOW_MS,
            std::future::pending::<()>(),
        )
        .await;
        assert_eq!(r.unwrap_err(), OperationError::Unavailable);
        assert_eq!(fake.reads(), 0);
    }
    for op in [
        Operation::Body("bad".into()),
        Operation::Body("<a@b>".into()),
        Operation::Stat(vec![]),
        Operation::Stat(vec!["a@b".into(); 501]),
        Operation::Stat(vec!["nope".into()]),
    ] {
        assert_eq!(
            select(&fake, WorkClass::Stream, op).await.unwrap_err(),
            OperationError::Invalid
        );
        assert_eq!(fake.reads(), 0);
    }
}

#[tokio::test]
async fn large_stat_operation_signs_on_the_operation_path() {
    let request: Vec<String> = (0..500)
        .map(|i| format!("segment-{i:04}-padding-padding@example.invalid"))
        .collect();
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))]).reply(served(
        1,
        Output::Stat {
            present: request.clone(),
            missing: vec![],
        },
    ));
    select(&fake, WorkClass::Ingest, Operation::Stat(request))
        .await
        .unwrap();
    let (_, authorization, body) = fake.0.execute_calls.borrow()[0].clone();
    assert!(body.len() > pool_channel::MAX_REQUEST_BYTES);
    assert_eq!(
        Channel::Account.verify(
            Some(&ACCOUNT_KEY),
            pool_channel::OPERATION_PATH,
            Some(&authorization),
            &body,
            NOW_S
        ),
        Ok(())
    );
}

// ------------------------------------------- deadline / cancellation

#[tokio::test]
async fn already_expired_deadline_does_no_reads() {
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))]);
    let r = run(
        &fake,
        &fake,
        keys(),
        WorkClass::Stream,
        body_op(),
        || NOW_MS,
        std::future::ready(()),
    )
    .await;
    assert_eq!(r.unwrap_err(), OperationError::Deadline);
    assert_eq!(fake.reads(), 0);
}

#[tokio::test]
async fn deadline_during_hint_reads_ends_without_execute() {
    let fake = Fake::new(&[row(1, 0)], &[(1, hint(1, 0, 0, true))]);
    fake.0.candidates_hang.set(true);
    let r = run(
        &fake,
        &fake,
        keys(),
        WorkClass::Stream,
        body_op(),
        || NOW_MS,
        tokio::time::sleep(Duration::from_millis(20)),
    )
    .await;
    assert_eq!(r.unwrap_err(), OperationError::Deadline);
    assert_eq!(fake.candidates(), 1);
    assert_eq!(fake.executes(), 0);
}

#[tokio::test]
async fn deadline_during_execute_is_one_call_and_no_failover() {
    let fake = Fake::new(
        &[row(1, 0), row(2, 1)],
        &[(1, hint(1, 0, 0, true)), (2, hint(2, 1, 0, true))],
    );
    fake.0.execute_hangs.set(true);
    let r = run(
        &fake,
        &fake,
        keys(),
        WorkClass::Stream,
        body_op(),
        || NOW_MS,
        tokio::time::sleep(Duration::from_millis(20)),
    )
    .await;
    assert_eq!(r.unwrap_err(), OperationError::Deadline);
    assert_eq!(executed_id(&fake), uid(1));
}

#[test]
fn cancelling_mid_execute_leaves_a_single_call_and_starts_nothing_more() {
    let fake = Fake::new(
        &[row(1, 0), row(2, 1)],
        &[(1, hint(1, 0, 0, true)), (2, hint(2, 1, 0, true))],
    );
    fake.0.execute_hangs.set(true);
    {
        let mut fut = pin!(select(&fake, WorkClass::Stream, body_op()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        assert_eq!(fake.executes(), 1);
        // Dropped here: cancellation.
    }
    assert_eq!(fake.executes(), 1);
    assert_eq!(fake.candidates(), 2);
    assert_eq!(fake.0.registry_auth.borrow().len(), 1);
}

#[test]
fn hint_reads_run_at_most_eight_at_a_time() {
    assert_eq!(HINT_CONCURRENCY, 8);
    let rows: Vec<_> = (1..=12).map(|n| row(n, 0)).collect();
    let hints: Vec<_> = (1..=12).map(|n| (n, hint(n, 0, n, true))).collect();
    let fake = Fake::new(&rows, &hints).reply(served(1, Output::Body(vec![])));
    fake.0.gated.set(true);

    let mut fut = pin!(select(&fake, WorkClass::Stream, body_op()));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    assert_eq!(fake.0.active.get(), 8);
    assert_eq!(fake.candidates(), 8);
    assert_eq!(fake.executes(), 0);

    fake.0.gate.set(true);
    for waker in fake.0.wakers.borrow_mut().drain(..) {
        waker.wake();
    }
    let mut spins = 0;
    let result = loop {
        spins += 1;
        assert!(spins < 1000, "selection did not make progress");
        if let Poll::Ready(r) = fut.as_mut().poll(&mut cx) {
            break r;
        }
    };
    assert_eq!(result.unwrap().provider_id, uid(1));
    assert_eq!(fake.candidates(), 12);
    assert_eq!(fake.0.peak.get(), 8, "never more than eight in flight");
    assert_eq!(executed_id(&fake), uid(1));
}
