//! Keep shell-oracle regressions in the workspace's existing native CI
//! lane. These use controlled external commands, not a real image; the
//! transcode-container job separately exercises the real shipped encoder.

#[test]
fn image_self_test_oracles_reject_false_positives_without_pipe_false_negatives() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test_determinism_oracle.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg("-v")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("python3 is required for image-oracle regression tests");
    assert!(
        output.status.success(),
        "image-oracle regressions failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
