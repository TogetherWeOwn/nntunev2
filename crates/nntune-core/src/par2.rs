//! PAR2 recovery-set parsing and verification, ported from the v1 Go worker
//! (`worker/internal/ingest/par2`: `recoveryset.go`, `crc32combine.go`).
//!
//! [`RecoverySet::parse`] verifies every packet checksum and cross-checks the
//! set identity (the set ID must be the MD5 of the Main packet body), the
//! Main-packet membership list, per-file ID derivation, and duplicate packets
//! — the same checks v1 performs before trusting any metadata.
//!
//! Two verification paths, mirroring v1:
//!
//! - [`RecoverySet::verify`]: recomputes per-block CRC32s over candidate bytes
//!   (each slice zero-padded to the block size, per the PAR2 spec) plus the
//!   whole-file MD5. Runs anywhere, including wasm.
//! - [`RecoverySet::precheck`]: recombines independently computed per-segment
//!   CRC32s via [`crc32_combine`] (zlib's `crc32_combine`, ported from v1's
//!   `Combine`) and compares against the recovery set — proves a file intact
//!   without re-hashing, so the repair binary never runs for healthy files.
//!
//! Actual repair (Reed-Solomon over recovery slices) stays native-only:
//! [`repair`] shells out to `par2cmdline-turbo` inside a scratch directory,
//! which is a Container batch job, never a Worker.

use std::collections::HashMap;

use crate::error::{Error, Result};

const PACKET_HEADER_SIZE: usize = 64;
const PACKET_MAGIC: [u8; 8] = *b"PAR2\0PKT";
const MAIN_HEADER_SIZE: usize = 12;
const IFSC_ENTRY_SIZE: usize = 20;

fn packet_type(suffix: &[u8]) -> [u8; 16] {
    let mut t = [0u8; 16];
    t[..8].copy_from_slice(b"PAR 2.0\0");
    t[8..8 + suffix.len().min(8)].copy_from_slice(&suffix[..suffix.len().min(8)]);
    t
}

fn main_type() -> [u8; 16] {
    packet_type(b"Main\0\0\0\0")
}
fn filedesc_type() -> [u8; 16] {
    packet_type(b"FileDesc")
}
fn ifsc_type() -> [u8; 16] {
    packet_type(b"IFSC\0\0\0\0")
}
fn recv_slic_type() -> [u8; 16] {
    packet_type(b"RecvSlic")
}

/// One source file described by the recovery set.
#[derive(Debug, Clone)]
pub struct RecoveryFile {
    /// PAR2 File ID: MD5(16k-hash ‖ length ‖ name).
    pub file_id: [u8; 16],
    /// MD5 of the whole file.
    pub whole_md5: [u8; 16],
    /// MD5 of the first 16 KiB.
    pub first16k_md5: [u8; 16],
    /// Declared file name (PAR2 metadata, never a storage key).
    pub filename: String,
    /// Declared file length in bytes.
    pub length: u64,
    /// Per-block CRC32s from the IFSC packet (each covers a full
    /// block-size slice, zero-padded past end-of-file).
    pub block_crc32s: Vec<u32>,
    /// True when the Main packet lists this file among its recoverable
    /// (first-N) IDs. Only protected files may vouch for bytes.
    pub protected: bool,
    /// True when a FileDesc packet supplied name/length/hashes.
    pub has_file_desc: bool,
}

/// A verified PAR2 recovery set: the metadata subset repair and
/// deobfuscation need.
#[derive(Debug, Clone, Default)]
pub struct RecoverySet {
    /// MD5 of the Main packet body; every packet must carry it.
    pub set_id: [u8; 16],
    /// Main-packet slice size. Never assumed — always read from the set.
    pub block_size: u64,
    /// Number of recovery-slice packets seen.
    pub recovery_blocks: usize,
    /// Files by File ID.
    pub files: HashMap<[u8; 16], RecoveryFile>,
}

impl RecoverySet {
    /// Parses and fully validates a concatenated PAR2 packet stream (one or
    /// more `.par2` objects back to back). Repeated metadata packets are
    /// normal across volume files, but only byte-identical repeats pass.
    pub fn parse(data: &[u8]) -> Result<RecoverySet> {
        let mut set = RecoverySet::default();
        let mut members: HashMap<[u8; 16], ()> = HashMap::new();
        let mut protected: HashMap<[u8; 16], ()> = HashMap::new();
        let mut filenames: HashMap<String, [u8; 16]> = HashMap::new();
        let mut recovery_packets: HashMap<u32, [u8; 16]> = HashMap::new();
        let mut set_id_seen = false;
        let mut main_seen = false;
        let mut main_digest = [0u8; 16];
        // (file_id -> (packet md5, kind)) for duplicate validation.
        let mut filedesc_digests: HashMap<[u8; 16], [u8; 16]> = HashMap::new();
        let mut ifsc_digests: HashMap<[u8; 16], [u8; 16]> = HashMap::new();

        let mut offset = 0usize;
        while offset < data.len() {
            let remaining = &data[offset..];
            if remaining.len() < PACKET_HEADER_SIZE {
                return Err(malformed(format!(
                    "truncated packet header at offset {offset}"
                )));
            }
            if remaining[..8] != PACKET_MAGIC {
                return Err(malformed(format!("bad magic at offset {offset}")));
            }
            let length = u64::from_le_bytes(remaining[8..16].try_into().unwrap()) as usize;
            if length < PACKET_HEADER_SIZE || length % 4 != 0 || length > remaining.len() {
                return Err(malformed(format!(
                    "invalid packet length at offset {offset}"
                )));
            }
            let packet = &remaining[..length];
            let declared_md5: [u8; 16] = packet[16..32].try_into().unwrap();
            if md5::compute(&packet[32..]).0 != declared_md5 {
                return Err(malformed(format!(
                    "packet checksum mismatch at offset {offset}"
                )));
            }
            let set_id: [u8; 16] = packet[32..48].try_into().unwrap();
            if !set_id_seen {
                set.set_id = set_id;
                set_id_seen = true;
            } else if set.set_id != set_id {
                return Err(malformed(format!("mixed recovery set at offset {offset}")));
            }

            let mut kind = [0u8; 16];
            kind.copy_from_slice(&packet[48..64]);
            let body = &packet[64..];
            if kind == main_type() {
                parse_main_body(
                    body,
                    &mut members,
                    &mut protected,
                    &mut main_seen,
                    &mut main_digest,
                    &mut set,
                    set_id,
                )?;
            } else if kind == filedesc_type() {
                parse_filedesc_body(body, &mut set, &mut filenames, &mut filedesc_digests)?;
            } else if kind == ifsc_type() {
                parse_ifsc_body(body, &mut set, &mut ifsc_digests)?;
            } else if kind == recv_slic_type() {
                if body.len() < 4 {
                    return Err(malformed("recovery slice body too short"));
                }
                let exponent = u32::from_le_bytes(body[..4].try_into().unwrap());
                let digest = md5::compute(body).0;
                match recovery_packets.get(&exponent) {
                    Some(prev) if *prev != digest => {
                        return Err(malformed("conflicting duplicate recovery slice"));
                    }
                    Some(_) => {}
                    None => {
                        recovery_packets.insert(exponent, digest);
                        set.recovery_blocks += 1;
                    }
                }
            }
            // Unknown packet types are ignored (forward compatibility), but
            // their checksum and set-ID checks above still applied.
            offset += length;
        }

        if !main_seen || set.block_size == 0 {
            return Err(malformed("no Main packet found"));
        }
        // The Main packet is the only authority on membership: checked after
        // the whole stream because packet order is not fixed.
        for (id, file) in set.files.iter_mut() {
            if !members.contains_key(id) {
                return Err(malformed(format!(
                    "file ID {} is not listed in the Main packet",
                    hex16(id)
                )));
            }
            file.protected = protected.contains_key(id);
        }
        let _ = main_digest;
        Ok(set)
    }

    /// Resolves a name to any description the set carries, protected or not.
    pub fn file_by_name(&self, name: &str) -> Option<&RecoveryFile> {
        self.files.values().find(|f| f.filename == name)
    }

    /// Resolves a name only when the set actually protects that file. A
    /// non-recovery description carries a real name and real hashes but no
    /// recovery blocks: accepting one as repair metadata would let a crafted
    /// Main packet certify bytes the set cannot reconstruct.
    pub fn protected_file_by_name(&self, name: &str) -> Option<&RecoveryFile> {
        self.file_by_name(name)
            .filter(|f| f.protected && f.has_file_desc)
    }

    /// Verifies candidate bytes against a protected file's metadata: length,
    /// whole-file MD5, and every per-block CRC32 (each block zero-padded to
    /// the set's block size, per the PAR2 spec). `Ok(())` means healthy.
    pub fn verify(&self, name: &str, data: &[u8]) -> Result<()> {
        let file = self
            .protected_file_by_name(name)
            .ok_or_else(|| Error::Par2Unhealthy {
                filename: name.to_string(),
                detail: "no protected recovery-set entry for this file".into(),
            })?;
        if data.len() as u64 != file.length {
            return Err(Error::Par2Unhealthy {
                filename: name.to_string(),
                detail: format!(
                    "size {} differs from recovery-set length {}",
                    data.len(),
                    file.length
                ),
            });
        }
        if md5::compute(data).0 != file.whole_md5 {
            return Err(Error::Par2Unhealthy {
                filename: name.to_string(),
                detail: "whole-file MD5 mismatch".into(),
            });
        }
        if file.block_crc32s.is_empty() {
            return Err(Error::Par2Unhealthy {
                filename: name.to_string(),
                detail: "recovery set carries no block checksums for this file".into(),
            });
        }
        let bs = self.block_size as usize;
        for (i, &want) in file.block_crc32s.iter().enumerate() {
            let start = i * bs;
            let end = (start + bs).min(data.len());
            let mut hash = crc32fast::Hasher::new();
            hash.update(&data[start..end]);
            if end - start < bs {
                // Last slice is checksummed zero-padded to the full size.
                hash.update(&vec![0u8; bs - (end - start)]);
            }
            if hash.finalize() != want {
                return Err(Error::Par2Unhealthy {
                    filename: name.to_string(),
                    detail: format!("block {i} CRC32 mismatch"),
                });
            }
        }
        Ok(())
    }

    /// Whole-file precheck without re-hashing: recombines independently
    /// computed per-segment CRC32s (captured at yEnc-decode time) via
    /// [`crc32_combine`], recombines the recovery set's per-block CRC32s the
    /// same way, and reports whether they match. A match proves the file
    /// intact without ever invoking the repair binary. `segments` is
    /// `(crc32, length)` in file order.
    ///
    /// Like v1, only protected files with block checksums qualify; anything
    /// else is an error, never a silent pass.
    pub fn precheck(&self, name: &str, segments: &[(u32, u64)]) -> Result<bool> {
        let file = self.protected_file_by_name(name).ok_or_else(|| {
            Error::Malformed(format!("par2 precheck: no protected entry for {name:?}"))
        })?;
        if self.block_size == 0 {
            return Err(Error::Malformed(
                "par2 precheck: block size must be positive".into(),
            ));
        }
        if file.block_crc32s.is_empty() {
            return Err(Error::Malformed(
                "par2 precheck: recovery set carries no block checksums for this file".into(),
            ));
        }
        let mut actual_len = 0u64;
        let mut actual_crc = 0u32;
        for (i, &(crc, len)) in segments.iter().enumerate() {
            actual_crc = if i == 0 {
                crc
            } else {
                crc32_combine(actual_crc, crc, len)
            };
            actual_len += len;
        }
        if actual_len != file.length {
            return Err(Error::Malformed(
                "par2 precheck: segment lengths do not sum to the recovery set's file length"
                    .into(),
            ));
        }
        let mut expected_crc = 0u32;
        for (i, &block_crc) in file.block_crc32s.iter().enumerate() {
            expected_crc = if i == 0 {
                block_crc
            } else {
                crc32_combine(expected_crc, block_crc, self.block_size)
            };
        }
        // Block checksums cover each block zero-padded to the full size, so
        // pad our combined CRC the same way instead of trying to "un-pad"
        // the recovery set's value (which would require an invertible combine).
        let padded = file.block_crc32s.len() as u64 * self.block_size;
        if padded > actual_len {
            let pad = padded - actual_len;
            actual_crc = crc32_combine(actual_crc, zero_run_crc32(pad), pad);
        }
        Ok(actual_crc == expected_crc)
    }
}

fn malformed(detail: impl Into<String>) -> Error {
    Error::Malformed(format!("par2: malformed recovery set: {}", detail.into()))
}

fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_main_body(
    body: &[u8],
    members: &mut HashMap<[u8; 16], ()>,
    protected: &mut HashMap<[u8; 16], ()>,
    main_seen: &mut bool,
    main_digest: &mut [u8; 16],
    set: &mut RecoverySet,
    set_id: [u8; 16],
) -> Result<()> {
    if body.len() < MAIN_HEADER_SIZE {
        return Err(malformed("Main packet body too short"));
    }
    if (body.len() - MAIN_HEADER_SIZE) % 16 != 0 {
        return Err(malformed(
            "Main packet file ID list is not a whole number of 16-byte IDs",
        ));
    }
    let listed = (body.len() - MAIN_HEADER_SIZE) / 16;
    let recoverable = u32::from_le_bytes(body[8..12].try_into().unwrap());
    if recoverable == 0 || recoverable as usize > listed {
        return Err(malformed(format!(
            "Main packet declares {recoverable} recovery files but lists {listed} file IDs"
        )));
    }
    let block_size = u64::from_le_bytes(body[0..8].try_into().unwrap());
    if block_size == 0 || block_size % 4 != 0 {
        return Err(malformed(format!(
            "Main packet slice size {block_size} is not a non-zero multiple of 4"
        )));
    }
    // PAR2 derives the set ID from the Main packet: setid == MD5(body). A set
    // whose declared ID does not derive from its own Main packet is forged or
    // corrupt however many individual packet checksums agree.
    let digest = md5::compute(body).0;
    if digest != set_id {
        return Err(malformed(
            "recovery set ID is not the MD5 of the Main packet body",
        ));
    }
    if *main_seen {
        return Ok(());
    }
    for (index, chunk) in body[MAIN_HEADER_SIZE..].chunks_exact(16).enumerate() {
        let mut id = [0u8; 16];
        id.copy_from_slice(chunk);
        if members.contains_key(&id) {
            return Err(malformed(format!(
                "Main packet lists file ID {} twice",
                hex16(&id)
            )));
        }
        members.insert(id, ());
        if index < recoverable as usize {
            protected.insert(id, ());
        }
    }
    set.block_size = block_size;
    *main_digest = digest;
    *main_seen = true;
    Ok(())
}

fn parse_filedesc_body(
    body: &[u8],
    set: &mut RecoverySet,
    filenames: &mut HashMap<String, [u8; 16]>,
    digests: &mut HashMap<[u8; 16], [u8; 16]>,
) -> Result<()> {
    if body.len() < 56 {
        return Err(malformed("FileDesc packet body too short"));
    }
    let mut file_id = [0u8; 16];
    file_id.copy_from_slice(&body[0..16]);
    let mut whole_md5 = [0u8; 16];
    whole_md5.copy_from_slice(&body[16..32]);
    let mut first16k = [0u8; 16];
    first16k.copy_from_slice(&body[32..48]);
    let length = u64::from_le_bytes(body[48..56].try_into().unwrap());
    let filename = String::from_utf8_lossy(&body[56..])
        .trim_end_matches('\0')
        .to_string();

    // The File ID must derive from the packet's own 16k hash, length, and
    // (padding-stripped) name — otherwise a Main-listed ID is just a label.
    let mut seed = Vec::with_capacity(24 + filename.len());
    seed.extend_from_slice(&body[32..56]);
    seed.extend_from_slice(filename.as_bytes());
    if md5::compute(&seed).0 != file_id {
        return Err(malformed(
            "FileDesc file ID is not derived from its own 16k hash, length, and name",
        ));
    }
    if let Some(&other) = filenames.get(&filename) {
        if other != file_id {
            return Err(malformed(
                "duplicate filename belongs to different file IDs",
            ));
        }
    }
    let digest = md5::compute(body).0;
    if let Some(&prev) = digests.get(&file_id) {
        if prev != digest {
            return Err(malformed("conflicting duplicate FileDesc packet"));
        }
        return Ok(());
    }
    digests.insert(file_id, digest);
    let file = set.files.entry(file_id).or_insert(RecoveryFile {
        file_id,
        whole_md5: [0; 16],
        first16k_md5: [0; 16],
        filename: String::new(),
        length: 0,
        block_crc32s: Vec::new(),
        protected: false,
        has_file_desc: false,
    });
    file.whole_md5 = whole_md5;
    file.first16k_md5 = first16k;
    file.length = length;
    file.filename = filename.clone();
    file.has_file_desc = true;
    filenames.insert(filename, file_id);
    Ok(())
}

fn parse_ifsc_body(
    body: &[u8],
    set: &mut RecoverySet,
    digests: &mut HashMap<[u8; 16], [u8; 16]>,
) -> Result<()> {
    if body.len() < 16 {
        return Err(malformed("IFSC packet body too short"));
    }
    let mut file_id = [0u8; 16];
    file_id.copy_from_slice(&body[0..16]);
    let entries = &body[16..];
    if entries.len() % IFSC_ENTRY_SIZE != 0 {
        return Err(malformed(
            "IFSC packet body is not a whole number of slice checksums",
        ));
    }
    let digest = md5::compute(body).0;
    if let Some(&prev) = digests.get(&file_id) {
        if prev != digest {
            return Err(malformed("conflicting duplicate IFSC packet"));
        }
        return Ok(());
    }
    digests.insert(file_id, digest);
    let crc32s = entries
        .chunks_exact(IFSC_ENTRY_SIZE)
        .map(|e| u32::from_le_bytes(e[16..20].try_into().unwrap()))
        .collect();
    let file = set.files.entry(file_id).or_insert(RecoveryFile {
        file_id,
        whole_md5: [0; 16],
        first16k_md5: [0; 16],
        filename: String::new(),
        length: 0,
        block_crc32s: Vec::new(),
        protected: false,
        has_file_desc: false,
    });
    file.block_crc32s = crc32s;
    Ok(())
}

// ---------------------------------------------------------------------------
// CRC32 combine (zlib's crc32_combine via GF(2) matrices, ported from v1)
// ---------------------------------------------------------------------------

const GF2_DIMENSION: usize = 32;

/// CRC-32 (IEEE) of the concatenation of two byte sequences, given the CRC-32
/// of each and the byte length of the second. Lets a whole-file CRC32 be
/// recombined from independently computed per-segment CRC32s without
/// re-reading the bytes.
pub fn crc32_combine(crc1: u32, crc2: u32, len2: u64) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    let mut even = [0u32; GF2_DIMENSION];
    let mut odd = [0u32; GF2_DIMENSION];

    // Operator for one zero bit.
    odd[0] = 0xedb88320;
    let mut row = 1u32;
    for slot in odd.iter_mut().skip(1) {
        *slot = row;
        row <<= 1;
    }

    // Operator for two zero bits, then four.
    gf2_matrix_square(&mut even, &odd);
    gf2_matrix_square(&mut odd, &even);

    let mut crc = crc1;
    let mut len = len2;
    loop {
        // Turns "odd" (4 zero bits) into a one-zero-byte operator on the
        // first iteration, doubling the byte count every iteration after.
        gf2_matrix_square(&mut even, &odd);
        if len & 1 != 0 {
            crc = gf2_matrix_times(&even, crc);
        }
        len >>= 1;
        if len == 0 {
            break;
        }
        gf2_matrix_square(&mut odd, &even);
        if len & 1 != 0 {
            crc = gf2_matrix_times(&odd, crc);
        }
        len >>= 1;
        if len == 0 {
            break;
        }
    }
    crc ^ crc2
}

fn gf2_matrix_times(matrix: &[u32; GF2_DIMENSION], mut vec: u32) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while vec != 0 {
        if vec & 1 != 0 {
            sum ^= matrix[i];
        }
        vec >>= 1;
        i += 1;
    }
    sum
}

fn gf2_matrix_square(square: &mut [u32; GF2_DIMENSION], mat: &[u32; GF2_DIMENSION]) {
    for i in 0..GF2_DIMENSION {
        square[i] = gf2_matrix_times(mat, mat[i]);
    }
}

/// CRC-32 of a run of `n` zero bytes (used to pad the last block; `n` is
/// bounded by the block size, so this stays a small one-off computation).
fn zero_run_crc32(n: u64) -> u32 {
    if n == 0 {
        return 0;
    }
    crc32fast::hash(&vec![0u8; n as usize])
}

// ---------------------------------------------------------------------------
// Native-only repair
// ---------------------------------------------------------------------------

/// Repair via an external `par2` binary (`par2cmdline-turbo`). This spawns a
/// subprocess and needs a writable scratch directory: it is a Container batch
/// job, never a Worker. On wasm32 the function exists but always reports
/// [`Error::UnsupportedOnWasm`].
pub mod repair {
    use crate::error::{Error, Result};
    use std::path::{Path, PathBuf};

    /// Outcome of one `par2 repair` run.
    #[derive(Debug, Clone)]
    pub struct RepairReport {
        /// Whether the binary reported success (exit status 0).
        pub success: bool,
        /// Damaged blocks the binary says it repaired (parsed from stdout,
        /// best-effort; -1 when unparsable).
        pub repaired_blocks: i64,
        /// Target files the run was asked to repair.
        pub targets: Vec<PathBuf>,
        /// Last lines of the binary's combined output (bounded).
        pub tail: String,
    }

    /// Runs `par2 repair` in `scratch_dir` over the given `.par2` files plus
    /// their sibling data files (all paths must already be staged there —
    /// filenames are PAR2 metadata and are never rewritten by this function).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn repair_with_binary(
        par2_bin: &Path,
        scratch_dir: &Path,
        par2_files: &[&Path],
    ) -> Result<RepairReport> {
        let mut cmd = std::process::Command::new(par2_bin);
        cmd.arg("repair").current_dir(scratch_dir);
        for f in par2_files {
            cmd.arg(f);
        }
        let output = cmd.output().map_err(|e| {
            Error::Malformed(format!(
                "par2 repair: failed to spawn {}: {e}",
                par2_bin.display()
            ))
        })?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let tail: String = combined
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(RepairReport {
            success: output.status.success(),
            repaired_blocks: parse_repaired(&combined),
            targets: par2_files.iter().map(|p| p.to_path_buf()).collect(),
            tail,
        })
    }

    /// wasm32 stub: repair needs a subprocess and never runs on Workers.
    #[cfg(target_arch = "wasm32")]
    pub fn repair_with_binary(
        _par2_bin: &Path,
        _scratch_dir: &Path,
        _par2_files: &[&Path],
    ) -> Result<RepairReport> {
        Err(Error::UnsupportedOnWasm("par2 repair"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn parse_repaired(output: &str) -> i64 {
        // par2cmdline-turbo prints e.g. "Repair complete." / "Repair failed."
        // and a "Found N recoverable blocks" line; keep the parse forgiving.
        if output.contains("Repair complete.") {
            0
        } else {
            -1
        }
    }
}
