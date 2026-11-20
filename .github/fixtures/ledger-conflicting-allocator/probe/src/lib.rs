//! Links the ledger-wasm-test artifact graph. With `conflict`, a dependency
//! also declares a global allocator, which must fail to link.
use nntune_api as _;

#[cfg(feature = "conflict")]
use conflicting_allocator_dep as _;
