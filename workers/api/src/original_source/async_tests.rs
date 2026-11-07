struct AsyncProjection {
    rows: [Option<SourceMetadata>; 2],
    envelope: Option<FieldEnvelope>,
    calls: Vec<&'static str>,
}

impl AsyncProjection {
    fn valid() -> Self {
        let sealed = seal_original_source(&Keys, 7, &prepare(A, 1, JOB)).unwrap();
        Self {
            rows: [Some(sealed.metadata), None],
            envelope: Some(sealed.envelope),
            calls: Vec::new(),
        }
    }
}

impl AsyncSourceRepository for AsyncProjection {
    async fn metadata(
        &mut self,
        _: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        self.calls.push("metadata");
        Ok(self.rows.clone())
    }
    async fn envelope(
        &mut self,
        _: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
        self.calls.push("envelope");
        Ok(self.envelope.clone())
    }
}

#[tokio::test]
async fn async_source_opens_the_same_bound_payload() {
    let mut store = AsyncProjection::valid();
    let source = read_original_source_async(&Allow, &mut store, &Keys, A)
        .await
        .unwrap();
    assert_eq!(
        source.encode().unwrap(),
        prepare(A, 1, JOB).encode().unwrap()
    );
    assert_eq!(store.calls, ["metadata", "envelope"]);
}

#[tokio::test]
async fn async_source_denies_before_any_store_call() {
    let mut store = AsyncProjection::valid();
    for track in [A, "not-a-track"] {
        assert!(matches!(
            read_original_source_async(&Deny, &mut store, &Keys, track).await,
            Err(SourceServiceError::Denied)
        ));
        assert!(store.calls.is_empty());
    }
    assert!(matches!(
        read_original_source_async(&Allow, &mut store, &Keys, "not-a-track").await,
        Err(SourceServiceError::Invalid)
    ));
    assert!(store.calls.is_empty());
}

#[tokio::test]
async fn async_source_rechecks_authority_after_each_await() {
    struct Expires(std::cell::Cell<usize>);
    impl SourceReadAuthorization for Expires {
        fn authorize(&self, _: &str) -> Result<(), SourceServiceError> {
            let remaining = self.0.get();
            self.0.set(remaining.saturating_sub(1));
            if remaining == 0 {
                Err(SourceServiceError::Denied)
            } else {
                Ok(())
            }
        }
    }
    for (checks, calls) in [(1, vec!["metadata"]), (2, vec!["metadata", "envelope"])] {
        let mut store = AsyncProjection::valid();
        assert!(matches!(
            read_original_source_async(&Expires(checks.into()), &mut store, &Keys, A).await,
            Err(SourceServiceError::Denied)
        ));
        assert_eq!(store.calls, calls);
    }
}

#[tokio::test]
async fn async_source_rejects_metadata_before_loading_bytes() {
    for mutation in 0..12 {
        let mut store = AsyncProjection::valid();
        let metadata = store.rows[0].as_mut().unwrap();
        match mutation {
            0 => metadata.track_id = B.into(),
            1 => metadata.source_job_id = "bad job".into(),
            2 => metadata.envelope_version = 1,
            3 => metadata.payload_format = 2,
            4 => metadata.kek_version = -1,
            5 => metadata.kek_version = 8,
            6 => metadata.ciphertext_len = 16,
            7 => metadata.ciphertext_len = MAX_CIPHERTEXT_BYTES as i32 + 1,
            8 => metadata.wrap_nonce_len = 13,
            9 => metadata.wrapped_dek_len = 49,
            10 => metadata.payload_nonce_len = 13,
            _ => store.rows[1] = store.rows[0].clone(),
        }
        assert!(read_original_source_async(&Allow, &mut store, &Keys, A)
            .await
            .is_err());
        assert_eq!(store.calls, ["metadata"], "mutation {mutation}");
    }
    let mut store = AsyncProjection::valid();
    store.rows = [None, None];
    assert!(matches!(
        read_original_source_async(&Allow, &mut store, &Keys, A).await,
        Err(SourceServiceError::Missing)
    ));
    assert_eq!(store.calls, ["metadata"]);
}

#[tokio::test]
async fn async_source_rejects_racing_missing_and_tampered_bytes() {
    for mutation in 0..5 {
        let mut store = AsyncProjection::valid();
        match mutation {
            0 => store.envelope = None,
            1 => store.envelope.as_mut().unwrap().ciphertext.push(0),
            2 => store
                .envelope
                .as_mut()
                .unwrap()
                .dek_wrapped
                .pop()
                .map(|_| ())
                .unwrap(),
            3 => store.envelope.as_mut().unwrap().ciphertext[0] ^= 1,
            _ => {
                store.envelope = Some(
                    seal_original_source(&Keys, 7, &prepare(B, 2, JOB))
                        .unwrap()
                        .envelope,
                )
            }
        }
        assert!(read_original_source_async(&Allow, &mut store, &Keys, A)
            .await
            .is_err());
        assert_eq!(store.calls, ["metadata", "envelope"]);
    }
}

#[tokio::test]
async fn async_source_cancellation_does_not_open_an_envelope() {
    struct Pending(bool);
    impl AsyncSourceRepository for Pending {
        async fn metadata(
            &mut self,
            _: &str,
        ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
            self.0 = true;
            std::future::pending().await
        }
        async fn envelope(
            &mut self,
            _: &SourceMetadata,
        ) -> Result<Option<FieldEnvelope>, SourceServiceError> {
            panic!("cancelled metadata must not load bytes")
        }
    }
    let mut store = Pending(false);
    {
        let mut read = Box::pin(read_original_source_async(&Allow, &mut store, &Keys, A));
        assert!(futures_util::poll!(&mut read).is_pending());
    }
    assert!(store.0);
}
