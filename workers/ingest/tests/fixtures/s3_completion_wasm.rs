use super::{now, valid_part_etag, Duration, FixtureVersionRemover, S3CompletionSender};
use crate::multipart::{
    readback_inventory::CompletedReadbackInventory,
    resume_checkpoint::{matches_checkpoint, CheckpointPart, ResumeBinding, ResumeCheckpoint},
    session_completion::{DispatchError, PrepareError, SessionCompletion},
    session_upload::DurableUpload,
    sidecar::SidecarPut,
    MultipartError, Plan, UploadPart, UploadTarget, UploadedPart, PART_BYTES,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    digest_enrollment::DigestEnrollment,
    digest_readback::EnrolledEnvelope,
    media::PLAIN_CHUNK_SIZE,
    media_envelope::{wrap_scheme2, EnvelopeContext},
    origin_sealer::SealedChunk,
    s3_locator::S3Binding,
};
use sha2::{Digest, Sha256};
use wasm_bindgen_futures::JsFuture;
use worker::{
    js_sys::{Function, Promise, Uint8Array},
    wasm_bindgen::{self, prelude::*},
};

#[path = "catalogue_upload_wasm.rs"]
mod catalogue;

// Mono, 8-bit, 19 constant samples at 44.1 kHz.
const FLAC: &[u8] = b"fLaC\x80\0\0\x22\0\x10\0\x10\0\0\0\0\0\0\x0a\xc4\x40\x70\0\0\0\x13\
    \x46\xd2\xbe\x27\x5e\xd2\xdd\x86\xab\x2b\x53\x88\xf7\x34\x6d\x15\
    \xff\xf8\x69\x02\0\x0f\xb7\0\x2a\x7a\xdd\xff\xf8\x69\x02\x01\x02\x81\0\x2a\x9d\x1d";
const TONE: &[u8] =
    include_bytes!("../../../../fixtures/synthetic-flac/private-staging/tone-a.flac");

fn id(value: u8) -> [u8; 16] {
    let mut bytes = [value; 16];
    bytes[6] = 0x40;
    bytes[8] = 0x80;
    bytes
}

#[wasm_bindgen]
pub async fn s3_completion_test(
    mode: String,
    read_page: Option<Function>,
) -> Result<String, JsValue> {
    let endpoint = match mode.as_str() {
        "http" => "http://fixture.test:8443",
        "path" => "https://fixture.test:8443/api",
        _ => "https://fixture.test:8443",
    };
    run(&mode, endpoint, read_page, None, None, false, None).await
}

#[wasm_bindgen]
pub async fn s3_completion_file_selection_test(
    source: Vec<u8>,
    select_file: Function,
) -> Result<String, JsValue> {
    if source.len() != PART_BYTES {
        return Err(JsValue::from_str("expected a two-part fixture source"));
    }
    run(
        "multipart-resume",
        "https://fixture.test:8443",
        None,
        None,
        None,
        false,
        Some((&source, &select_file)),
    )
    .await
}

#[wasm_bindgen]
pub async fn s3_multipart_delayed_status_test(observe: Function) -> Result<String, JsValue> {
    run(
        "multipart-resume-full",
        "https://fixture.test:8443",
        None,
        None,
        Some(&observe),
        false,
        None,
    )
    .await
}

#[wasm_bindgen]
pub async fn s3_multipart_conflicting_status_test(observe: Function) -> Result<String, JsValue> {
    run(
        "multipart-resume-full",
        "https://fixture.test:8443",
        None,
        None,
        Some(&observe),
        true,
        None,
    )
    .await
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
pub async fn upload_for_playback(
    endpoint: String,
    publish: bool,
    reconcile_completion: Option<Function>,
) -> Result<String, JsValue> {
    let mode = if publish {
        "readback-publish"
    } else {
        "readback-playback"
    };
    run(
        mode,
        &endpoint,
        None,
        reconcile_completion,
        None,
        false,
        None,
    )
    .await
}

async fn run(
    mode: &str,
    endpoint: &str,
    read_page: Option<Function>,
    reconcile_completion: Option<Function>,
    observe: Option<&Function>,
    conflicting_status: bool,
    file_selection: Option<(&[u8], &Function)>,
) -> Result<String, JsValue> {
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
    // The lost-acknowledgement inventory is read only after the failed PUT.
    let timeout_mode = mode == "multipart-resume-timeout";
    let mut read_page = read_page;
    let (mode, staged_inventory) = if mode == "multipart-resume-lost-ack" {
        let staged_inventory = read_page
            .take()
            .ok_or_else(|| JsValue::from_str("missing staged part inventory fixture"))?;
        ("multipart-resume", Some(staged_inventory))
    } else {
        (mode, None)
    };
    let readback = mode.starts_with("readback");
    let multipart_resume = matches!(
        mode,
        "multipart-resume" | "multipart-resume-full" | "multipart-resume-timeout"
    );
    let resume_bytes;
    let plaintext: &[u8] = if let Some((source, _)) = file_selection {
        source
    } else if matches!(mode, "readback-publish" | "readback-playback") {
        TONE
    } else if multipart_resume {
        resume_bytes = vec![42; PART_BYTES];
        &resume_bytes
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
        "readback-publish" | "readback-playback" => 5000,
        _ if multipart_resume => 5000,
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
            staged_inventory,
            mode == "multipart-resume",
            timeout_mode,
            read_page,
            observe,
            conflicting_status,
            file_selection.map(|(_, select_file)| (select_file, manifest.plaintext_sha256())),
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
    let pending_publication = if mode == "readback-publish" {
        Some(
            catalogue::prepare(
                &context,
                &manifest,
                record.as_ref().unwrap(),
                map_id,
                target,
            )
            .await,
        )
    } else {
        None
    };
    let prepare = |receipt| {
        SessionCompletion::prepare(binding, &context, vec![receipt], &checkpoint).unwrap()
    };
    let dispatch_error = |error| JsValue::from_str(&format!("{error:?}"));
    let durable = initiated
        .is_some()
        .then(|| DurableUpload::new(target, &context, binding.session, until));
    let complete = async |completion| match &durable {
        Some(durable) => sender.complete(durable, completion).await,
        None => sender.complete_fixture(completion, until).await,
    };
    let completed = match (complete(prepare(receipt)).await, reconcile_completion) {
        (
            Err(error @ DispatchError::Unconfirmed(Some(MultipartError::EmbeddedError))),
            Some(reconcile),
        ) => {
            pending_publication
                .as_ref()
                .unwrap()
                .assert_unpublished(target)
                .await;
            // The synthetic origin confirms the upload and its part are still pending.
            // Only then does the fixture dispatch again from the same checkpoint.
            assert_eq!(
                reconcile
                    .call1(&JsValue::UNDEFINED, &dispatch_error(error))?
                    .as_bool(),
                Some(true)
            );
            let restored = part.verify_reply(etag.clone(), checksum).unwrap();
            complete(prepare(restored)).await.map_err(dispatch_error)?
        }
        (result, _) => result.map_err(dispatch_error)?,
    };
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
        if let Some(pending) = pending_publication {
            let object = crate::admin_review::ObjectRef {
                media: completed.metadata().clone(),
                plaintext_sha256: *verified.origin().plaintext_sha256(),
            };
            return Ok(catalogue::publish(pending, &verified, context.identities[0], object).await);
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
    staged_inventory: Option<Function>,
    interrupt_second: bool,
    timeout_mode: bool,
    read_page: Option<Function>,
    observe: Option<&Function>,
    conflicting_status: bool,
    file_selection: Option<(&Function, &[u8; 32])>,
) -> Result<String, JsValue> {
    let [first, second] = parts else {
        panic!("expected two fixture parts");
    };
    let dispatch_error = |error| JsValue::from_str(&format!("{error:?}"));
    let (target, _, _, _) = first.request();
    let inventory = if let Some(read_page) = read_page {
        let empty = ResumeCheckpoint {
            binding,
            parts: &[],
        };
        let (coherent, status) = read_inventory_status(&read_page, target, &empty)?;
        if !coherent {
            return Err(JsValue::from_str(
                "initial inventory disagrees with checkpoint",
            ));
        }
        assert!(matches!(
            SessionCompletion::prepare(binding, context, Vec::new(), &empty),
            Err(PrepareError::Parts(MultipartError::Geometry))
        ));
        Some((read_page, status))
    } else {
        None
    };
    let upload = DurableUpload::new(target, context, binding.session, until);
    let first = sender
        .put_part(binding, context, first, &upload, until)
        .await
        .map_err(dispatch_error)?;
    if let Some((read_page, initial_status)) = inventory {
        let second = sender
            .put_part(binding, context, second, &upload, until)
            .await
            .map_err(dispatch_error)?;
        return resume_from_inventory(
            &read_page,
            target,
            binding,
            context,
            [first, second],
            initial_status,
        );
    }
    let mut receipts = vec![first];
    if let Some(observe) = observe {
        let report = |stage, part_count: usize| {
            observe
                .call2(
                    &JsValue::NULL,
                    &JsValue::from_str(stage),
                    &part_count.into(),
                )
                .map(Some)
        };
        let first_etag = receipts[0].etag().to_owned();
        let early_parts = [checkpoint_part(&receipts[0], &first_etag)];
        let conflicting_etag = "\"conflicting-part\"";
        let status_parts = [CheckpointPart {
            etag: if conflicting_status {
                conflicting_etag
            } else {
                first_etag.as_str()
            },
            ..early_parts[0]
        }];
        let delayed = report("status", status_parts.len())?;
        let attempt_until = if timeout_mode {
            now() + Duration::from_millis(500)
        } else {
            until
        };
        let second_result = sender
            .put_part(binding, context, second, &upload, attempt_until)
            .await;
        if interrupt_second || timeout_mode {
            assert!(matches!(
                second_result,
                Err(DispatchError::Unconfirmed(None))
            ));
        } else {
            receipts.push(second_result.map_err(dispatch_error)?);
        }
        report("confirmed", receipts.len())?;
        let current_parts;
        let status_parts = if let Some(reply) = delayed {
            JsFuture::from(Promise::resolve(&reply)).await?;
            status_parts.as_slice()
        } else {
            current_parts = checkpoint_parts(&receipts);
            current_parts.as_slice()
        };
        let status = ResumeCheckpoint {
            binding,
            parts: status_parts,
        };
        if !receipts
            .get(..status.parts.len())
            .is_some_and(|prefix| matches_checkpoint(binding, prefix, &status))
        {
            return Err(JsValue::from_str("conflicting multipart status"));
        }
        report("resumed", receipts.len())?;
    } else {
        let attempt_until = if timeout_mode {
            now() + Duration::from_millis(500)
        } else {
            until
        };
        let second_result = sender
            .put_part(binding, context, second, &upload, attempt_until)
            .await;
        if interrupt_second || timeout_mode {
            assert!(matches!(
                second_result,
                Err(DispatchError::Unconfirmed(None))
            ));
        } else {
            receipts.push(second_result.map_err(dispatch_error)?);
        }
    }
    let initial_etags: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.etag().to_owned())
        .collect();
    let initial_saved: Vec<_> = receipts
        .iter()
        .zip(&initial_etags)
        .map(|(receipt, etag)| checkpoint_part(receipt, etag))
        .collect();
    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &initial_saved,
    };
    assert!(matches_checkpoint(binding, &receipts, &checkpoint));
    if initial_saved.len() == 1 {
        if let Some((select_file, source_digest)) = file_selection {
            wait_for_original_file(&checkpoint, source_digest, select_file).await?;
        }
        let receipt = match staged_inventory {
            Some(read_page) => recover_staged_part(&read_page, initial_saved[0], second)?,
            None => sender
                .put_part(binding, context, second, &upload, until)
                .await
                .map_err(dispatch_error)?,
        };
        receipts.push(receipt);
    }
    if timeout_mode {
        worker::Delay::from(Duration::ZERO).await;
    }
    let etags: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.etag().to_owned())
        .collect();
    let saved: Vec<_> = receipts
        .iter()
        .zip(&etags)
        .map(|(receipt, etag)| checkpoint_part(receipt, etag))
        .collect();
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

#[derive(serde::Deserialize)]
struct InventoryPart {
    number: u32,
    len: usize,
    etag: String,
    digest: [u8; 32],
}

#[derive(serde::Deserialize)]
struct InventoryPage {
    next_marker: Option<u32>,
    parts: Vec<InventoryPart>,
}

fn resume_from_inventory<'a>(
    read_page: &Function,
    target: UploadTarget<'a>,
    binding: ResumeBinding<'a>,
    context: &'a EnvelopeContext,
    receipts: [UploadedPart<'a>; 2],
    initial_status: serde_json::Value,
) -> Result<String, JsValue> {
    let [first_etag, second_etag] = receipts.each_ref().map(|part| part.etag().to_owned());
    let saved = [
        checkpoint_part(&receipts[0], &first_etag),
        checkpoint_part(&receipts[1], &second_etag),
    ];
    let checkpoint = ResumeCheckpoint {
        binding,
        parts: &saved,
    };
    let mut statuses = vec![initial_status];
    for _ in 0..2 {
        let (coherent, status) = read_inventory_status(read_page, target, &checkpoint)?;
        statuses.push(status);
        if coherent {
            let completion =
                SessionCompletion::prepare(binding, context, receipts.into(), &checkpoint)
                    .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
            let manifest: Vec<_> = completion.manifest().collect();
            return Ok(
                serde_json::json!({ "statuses": statuses, "manifest": manifest }).to_string(),
            );
        }
    }
    Err(JsValue::from_str("fixture inventory did not recover"))
}

fn read_inventory_status(
    read_page: &Function,
    target: UploadTarget<'_>,
    checkpoint: &ResumeCheckpoint<'_>,
) -> Result<(bool, serde_json::Value), JsValue> {
    // The callback models one complete page or a read with one continuation.
    // A thrown provider error leaves the caller without an inventory verdict.
    let page = |marker: JsValue| -> Result<InventoryPage, JsValue> {
        let reply = read_page.call4(
            &JsValue::UNDEFINED,
            &JsValue::from_str(target.bucket),
            &JsValue::from_str(target.key),
            &JsValue::from_str(target.upload_id),
            &marker,
        )?;
        serde_wasm_bindgen::from_value(reply).map_err(|error| JsValue::from_str(&error.to_string()))
    };
    let mut first = page(JsValue::NULL)?;
    if let Some(marker) = first.next_marker {
        let last = page(JsValue::from(marker))?;
        assert!(last.next_marker.is_none());
        first.parts.extend(last.parts);
    }
    let coherent = checkpoint.matches_parts(first.parts.iter().map(|part| CheckpointPart {
        number: part.number,
        len: part.len,
        etag: &part.etag,
        digest: part.digest,
    }));
    Ok((coherent, inventory_status(coherent, target, checkpoint)))
}

fn inventory_status(
    coherent: bool,
    target: UploadTarget<'_>,
    checkpoint: &ResumeCheckpoint<'_>,
) -> serde_json::Value {
    let saved: Vec<_> = checkpoint
        .parts
        .iter()
        .map(|part| {
            serde_json::json!({
                "number": part.number,
                "len": part.len,
                "etag": part.etag,
                "digest": part.digest,
            })
        })
        .collect();
    let status = if coherent { "resumable" } else { "pending" };
    serde_json::json!({ "status": status, "upload_id": target.upload_id, "parts": saved })
}

fn checkpoint_parts<'a>(receipts: &'a [UploadedPart<'_>]) -> Vec<CheckpointPart<'a>> {
    receipts
        .iter()
        .map(|receipt| checkpoint_part(receipt, receipt.etag()))
        .collect()
}

fn checkpoint_part<'a>(receipt: &UploadedPart<'_>, etag: &'a str) -> CheckpointPart<'a> {
    CheckpointPart {
        number: receipt.number,
        len: receipt.len,
        etag,
        digest: receipt.digest,
    }
}

// Fixture handoff only. The browser's IngestRequests.upload adapter must supply
// its canonical pending attempt and saved plaintext digest before file selection.
// Keep the authoritative checkpoint, sealed bytes and receipts with the sender.
async fn wait_for_original_file(
    pending: &ResumeCheckpoint<'_>,
    source_digest: &[u8; 32],
    select_file: &Function,
) -> Result<(), JsValue> {
    let target = pending.binding.plan.target;
    let size = pending.binding.plan.plaintext_len;
    let offer = JsValue::from_str(
        &serde_json::json!({
            "session": pending.binding.session,
            "bucket": target.bucket,
            "key": target.key,
            "uploadId": target.upload_id,
            "size": size,
            "contentSha256": hex::encode(source_digest),
            "confirmedParts": pending.parts.len(),
        })
        .to_string(),
    );
    let mut mismatch = false;
    loop {
        let selection = select_file.call2(&JsValue::UNDEFINED, &offer, &JsValue::from(mismatch))?;
        let bytes = JsFuture::from(Promise::resolve(&selection))
            .await?
            .dyn_into::<Uint8Array>()
            .map_err(|_| JsValue::from_str("no fixture file selected"))?;
        if u64::from(bytes.length()) == size
            && <[u8; 32]>::from(Sha256::digest(bytes.to_vec())) == *source_digest
        {
            return Ok(());
        }
        mismatch = true;
    }
}

fn recover_staged_part<'a>(
    read_page: &Function,
    confirmed: CheckpointPart<'_>,
    part: &UploadPart<'a>,
) -> Result<UploadedPart<'a>, JsValue> {
    // This fixture accepts a terminal two-part snapshot. Keep the acknowledged
    // prefix pinned and derive the recovered receipt from the sealed request.
    #[derive(serde::Deserialize)]
    struct ListedPart {
        number: u32,
        len: usize,
        etag: String,
        digest: [u8; 32],
    }

    #[derive(serde::Deserialize)]
    struct Inventory {
        next_marker: Option<u32>,
        parts: [ListedPart; 2],
    }

    let pending = || JsValue::from_str("unconfirmed final part inventory");
    let (target, number, body, _) = part.request();
    let reply = read_page.call4(
        &JsValue::UNDEFINED,
        &JsValue::from_str(target.bucket),
        &JsValue::from_str(target.key),
        &JsValue::from_str(target.upload_id),
        &JsValue::NULL,
    )?;
    let Inventory {
        next_marker,
        parts: [prefix, staged],
    } = serde_wasm_bindgen::from_value(reply).map_err(|_| pending())?;
    if next_marker.is_some()
        || confirmed
            != (CheckpointPart {
                number: prefix.number,
                len: prefix.len,
                etag: &prefix.etag,
                digest: prefix.digest,
            })
        || (staged.number, staged.len) != (number, body.len())
        || !valid_part_etag(&staged.etag)
    {
        return Err(pending());
    }
    part.verify_reply(staged.etag, &STANDARD.encode(staged.digest))
        .map_err(|_| pending())
}
