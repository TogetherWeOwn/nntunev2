use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use nntune_core::nntp::NntpClient as CoreClient;
use nntune_core::transports::NativeTlsTransport;
use nntune_stream::body::{run_body, BodyRequest};
use nntune_stream::delivered::{parse_delivered_report, DeliveredInterval};
use nntune_stream::meter::{reconcile, CompletePlay, MemoryMeter, RecordOutcome, UsageLedger};
use nntune_stream::nntp::{NntpClient, NntpError};
use nntune_stream::pool::Pool;
use nntune_stream::probe::ProbeError;
use nntune_stream::yenc::{crc32, decode, decode_part};

const ARTICLE_ID: &str = "fixture@synthetic.invalid";

#[test]
fn yenc_requires_complete_control_words() {
    for article in [
        "=ybeginning size=1 name=a\nA\n=yend size=1\n",
        "=ybegin size=1 name=a\nA\n=yending size=1\n",
        "=ybegin part=1 size=1 name=a\n=ypartial begin=1 end=1\nA\n=yend size=1\n",
    ] {
        assert!(decode(article.as_bytes()).is_err(), "{article}");
    }
}

#[test]
fn yenc_control_prefix_can_be_escaped_payload() {
    let encoded = b"=yendXYZ";
    let mut payload = vec![b'y'.wrapping_sub(64).wrapping_sub(42)];
    payload.extend(encoded[2..].iter().map(|byte| byte.wrapping_sub(42)));
    let crc = crc32(&payload);
    let article = format!("=ybegin\tsize=7 name=a\n=yendXYZ\n=yend\tsize=7 crc32={crc:08x}\n");
    assert_eq!(decode(article.as_bytes()), Ok(payload));
}

#[test]
fn yenc_rejects_ambiguous_control_fields() {
    let crc = crc32(&[b'A' - 42]);
    for article in [
        "=ybegin size=1 size=2 name=a\nA\n=yend size=1\n".to_owned(),
        "=ybegin size=1 name=a\nA\n=yend size=1 size=2\n".to_owned(),
        "=ybegin part=1 size=1 name=a\n=ypart begin=1 end=1 end=2\nA\n=yend size=1\n".to_owned(),
        format!("=ybegin size=1 name=a\nA\n=yend size=1 crc32={crc:08x} crc32=broken\n"),
    ] {
        assert!(decode(article.as_bytes()).is_err(), "{article}");
    }
}

#[test]
fn yenc_part_metadata_must_agree() {
    let part = |header: &str, trailer: &str| {
        format!("=ybegin size=1 {header}name=a\n=ypart begin=1 end=1\nA\n=yend size=1{trailer}\n")
    };
    for (header, trailer) in [
        ("", ""),
        ("part=0 ", ""),
        ("part=2 total=1 ", ""),
        ("part=1 total=unknown ", ""),
        ("part=1 part=2 ", ""),
        ("part=1 ", " part=2"),
        ("part=1 ", " part="),
    ] {
        assert!(
            decode_part(part(header, trailer).as_bytes()).is_err(),
            "{header:?} {trailer:?}"
        );
    }
    for (header, trailer) in [("part=1 total=1 ", " part=1"), ("part=1 ", "")] {
        assert_eq!(
            decode(part(header, trailer).as_bytes()),
            Ok(vec![b'A' - 42])
        );
    }
    assert!(decode(b"=ybegin size=1 name=a\nA\n=yend size=1 part=1\n").is_err());
}

#[test]
fn nntp_requires_a_complete_greeting_line() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.write_all(b"200 ready\r").unwrap();
    });
    let result = NntpClient::connect(&addr);
    peer.join().unwrap();
    assert!(matches!(result, Err(NntpError::Io(_))));
}

fn with_body_reply(reply: &'static [u8], check: impl FnOnce(&mut NntpClient)) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut socket = BufReader::new(socket);
        socket.get_mut().write_all(b"200 ready\n").unwrap();
        let mut request = String::new();
        socket.read_line(&mut request).unwrap();
        assert_eq!(request, format!("BODY <{ARTICLE_ID}>\r\n"));
        socket.get_mut().write_all(reply).unwrap();
    });
    let mut client = NntpClient::connect(&addr).unwrap();
    check(&mut client);
    peer.join().unwrap();
}

#[test]
fn nntp_requires_a_complete_status_line() {
    for response in [b"430 no article".as_slice(), b"480 authenticate\r"] {
        with_body_reply(response, |client| {
            assert!(matches!(client.article(ARTICLE_ID), Err(NntpError::Io(_))));
            assert_eq!(
                client.article(ARTICLE_ID),
                Err(NntpError::UnexpectedResponse(
                    "NNTP connection unusable".into()
                ))
            );
        });
    }
}

#[test]
fn nntp_requires_a_complete_body_terminator() {
    for response in [
        b"222 body\r\n..dot\r\n.".as_slice(),
        b"222 body\r\n..dot\r\n.\r",
    ] {
        with_body_reply(response, |client| {
            assert!(matches!(client.article(ARTICLE_ID), Err(NntpError::Io(_))));
            assert_eq!(
                client.article(ARTICLE_ID),
                Err(NntpError::UnexpectedResponse(
                    "NNTP connection unusable".into()
                ))
            );
        });
    }
    with_body_reply(b"222 body\r\n..dot\r\n.\r\n", |client| {
        assert_eq!(client.article(ARTICLE_ID).unwrap(), b".dot\n");
    });
}

fn receipt(start: u64, end: u64, file_bytes: Option<u64>) -> DeliveredInterval {
    DeliveredInterval {
        user_id: "member".into(),
        play_id: "play".into(),
        track_id: "track".into(),
        start,
        end,
        file_bytes,
    }
}

#[test]
fn delivered_length_never_underflows_for_a_reversed_span() {
    let reversed = receipt(8, 7, None);
    assert!(reversed.is_empty());
    assert_eq!(reversed.len(), 0);
    assert!(parse_delivered_report(&serde_json::to_vec(&reversed).unwrap(), "member").is_err());
}

#[test]
fn delivered_file_size_must_fit_the_ledger_snapshot() {
    let limit = i64::MAX as u64;
    for (size, accepted) in [(None, true), (Some(limit), true), (Some(limit + 1), false)] {
        let bytes = serde_json::to_vec(&receipt(0, 1, size)).unwrap();
        assert_eq!(parse_delivered_report(&bytes, "member").is_ok(), accepted);
    }
}

#[test]
fn zero_byte_plays_never_claim_a_ledger_key() {
    assert!(CompletePlay::new("member", "track", "play", 0).is_err());
    let delivered = CompletePlay::new("member", "track", "play", 1).unwrap();
    let mut empty = delivered.clone();
    empty.bytes = 0;
    let mut ledger = UsageLedger::default();
    assert_eq!(ledger.record(&empty), RecordOutcome::Conflict);
    assert!(ledger.is_empty());
    assert_eq!(ledger.record(&delivered), RecordOutcome::Recorded);
    let report = reconcile(&[empty], &ledger, &mut MemoryMeter::default());
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].ledger_bytes, Some(1));
}

#[tokio::test]
async fn invalid_body_segment_is_refused_before_pool_access() {
    let pool = RefCell::new(Pool::<CoreClient<NativeTlsTransport>>::new(4));
    let request = BodyRequest {
        job_id: "job".into(),
        user_id: "member".into(),
        idempotency_key: "request".into(),
        segment_id: "missing-at-sign".into(),
    };
    let result = run_body(
        &pool,
        || async { panic!("invalid request must never dial") },
        |_| panic!("invalid request must never touch the pool"),
        &request,
    )
    .await;
    assert_eq!(
        result,
        Err(ProbeError::BadRequest(
            "segment id must be a valid message-id"
        ))
    );
    assert_eq!(pool.borrow().in_use(), 0);
}
