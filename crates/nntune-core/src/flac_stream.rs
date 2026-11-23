//! FLAC stream and optional STREAMINFO PCM checksum checks. This does not
//! establish authenticated transport EOF or produce a publication receipt.

use crate::flac_continuity::FrameContinuity;
use crate::flac_frame::FrameReader;
use crate::flac_preamble::FlacPreamble;
use crate::flac_prepare::FlacSeekIndex;

#[derive(Default)]
pub(crate) struct FlacStream {
    preamble: FlacPreamble,
    continuity: Option<FrameContinuity>,
    header: [u8; 16],
    filled: usize,
    frame: Option<FrameReader>,
    pcm_md5: Option<md5::Context>,
    pcm_scratch: Option<Vec<i32>>,
    collect_seek: bool,
    bytes_seen: u64,
    seek: Option<FlacSeekIndex>,
    failed: bool,
}

impl FlacStream {
    pub(crate) fn new(collect_seek: bool) -> Self {
        Self {
            collect_seek,
            ..Self::default()
        }
    }

    pub(crate) fn push(&mut self, input: &[u8]) -> Result<(), ()> {
        if self.failed {
            return Err(());
        }
        self.consume(input).inspect_err(|()| self.failed = true)
    }

    fn consume(&mut self, mut input: &[u8]) -> Result<(), ()> {
        if self.collect_seek {
            self.bytes_seen = self.bytes_seen.checked_add(input.len() as u64).ok_or(())?;
        }
        if self.continuity.is_none() {
            let Some((info, audio)) = self.preamble.push(input)? else {
                return Ok(());
            };
            input = audio;
            if self.collect_seek {
                let audio_start = self.bytes_seen - audio.len() as u64;
                self.seek = Some(FlacSeekIndex::new(info.sample_rate, audio_start));
            }
            self.continuity = Some(FrameContinuity::new(&info));
            // RFC 9639: an all-zero checksum means it is not provided.
            if info.pcm_md5 != [0; 16] {
                self.pcm_md5 = Some(md5::Context::new());
                self.pcm_scratch = Some(Vec::new());
            }
        }
        let continuity = self.continuity.as_mut().ok_or(())?;
        while let Some((&byte, rest)) = input.split_first() {
            if let Some(frame) = &mut self.frame {
                if !frame.read(&mut input)? {
                    return Ok(());
                }
                let (header, pcm) = continuity.accept_with_pcm(self.frame.take().ok_or(())?)?;
                if let Some(md5) = &mut self.pcm_md5 {
                    let pcm = pcm.ok_or(())?;
                    hash_pcm(md5, header.bits_per_sample, &pcm);
                    self.pcm_scratch = Some(pcm);
                }
                if let Some(seek) = &mut self.seek {
                    let frame_end = self.bytes_seen - input.len() as u64;
                    seek.record_frame(header.block_size, frame_end)?;
                }
            } else {
                // Feed only header bytes into scratch. The body always starts
                // in the original input, even when its header crossed a split.
                *self.header.get_mut(self.filled).ok_or(())? = byte;
                self.filled += 1;
                input = rest;
                let mut header = &self.header[..self.filled];
                if let Some(frame) = FrameReader::start_with_pcm(
                    &mut header,
                    continuity.stream_info(),
                    &mut self.pcm_scratch,
                )? {
                    if !header.is_empty() {
                        return Err(());
                    }
                    self.frame = Some(frame);
                    self.filled = 0;
                } else if self.filled == self.header.len() {
                    return Err(());
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn finish_at_eof(self) -> Result<(), ()> {
        self.finish_with_index().map(|_| ())
    }

    /// Call only once the outer reader has confirmed EOF. Partial metadata,
    /// headers, subframes and footers cannot count as a finished stream.
    pub(crate) fn finish_with_index(self) -> Result<Option<FlacSeekIndex>, ()> {
        if self.failed || self.filled != 0 || self.frame.is_some() {
            return Err(());
        }
        let continuity = self.continuity.ok_or(())?;
        let expected = continuity.stream_info().pcm_md5;
        continuity.finish_at_frame_boundary()?;
        if self.pcm_md5.map_or([0; 16], |md5| md5.finalize().0) != expected {
            return Err(());
        }
        Ok(self.seek)
    }
}

fn hash_pcm(md5: &mut md5::Context, depth: u8, pcm: &[i32]) {
    // Signed little-endian, interleaved by sample, sign-extended to a whole byte.
    // MD5 is FLAC's format checksum only, not an authentication primitive.
    let width = usize::from(depth.div_ceil(8));
    let mut buf = [0; 1024];
    for chunk in pcm.chunks(256) {
        let bytes = &mut buf[..chunk.len() * width];
        for (&sample, dest) in chunk.iter().zip(bytes.chunks_exact_mut(width)) {
            dest.copy_from_slice(&sample.to_le_bytes()[..width]);
        }
        md5.consume(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{collections::HashSet, fs, path::Path};

    // Synthetic 16-sample mono constant frame followed by a three-sample tail.
    const FIRST: &[u8] = &[255, 248, 105, 2, 0, 15, 183, 0, 42, 122, 221];
    const TAIL: &[u8] = &[255, 248, 105, 2, 1, 2, 129, 0, 42, 157, 29];

    fn metadata(total: u64) -> Vec<u8> {
        let mut bytes = b"fLaC\0\0\0\x22\0\x10\0\x10\0\0\0\0\0\0".to_vec();
        bytes.extend_from_slice(&((44_100_u64 << 44) | (7 << 36) | total).to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&[0x81, 0, 0, 2, 0, 0]);
        bytes
    }

    #[test]
    fn accepts_complete_flac_at_every_stream_split() {
        let wire = [metadata(19).as_slice(), FIRST, TAIL].concat();
        for split in 0..=wire.len() {
            let mut stream = FlacStream::default();
            stream.push(&wire[..split]).unwrap();
            stream.push(&[]).unwrap();
            stream.push(&wire[split..]).unwrap();
            assert_eq!(stream.finish_at_eof(), Ok(()), "split {split}");
        }
        let mut stream = FlacStream::default();
        for byte in wire.chunks(1) {
            stream.push(byte).unwrap();
        }
        assert_eq!(stream.finish_at_eof(), Ok(()));
    }

    fn admin_upload_clips() -> Vec<Vec<u8>> {
        #[derive(serde::Deserialize)]
        struct Manifest {
            tracks: Vec<Track>,
        }

        #[derive(serde::Deserialize)]
        struct Track {
            file: String,
            bytes: usize,
            sha256: String,
        }

        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic-flac/admin-upload");
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.tracks.len(), 10);
        let mut digests = HashSet::new();
        manifest
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| {
                assert_eq!(track.file, format!("fixture-{}.flac", index + 1));
                let clip = fs::read(directory.join(&track.file)).unwrap();
                assert_eq!(clip.len(), track.bytes, "{}", track.file);
                let digest: String = Sha256::digest(&clip)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                assert_eq!(digest, track.sha256, "{}", track.file);
                assert!(digests.insert(digest), "duplicate fixture {}", track.file);
                clip
            })
            .collect()
    }

    #[test]
    fn accepts_each_admin_upload_clip_as_a_complete_flac_stream() {
        for (index, clip) in admin_upload_clips().iter().enumerate() {
            let mut stream = FlacStream::default();
            assert_eq!(
                stream.push(clip).and_then(|()| stream.finish_at_eof()),
                Ok(()),
                "fixture-{}.flac",
                index + 1
            );
        }
    }

    #[test]
    fn refuses_two_admin_upload_clips_concatenated_as_one_flac_stream() {
        let clips = admin_upload_clips();
        let mut stream = FlacStream::default();
        stream.push(&clips[0]).unwrap();
        assert_eq!(
            stream.push(&clips[1]).and_then(|()| stream.finish_at_eof()),
            Err(())
        );
    }

    #[test]
    fn rejects_truncation_and_pending_bytes_at_eof() {
        let wire = [metadata(19).as_slice(), FIRST, TAIL].concat();
        for end in 0..wire.len() {
            let mut stream = FlacStream::default();
            stream.push(&wire[..end]).unwrap();
            assert_eq!(stream.finish_at_eof(), Err(()), "prefix {end}");
        }
        let unknown_total = [metadata(0).as_slice(), FIRST].concat();
        let mut stream = FlacStream::default();
        stream.push(&unknown_total).unwrap();
        assert_eq!(stream.finish_at_eof(), Ok(()));
        for extra in [b"\0".as_slice(), &FIRST[..7]] {
            let mut stream = FlacStream::default();
            stream.push(&unknown_total).unwrap();
            stream.push(extra).unwrap();
            assert_eq!(stream.finish_at_eof(), Err(()));
        }
    }

    #[test]
    fn poisons_stream_on_bad_footer_or_duplicate_frame() {
        let mut bad_footer = TAIL.to_vec();
        *bad_footer.last_mut().unwrap() ^= 1;
        for invalid in [bad_footer.as_slice(), FIRST] {
            let mut stream = FlacStream::default();
            stream
                .push(&[metadata(0).as_slice(), FIRST].concat())
                .unwrap();
            assert_eq!(stream.push(invalid), Err(()));
            assert_eq!(stream.push(TAIL), Err(()));
            assert_eq!(stream.finish_at_eof(), Err(()));
        }
    }

    #[test]
    fn verifies_signed_interleaved_pcm_at_every_input_split() {
        // Independent 12-bit stereo: (1451, -1484), 192 times. Coupled
        // 4-bit left/side: (-8, 7), (7, -8), alternating for 16 samples.
        // Coupled 32-bit mid/side: (i32::MIN, i32::MAX), 16 times.
        let cases: &[(u8, u16, u128, &[u8])] = &[
            (
                12,
                192,
                0x29b95229c41b5562535dcb8c20cbb768,
                &[
                    0xff, 0xf8, 0x19, 0x14, 0, 0x11, 0, 0x5a, 0xb0, 0x0a, 0x34, 0x56, 0x21,
                ],
            ),
            (
                4,
                16,
                0x90d97e0a2645e00784876540a08e5c6f,
                &[
                    0xff, 0xf8, 0x69, 0x80, 0, 0x0f, 0x6a, 0x02, 0x87, 0x87, 0x87, 0x87, 0x87,
                    0x87, 0x87, 0x87, 0x02, 0x8b, 0xe2, 0xf8, 0xbe, 0x2f, 0x8b, 0xe2, 0xf8, 0xbe,
                    0x2f, 0x11, 0x44,
                ],
            ),
            (
                32,
                16,
                0xedc0e3d9615e142c54a0c0e89c6b557f,
                &[
                    0xff, 0xf8, 0x69, 0xa0, 0, 0x0f, 0x29, 0, 0xff, 0xff, 0xff, 0xff, 0, 0x80, 0,
                    0, 0, 0x80, 0x35, 0x36,
                ],
            ),
        ];
        for &(depth, block, md5, frame) in cases {
            let mut prefix = metadata(u64::from(block));
            prefix[8..10].copy_from_slice(&block.to_be_bytes());
            prefix[10..12].copy_from_slice(&block.to_be_bytes());
            let format =
                (44_100_u64 << 44) | (1 << 41) | (u64::from(depth - 1) << 36) | u64::from(block);
            prefix[18..26].copy_from_slice(&format.to_be_bytes());
            prefix[26..42].copy_from_slice(&md5.to_be_bytes());
            let wire = [prefix.as_slice(), frame].concat();
            for split in 0..=wire.len() {
                let mut stream = FlacStream::default();
                stream.push(&wire[..split]).unwrap();
                stream.push(&wire[split..]).unwrap();
                assert_eq!(stream.finish_at_eof(), Ok(()), "{depth}-bit split {split}");
            }
        }
    }

    #[test]
    fn rejects_wrong_checksum_after_valid_frames() {
        let mut prefix = metadata(19);
        prefix[26..42].copy_from_slice(&0x46d2be275ed2dd86ab2b5388f7346d15_u128.to_be_bytes());
        for matches in [true, false] {
            let mut stream = FlacStream::default();
            stream.push(&prefix).unwrap();
            stream.push(FIRST).unwrap();
            stream.push(TAIL).unwrap();
            assert_eq!(stream.finish_at_eof().is_ok(), matches);
            prefix[26] ^= 1;
        }
    }
}
