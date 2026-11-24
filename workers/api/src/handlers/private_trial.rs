//! Protected transport for a selected staging fixture.

pub const PATH: &str = "/v1/admin/private-trial/upload";
pub const FIXTURE_BYTES: usize = 177_175;

mod caller;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod registration;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod sealed_fixture;
pub use caller::*;
