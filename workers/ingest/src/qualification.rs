//! Actions-only raw bridges, absent from ordinary generated artifacts. These
//! have no public router entry and accept no caller-selected producer identity.
//! Synthetic structural FLAC input can also exercise the real tag rewrite;
//! neither fixture establishes authenticated upload or PCM decoder acceptance.

use crate::flac::{rewrite_tags, FlacWalk, StreamInfo, TaggedFlac, TrackTags};
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

async fn prepare(mut req: worker::Request, env: worker::Env) -> worker::Result<worker::Response> {
    if req.method() != worker::Method::Post {
        return worker::Response::error("qualification unavailable", 503);
    }
    let flac = req.path() == "/prepare-flac";
    let commit = req.path() == "/prepare-commit";
    let tagged = if flac {
        use futures_util::StreamExt;
        let mut stream = req.stream()?;
        let mut input = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if chunk.len() > (128 * 1024usize).saturating_sub(input.len()) {
                return worker::Response::error("qualification unavailable", 503);
            }
            input.extend_from_slice(&chunk);
        }
        let tags = TrackTags {
            title: "Qualification title",
            artist: "Qualification artist",
            album: "Qualification album",
            album_artist: None,
            date: None,
            track_number: None,
            track_total: None,
            disc_number: None,
            disc_total: None,
            recording_mbid: "00000000-0000-4000-8000-000000000001",
            release_mbid: "00000000-0000-4000-8000-000000000002",
            artist_mbids: &[],
            isrc: None,
        };
        match rewrite_tags(&input, &tags) {
            Ok(tagged) => tagged,
            Err(_) => return worker::Response::error("qualification unavailable", 503),
        }
    } else {
        tagged()
    };
    let result = prepare_and_send(
        &env,
        PreparationInput {
            job: if commit {
                "qualification-commit-job"
            } else if flac {
                "qualification-flac-job"
            } else {
                "qualification-job"
            },
            operation: if commit {
                "qualification-commit-file"
            } else if flac {
                "qualification-flac-file"
            } else {
                "qualification-file"
            },
            predecessor: 0,
            file_index: 0,
            blocks_per_article: 11,
            tagged,
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
