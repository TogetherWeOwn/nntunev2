use super::{now, Duration, S3CompletionSender};
use crate::multipart::{
    resume_checkpoint::{CheckpointPart, ResumeBinding, ResumeCheckpoint},
    session_completion::SessionCompletion,
    Plan, UploadTarget,
};
use nntune_core::{
    media_envelope::EnvelopeContext, origin_sealer::OriginSealer, s3_locator::S3Binding,
};
use sha2::{Digest, Sha256};
use worker::wasm_bindgen::{self, prelude::*};

fn id(value: u8) -> [u8; 16] {
    let mut bytes = [value; 16];
    bytes[6] = 0x40;
    bytes[8] = 0x80;
    bytes
}

#[wasm_bindgen]
pub async fn s3_completion_test(mode: String) -> Result<String, JsValue> {
    let configuration = id(7);
    let bucket = "fixture-bucket";
    let endpoint = match mode.as_str() {
        "http" => "http://fixture.test:8443",
        "path" => "https://fixture.test:8443/api",
        _ => "https://fixture.test:8443",
    };
    let sender = S3CompletionSender::new(
        if matches!(mode.as_str(), "foreign" | "part-foreign" | "init-foreign") {
            id(8)
        } else {
            configuration
        },
        if mode == "bucket" {
            "other-bucket"
        } else {
            bucket
        }
        .into(),
        endpoint,
        "fixture-region".into(),
        "FIXTUREACCESS".into(),
        "fixture-secret".into(),
    )
    .map_err(|_| JsValue::from_str("invalid configuration"))?;
    let mut sealer = OriginSealer::new(1).unwrap();
    let mut context = EnvelopeContext {
        identities: [0, 1, 2, 0xab, 0xcd, 7].map(id),
        plaintext_len: 1,
        header: sealer.header().to_bytes(),
    };
    let [environment, library, object, generation] =
        [1, 2, 0xab, 0xcd].map(|value| hex::encode(id(value)));
    let key = format!("media/{object}/{generation}");
    let budget = match mode.as_str() {
        "deadline" => 200,
        "expired" => 0,
        _ => 2000,
    };
    let until = now() + Duration::from_millis(budget);
    let initiated = if mode.starts_with("init") {
        let requested_key = if mode == "init-other" {
            "media/efefefefefef40ef80efefefefefefef/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd"
        } else {
            &key
        };
        let upload = sender
            .initiate(&context, bucket, requested_key, until)
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        assert_eq!(upload.configuration(), configuration);
        if mode != "init-upload-completion-head" {
            return Ok(upload.target().upload_id.into());
        }
        Some(upload)
    } else {
        None
    };
    let target = initiated.as_ref().map_or(
        UploadTarget {
            bucket,
            key: &key,
            upload_id: "fixture+/=?",
        },
        |upload| upload.target(),
    );
    let configuration = initiated
        .as_ref()
        .map_or(configuration, |upload| upload.configuration());
    let plan = Plan::new(target, sealer.header(), 1).unwrap();
    let mut partition = plan.partition();
    assert!(partition
        .push(&sealer.seal_chunk(&[42]).unwrap())
        .unwrap()
        .is_none());
    let part = partition.finish().unwrap().unwrap();
    let (_, number, bytes, checksum) = part.request();
    let binding = ResumeBinding {
        plan: if mode == "part-other" {
            Plan::new(
                UploadTarget {
                    upload_id: "other",
                    ..target
                },
                sealer.header(),
                1,
            )
            .unwrap()
        } else {
            plan
        },
        configuration: if mode == "part-context" {
            id(8)
        } else {
            configuration
        },
        environment: &environment,
        library: &library,
        session: "fixture-session",
        object: &object,
        generation: &generation,
        envelope_scheme: 2,
    };
    let receipt = if mode.starts_with("part") || initiated.is_some() {
        let uploaded = sender
            .put_part(binding, &context, &part, until)
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        if mode.starts_with("part") {
            return Ok(uploaded.etag);
        }
        uploaded
    } else {
        part.verify_reply("\"fixture-etag\"".into(), checksum)
            .unwrap()
    };
    let etag = receipt.etag.clone();
    let parts = [CheckpointPart {
        number,
        len: bytes.len(),
        etag: &etag,
        digest: Sha256::digest(bytes).into(),
    }];
    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &parts,
    };
    let completion =
        SessionCompletion::prepare(binding, &context, vec![receipt], &checkpoint).unwrap();
    let completed = sender
        .complete(completion, until)
        .await
        .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
    if mode == "head-context" {
        context.identities[1] = id(8);
    }
    if mode.starts_with("head") || initiated.is_some() {
        sender
            .confirm_head(&completed, &context, until)
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
    }
    let S3Binding::VersionId(version) = completed.metadata().locator().binding() else {
        return Err(JsValue::from_str("missing fixture version"));
    };
    Ok(version.clone())
}
