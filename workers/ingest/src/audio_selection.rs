//! Digest-bound direct-FLAC selections and bounded tag-prefix extraction.
//!
//! Unmounted producer component, not a manifest freeze or catalog writer. Other
//! audio formats and archive expansion are explicitly refused, never silently
//! omitted. Runtime adapters must read only verified/repaired scratch selections,
//! enforce the requested range and an I/O deadline, and cancel on stream drop.

use futures_util::{stream::LocalBoxStream, StreamExt};

use crate::{
    audio_manifest::{AudioHints, AudioSelector},
    audio_tags::{read_flac_hints, MAX_TAG_PREFIX_BYTES},
    dispatch::content_sha256_hex,
    nzb_intake::{estimate_nzb, MAX_NZB_BYTES},
    parse::parse_upload_files,
};

pub const MAX_JOB_PREFIX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PREFIX_CHUNKS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionError {
    Identity,
    Invalid,
    Unsupported,
    NoAudio,
    Bounds,
    Transport,
}

/// No Debug: even a private selector is not public job-status information.
pub struct DirectAudioSelection {
    original_content_sha256: String,
    audio_ordinal: u32,
    selector: AudioSelector,
}

impl DirectAudioSelection {
    pub fn original_content_sha256(&self) -> &str {
        &self.original_content_sha256
    }

    pub fn audio_ordinal(&self) -> u32 {
        self.audio_ordinal
    }

    pub fn selector(&self) -> &AudioSelector {
        &self.selector
    }
}

pub struct AudioSelectionPlan {
    original_content_sha256: String,
    selections: Vec<DirectAudioSelection>,
}

impl AudioSelectionPlan {
    /// The expected digest comes from the accepted job/original, not the body.
    /// Recheck even when Parsing checked an earlier read of the same KV key.
    pub fn from_original(body: &str, expected_hash: &str) -> Result<Self, SelectionError> {
        if body.len() > MAX_NZB_BYTES {
            return Err(SelectionError::Bounds);
        }
        if content_sha256_hex(body.as_bytes()) != expected_hash {
            return Err(SelectionError::Identity);
        }
        let estimate = estimate_nzb(body).map_err(|_| SelectionError::Invalid)?;
        // Preserve intake's count and message-id checks before using the shared
        // parser's richer role/container projection. Never zip MB recording ids.
        parse_upload_files(body.as_bytes(), &estimate).map_err(|_| SelectionError::Invalid)?;
        let document =
            nntune_core::nzb::parse(body.as_bytes()).map_err(|_| SelectionError::Invalid)?;
        let mut selections = Vec::new();
        for (index, file) in document.files.iter().enumerate() {
            if file.ordinal != index {
                return Err(SelectionError::Invalid);
            }
            match (file.role.as_str(), file.container.as_str()) {
                ("content", "flac") => selections.push(DirectAudioSelection {
                    original_content_sha256: expected_hash.to_owned(),
                    audio_ordinal: (selections.len() + 1) as u32,
                    selector: AudioSelector {
                        nzb_file_ordinal: index,
                        archive_member: None,
                        audio_selection_index: 0,
                    },
                }),
                ("par2" | "nfo", _) => {}
                // Unknown files might be unsupported audio or archives. Refuse
                // the plan rather than freezing an accidentally partial album.
                _ => return Err(SelectionError::Unsupported),
            }
        }
        if selections.is_empty() {
            return Err(SelectionError::NoAudio);
        }
        Ok(Self {
            original_content_sha256: expected_hash.to_owned(),
            selections,
        })
    }

    pub fn selections(&self) -> &[DirectAudioSelection] {
        &self.selections
    }

    pub fn original_content_sha256(&self) -> &str {
        &self.original_content_sha256
    }
}

/// This is an internal capability, not a URL supplied by a client. The adapter
/// resolves the selection through the same digest-bound original and verified
/// round, validates scratch metadata and requests at most `cap` bytes. Oversize
/// chunks are refused here before copying. A full prefix drops/cancels the body;
/// no extra poll is spent to discover EOF beyond the allowed range.
pub trait PrefixReader {
    fn prefix<'a>(
        &'a mut self,
        selection: &'a DirectAudioSelection,
        cap: usize,
    ) -> LocalBoxStream<'a, Result<Vec<u8>, ()>>;
}

pub struct ExtractedAudio {
    pub audio_ordinal: u32,
    pub selector: AudioSelector,
    pub hints: AudioHints,
}

/// Preserve every selection even if its metadata is malformed, incomplete or
/// beyond the job budget. Those cases get no hints/art, not a new audio ordinal.
/// A transport or adapter-bound violation refuses the entire candidate, so an
/// I/O outage cannot be frozen as a permanent no-tags result. No picture bytes
/// or claimed fingerprint are accepted by this component.
pub async fn extract_hints(
    plan: &AudioSelectionPlan,
    reader: &mut impl PrefixReader,
) -> Result<Vec<ExtractedAudio>, SelectionError> {
    let mut remaining = MAX_JOB_PREFIX_BYTES;
    let mut entries = Vec::with_capacity(plan.selections.len());
    for selection in &plan.selections {
        let cap = MAX_TAG_PREFIX_BYTES.min(remaining);
        let hints = if cap == 0 {
            AudioHints::default()
        } else {
            let mut stream = reader.prefix(selection, cap);
            let mut prefix = Vec::new();
            let mut chunks = 0;
            while prefix.len() < cap {
                let Some(chunk) = stream.next().await else {
                    break;
                };
                let chunk = chunk.map_err(|_| SelectionError::Transport)?;
                chunks += 1;
                if chunks > MAX_PREFIX_CHUNKS || chunk.len() > cap - prefix.len() {
                    return Err(SelectionError::Bounds);
                }
                prefix.extend_from_slice(&chunk);
            }
            remaining -= prefix.len();
            read_flac_hints(&prefix).unwrap_or_default()
        };
        entries.push(ExtractedAudio {
            audio_ordinal: selection.audio_ordinal,
            selector: selection.selector.clone(),
            hints,
        });
    }
    Ok(entries)
}
