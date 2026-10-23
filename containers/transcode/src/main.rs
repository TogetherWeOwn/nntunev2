//! Transcode Container entry point. Configuration comes only from the
//! image environment; requests can never choose the binary or the
//! scratch root.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use nntune_transcode_server::{serve, ServerConfig, SERVER_PORT};

fn main() -> std::io::Result<()> {
    let scratch_root = PathBuf::from(
        std::env::var("TRANSCODE_SCRATCH_DIR").unwrap_or_else(|_| "/scratch".to_string()),
    );
    let ffmpeg_bin = PathBuf::from(
        std::env::var("TRANSCODE_FFMPEG_BIN").unwrap_or_else(|_| "/usr/bin/ffmpeg".to_string()),
    );
    let port: u16 = std::env::var("TRANSCODE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SERVER_PORT);
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    eprintln!("transcode server listening on port {port}");
    serve(
        listener,
        Arc::new(ServerConfig {
            scratch_root,
            ffmpeg_bin,
        }),
    )
}
