//! Repair Container entry point. Configuration comes only from the
//! image environment; requests can never choose the binary or the
//! scratch root.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use nntune_repair_server::{serve, ServerConfig, SERVER_PORT};

fn main() -> std::io::Result<()> {
    let scratch_root = PathBuf::from(
        std::env::var("REPAIR_SCRATCH_DIR").unwrap_or_else(|_| "/scratch".to_string()),
    );
    let par2_bin = PathBuf::from(
        std::env::var("REPAIR_PAR2_BIN").unwrap_or_else(|_| "/usr/local/bin/par2".to_string()),
    );
    let port: u16 = std::env::var("REPAIR_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SERVER_PORT);
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    eprintln!("repair server listening on port {port}");
    serve(
        listener,
        Arc::new(ServerConfig {
            scratch_root,
            par2_bin,
        }),
    )
}
