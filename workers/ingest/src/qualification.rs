//! Actions-only raw bridges, absent from ordinary generated artifacts. These
//! have no public router entry and accept no caller-selected producer identity.
//! The synthetic tagged output is not FLAC parser/tagger acceptance.

use crate::flac::{FlacWalk, StreamInfo, TaggedFlac};
use crate::posted_generations::dispatch::{
    preparation::{prepare_and_send, recover_delivery, PreparationInput},
    DispatchPointer, DispatchSend,
};

fn tagged() -> TaggedFlac {
    // Crosses the real R2 range-reader boundary; the untrusted sidecar must not
    // determine the persisted digest. No media/article/key material is used.
    TaggedFlac {
        bytes: (0..65_537).map(|i| (i % 251) as u8).collect(),
        walk: FlacWalk {
            streaminfo: StreamInfo {
                min_block_size: 16,
                max_block_size: 16,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 0,
                md5: [0; 16],
            },
            frame_count: 0,
            total_samples: 0,
            seek_index: Vec::new(),
            has_seektable: false,
            first_frame_offset: 0,
            frame_spans: Vec::new(),
        },
        sha256_hex: "untrusted-sidecar".into(),
        art: Vec::new(),
        skipped_art: Vec::new(),
    }
}

async fn prepare(req: worker::Request, env: worker::Env) -> worker::Result<worker::Response> {
    if req.method() != worker::Method::Post {
        return worker::Response::error("qualification unavailable", 503);
    }
    let result = prepare_and_send(
        &env,
        PreparationInput {
            job: "qualification-job",
            operation: "qualification-file",
            predecessor: 0,
            file_index: 0,
            blocks_per_article: 11,
            tagged: tagged(),
        },
    )
    .await;
    match result {
        Ok(DispatchSend::Acknowledged) => worker::Response::empty().map(|r| r.with_status(202)),
        _ => worker::Response::error("qualification unavailable", 503),
    }
}

async fn recover(mut req: worker::Request, env: worker::Env) -> worker::Result<worker::Response> {
    // The harness supplies an opaque persisted pointer, never acquisition data.
    // Stream before parsing to bound even malformed harness requests.
    use futures_util::StreamExt;
    if req.method() != worker::Method::Post {
        return worker::Response::error("qualification unavailable", 503);
    }
    let mut stream = req.stream()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > 256usize.saturating_sub(bytes.len()) {
            return worker::Response::error("qualification unavailable", 503);
        }
        bytes.extend_from_slice(&chunk);
    }
    let pointer: DispatchPointer = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return worker::Response::error("qualification unavailable", 503),
    };
    match recover_delivery(&env, &pointer).await {
        Ok(()) => worker::Response::empty().map(|r| r.with_status(204)),
        Err(_) => worker::Response::error("qualification unavailable", 503),
    }
}

#[worker::wasm_bindgen::prelude::wasm_bindgen(wasm_bindgen = worker::wasm_bindgen, wasm_bindgen_futures = worker::wasm_bindgen_futures, js_sys = worker::js_sys)]
pub async fn qualification_prepare(
    req: worker::web_sys::Request,
    env: worker::Env,
) -> Result<worker::web_sys::Response, worker::wasm_bindgen::JsValue> {
    prepare(req.into(), env)
        .await
        .map(Into::into)
        .map_err(|_| worker::wasm_bindgen::JsValue::from_str("qualification unavailable"))
}

#[worker::wasm_bindgen::prelude::wasm_bindgen(wasm_bindgen = worker::wasm_bindgen, wasm_bindgen_futures = worker::wasm_bindgen_futures, js_sys = worker::js_sys)]
pub async fn qualification_recover(
    req: worker::web_sys::Request,
    env: worker::Env,
) -> Result<worker::web_sys::Response, worker::wasm_bindgen::JsValue> {
    recover(req.into(), env)
        .await
        .map(Into::into)
        .map_err(|_| worker::wasm_bindgen::JsValue::from_str("qualification unavailable"))
}
