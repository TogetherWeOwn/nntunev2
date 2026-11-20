//! Verified-candidate producer: owner/job/digest gates before any I/O, bound
//! scratch reads, bounded hints and full manifest grouping. A fake store stands
//! in for scratch; no network, spool, catalogue or position work happens.
use std::{cell::RefCell, collections::HashMap, future::Future, rc::Rc};

use futures_util::{
    future::{self, FutureExt, LocalBoxFuture},
    stream::{self, StreamExt},
};
use nntune_ingest::{
    audio_grouping::GroupingError,
    audio_producer::{prepare_verified_candidate, ProducerError, VerifiedCandidateInput},
    audio_scratch::{ScratchPrefixResponse, ScratchPrefixStore},
    audio_selection::SelectionError,
    dispatch::content_sha256_hex,
    verify::{StagedFile, VerifyProgress},
};

const JOB: &str = "job_0123456789abcdef";
const OWNER: &str = "owner-1";
const ROUND: u32 = 1;

fn flac(comments: &[&str]) -> Vec<u8> {
    let mut bytes = b"fLaC".to_vec();
    bytes.extend([0, 0, 0, 34]);
    let mut info = [0; 34];
    info[10..18].copy_from_slice(&((44_100_u64 << 44) | 88_200).to_be_bytes());
    bytes.extend(info);
    let mut body = vec![0, 0, 0, 0];
    body.extend((comments.len() as u32).to_le_bytes());
    for comment in comments {
        body.extend((comment.len() as u32).to_le_bytes());
        body.extend(comment.as_bytes());
    }
    let len = body.len() as u32;
    bytes.extend([0x84, (len >> 16) as u8, (len >> 8) as u8, len as u8]);
    bytes.extend(body);
    bytes
}

struct Fixture {
    xml: String,
    hash: String,
    files: Vec<(String, Vec<u8>)>,
}

fn fixture(files: Vec<(&str, Vec<u8>)>) -> Fixture {
    let mut xml = String::from("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">");
    for (i, (name, _)) in files.iter().enumerate() {
        xml.push_str(&format!("<file poster=\"test\" date=\"0\" subject=\"&quot;{name}&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part{i}@example.invalid</segment></segments></file>"));
    }
    xml.push_str("</nzb>");
    Fixture {
        hash: content_sha256_hex(xml.as_bytes()),
        xml,
        files: files.into_iter().map(|(n, b)| (n.to_owned(), b)).collect(),
    }
}

impl Fixture {
    fn progress(&self) -> VerifyProgress {
        let mut progress = VerifyProgress::new(ROUND);
        for (index, (name, bytes)) in self.files.iter().enumerate() {
            progress.staged.insert(
                index,
                StagedFile {
                    key: VerifyProgress::file_key(JOB, ROUND, index, name),
                    size: bytes.len() as u64,
                    sha256: content_sha256_hex(bytes),
                    missing_blocks: 0,
                    size_refused_blocks: 0,
                    par2_set_id: None,
                    spool: vec![],
                    crc32: 0,
                    missing: false,
                },
            );
        }
        progress
    }

    fn store(&self) -> Store {
        Store {
            reads: Rc::default(),
            fail_read: None,
            objects: self
                .files
                .iter()
                .enumerate()
                .map(|(i, (n, b))| (VerifyProgress::file_key(JOB, ROUND, i, n), b.clone()))
                .collect(),
        }
    }
}

struct Store {
    reads: Rc<RefCell<Vec<String>>>,
    /// 0-based read number that fails with a transport error.
    fail_read: Option<usize>,
    objects: HashMap<String, Vec<u8>>,
}

impl ScratchPrefixStore for Store {
    fn read<'a>(
        &'a self,
        expected: &'a StagedFile,
        length: usize,
    ) -> LocalBoxFuture<'a, Result<ScratchPrefixResponse<'a>, ()>> {
        async move {
            let read_no = self.reads.borrow().len();
            self.reads.borrow_mut().push(expected.key.clone());
            if self.fail_read == Some(read_no) {
                return Err(());
            }
            let data = self.objects.get(&expected.key).ok_or(())?;
            Ok(ScratchPrefixResponse {
                staged: expected.clone(),
                object_size: data.len() as u64,
                range_offset: 0,
                range_length: length as u64,
                body: stream::iter(vec![Ok(data[..length].to_vec())]).boxed_local(),
            })
        }
        .boxed_local()
    }
}

async fn produce<D: Future<Output = ()> + Unpin>(
    fx: &Fixture,
    job_id: &str,
    user_id: &str,
    store: Store,
    deadline: D,
) -> Result<nntune_ingest::audio_manifest::AcceptedAudioManifest, ProducerError> {
    let progress = fx.progress();
    prepare_verified_candidate(
        VerifiedCandidateInput {
            job_id,
            user_id,
            original: &fx.xml,
            expected_hash: &fx.hash,
            progress: &progress,
        },
        store,
        deadline,
    )
    .await
}

fn tagged(album: &str, disc: &str) -> Vec<u8> {
    flac(&[
        &format!("ALBUM={album}"),
        "ARTIST=The Artist",
        &format!("DISCNUMBER={disc}"),
        "TRACKNUMBER=1",
    ])
}

#[tokio::test]
async fn two_flac_candidate_preserves_ordinals_selectors_groups_and_discs() {
    // PAR2/NFO extras are skipped but keep their original document positions.
    let fx = fixture(vec![
        ("a.par2", b"par".to_vec()),
        ("one.flac", tagged("Same Album", "1")),
        ("readme.nfo", b"notes".to_vec()),
        ("two.flac", tagged("same  album", "2")),
    ]);
    let store = fx.store();
    let reads = store.reads.clone();
    let manifest = produce(&fx, JOB, OWNER, store, future::pending::<()>())
        .await
        .unwrap();
    assert!(manifest.is_for(JOB, OWNER));
    assert_eq!(manifest.original_content_sha256(), fx.hash);
    let entries = manifest.entries();
    assert_eq!(entries.len(), 2);
    let shape: Vec<_> = entries
        .iter()
        .map(|e| {
            (
                e.audio_ordinal,
                e.source_selector.nzb_file_ordinal,
                e.source_selector.archive_member.clone(),
                e.source_selector.audio_selection_index,
                e.artist_group_ref,
                e.release_group_ref,
            )
        })
        .collect();
    assert_eq!(shape, [(1, 1, None, 0, 1, 1), (2, 3, None, 0, 1, 1)]);
    assert_eq!(entries[0].hints.album.as_deref(), Some("Same Album"));
    assert_eq!(entries[1].hints.disc_no, Some(2));
    assert_eq!(entries[1].hints.duration_ms, Some(2000));
    assert_eq!(manifest.effective_discs(), Some(&[Some(1), Some(2)][..]));
    let keys = reads.borrow().clone();
    assert_eq!(
        keys,
        [
            VerifyProgress::file_key(JOB, ROUND, 1, "one.flac"),
            VerifyProgress::file_key(JOB, ROUND, 3, "two.flac"),
        ]
    );
}

#[tokio::test]
async fn second_file_transport_failure_returns_no_partial_candidate() {
    let fx = fixture(vec![
        ("one.flac", tagged("A", "1")),
        ("two.flac", tagged("A", "2")),
    ]);
    let mut store = fx.store();
    store.fail_read = Some(1);
    let reads = store.reads.clone();
    let result = produce(&fx, JOB, OWNER, store, future::pending::<()>()).await;
    assert!(matches!(
        result,
        Err(ProducerError::Selection(SelectionError::Transport))
    ));
    assert_eq!(reads.borrow().len(), 2);
}

#[tokio::test]
async fn invalid_job_owner_or_digest_refuses_before_any_store_io() {
    let fx = fixture(vec![("one.flac", tagged("A", "1"))]);
    let long = "o".repeat(129);
    for (job, owner) in [
        ("bad/job", OWNER),
        ("", OWNER),
        (JOB, ""),
        (JOB, long.as_str()),
        (JOB, "own\ner"),
    ] {
        let store = fx.store();
        let reads = store.reads.clone();
        let result = produce(&fx, job, owner, store, future::pending::<()>()).await;
        assert!(matches!(result, Err(ProducerError::Identity)), "{job:?}");
        assert!(reads.borrow().is_empty());
    }
    let store = fx.store();
    let reads = store.reads.clone();
    let progress = fx.progress();
    let result = prepare_verified_candidate(
        VerifiedCandidateInput {
            job_id: JOB,
            user_id: OWNER,
            original: &fx.xml,
            expected_hash: &"0".repeat(64),
            progress: &progress,
        },
        store,
        future::pending::<()>(),
    )
    .await;
    assert!(matches!(
        result,
        Err(ProducerError::Selection(SelectionError::Identity))
    ));
    assert!(reads.borrow().is_empty());
}

#[tokio::test]
async fn damaged_progress_or_unsupported_original_refuses_without_io() {
    let fx = fixture(vec![("one.flac", tagged("A", "1"))]);
    let mut damaged = fx.progress();
    damaged.missing_blocks = 1;
    let store = fx.store();
    let reads = store.reads.clone();
    let result = prepare_verified_candidate(
        VerifiedCandidateInput {
            job_id: JOB,
            user_id: OWNER,
            original: &fx.xml,
            expected_hash: &fx.hash,
            progress: &damaged,
        },
        store,
        future::pending::<()>(),
    )
    .await;
    assert!(matches!(
        result,
        Err(ProducerError::Selection(SelectionError::Identity))
    ));
    assert!(reads.borrow().is_empty());

    let mixed = fixture(vec![
        ("one.flac", tagged("A", "1")),
        ("two.mp3", b"mp3".to_vec()),
    ]);
    let store = mixed.store();
    let reads = store.reads.clone();
    let result = produce(&mixed, JOB, OWNER, store, future::pending::<()>()).await;
    assert!(matches!(
        result,
        Err(ProducerError::Selection(SelectionError::Unsupported))
    ));
    assert!(reads.borrow().is_empty());
}

#[tokio::test]
async fn non_ascii_grouping_text_refuses_the_candidate() {
    let fx = fixture(vec![("one.flac", flac(&["ALBUM=Caf\u{e9}"]))]);
    let result = produce(&fx, JOB, OWNER, fx.store(), future::pending::<()>()).await;
    assert!(matches!(
        result,
        Err(ProducerError::Grouping(GroupingError::UnsupportedText))
    ));
}

#[tokio::test]
async fn ready_deadline_refuses_as_transport() {
    let fx = fixture(vec![("one.flac", tagged("A", "1"))]);
    let result = produce(&fx, JOB, OWNER, fx.store(), future::ready(())).await;
    assert!(matches!(
        result,
        Err(ProducerError::Selection(SelectionError::Transport))
    ));
}

#[tokio::test]
async fn malformed_flac_keeps_its_entry_with_empty_hints() {
    let fx = fixture(vec![
        ("one.flac", tagged("A", "1")),
        ("bad.flac", b"fLaC\x00\x00".to_vec()),
    ]);
    let manifest = produce(&fx, JOB, OWNER, fx.store(), future::pending::<()>())
        .await
        .unwrap();
    let entries = manifest.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].audio_ordinal, 2);
    assert_eq!(entries[1].source_selector.nzb_file_ordinal, 1);
    assert!(entries[1].hints == Default::default());
    assert_eq!(
        (entries[1].artist_group_ref, entries[1].release_group_ref),
        (2, 2)
    );
    assert_eq!(manifest.effective_discs(), Some(&[Some(1), Some(1)][..]));
}
