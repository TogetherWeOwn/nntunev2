//! Demo crate.

/// Maximum retries.
pub const MAX_RETRIES: u32 = 3;

/// Run the job.
pub fn run_job() -> bool {
    helper_inner()
}

fn helper_inner() -> bool {
    true
}
