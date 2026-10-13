//! NNTune v2 Subsonic-compatible API surface (Rust).
//!
//! Ported from v1 `routes/subsonic.php` behaviour: the same 48 endpoints,
//! the same envelope, the same refusal codes. The [`Library`] trait is the
//! seam where PlanetScale (via Hyperdrive) plugs in; [`MemStore`] backs
//! tests and golden fixtures.

pub mod app;
pub mod auth;
pub mod endpoints;
pub mod envelope;
pub mod library;
pub mod routes;
pub mod value;

pub use app::{HttpResponse, SubsonicApp};
pub use auth::{AuthStore, Credentials, Identity};
pub use endpoints::Params;
pub use library::{Library, MemStore};
pub use routes::{SUPPORTED_ENDPOINTS, UNSUPPORTED_ENDPOINTS};
