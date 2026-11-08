use super::*;
use crate::probe::ProbeTally;

fn checkpoint() -> JobCheckpoint {
    let message =
        crate::dispatch::build_upload_dispatch("stage_user", crate::fixtures::SMALL_NZB).unwrap();
    let mut cp = crate::checkpoint::initial(&message);
    cp.source = JobSource::AdminUpload;
    cp.user_id.clear();
    cp.job_id = format!("adm_{}", "a".repeat(43));
    cp.idempotency_key = format!("admin:{}", "a".repeat(64));
    cp
}

fn identity(cp: &JobCheckpoint) -> Identity<'_> {
    Identity {
        job_id: &cp.job_id,
        user_id: &cp.user_id,
        idempotency_key: &cp.idempotency_key,
    }
}

fn initial() -> Snapshot {
    let cp = checkpoint();
    Snapshot::initial(cp.clone(), &identity(&cp)).unwrap()
}

fn document() -> Value {
    serde_json::from_str(&initial().encode().unwrap()).unwrap()
}

fn decode(value: &Value) -> Result<Snapshot, CheckpointError> {
    Snapshot::decode(&value.to_string(), 1, &identity(&checkpoint()))
}

#[test]
fn strict_round_trip_and_checked_generation_revision_metadata() {
    let first = initial();
    let cp = first.checkpoint();
    let restored = Snapshot::decode(&first.encode().unwrap(), 1, &identity(cp)).unwrap();
    assert!(restored == first);
    assert_eq!(first.status(), "queued");
    let mut next = cp.clone();
    crate::jobs::advance_checkpoint(&mut next, false);
    let second = first.successor(next.clone(), false, &identity(cp)).unwrap();
    assert_eq!(
        (second.revision(), second.generation(), second.status()),
        (2, 1, "running")
    );
    let manual = second.successor(next, true, &identity(cp)).unwrap();
    assert_eq!((manual.revision(), manual.generation()), (3, 3));
    assert_eq!(first.revision(), 1);
    let mut value = document();
    value["generation"] = i64::MAX.into();
    let last = Snapshot::decode(&value.to_string(), i64::MAX, &identity(cp)).unwrap();
    assert!(matches!(
        last.successor(cp.clone(), false, &identity(cp)),
        Err(CheckpointError::Exhausted)
    ));
}

#[test]
fn envelope_rejects_null_unknown_missing_and_invalid_counter_types() {
    assert!(decode(&Value::Null).is_err());
    for path in [
        vec!["schema_version"],
        vec!["generation"],
        vec!["checkpoint"],
        vec!["checkpoint", "attempt"],
        vec!["checkpoint", "repair_rounds"],
    ] {
        let mut value = document();
        if path.len() == 1 {
            value.as_object_mut().unwrap().remove(path[0]);
        } else {
            value["checkpoint"].as_object_mut().unwrap().remove(path[1]);
        }
        assert!(decode(&value).is_err());
    }
    for version in [
        Value::Null,
        0.into(),
        2.into(),
        "1".into(),
        true.into(),
        serde_json::json!(1.0),
    ] {
        let mut value = document();
        value["schema_version"] = version;
        assert!(decode(&value).is_err());
    }
    for generation in [
        Value::Null,
        0.into(),
        (-1).into(),
        2.into(),
        "1".into(),
        serde_json::json!(1.5),
    ] {
        let mut value = document();
        value["generation"] = generation;
        assert!(decode(&value).is_err());
    }
    for revision in [0, -1] {
        assert!(Snapshot::decode(
            &initial().encode().unwrap(),
            revision,
            &identity(&checkpoint())
        )
        .is_err());
    }
    for place in ["envelope", "checkpoint", "tally"] {
        let mut value = document();
        match place {
            "envelope" => {
                value["extra"] = Value::Null;
            }
            "checkpoint" => {
                value["checkpoint"]["extra"] = Value::Null;
            }
            _ => {
                value["checkpoint"]["probe_tally"] =
                    serde_json::json!({"total_batches":1,"missing_by_batch":{},"extra":null});
            }
        }
        assert!(decode(&value).is_err());
    }
}

#[test]
fn identity_source_neutral_fields_and_failure_vocabulary_fail_closed() {
    for (field, invalid) in [
        ("job_id", "a".into()),
        ("user_id", "a".into()),
        ("idempotency_key", "a".into()),
        ("source", "nzb_upload".into()),
        ("attempt", 0.into()),
        ("attempt", 6.into()),
        ("attempt", "1".into()),
        ("repair_rounds", 1.into()),
        ("missing_blocks", 1.into()),
        ("par2_set_id", "a".into()),
        ("provider_id", "a".into()),
        ("transcode_next", 1.into()),
        ("transcode_inputs", serde_json::json!([{}])),
        ("failure_code", "a".into()),
        ("failure_reason", "a".into()),
        ("admin_release_mbid", "a".into()),
    ] {
        let mut value = document();
        value["checkpoint"][field] = invalid;
        assert!(decode(&value).is_err(), "{field}");
    }
    for state in [
        "searching",
        "verifying",
        "repairing",
        "transcoding",
        "a",
        "probing",
        "writing",
        "done",
    ] {
        let mut value = document();
        value["checkpoint"]["state"] = state.into();
        assert!(decode(&value).is_err(), "{state}");
    }
    for state in [
        "queued",
        "parsing",
        "matching",
        "needs_review",
        "needs_archive_read",
        "rejected",
        "failed",
    ] {
        let mut value = document();
        value["checkpoint"]["state"] = state.into();
        assert!(decode(&value).is_ok(), "{state}");
    }
    let mut value = document();
    value["checkpoint"]["failure_code"] = "probe_failed".into();
    value["checkpoint"]["failure_reason"] = "retry budget exhausted".into();
    value["checkpoint"]["state"] = "needs_review".into();
    assert!(decode(&value).is_ok());
}

fn maximal() -> Value {
    let mut cp = checkpoint();
    cp.state = JobState::Writing;
    cp.admin_release_mbid = Some("11111111-1111-4111-8111-111111111111".into());
    let mut tally = ProbeTally::new(MAX_BATCHES);
    tally
        .missing_by_batch
        .extend((0..MAX_BATCHES).map(|i| (i, 0)));
    cp.probe_tally = Some(tally);
    let mut value = document();
    value["checkpoint"] = serde_json::to_value(cp).unwrap();
    value
}

#[test]
fn maximal_numeric_tally_round_trips_and_invalid_geometry_refuses() {
    assert_eq!(MAX_BATCHES, 120);
    let value = maximal();
    let snapshot = decode(&value).unwrap();
    assert_eq!(
        snapshot
            .checkpoint()
            .probe_tally
            .as_ref()
            .unwrap()
            .missing_by_batch
            .len(),
        120
    );
    assert!(snapshot.encode().unwrap().len() < MAX_CHECKPOINT_BYTES);
    for (index, count) in [("120", 0), ("01", 0), ("+1", 0), ("-1", 0), ("0", 501)] {
        let mut bad = value.clone();
        bad["checkpoint"]["probe_tally"]["missing_by_batch"][index] = count.into();
        assert!(decode(&bad).is_err());
    }
    for total in [0, 121] {
        let mut bad = value.clone();
        bad["checkpoint"]["probe_tally"]["total_batches"] = total.into();
        assert!(decode(&bad).is_err());
    }
    let mut bad = value.clone();
    bad["checkpoint"]["probe_tally"]["missing_by_batch"]
        .as_object_mut()
        .unwrap()
        .remove("119");
    assert!(decode(&bad).is_err());
    bad["checkpoint"]["state"] = "probing".into();
    assert!(decode(&bad).is_ok());
    bad["checkpoint"]["probe_tally"]["missing_by_batch"]["0"] = 500.into();
    assert!(decode(&bad).is_ok()); // Actual last-batch geometry still needs the authenticated original.
    bad["checkpoint"]["state"] = "done".into();
    assert!(decode(&bad).is_err());
    let mut done = value;
    done["checkpoint"]["state"] = "done".into();
    assert_eq!(decode(&done).unwrap().status(), "done");
}

#[test]
fn duplicate_keys_noncanonical_numbers_and_oversize_input_refuse() {
    let encoded = initial().encode().unwrap();
    let at_limit = format!(
        "{}{}",
        encoded,
        " ".repeat(MAX_CHECKPOINT_BYTES - encoded.len())
    );
    assert_eq!(at_limit.len(), MAX_CHECKPOINT_BYTES);
    assert!(Snapshot::decode(&at_limit, 1, &identity(&checkpoint())).is_ok());
    for duplicate in [
        encoded.replacen("\"generation\":1", "\"generation\":1,\"generation\":1", 1),
        encoded.replacen("\"attempt\":1", "\"attempt\":1,\"attempt\":1", 1),
        maximal()
            .to_string()
            .replacen("\"0\":0", "\"0\":0,\"0\":0", 1),
        encoded.replacen("\"attempt\":1", "\"attempt\":1e0", 1),
        format!("{}{}", encoded, " ".repeat(MAX_CHECKPOINT_BYTES)),
    ] {
        assert!(Snapshot::decode(&duplicate, 1, &identity(&checkpoint())).is_err());
    }
    // Legacy decoding remains backward compatible; the strict admin codec does not.
    let mut old = document()["checkpoint"].clone();
    old.as_object_mut().unwrap().remove("repair_rounds");
    assert!(serde_json::from_value::<JobCheckpoint>(old.clone()).is_ok());
    let mut envelope = document();
    envelope["checkpoint"] = old;
    assert!(decode(&envelope).is_err());
}
