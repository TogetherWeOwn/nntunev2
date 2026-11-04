//! Binding-only forwarding tests. Fake account replies, no Worker or real dial.

use std::cell::{Cell, RefCell};
use std::future::pending;

use nntune_core::pool_channel::{self, Channel};
use nntune_core::pool_operation::{
    Operation, OperationError, OperationReply, Output, Served, INGEST_BODY_PATH, INGEST_STAT_PATH,
};
use nntune_stream::account_dispatch::OperationRequest;
use nntune_stream::account_ingress::{operation, run};
use nntune_stream::account_pool::WorkClass;
use nntune_stream::account_route::{ChannelKeys, RegistryError, RegistrySource};
use nntune_stream::account_selection::AccountSource;
use serde_json::{json, Value};

const ID: &str = "11111111-1111-4111-8111-111111111111";
const KEY: &[u8; 32] = &[7; 32];
const NOW: u64 = 1_700_000_000_000;
const ARTICLE: &str = "one@example.invalid";

fn body() -> Value {
    json!({"job_id":"job_one", "user_id":"user_one", "idempotency_key":"idem_one", "segment_id":ARTICLE})
}

fn stat() -> Value {
    json!({"job_id":"job_one", "user_id":"user_one", "idempotency_key":"idem_one", "batch_index":8, "segment_ids":[ARTICLE, ARTICLE], "segment_bytes":[null,12]})
}

#[derive(Default)]
struct Fake {
    reads: Cell<usize>,
    requests: RefCell<Vec<OperationRequest>>,
    fail: Cell<bool>,
}

impl RegistrySource for Fake {
    async fn fetch(&self, auth: &str) -> Result<Vec<u8>, RegistryError> {
        pool_channel::verify(
            Some(KEY),
            pool_channel::REGISTRY_PATH,
            Some(auth),
            b"{}",
            NOW / 1000,
        )
        .unwrap();
        self.reads.set(self.reads.get() + 1);
        Ok(serde_json::to_vec(&json!({"accounts":[{
            "id":ID,"revision":1,"secret_ref":ID,"can_read":true,"max_connections":2,
            "stream_reserve":1,"ingest_limit":1,"tier":0,"enabled":true
        }]}))
        .unwrap())
    }
}

impl AccountSource for Fake {
    async fn candidate(&self, id: &str, auth: &str, body: &[u8]) -> Result<Vec<u8>, RegistryError> {
        assert_eq!(id, ID);
        Channel::Account
            .verify(
                Some(KEY),
                pool_channel::CANDIDATE_PATH,
                Some(auth),
                body,
                NOW / 1000,
            )
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(body).unwrap()["class"],
            "ingest"
        );
        self.reads.set(self.reads.get() + 1);
        Ok(serde_json::to_vec(&json!({"id":ID,"tier":0,"in_flight":0,"eligible":true})).unwrap())
    }

    async fn execute(&self, id: &str, auth: &str, body: &[u8]) -> OperationReply {
        assert_eq!(id, ID);
        Channel::Account
            .verify(
                Some(KEY),
                pool_channel::OPERATION_PATH,
                Some(auth),
                body,
                NOW / 1000,
            )
            .unwrap();
        let req: OperationRequest = serde_json::from_slice(body).unwrap();
        assert_eq!(req.class, WorkClass::Ingest);
        let output = match &req.operation {
            Operation::Body(id) => {
                assert_eq!(id, ARTICLE);
                Output::Body(vec![1, 2, 3])
            }
            Operation::Stat(ids) => Output::Stat {
                present: vec![ids[0].clone()],
                missing: vec![ids[1].clone()],
            },
        };
        self.requests.borrow_mut().push(req);
        if self.fail.get() {
            OperationReply::Failed(OperationError::Unavailable)
        } else {
            OperationReply::Served(Served {
                provider_id: ID.into(),
                output,
            })
        }
    }
}

async fn forward(fake: &Fake, path: &str, raw: &[u8], user: &str) -> OperationReply {
    run(
        fake,
        fake,
        ChannelKeys {
            account: Some(KEY),
            resolver: Some(KEY),
        },
        path,
        raw,
        user,
        || NOW,
        pending(),
    )
    .await
}

#[tokio::test]
async fn body_is_forwarded_once_with_fixed_ingest_class_and_actual_identity() {
    let fake = Fake::default();
    let reply = forward(
        &fake,
        INGEST_BODY_PATH,
        &serde_json::to_vec(&body()).unwrap(),
        "user_one",
    )
    .await;
    assert_eq!(
        reply,
        OperationReply::Served(Served {
            provider_id: ID.into(),
            output: Output::Body(vec![1, 2, 3])
        })
    );
    assert_eq!(fake.requests.borrow().len(), 1);
    let raw = serde_json::to_value(&fake.requests.borrow()[0]).unwrap();
    assert!(raw.get("user_id").is_none());
    assert!(raw.get("job_id").is_none());
}

#[tokio::test]
async fn stat_preserves_duplicate_occurrences_and_does_not_forward_ranges_or_batch_index() {
    let fake = Fake::default();
    let reply = forward(
        &fake,
        INGEST_STAT_PATH,
        &serde_json::to_vec(&stat()).unwrap(),
        "user_one",
    )
    .await;
    assert!(matches!(
        reply,
        OperationReply::Served(Served {
            output: Output::Stat { .. },
            ..
        })
    ));
    let raw = serde_json::to_value(&fake.requests.borrow()[0]).unwrap();
    assert!(raw.get("batch_index").is_none());
    assert!(raw.get("segment_bytes").is_none());
}

#[tokio::test]
async fn wrong_owner_class_account_unknown_and_duplicate_fields_refuse_before_reads() {
    let mut cases = Vec::new();
    for (path, envelope) in [(INGEST_BODY_PATH, body()), (INGEST_STAT_PATH, stat())] {
        for field in ["class", "account_id", "provider_id", "unexpected"] {
            let mut value = envelope.clone();
            value[field] = json!(ID);
            cases.push((path, serde_json::to_vec(&value).unwrap()));
        }
        let raw = serde_json::to_string(&envelope).unwrap();
        cases.push((
            path,
            raw.replacen('{', "{\"user_id\":\"user_one\",", 1)
                .into_bytes(),
        ));
    }
    for (path, raw) in cases {
        let fake = Fake::default();
        assert_eq!(
            forward(&fake, path, &raw, "user_one").await,
            OperationReply::Failed(OperationError::Invalid)
        );
        assert_eq!(fake.reads.get(), 0);
        assert!(fake.requests.borrow().is_empty());
    }
    let fake = Fake::default();
    assert_eq!(
        forward(
            &fake,
            INGEST_BODY_PATH,
            &serde_json::to_vec(&body()).unwrap(),
            "another_user"
        )
        .await,
        OperationReply::Failed(OperationError::Invalid)
    );
    assert_eq!(fake.reads.get(), 0);
}

#[test]
fn malformed_ids_arrays_paths_and_oversize_are_invalid() {
    for raw in [b"[]".to_vec(), b"null".to_vec(), vec![b' '; 300_000]] {
        assert!(operation(INGEST_BODY_PATH, &raw, "user_one").is_err());
    }
    let mut request = body();
    request["segment_id"] = json!("one\r\nQUIT@example.invalid");
    assert!(operation(
        INGEST_BODY_PATH,
        &serde_json::to_vec(&request).unwrap(),
        "user_one"
    )
    .is_err());
    assert!(operation("/body", &serde_json::to_vec(&body()).unwrap(), "user_one").is_err());
    let mut request = stat();
    request["segment_ids"] = json!([]);
    assert!(operation(
        INGEST_STAT_PATH,
        &serde_json::to_vec(&request).unwrap(),
        "user_one"
    )
    .is_err());
}

#[tokio::test]
async fn operation_refusal_has_no_second_attempt_or_attribution() {
    let fake = Fake::default();
    fake.fail.set(true);
    let reply = forward(
        &fake,
        INGEST_BODY_PATH,
        &serde_json::to_vec(&body()).unwrap(),
        "user_one",
    )
    .await;
    assert_eq!(reply, OperationReply::Failed(OperationError::Unavailable));
    assert_eq!(fake.requests.borrow().len(), 1);
    assert!(reply.into_response().provider_id.is_none());
}

#[test]
fn worker_activation_remains_hard_disabled_before_ingress_io() {
    let runtime = include_str!("../src/account_runtime.rs");
    assert!(runtime.contains("const OPERATIONS_QUALIFIED: bool = false;"));
    let ingress = runtime
        .split("pub(crate) async fn ingest_fetch")
        .nth(1)
        .unwrap();
    assert!(
        ingress.find("!OPERATIONS_QUALIFIED").unwrap()
            < ingress.find("read_request(req, cap)").unwrap()
    );
    assert!(ingress.find("!OPERATIONS_QUALIFIED").unwrap() < ingress.find(".secret(").unwrap());
    assert!(ingress.contains("req.headers().has(\"origin\")"));
    assert!(ingress.contains("req.headers().has(\"cookie\")"));
    assert!(ingress.contains("req.url()?.query().is_some()"));
}
