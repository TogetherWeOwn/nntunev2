use super::{now, Duration, FixtureVersionRemover, S3CompletionSender};
use crate::multipart::{
    readback_inventory::CompletedReadbackInventory,
    resume_checkpoint::{matches_checkpoint, CheckpointPart, ResumeBinding, ResumeCheckpoint},
    session_completion::{DispatchError, SessionCompletion},
    session_upload::DurableUpload,
    sidecar::SidecarPut,
    Plan, UploadPart, UploadTarget, UploadedPart, PART_BYTES,
};
use nntune_core::{
    digest_enrollment::DigestEnrollment,
    digest_readback::EnrolledEnvelope,
    media::PLAIN_CHUNK_SIZE,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    origin_sealer::SealedChunk,
    s3_locator::S3Binding,
};
use sha2::{Digest, Sha256};
use worker::wasm_bindgen::{self, prelude::*};

#[path = "catalogue_upload_wasm.rs"]
mod catalogue;

// Mono, 8-bit, 19 constant samples at 44.1 kHz.
const FLAC: &[u8] = b"fLaC\x80\0\0\x22\0\x10\0\x10\0\0\0\0\0\0\x0a\xc4\x40\x70\0\0\0\x13\
    \x46\xd2\xbe\x27\x5e\xd2\xdd\x86\xab\x2b\x53\x88\xf7\x34\x6d\x15\
    \xff\xf8\x69\x02\0\x0f\xb7\0\x2a\x7a\xdd\xff\xf8\x69\x02\x01\x02\x81\0\x2a\x9d\x1d";

fn id(value: u8) -> [u8; 16] {
    let mut bytes = [value; 16];
    bytes[6] = 0x40;
    bytes[8] = 0x80;
    bytes
}

#[wasm_bindgen]
pub async fn s3_completion_test(mode: String) -> Result<String, JsValue> {
    let endpoint = match mode.as_str() {
        "http" => "http://fixture.test:8443",
        "path" => "https://fixture.test:8443/api",
        _ => "https://fixture.test:8443",
    };
    run(&mode, endpoint).await
}

#[wasm_bindgen]
pub async fn s3_fixture_retirement_test(
    bucket: String,
    key: String,
    version: String,
) -> Result<(), JsValue> {
    let remover = FixtureVersionRemover::new(
        id(7),
        "https://fixture.test:8443",
        "fixture-region".into(),
        "FIXTUREACCESS".into(),
        "fixture-secret".into(),
    )
    .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
    remover
        .delete_and_confirm(&bucket, &key, &version, now() + Duration::from_secs(5))
        .await
        .map_err(|e| JsValue::from_str(&format!("{e:?}")))
}

#[wasm_bindgen]
pub async fn upload_for_playback(endpoint: String, publish: bool) -> Result<String, JsValue> {
    let mode = if publish {
        "readback-publish"
    } else {
        "readback-playback"
    };
    run(mode, &endpoint).await
}

async fn run(mode: &str, endpoint: &str) -> Result<String, JsValue> {
    let configuration = id(7);
    let bucket = "fixture-bucket";
    let sender = S3CompletionSender::new(
        if matches!(
            mode,
            "foreign" | "part-foreign" | "init-foreign" | "sidecar-foreign"
        ) {
            id(8)
        } else {
            configuration
        },
        if matches!(mode, "bucket" | "sidecar-bucket") {
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
    let identities = [0, 1, 2, 0xab, 0xcd, 7].map(id);
    if mode.starts_with("sidecar") {
        let map_id = id(0xef);
        let mut enrollment =
            DigestEnrollment::new(1, identities[1..].try_into().unwrap(), map_id).unwrap();
        let context = EnvelopeContext {
            identities,
            plaintext_len: 1,
            header: enrollment.header().to_bytes(),
        };
        let page = enrollment.seal_chunk(&[42]).unwrap().page.unwrap();
        let expected_page = hex::encode(&page);
        let manifest = enrollment.finish().unwrap();
        let put = SidecarPut::new(bucket, &context, &manifest, map_id, 0, &page).unwrap();
        let budget = if mode == "sidecar-expired" { 0 } else { 2000 };
        let completed = sender
            .put_sidecar(put, now() + Duration::from_millis(budget))
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        return Ok(serde_json::json!({
            "version": completed.pin().1,
            "expectedPage": expected_page,
        })
        .to_string());
    }
    let readback = mode.starts_with("readback");
    let multipart_resume = matches!(mode, "multipart-resume" | "multipart-resume-full");
    let padded;
    let plaintext: &[u8] = if matches!(mode, "readback-publish" | "readback-playback") {
        // Add one FLAC PADDING block so the suffix crosses into a second
        // encrypted chunk while preserving the existing valid audio frames.
        let padding = PLAIN_CHUNK_SIZE as usize + 1 - FLAC.len() - 4;
        let mut bytes = FLAC[..42].to_vec();
        bytes[4] = 0;
        bytes.push(0x81);
        bytes.extend_from_slice(&(padding as u32).to_be_bytes()[1..]);
        bytes.resize(bytes.len() + padding, 0);
        bytes.extend_from_slice(&FLAC[42..]);
        padded = bytes;
        &padded
    } else if multipart_resume {
        padded = vec![42; PART_BYTES];
        &padded
    } else if readback {
        FLAC
    } else {
        &[42]
    };
    let plaintext_len = plaintext.len() as u64;
    let map_id = id(0xef);
    let mut enrollment =
        DigestEnrollment::new(plaintext_len, identities[1..].try_into().unwrap(), map_id).unwrap();
    let header = enrollment.header();
    let mut context = EnvelopeContext {
        identities,
        plaintext_len,
        header: header.to_bytes(),
    };
    let root = [17; 32];
    let record =
        readback.then(|| wrap_scheme2(&root, &context, enrollment.key().expose_secret()).unwrap());
    let chunks: Vec<_> = plaintext
        .chunks(PLAIN_CHUNK_SIZE as usize)
        .map(|chunk| enrollment.seal_chunk(chunk).unwrap())
        .collect();
    let manifest = enrollment.finish().unwrap();
    let [environment, library, object, generation] =
        [1, 2, 0xab, 0xcd].map(|value| hex::encode(id(value)));
    let key = format!("media/{object}/{generation}");
    let budget = match mode {
        "readback-publish" | "readback-playback" | "multipart-resume" | "multipart-resume-full" => {
            5000
        }
        "deadline" => 200,
        "expired" | "reconcile-expired" => 0,
        _ => 2000,
    };
    let until = now() + Duration::from_millis(budget);
    if mode.starts_with("reconcile") {
        let id = if mode == "reconcile-replay" {
            "new+/=?".into()
        } else {
            sender
                .find_incomplete(&key, until)
                .await
                .map_err(|()| JsValue::from_str("unconfirmed listing"))?
        };
        sender
            .abort_incomplete(&key, &id, until)
            .await
            .map_err(|()| JsValue::from_str("unconfirmed abort"))?;
        return Ok(id);
    }
    let initiated = if mode.starts_with("init") || readback {
        let requested_key = if mode == "init-other" {
            "media/efefefefefef40ef80efefefefefefef/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd"
        } else {
            &key
        };
        let upload = sender
            .initiate(
                &context,
                bucket,
                requested_key,
                &super::session_upload::completion_marker("fixture-lease"),
                until,
            )
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        assert_eq!(upload.configuration(), configuration);
        if !matches!(
            mode,
            "init-upload-completion-head" | "init-completion-response-lost"
        ) && !readback
        {
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
    let plan = Plan::new(target, header, plaintext_len).unwrap();
    let mut partition = plan.partition();
    let mut parts = Vec::new();
    let mut page = None;
    let chunk_count = chunks.len();
    for (index, sealed) in chunks.into_iter().enumerate() {
        let part = partition
            .push(&SealedChunk {
                digest: Sha256::digest(&sealed.bytes).into(),
                bytes: sealed.bytes,
                final_chunk: index + 1 == chunk_count,
            })
            .unwrap();
        assert!(
            multipart_resume || part.is_none(),
            "expected a part only on finish"
        );
        parts.extend(part);
        if sealed.page.is_some() {
            assert!(page.is_none());
            page = sealed.page;
        }
    }
    parts.extend(partition.finish().unwrap());
    let binding = ResumeBinding {
        plan: if mode == "part-other" {
            Plan::new(
                UploadTarget {
                    upload_id: "other",
                    ..target
                },
                header,
                plaintext_len,
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
    if multipart_resume {
        return resume_multipart(
            &sender,
            binding,
            &context,
            &parts,
            until,
            mode == "multipart-resume",
        )
        .await;
    }
    let [part] = parts.as_slice() else {
        panic!("expected one fixture part");
    };
    let (_, number, bytes, checksum) = part.request();
    let receipt = if mode.starts_with("part") || initiated.is_some() {
        let durable_target = if mode == "part-durable-id" {
            UploadTarget {
                upload_id: "different-durable-id",
                ..target
            }
        } else {
            target
        };
        let durable_session = if mode == "part-durable-session" {
            "different-session"
        } else {
            "fixture-session"
        };
        let durable = DurableUpload::new(durable_target, &context, durable_session, until);
        let uploaded = sender
            .put_part(binding, &context, part, &durable, until)
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
    let completed = if initiated.is_some() {
        let durable = DurableUpload::new(target, &context, binding.session, until);
        sender.complete(&durable, completion).await
    } else {
        sender.complete_fixture(completion, until).await
    }
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
    if let Some(record) = record {
        let page = page.unwrap();
        let put = SidecarPut::new(bucket, &context, &manifest, map_id, 0, &page).unwrap();
        let sidecars = [sender
            .put_sidecar(put, until)
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?];
        let inventory = CompletedReadbackInventory::assemble(
            bucket,
            configuration,
            &context,
            map_id,
            &manifest,
            &completed,
            &sidecars,
        )
        .unwrap();
        let envelope = EnrolledEnvelope {
            authorized_root: &root,
            context: &context,
            trusted_record: &record,
            retained_record: &record,
        };
        let readback_until = if mode == "readback-deadline" {
            now() + Duration::from_millis(500)
        } else {
            until
        };
        let verified = sender
            .verify_readback(&inventory, envelope, readback_until)
            .await
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        if mode == "readback-publish" {
            let object = crate::admin_review::ObjectRef {
                media: completed.metadata().clone(),
                plaintext_sha256: *verified.origin().plaintext_sha256(),
            };
            return Ok(catalogue::publish(
                &verified,
                &context,
                &record,
                map_id,
                object,
                target.upload_id,
            )
            .await);
        }
    }
    let S3Binding::VersionId(version) = completed.metadata().locator().binding() else {
        return Err(JsValue::from_str("missing fixture version"));
    };
    Ok(version.clone())
}

async fn resume_multipart<'a>(
    sender: &S3CompletionSender,
    binding: ResumeBinding<'a>,
    context: &'a EnvelopeContext,
    parts: &[UploadPart<'a>],
    until: std::time::SystemTime,
    interrupt_second: bool,
) -> Result<String, JsValue> {
    let [first, second] = parts else {
        panic!("expected two fixture parts");
    };
    let dispatch_error = |error| JsValue::from_str(&format!("{error:?}"));
    let (target, _, _, _) = first.request();
    let mut receipts = {
        let upload = DurableUpload::new(target, context, binding.session, until);
        let first = sender
            .put_part(binding, context, first, &upload, until)
            .await
            .map_err(dispatch_error)?;
        let second = sender
            .put_part(binding, context, second, &upload, until)
            .await;
        if interrupt_second {
            assert!(matches!(second, Err(DispatchError::Unconfirmed(None))));
            vec![first]
        } else {
            vec![first, second.map_err(dispatch_error)?]
        }
    };
    let etags: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.etag().to_owned())
        .collect();
    let mut saved: Vec<_> = receipts
        .iter()
        .zip(&etags)
        .map(|(receipt, etag)| checkpoint_part(receipt, etag))
        .collect();

    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &saved,
    };
    // The fixture stands in for the caller's checkpoint store and scoped
    // handoff. The sealed bytes and verified receipts survive across attempts.
    assert!(matches_checkpoint(binding, &receipts, &checkpoint));
    let resumed_etag;
    if saved.len() == 1 {
        let upload = DurableUpload::new(target, context, binding.session, until);
        let receipt = sender
            .put_part(binding, context, second, &upload, until)
            .await
            .map_err(dispatch_error)?;
        resumed_etag = receipt.etag().to_owned();
        saved.push(checkpoint_part(&receipt, &resumed_etag));
        receipts.push(receipt);
    }
    let completed_checkpoint = ResumeCheckpoint {
        binding,
        parts: &saved,
    };
    let completion = SessionCompletion::prepare(binding, context, receipts, &completed_checkpoint)
        .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
    let completed = sender
        .complete(&upload, completion)
        .await
        .map_err(dispatch_error)?;
    let S3Binding::VersionId(version) = completed.metadata().locator().binding() else {
        return Err(JsValue::from_str("missing fixture version"));
    };
    Ok(version.clone())
}

fn checkpoint_part<'a>(receipt: &UploadedPart<'_>, etag: &'a str) -> CheckpointPart<'a> {
    CheckpointPart {
        number: receipt.number,
        len: receipt.len,
        etag,
        digest: receipt.digest,
    }
}
