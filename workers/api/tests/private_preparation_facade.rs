//! Node VM composition contract, not actual worker-build/workerd qualification.
#[test]
fn private_facade_exports_and_public_delegation_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/private_preparation_facade.mjs");
    let status = std::process::Command::new("node")
        .args(["--experimental-vm-modules", "--test"])
        .arg(path)
        .status()
        .expect("Node is required for the facade composition contract");
    assert!(status.success(), "facade composition contract failed");
}
