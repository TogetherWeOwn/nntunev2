//! NNTune v2 accounts/auth/session/vault/library: security-critical core
//! (`auth`, `session`, `vault`, `webauthn`, `library`), the
//! framework-agnostic HTTP handlers (`handlers`, native-tested against
//! fake stores), and the Worker + Hyperdrive entry (`worker_entry`).

pub mod auth;
pub mod b64;
pub mod handlers;
pub mod ledger;
pub mod library;
pub mod session;
pub mod vault;
pub mod webauthn;
pub mod worker_entry;

#[worker::event(fetch)]
pub async fn main(
    req: worker::Request,
    env: worker::Env,
    _ctx: worker::Context,
) -> worker::Result<worker::Response> {
    worker_entry::dispatch(req, env).await
}
