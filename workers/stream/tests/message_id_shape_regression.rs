//! Acceptance: message-ids that are printable ASCII but not shaped like
//! `local@domain` (or that are non-ASCII / carry control bytes outside the
//! small legacy blacklist) must be rejected with `InvalidMessageId` before
//! any `BODY` command is serialized to the socket.
//!
//! Contract under test (public seam only: `NntpClient::article`):
//! - the shared segment-identity rule, validated byte-for-byte as given:
//!   printable ASCII only, exactly one `@`, both sides nonempty, under the
//!   250-byte ceiling, with no trim or `<>` bracket stripping — a padded
//!   or bracketed spelling is refused, never normalized into acceptance;
//! - every violation returns `Err(InvalidMessageId)` echoing the caller's
//!   id while the server observes zero post-greeting bytes;
//! - a violation is local input, never a provider failure, so it cannot
//!   quarantine the pool (`is_provider_failure() == false`);
//! - bare well-shaped ids keep normal handling.
//!
//! Bounded loopback fixtures only; no external network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use nntune_stream::nntp::{NntpClient, NntpError};

/// Connect a client and call `article(id)`, returning the outcome plus the
/// number of post-greeting bytes the server observed from the client.
fn attempt_with_byte_count(message_id: &str) -> (Result<Vec<u8>, NntpError>, usize) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        stream
            .write_all(b"200 shape NNTP ready\r\n")
            .expect("greeting");
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("read timeout");
        let mut buf = [0u8; 4096];
        let observed = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                0
            }
            Err(e) => panic!("unexpected server read error: {e:?}"),
        };
        tx.send(observed).expect("report byte count");
    });

    let mut client = NntpClient::connect(&addr).expect("greeting arrives");
    let outcome = client.article(message_id);
    // Drop the connection so the server thread is never stuck past its
    // 250 ms read bound even on a slow CI worker.
    drop(client);
    let observed = rx.recv_timeout(Duration::from_secs(5)).expect("byte count");
    server.join().expect("server thread");
    (outcome, observed)
}

fn assert_rejected_before_wire_write(message_id: &str) {
    let (outcome, observed) = attempt_with_byte_count(message_id);
    match outcome {
        Err(NntpError::InvalidMessageId(echo)) => {
            assert_eq!(
                echo,
                message_id.to_string(),
                "rejection must echo the offending id {message_id:?}"
            );
            assert!(
                !NntpError::InvalidMessageId(echo).is_provider_failure(),
                "malformed local input must not be a provider failure: {message_id:?}"
            );
        }
        other => panic!("malformed id {message_id:?} must yield InvalidMessageId, got {other:?}"),
    }
    assert_eq!(
        observed, 0,
        "malformed id {message_id:?} must write zero socket bytes before rejection"
    );
}

/// Serve one `BODY` round-trip with a scripted reply; return the client
/// outcome and the exact command line the server received.
fn fetch_with_scripted_reply(
    message_id: &str,
    reply: &[u8],
) -> (Result<Vec<u8>, NntpError>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
    let addr = listener.local_addr().expect("local addr");
    let reply = reply.to_vec();
    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept client");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeouts");
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        writer
            .write_all(b"200 control NNTP ready\r\n")
            .expect("greeting");
        let mut command = String::new();
        reader.read_line(&mut command).expect("BODY command");
        writer.write_all(&reply).expect("scripted reply");
        writer.flush().expect("flush");
        let mut trailing = Vec::new();
        reader
            .read_to_end(&mut trailing)
            .expect("remaining client bytes");
        command.push_str(&String::from_utf8(trailing).expect("UTF-8 BODY command"));
        tx.send(command).expect("report command");
    });

    let mut client = NntpClient::connect(&addr).expect("greeting arrives");
    let outcome = client.article(message_id);
    drop(client);
    let command = rx.recv_timeout(Duration::from_secs(5)).expect("command");
    server.join().expect("server thread");
    (outcome, command)
}

#[test]
fn ids_without_exactly_one_at_are_rejected_before_any_wire_write() {
    let malformed: &[&str] = &[
        "not-an-id",         // no @ at all
        "a@b@c",             // two @
        "a@@synthetic.test", // adjacent @@
        "<not-an-id>",       // bracketed, still no @
        "<a@b@c>",           // bracketed, still two @
    ];
    for id in malformed {
        assert_rejected_before_wire_write(id);
    }
}

#[test]
fn ids_with_empty_local_or_domain_side_are_rejected_before_any_wire_write() {
    let malformed: &[&str] = &[
        "@synthetic.test",   // empty local side
        "local@",            // empty domain side
        "@",                 // both sides empty
        "<@synthetic.test>", // bracketed empty local
        "<local@>",          // bracketed empty domain
    ];
    for id in malformed {
        assert_rejected_before_wire_write(id);
    }
}

#[test]
fn non_ascii_ids_are_rejected_before_any_wire_write() {
    let malformed: &[&str] = &[
        "s\u{e9}g@synthetic.test",     // non-ASCII in local side
        "seg@h\u{f6}st.test",          // non-ASCII in domain side
        "<seg@synthetic.t\u{20ac}st>", // bracketed non-ASCII
    ];
    for id in malformed {
        assert_rejected_before_wire_write(id);
    }
}

#[test]
fn ids_with_control_bytes_outside_legacy_blacklist_are_rejected_before_wire_write() {
    // Control/DEL bytes with a well-shaped local@domain around them: none is
    // printable ASCII, so none may be serialized into a BODY command.
    let malformed: &[&str] = &[
        "a\u{1}b@synthetic.test",  // SOH
        "a\u{7}b@synthetic.test",  // BEL
        "a\u{b}b@synthetic.test",  // VT
        "a\u{1b}b@synthetic.test", // ESC
        "a\u{7f}b@synthetic.test", // DEL
        "seg@synthetic.te\u{2}st", // STX in domain side
    ];
    for id in malformed {
        assert_rejected_before_wire_write(id);
    }
}

#[test]
fn well_shaped_ids_retain_normal_handling() {
    // Unbracketed, 430 path: BODY goes out, reply surfaces as NoSuchArticle.
    let (outcome, command) =
        fetch_with_scripted_reply("good-seg0@synthetic.test", b"430 no such article\r\n");
    assert_eq!(
        command, "BODY <good-seg0@synthetic.test>\r\n",
        "well-shaped id must reach the wire as a single BODY command"
    );
    assert!(
        matches!(outcome, Err(NntpError::NoSuchArticle(_))),
        "scripted 430 must surface as NoSuchArticle, got {outcome:?}"
    );

    // Bare well-shaped id, 222 path: returns the dot-unstuffed body.
    let (outcome, command) = fetch_with_scripted_reply(
        "good-seg0@synthetic.test",
        b"222 article follows\r\nHello\r\n.\r\n",
    );
    assert_eq!(
        command, "BODY <good-seg0@synthetic.test>\r\n",
        "bare id must reach the wire as a single BODY command"
    );
    assert_eq!(
        outcome.expect("222 must fetch"),
        b"Hello\n",
        "well-shaped control must complete normal body handling"
    );
}

#[test]
fn padded_and_bracketed_spellings_are_rejected_without_normalization() {
    // Well-shaped once stripped, but the shared rule never strips: every
    // spelling is refused before any wire write.
    let refused: &[&str] = &[
        "<good-seg0@synthetic.test>",
        "  <good-seg1@synthetic.test>  ",
        " good-seg0@synthetic.test",
        "good-seg0@synthetic.test ",
        "\tgood-seg0@synthetic.test",
    ];
    for id in refused {
        assert_rejected_before_wire_write(id);
    }
}

#[test]
fn overlong_well_shaped_id_stays_rejected_under_existing_ceiling() {
    // Well-shaped local@domain far past the adapter's existing 250-byte
    // ceiling: the shape fix must not lift or drop that ceiling.
    let overlong = format!("{}@synthetic.test", "a".repeat(300));
    assert_rejected_before_wire_write(&overlong);
}
