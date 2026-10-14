//! NNTune v2 Subsonic-compatible API surface (Rust).
//!
//! Ported from v1 `routes/subsonic.php` behaviour: the same 49 endpoints,
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

pub use app::{oversized_body_response, HttpResponse, SubsonicApp, MAX_BODY_BYTES};
pub use auth::{AuthStore, Credentials, Identity};
pub use endpoints::Params;
pub use library::{Library, MemStore, UserAccount};
pub use routes::{PUBLIC_ENDPOINTS, SUPPORTED_ENDPOINTS, UNSUPPORTED_ENDPOINTS};
