//! Protected transport for a selected staging fixture.

pub const PATH: &str = "/v1/admin/private-trial/upload";
pub const FIXTURE_BYTES: usize = 177_175;

mod caller;
pub use caller::*;
