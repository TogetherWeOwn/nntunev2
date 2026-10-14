use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub scratch: PathBuf,
    pub binary: PathBuf,
}

impl Fixture {
    pub fn new(script: &str) -> Self {
        let base = std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR")
            .or_else(|| std::env::var_os("PAPERCLIP_SCRATCH_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let mut sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = loop {
            let root = base.join(format!(
                "nntune-repair-test-{}-{sequence}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => break root,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    sequence = NEXT.fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => panic!("create owned test directory: {err}"),
            }
        };
        let scratch = root.join("job space");
        fs::create_dir(&scratch).unwrap();
        let binary = root.join("fake-par2.sh");
        fs::write(&binary, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(scratch.join("set.par2"), b"fake binary fixture\n").unwrap();
        Self {
            root,
            scratch,
            binary,
        }
    }

    pub fn input(&self) -> &Path {
        Path::new("set.par2")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only this uniquely created, test-owned tree is removed.
        let _ = fs::remove_dir_all(&self.root);
    }
}
