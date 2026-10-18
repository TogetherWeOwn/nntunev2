//! Refill-18 pin: hostile message-ids are rejected with
//! `InvalidMessageId` before any socket write, so a hostile NZB cannot smuggle
//! CR/LF (extra NNTP commands) or desynchronize the pooled connection.
//!
//! Each hostile id gets a fresh connection: the fake server greets, then
//! watches for *any* client bytes. `article()` must return
//! `Err(InvalidMessageId)` while the server observes zero bytes. A
//! well-formed control id must proceed to normal handling (the server sees
//! the `BODY` command and the client surfaces the scripted reply).

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
            .write_all(b"200 hostile-id NNTP ready\r\n")
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
                "a malformed id says nothing about the provider: {message_id:?}"
            );
        }
        other => panic!("hostile id {message_id:?} must yield InvalidMessageId, got {other:?}"),
    }
    assert_eq!(
        observed, 0,
        "hostile id {message_id:?} must write zero socket bytes before rejection"
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
        let _ = stream
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
fn hostile_message_ids_rejected_before_any_wire_write() {
    // Empty, bracket-only, and control-byte hostile ids (post-trim /
    // post-`<>`-strip view in comments): none may reach the socket.
    let hostile: &[&str] = &[
        "",                             // empty
        "   ",                          // whitespace-only -> empty after trim
        "<>",                           // bracket-only -> empty after strip
        "<<<>>>",                       // bracket-only, nested
        "<   >",                        // bracket-wrapped whitespace -> inner spaces
        "x>\r\nBODY <other@article>",   // CRLF command smuggle
        "seg\r\nINJECT@synthetic.test", // CRLF injection
        "a\nb@c",                       // LF
        "a\rb@c",                       // CR
        "a\0b@c",                       // NUL
        "a b@c",                        // space
        "a\tb@c",                       // tab
        "a<b@c",                        // inner angle bracket
        "a>b@c",                        // inner angle bracket
    ];
    for id in hostile {
        assert_rejected_before_wire_write(id);
    }
}

#[test]
fn well_formed_id_proceeds_to_normal_handling() {
    // 430 path: the id passes validation, the BODY command goes out, and the
    // missing-article reply surfaces as NoSuchArticle (never InvalidMessageId).
    let (outcome, command) =
        fetch_with_scripted_reply("good-seg0@synthetic.test", b"430 no such article\r\n");
    assert_eq!(
        command, "BODY <good-seg0@synthetic.test>\r\n",
        "well-formed id must reach the wire as a single BODY command"
    );
    assert!(
        matches!(outcome, Err(NntpError::NoSuchArticle(_))),
        "scripted 430 must surface as NoSuchArticle, got {outcome:?}"
    );

    // Bracketed form strips to the same wire command; 222 path returns the
    // dot-unstuffed body.
    let (outcome, command) = fetch_with_scripted_reply(
        "<good-seg0@synthetic.test>",
        b"222 article follows\r\nHello\r\n.\r\n",
    );
    assert_eq!(
        command, "BODY <good-seg0@synthetic.test>\r\n",
        "bracketed id must strip to the same BODY command"
    );
    assert_eq!(
        outcome.expect("222 must fetch"),
        b"Hello\n",
        "well-formed control must complete normal body handling"
    );
}
