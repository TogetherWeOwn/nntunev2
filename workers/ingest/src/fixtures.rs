//! Synthetic ingest fixtures for unit + pipeline tests.
//!
//! Stand-in for the synthetic NNTP corpus owned by the synthetic-server
//! card: same shapes (yEnc single/multi-part subjects, one gapped set, one
//! par2 set), embedded so the pipeline E2E runs in CI today. When that card
//! lands, these fixtures defer to its corpus; the message-ids below are
//! reserved for that handoff.

/// Minimal two-segment NZB used across dispatch/parse/probe tests.
pub const SMALL_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track01.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="100" number="1">seg-one@synthetic.invalid</segment>
<segment bytes="100" number="2">seg-two@synthetic.invalid</segment>
</segments>
</file>
</nzb>"#;

/// Message-ids referenced by [`SMALL_NZB`].
pub const SMALL_NZB_SEGMENTS: [&str; 2] =
    ["seg-one@synthetic.invalid", "seg-two@synthetic.invalid"];

/// A gapped NZB: the second segment is missing server-side (430), so the
/// probe reports it missing and the repair path must cover it.
pub const GAPPED_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track02 [01/02] - &quot;track02.flac&quot; yEnc (1/2)">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="100" number="1">gap-one@synthetic.invalid</segment>
<segment bytes="100" number="2">gap-missing@synthetic.invalid</segment>
</segments>
</file>
</nzb>"#;

/// par2 set id covering [`GAPPED_NZB`].
pub const GAPPED_PAR2_SET: &str = "par2-gap-demo-01";

/// Malformed NZB: the `<file>` element is never closed, so intake must
/// refuse it as `NZB_MALFORMED` (400) before it reaches the pipeline.
pub const MALFORMED_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="track01.flac">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="100" number="1">seg-one@synthetic.invalid</segment>
</segments>
</nzb>"#;

/// Declared-bytes over cap: two segments claiming 40 GiB each (80 GiB total
/// against the 64 GiB cap) in a few hundred bytes of XML, so intake must
/// refuse it as `NZB_DECLARED_BYTES_OVER_CAP` (413).
pub const OVER_BYTES_NZB: &str = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="big.iso">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="42949672960" number="1">big-one@synthetic.invalid</segment>
<segment bytes="42949672960" number="2">big-two@synthetic.invalid</segment>
</segments>
</file>
</nzb>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nzb_intake::{estimate_nzb, IntakeRejection};

    #[test]
    fn fixtures_pass_the_intake_guard() {
        let small = estimate_nzb(SMALL_NZB).unwrap();
        assert_eq!(small.file_count, 1);
        assert_eq!(small.segment_count, 2);
        assert_eq!(small.declared_bytes, 200);
        let gapped = estimate_nzb(GAPPED_NZB).unwrap();
        assert_eq!(gapped.segment_count, 2);
    }

    #[test]
    fn rejection_fixtures_carry_exact_codes() {
        let malformed = estimate_nzb(MALFORMED_NZB).unwrap_err();
        assert_eq!(malformed, IntakeRejection::Malformed);
        assert_eq!(malformed.code(), "NZB_MALFORMED");
        assert_eq!(malformed.http_status(), 400);
        let over_bytes = estimate_nzb(OVER_BYTES_NZB).unwrap_err();
        assert_eq!(over_bytes.code(), "NZB_DECLARED_BYTES_OVER_CAP");
        assert_eq!(over_bytes.http_status(), 413);
        match over_bytes {
            IntakeRejection::DeclaredBytesOverCap { declared, max } => {
                assert_eq!(declared, 2 * 42_949_672_960);
                assert_eq!(max, crate::nzb_intake::MAX_DECLARED_BYTES);
            }
            other => panic!("expected declared-bytes rejection, got {other:?}"),
        }
    }
}
