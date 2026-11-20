//! Selected-file extraction only; no source publication, network or membership.
use futures_util::{stream, StreamExt};
use nntune_ingest::{
    audio_selection::{
        extract_hints, AudioSelectionPlan, PrefixReader, SelectionError, MAX_JOB_PREFIX_BYTES,
    },
    audio_tags::MAX_TAG_PREFIX_BYTES,
    dispatch::content_sha256_hex,
};

fn original(names: &[&str]) -> String {
    let mut xml = String::from("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">");
    for (i, name) in names.iter().enumerate() {
        xml.push_str(&format!("<file poster=\"test\" date=\"0\" subject=\"&quot;{name}&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part{i}@example.invalid</segment></segments></file>"));
    }
    xml.push_str("</nzb>");
    xml
}

fn plan(names: &[&str]) -> AudioSelectionPlan {
    let xml = original(names);
    AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())).unwrap()
}

fn flac() -> Vec<u8> {
    let mut bytes = b"fLaC".to_vec();
    bytes.extend([128, 0, 0, 34]);
    let mut info = [0; 34];
    info[10..18].copy_from_slice(&((44_100_u64 << 44) | 88_200).to_be_bytes());
    bytes.extend(info);
    bytes
}

struct Reader {
    calls: Vec<(usize, usize)>,
    chunks: Vec<Result<Vec<u8>, ()>>,
}

impl PrefixReader for Reader {
    fn prefix<'a>(
        &'a mut self,
        selection: &'a nntune_ingest::audio_selection::DirectAudioSelection,
        cap: usize,
    ) -> futures_util::stream::LocalBoxStream<'a, Result<Vec<u8>, ()>> {
        self.calls
            .push((selection.selector().nzb_file_ordinal, cap));
        stream::iter(self.chunks.clone()).boxed_local()
    }
}

#[test]
fn selections_preserve_original_indexes_but_only_audio_consumes_ordinals() {
    let plan = plan(&["readme.nfo", "z.flac", "z.par2", "a.flac"]);
    let selections = plan.selections();
    assert_eq!(selections.len(), 2);
    assert_eq!(selections[0].audio_ordinal(), 1);
    assert_eq!(selections[0].selector().nzb_file_ordinal, 1);
    assert_eq!(selections[1].audio_ordinal(), 2);
    assert_eq!(selections[1].selector().nzb_file_ordinal, 3);
    assert!(selections[0].selector().archive_member.is_none());
    assert_eq!(selections[0].selector().audio_selection_index, 0);
}

#[test]
fn changed_original_is_rejected_before_selection() {
    let xml = original(&["a.flac"]);
    let changed = xml.replace("a.flac", "b.flac");
    assert!(matches!(
        AudioSelectionPlan::from_original(&changed, &content_sha256_hex(xml.as_bytes())),
        Err(SelectionError::Identity)
    ));
    assert!(matches!(
        AudioSelectionPlan::from_original(&xml, &"A".repeat(64)),
        Err(SelectionError::Identity)
    ));
}

#[test]
fn unsupported_audio_and_archives_are_not_silently_dropped() {
    for name in ["a.mp3", "a.rar", "a.zip"] {
        let xml = original(&["a.flac", name]);
        assert!(
            matches!(
                AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())),
                Err(SelectionError::Unsupported)
            ),
            "{name}"
        );
    }
    let xml = original(&["readme.nfo"]);
    assert!(matches!(
        AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())),
        Err(SelectionError::NoAudio)
    ));
}

#[tokio::test]
async fn small_files_leave_budget_for_later_selected_files() {
    let plan = plan(&["a.flac", "b.flac", "c.flac", "d.flac", "e.flac"]);
    let mut reader = Reader {
        calls: vec![],
        chunks: vec![Ok(flac())],
    };
    let entries = extract_hints(&plan, &mut reader).await.unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(reader.calls.len(), 5);
    for entry in entries {
        assert_eq!(entry.hints.duration_ms, Some(2000));
        assert!(entry.hints.fingerprint.is_none());
    }
}

#[tokio::test]
async fn exhaustion_preserves_selection_without_more_io_or_partial_hints() {
    let plan = plan(&["a.flac", "b.flac", "c.flac", "d.flac", "e.flac"]);
    let mut reader = Reader {
        calls: vec![],
        chunks: vec![Ok(vec![0; MAX_TAG_PREFIX_BYTES])],
    };
    let entries = extract_hints(&plan, &mut reader).await.unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(reader.calls.len(), 4);
    assert_eq!(
        reader.calls.iter().map(|(_, cap)| cap).sum::<usize>(),
        MAX_JOB_PREFIX_BYTES
    );
    assert!(entries
        .iter()
        .all(|entry| entry.hints.duration_ms.is_none()));
    assert_eq!(entries[4].selector.nzb_file_ordinal, 4);
    assert_eq!(entries[4].audio_ordinal, 5);
}

#[tokio::test]
async fn oversize_chunks_and_transport_failures_refuse_the_whole_extraction() {
    let plan = plan(&["a.flac"]);
    for chunks in [
        vec![Ok(vec![0; MAX_TAG_PREFIX_BYTES + 1])],
        vec![Ok(flac()), Err(())],
    ] {
        let mut reader = Reader {
            calls: vec![],
            chunks,
        };
        assert!(extract_hints(&plan, &mut reader).await.is_err());
    }
}

#[tokio::test]
async fn full_prefix_never_polls_the_stream_again() {
    let plan = plan(&["a.flac"]);
    // The trailing error must not be polled once the requested prefix is full.
    let mut reader = Reader {
        calls: vec![],
        chunks: vec![Ok(vec![0; MAX_TAG_PREFIX_BYTES]), Err(())],
    };
    assert!(extract_hints(&plan, &mut reader).await.is_ok());
}

#[tokio::test]
async fn budget_remainder_shrinks_the_next_request() {
    let plan = plan(&["a.flac", "b.flac", "c.flac", "d.flac", "e.flac", "f.flac"]);
    struct RemainderReader(Vec<usize>);
    impl PrefixReader for RemainderReader {
        fn prefix<'a>(
            &'a mut self,
            _: &'a nntune_ingest::audio_selection::DirectAudioSelection,
            cap: usize,
        ) -> futures_util::stream::LocalBoxStream<'a, Result<Vec<u8>, ()>> {
            self.0.push(cap);
            let len = cap.min(MAX_TAG_PREFIX_BYTES - 1);
            stream::iter([Ok(vec![0; len])]).boxed_local()
        }
    }
    let mut reader = RemainderReader(vec![]);
    let entries = extract_hints(&plan, &mut reader).await.unwrap();
    assert_eq!(entries.len(), 6);
    assert_eq!(
        reader.0,
        [
            MAX_TAG_PREFIX_BYTES,
            MAX_TAG_PREFIX_BYTES,
            MAX_TAG_PREFIX_BYTES,
            MAX_TAG_PREFIX_BYTES,
            4
        ]
    );
}

#[tokio::test]
async fn empty_chunk_flood_is_bounded() {
    let plan = plan(&["a.flac"]);
    let mut reader = Reader {
        calls: vec![],
        chunks: vec![Ok(vec![]); 4097],
    };
    assert!(matches!(
        extract_hints(&plan, &mut reader).await,
        Err(SelectionError::Bounds)
    ));
}

#[test]
fn malformed_original_and_oversized_original_are_refused() {
    for body in ["<nzb>".to_owned(), "x".repeat(2 * 1024 * 1024 + 1)] {
        assert!(
            AudioSelectionPlan::from_original(&body, &content_sha256_hex(body.as_bytes())).is_err()
        );
    }
}
