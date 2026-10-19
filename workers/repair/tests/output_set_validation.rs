//! Acceptance tests: a `Repaired` result must carry exactly one
//! output for every Data file in the manifest — unique file names,
//! unique canonical object keys, no extra or parity outputs — and
//! the whole set is validated before the runtime copies any output
//! or writes the durable result marker. Anything else is the
//! retryable 502 `container_unavailable` fault, never a recorded
//! success. Synthetic manifests/results only; no R2 or Container.

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair::contract::{
    validate_repaired_output_set, FileRole, OutputFile, RepairResult, RepairStatus, ScratchFile,
    ScratchManifest,
};

fn data_file(file_name: &str, size: u64) -> ScratchFile {
    ScratchFile {
        file_name: file_name.to_string(),
        key: object_key_for_file_name(file_name),
        size,
        role: FileRole::Data,
    }
}

/// Two Data files plus the parity set, as `validate_manifest`
/// requires at least one Par2 file.
fn two_data_manifest() -> ScratchManifest {
    ScratchManifest {
        staging_id: "staging-0000000000000001".to_string(),
        files: vec![
            data_file("track01.flac", 7),
            data_file("track02.flac", 9),
            ScratchFile {
                file_name: "set.par2".to_string(),
                key: object_key_for_file_name("set.par2"),
                size: 8,
                role: FileRole::Par2,
            },
        ],
    }
}

fn output(file_name: &str, size: u64) -> OutputFile {
    OutputFile {
        file_name: file_name.to_string(),
        key: object_key_for_file_name(file_name),
        size,
    }
}

fn repaired(files: Vec<OutputFile>) -> RepairResult {
    RepairResult {
        status: RepairStatus::Repaired,
        exit_code: Some(0),
        files,
        manifest_digest: String::new(),
    }
}

fn assert_rejected_502(manifest: &ScratchManifest, result: &RepairResult, context: &str) {
    match validate_repaired_output_set(manifest, result) {
        Err(err) => assert_eq!(
            (err.status, err.code),
            (502, "container_unavailable"),
            "{context}: rejection must keep the retryable 502 contract"
        ),
        Ok(()) => panic!("{context}: invalid output set was accepted"),
    }
}

#[test]
fn complete_result_covering_every_data_file_is_accepted() {
    let manifest = two_data_manifest();
    // Order does not matter; the set must match.
    let result = repaired(vec![output("track02.flac", 9), output("track01.flac", 7)]);
    validate_repaired_output_set(&manifest, &result).unwrap();
}

#[test]
fn empty_output_set_for_a_two_data_file_manifest_is_rejected() {
    let manifest = two_data_manifest();
    assert_rejected_502(&manifest, &repaired(vec![]), "empty output set");
}

#[test]
fn result_missing_one_data_file_is_rejected() {
    let manifest = two_data_manifest();
    let result = repaired(vec![output("track01.flac", 7)]);
    assert_rejected_502(&manifest, &result, "track02.flac has no output");
}

#[test]
fn repeated_single_file_does_not_stand_in_for_the_missing_one() {
    // Two outputs for two Data files, but both name the same file:
    // a bare length check would wrongly accept this.
    let manifest = two_data_manifest();
    let result = repaired(vec![output("track01.flac", 7), output("track01.flac", 7)]);
    assert_rejected_502(&manifest, &result, "duplicate track01.flac");
}

#[test]
fn duplicate_output_alongside_the_complete_set_is_rejected() {
    let manifest = two_data_manifest();
    let result = repaired(vec![
        output("track01.flac", 7),
        output("track02.flac", 9),
        output("track01.flac", 7),
    ]);
    assert_rejected_502(&manifest, &result, "complete set plus a duplicate");
}

#[test]
fn parity_file_listed_as_an_output_is_rejected() {
    let manifest = two_data_manifest();
    let result = repaired(vec![
        output("track01.flac", 7),
        output("track02.flac", 9),
        output("set.par2", 8),
    ]);
    assert_rejected_502(&manifest, &result, "par2 file is not a Data output");
}

#[test]
fn unknown_extra_output_is_rejected() {
    let manifest = two_data_manifest();
    let result = repaired(vec![
        output("track01.flac", 7),
        output("track02.flac", 9),
        output("extra.flac", 3),
    ]);
    assert_rejected_502(&manifest, &result, "output absent from the manifest");
}

#[test]
fn output_with_noncanonical_key_is_rejected() {
    let manifest = two_data_manifest();
    let forged = OutputFile {
        file_name: "track02.flac".to_string(),
        // Key belonging to the other Data file: not the canonical
        // key for this file name, and not unique across the set.
        key: object_key_for_file_name("track01.flac"),
        size: 9,
    };
    let result = repaired(vec![output("track01.flac", 7), forged]);
    assert_rejected_502(&manifest, &result, "key is not canonical for its file name");
}

#[test]
fn zero_length_data_file_is_still_a_required_output() {
    // A size-0 Data entry is a missing target the Container
    // recreates; a `Repaired` result omitting it is incomplete.
    let manifest = ScratchManifest {
        files: vec![
            data_file("track01.flac", 0),
            data_file("track02.flac", 9),
            ScratchFile {
                file_name: "set.par2".to_string(),
                key: object_key_for_file_name("set.par2"),
                size: 8,
                role: FileRole::Par2,
            },
        ],
        ..two_data_manifest()
    };
    let result = repaired(vec![output("track02.flac", 9)]);
    assert_rejected_502(&manifest, &result, "zero-length Data file omitted");
}

#[test]
fn recreated_zero_length_data_file_completes_the_set() {
    // The recreated output carries its repaired on-disk size, which
    // need not equal the manifest's 0; only membership in the set
    // is required.
    let manifest = ScratchManifest {
        files: vec![
            data_file("track01.flac", 0),
            data_file("track02.flac", 9),
            ScratchFile {
                file_name: "set.par2".to_string(),
                key: object_key_for_file_name("set.par2"),
                size: 8,
                role: FileRole::Par2,
            },
        ],
        ..two_data_manifest()
    };
    let result = repaired(vec![output("track01.flac", 123), output("track02.flac", 9)]);
    validate_repaired_output_set(&manifest, &result).unwrap();
}

#[test]
fn empty_output_set_matches_a_manifest_with_no_data_files() {
    // The rule is set equality with the Data files, not an
    // unconditional "at least one output" requirement.
    let manifest = ScratchManifest {
        staging_id: "staging-0000000000000001".to_string(),
        files: vec![ScratchFile {
            file_name: "set.par2".to_string(),
            key: object_key_for_file_name("set.par2"),
            size: 8,
            role: FileRole::Par2,
        }],
    };
    validate_repaired_output_set(&manifest, &repaired(vec![])).unwrap();
}

#[test]
fn unrepairable_result_keeps_its_unchanged_semantics() {
    // Unrepairable carries no outputs and must flow through to the
    // stored verdict exactly as before.
    let manifest = two_data_manifest();
    let result = RepairResult {
        status: RepairStatus::Unrepairable,
        exit_code: Some(1),
        files: vec![],
        manifest_digest: String::new(),
    };
    validate_repaired_output_set(&manifest, &result).unwrap();
}

#[test]
fn runtime_validates_the_set_before_any_copy_or_result_marker() {
    // Minimal wiring regression: in the wasm runtime, the validator
    // call must precede both the first output copy and the durable
    // `out/result.json` write, so a rejected set copies nothing and
    // records no success marker.
    let runtime = include_str!("../src/runtime.rs");
    let validate = runtime
        .find("validate_repaired_output_set(&manifest, &result)?")
        .expect("runtime calls the output-set validator");
    let copy = runtime
        .find("self.fetch_output(")
        .expect("runtime copies outputs");
    let marker = runtime
        .find("result_key(prefix), result_body")
        .expect("runtime writes the result marker");
    assert!(
        validate < copy,
        "validator must run before the first output copy"
    );
    assert!(
        copy < marker,
        "result marker must land only after validated outputs are copied"
    );
}
