//! S7 RED: forced NNTP 481/482 on the miss path must surface the
//! documented `provider_auth_expired` code as an inline re-auth signal,
//! never a quarantined outage and never credential material.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::nntp::NntpError;
use nntune_stream::play::{play, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;

const SECRET: &[u8] = b"provider-auth-expired-secret";

#[test]
fn stream_documents_provider_auth_expired_code() {
    assert_eq!(
        nntune_stream::nntp::PROVIDER_AUTH_EXPIRED_CODE,
        "provider_auth_expired"
    );
    assert_eq!(
        nntune_stream::play::PROVIDER_AUTH_EXPIRED_CODE,
        "provider_auth_expired"
    );
}

#[test]
fn nntp_auth_expired_is_not_a_quarantineable_provider_failure() {
    let err = NntpError::AuthExpired;
    assert!(
        !err.is_provider_failure(),
        "stale creds must prompt re-auth, not quarantine the pool"
    );
    assert_eq!(err.code(), "provider_auth_expired");
}

#[test]
fn play_error_carries_documented_code_without_secrets() {
    let secret = "s3cret-provider-password";
    let err = PlayError::ProviderAuthExpired;
    let rendered = format!("{err:?}");
    assert!(
        !rendered.contains(secret),
        "secret in PlayError: {rendered}"
    );
    assert_eq!(err.code(), "provider_auth_expired");
}

/// Fake NNTP server that greets, then answers every `BODY` with `reply`.
struct AuthRejectingNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl AuthRejectingNntp {
    fn start(reply: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_c.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                Self::serve(stream, reply);
            }
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, reply: &str) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.starts_with("BODY ") {
                let _ = writer.write_all(reply.as_bytes());
            } else {
                let _ = writer.write_all(b"500 unknown command\r\n");
            }
        }
    }
}

impl Drop for AuthRejectingNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn fake_481_482_replies_drive_play_to_provider_auth_expired_without_quarantine() {
    // Mutant-killing: real 481/482 status lines travel through
    // NntpClient::article -> play. Mapping either code to any other
    // NntpError/PlayError (or quarantining on it) fails this test.
    for reply in [
        "481 authentication rejected\r\n",
        "482 authentication required\r\n",
    ] {
        let server = AuthRejectingNntp::start(reply);
        let mut pool: StreamPool = Pool::new(4);
        let mut cache = ContentCache::new();
        let mut meter = MemoryMeter::default();
        let token = grant::issue("user-1", "track-a", 9_999_999_999, SECRET);
        let segments = vec!["<a-seg0@synthetic.test>".to_string()];
        let input = PlayInput {
            token: &token,
            track_id: "track-a",
            range: None,
            server_addr: server.addr,
            segments: &segments,
            cipher_key: 0,
            now_unix: 1_700_000_000,
            up_next: None,
        };
        // PlayResponse is not Debug, so no expect_err here.
        let err = match play(&mut pool, &mut cache, &mut meter, SECRET, &input) {
            Ok(_) => panic!("stale credentials must fail the play"),
            Err(e) => e,
        };
        assert_eq!(
            err,
            PlayError::ProviderAuthExpired,
            "reply {reply:?} must surface as the re-auth signal"
        );
        assert_eq!(err.code(), "provider_auth_expired");
        assert!(
            !pool.quarantined(),
            "reply {reply:?}: stale creds are not a provider outage"
        );
        assert!(
            !meter.has_kind(MeterKind::ProviderError),
            "reply {reply:?}: no provider-error metering on the re-auth path"
        );
        assert_eq!(pool.in_use(), 0, "the lease is returned after settle");
    }
}
