//! Protected transport for a selected staging fixture.

pub const PATH: &str = "/v1/admin/private-trial/upload";
pub const FIXTURE_BYTES: usize = 177_175;

mod caller;
#[cfg(target_arch = "wasm32")]
pub(crate) use caller::worker;
pub mod cleanup;
#[cfg(all(target_arch = "wasm32", feature = "staging-fixture-cleanup"))]
mod cleanup_runner;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod registered;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod registration;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod sealed_fixture;
pub use caller::*;
