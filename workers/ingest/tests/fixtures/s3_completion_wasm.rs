use super::{now, CompletionRequest, Duration, S3CompletionSender};
use crate::multipart::UploadTarget;
use worker::wasm_bindgen::{self, prelude::*};

#[wasm_bindgen]
pub async fn s3_completion_test(mode: String) -> Result<String, JsValue> {
    let configuration = [7; 16];
    let sender = S3CompletionSender::new(
        configuration,
        "fixture-bucket".into(),
        "https://fixture.test:8443",
        "fixture-region".into(),
        "FIXTUREACCESS".into(),
        "fixture-secret".into(),
    )
    .map_err(|_| JsValue::from_str("invalid configuration"))?;
    let request = CompletionRequest {
        configuration: if mode == "foreign" {
            [8; 16]
        } else {
            configuration
        },
        target: UploadTarget {
            bucket: "fixture-bucket",
            key: "media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd",
            upload_id: "fixture+/=?",
        },
        body: b"<CompleteMultipartUpload/>".to_vec(),
    };
    let budget = match mode.as_str() {
        "deadline" => 200,
        "expired" => 0,
        _ => 2000,
    };
    let reply = sender
        .send(request, now() + Duration::from_millis(budget))
        .await
        .map_err(|_| JsValue::from_str("completion send unconfirmed"))?;
    assert_eq!(reply.configuration, configuration);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body.as_slice(), b"fixture receipt");
    reply
        .version
        .ok_or(JsValue::from_str("missing fixture version"))
}
