use super::*;

fn wire() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "job_id": "job_1",
        "idempotency_key": format!("admin:{}", "a".repeat(64)),
        "generation": 1,
    })
}

#[test]
fn delivery_roundtrip_contains_only_bounded_wake_identity_and_generation() {
    let value = wire();
    let delivery = Delivery::decode(&value.to_string()).unwrap();
    assert_eq!(delivery.job_id(), "job_1");
    assert_eq!(
        delivery.idempotency_key(),
        value["idempotency_key"].as_str().unwrap()
    );
    assert_eq!(delivery.generation(), 1);
    let encoded = delivery.encode().unwrap();
    assert!(encoded.len() < 512);
    assert!(serde_json::from_str::<crate::queue::QueueMsg>(&encoded).is_err());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
        value
    );
    assert!(Delivery::decode(&encoded).unwrap() == delivery);
    let padded = format!("{}{}", encoded, " ".repeat(512 - encoded.len()));
    assert!(Delivery::decode(&padded).is_ok());
    assert_eq!(
        Delivery::decode(&(padded + " ")).err(),
        Some(StoreError::Invalid)
    );
}

#[test]
fn delivery_refuses_legacy_extra_missing_duplicate_and_malformed_fields() {
    for field in ["schema_version", "job_id", "idempotency_key", "generation"] {
        let mut value = wire();
        value.as_object_mut().unwrap().remove(field);
        assert_eq!(
            Delivery::decode(&value.to_string()).err(),
            Some(StoreError::Invalid)
        );
        let encoded = wire().to_string();
        let duplicate = format!("{{\"{field}\":{},{}", wire()[field], &encoded[1..]);
        assert_eq!(
            Delivery::decode(&duplicate).err(),
            Some(StoreError::Invalid)
        );
    }
    for field in [
        "user_id",
        "source",
        "state",
        "attempt",
        "provider_id",
        "payload_ref",
        "checkpoint",
    ] {
        let mut value = wire();
        value[field] = serde_json::Value::Null;
        assert_eq!(
            Delivery::decode(&value.to_string()).err(),
            Some(StoreError::Invalid)
        );
    }
    for (field, values) in [
        (
            "schema_version",
            vec![
                serde_json::json!(0),
                serde_json::json!(2),
                serde_json::json!(256),
                serde_json::json!("1"),
            ],
        ),
        (
            "generation",
            vec![
                serde_json::json!(0),
                serde_json::json!(-1),
                serde_json::json!(1.5),
                serde_json::json!("1"),
                serde_json::json!(u64::MAX),
            ],
        ),
        (
            "job_id",
            vec![
                serde_json::json!(""),
                serde_json::json!("../other"),
                serde_json::json!("a".repeat(65)),
                serde_json::Value::Null,
            ],
        ),
        (
            "idempotency_key",
            vec![
                serde_json::json!(""),
                serde_json::json!(format!("admin:{}", "A".repeat(64))),
                serde_json::json!(format!("admin:{}", "a".repeat(63))),
                serde_json::json!("ingest:u1:abc:upload"),
            ],
        ),
    ] {
        for invalid in values {
            let mut value = wire();
            value[field] = invalid;
            assert_eq!(
                Delivery::decode(&value.to_string()).err(),
                Some(StoreError::Invalid)
            );
        }
    }
    for encoded in ["null", "[]", "{}", "{", "true"] {
        assert_eq!(Delivery::decode(encoded).err(), Some(StoreError::Invalid));
    }
    let mut value = wire();
    value["generation"] = i64::MAX.into();
    assert!(Delivery::decode(&value.to_string()).is_ok());
}
