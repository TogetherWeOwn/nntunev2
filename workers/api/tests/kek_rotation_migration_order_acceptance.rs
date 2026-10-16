//! Test-only regression coverage for KEK migration discovery and ordering.
//!
//! The KEK follow-up must be a uniquely numbered, manifest-listed migration
//! after the existing schema history so deployment discovery applies it.

use std::fs;
use std::path::{Path, PathBuf};

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

fn migration_number(filename: &str) -> u32 {
    let (number, _) = filename
        .split_once('_')
        .unwrap_or_else(|| panic!("migration filename must have a numeric prefix: {filename}"));
    assert_eq!(
        number.len(),
        4,
        "migration prefix must be four digits: {filename}"
    );
    number
        .parse()
        .unwrap_or_else(|error| panic!("migration prefix must be numeric in {filename}: {error}"))
}

fn migration_files(root: &Path) -> Vec<String> {
    let mut files = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", root.display()))
        .map(|entry| entry.expect("migration directory entry").path())
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "sql")
        })
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("migration filename must be UTF-8")
                .to_owned()
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[test]
fn kek_rewrap_migration_is_discoverable_and_appended_after_existing_migrations() {
    let root = migrations_dir();
    let files = migration_files(&root);
    let kek_migrations = files
        .iter()
        .filter(|filename| filename.ends_with("_kek_rotation.sql"))
        .collect::<Vec<_>>();
    assert_eq!(
        kek_migrations.len(),
        1,
        "expected exactly one KEK rotation migration in {}",
        root.display()
    );
    let kek_migration = kek_migrations[0];
    let kek_number = migration_number(kek_migration);
    let max_number = files
        .iter()
        .map(|filename| migration_number(filename))
        .max()
        .expect("at least one migration must exist");
    assert_eq!(
        kek_number, max_number,
        "KEK migration {kek_migration} must be appended after all existing migration numbers"
    );
    assert_eq!(
        files
            .iter()
            .filter(|filename| migration_number(filename) == kek_number)
            .count(),
        1,
        "migration number {kek_number:04} must be unique"
    );

    let manifest = fs::read_to_string(root.join("manifest.toml"))
        .unwrap_or_else(|error| panic!("could not read migration manifest: {error}"));
    let manifest_entries = manifest
        .split("[[migration]]")
        .filter_map(|entry| {
            entry.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "file").then(|| value.trim().trim_matches('"').to_owned())
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        manifest_entries
            .iter()
            .filter(|filename| filename.as_str() == kek_migration)
            .count(),
        1,
        "KEK migration {kek_migration} must be listed exactly once in migrations/manifest.toml"
    );
    assert_eq!(
        manifest_entries.last().map(String::as_str),
        Some(kek_migration.as_str()),
        "KEK migration {kek_migration} must be last in manifest discovery order"
    );
}
