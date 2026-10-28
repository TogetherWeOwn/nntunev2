//! Acceptance: intake and outbound envelopes must use the wire-safe identity
//! contract: bare printable ASCII, exactly one non-edge '@', at most 250 B.
//! No normalization may turn a refused spelling into an accepted identity.
//!
//! Sensitivity: accepting 251/512 B, trimming or unbracketing during parse,
//! or bypassing any outbound validator fails an independent vector test.
//! Expectations come from the shared synthetic table, never another validator.
//! GREEN/legacy-test reconciliation belongs to the paired implementation.

#[macro_use]
#[path = "../../stream/tests/support/segment_identity_vectors.rs"]
mod identity_vectors;

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::nzb_intake::estimate_nzb;
use nntune_ingest::parse::{parse_upload, parse_upload_files};
use nntune_ingest::pool_body::build_body_request;
use nntune_ingest::probe::{
    build_probe_request, build_probe_request_with_ranges, validate_message_id,
};
use nntune_ingest::queue::QueueMsg;

fn xml(id: &str) -> String {
    // Preserve CR/LF and tabs through XML decoding rather than accidentally
    // testing XML newline normalization. Escape brackets as XML text too.
    let text = id
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#9;");
    format!(
        "<?xml version=\"1.0\"?><nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\
         <file poster=\"a@b\" date=\"1\" subject=\"&quot;identity.flac&quot; yEnc (1/1)\">\
         <groups><group>alt.test</group></groups><segments>\
         <segment bytes=\"42\" number=\"1\">{text}</segment>\
         </segments></file></nzb>"
    )
}

fn job() -> QueueMsg {
    build_upload_dispatch("user_01", &xml("control@synthetic.invalid"))
        .expect("synthetic intake control")
}

mod validator {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                assert_eq!(validate_message_id(&id), $want, "ingest validator: {id:?}");
            }
        };
    }
    identity_cases!(case);
}

mod probe_envelope {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let result = build_probe_request(&job(), 0, &[&id]);
                assert_eq!(result.is_ok(), $want, "probe envelope: {id:?}: {result:?}");
                if let Ok(request) = result {
                    assert_eq!(request.segment_ids, [id], "identity must be preserved");
                }
            }
        };
    }
    identity_cases!(case);
}

mod ranged_probe_envelope {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let result = build_probe_request_with_ranges(&job(), 0, &[(&id, Some(42))]);
                assert_eq!(result.is_ok(), $want, "ranged probe: {id:?}: {result:?}");
                if let Ok(request) = result {
                    assert_eq!(request.segment_ids, [id], "identity must be preserved");
                }
            }
        };
    }
    identity_cases!(case);
}

mod body_envelope {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let result = build_body_request(&job(), &id);
                assert_eq!(result.is_ok(), $want, "body envelope: {id:?}: {result:?}");
                if let Ok(request) = result {
                    assert_eq!(request.segment_id, id, "identity must be preserved");
                }
            }
        };
    }
    identity_cases!(case);
}

mod parsed_intake {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let document = xml(&id);
                let estimate = estimate_nzb(&document).expect("one-file one-segment XML");
                let result = parse_upload(document.as_bytes(), &estimate);
                assert_eq!(result.is_ok(), $want, "parsed intake: {id:?}: {result:?}");
                if let Ok(parsed) = result {
                    assert_eq!(parsed.message_ids, [id], "identity must be preserved");
                }
            }
        };
    }
    identity_cases!(case);
}

mod file_intake {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let document = xml(&id);
                let estimate = estimate_nzb(&document).expect("one-file one-segment XML");
                let result = parse_upload_files(document.as_bytes(), &estimate);
                assert_eq!(result.is_ok(), $want, "file intake: {id:?}: {result:?}");
                if let Ok(files) = result {
                    assert_eq!(files.len(), 1);
                    assert_eq!(files[0].segment_ids, [id], "identity must be preserved");
                }
            }
        };
    }
    identity_cases!(case);
}

#[test]
fn empty_track_is_refused_at_dispatch_and_probe_intake() {
    assert!(build_upload_dispatch("user_01", "<nzb/>").is_err());
    assert!(build_upload_dispatch("user_01", "<nzb><file><segments/></file></nzb>").is_err());
    assert!(build_probe_request(&job(), 0, &[]).is_err());
    assert!(build_probe_request_with_ranges(&job(), 0, &[]).is_err());
}
