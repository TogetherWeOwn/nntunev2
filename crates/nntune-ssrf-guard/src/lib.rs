//! NNTune v2 shim over the shared velmora SSRF address guard and test vectors.
//!
//! The predicate and vectors have one owner in velmora. Keep worker call sites
//! delegating here; do not fork the predicate. Native + wasm32, std-only.

pub use velmora_ssrf_guard::*;
