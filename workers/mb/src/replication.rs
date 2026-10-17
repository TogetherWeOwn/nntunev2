//! Per-packet replication accounting and an in-memory SLO probe.
//!
//! The applier persists the same fields in `mb_replication_packet` (migration
//! 0016); this module keeps the probe deterministic and independently testable.
use serde::Serialize;

/// Replication lag budget promised by ADR 0007.
pub const REPLICATION_LAG_SLO_SECS: i64 = 2 * 60 * 60;
/// Cap on individually enumerated `missing_sequence` breaches per evaluation.
/// A corrupt `latest_sequence` cursor must not turn one probe call into an
/// unbounded scan; past the cap the report carries one
/// `missing_sequence_truncated` breach instead of silently stopping.
pub const MAX_MISSING_SEQUENCE_BREACHES: usize = 10_000;
/// Counter name emitted with every non-empty breach report.
pub const BREACH_METRIC_NAME: &str = "mb_replication_slo_breach_total";

/// One applied MusicBrainz packet, with Unix epoch timestamps in seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketRecord {
    pub packet_id: String,
    pub sequence: i64,
    pub published_at: i64,
    pub applied_at: i64,
    pub lag_secs: i64,
}

/// Per-packet ledger. Recording a sequence again replaces its prior record,
/// making a retried applier write idempotent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PacketLedger {
    records: Vec<PacketRecord>,
}

impl PacketLedger {
    /// Record an applied packet and calculate its lag from the timestamps.
    pub fn record(&mut self, sequence: i64, published_at: i64, applied_at: i64) {
        self.record_packet(sequence.to_string(), sequence, published_at, applied_at);
    }

    /// Record a packet with its upstream identifier and sequence number.
    ///
    /// Callers must satisfy the durable schema's constraints (migration
    /// 0016): `sequence >= 0`, `applied_at >= published_at`, and `packet_id`
    /// unique across sequences. This in-memory model does not re-validate
    /// them; in particular clock skew (`applied_at < published_at`) clamps
    /// `lag_secs` to 0 here while the applier's insert is rejected by the
    /// SQL `CHECK`, so pre-validate before recording.
    pub fn record_packet(
        &mut self,
        packet_id: String,
        sequence: i64,
        published_at: i64,
        applied_at: i64,
    ) {
        let record = PacketRecord {
            packet_id,
            sequence,
            published_at,
            applied_at,
            lag_secs: applied_at.saturating_sub(published_at),
        };
        if let Some(existing) = self.records.iter_mut().find(|r| r.sequence == sequence) {
            *existing = record;
        } else {
            self.records.push(record);
        }
    }

    /// Return records in packet-sequence order, independent of insertion order.
    pub fn records(&self) -> Vec<PacketRecord> {
        let mut records = self.records.clone();
        records.sort_by_key(|record| record.sequence);
        records
    }
}

/// Machine-readable SLO violation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplicationBreach {
    pub kind: &'static str,
    pub sequence: Option<i64>,
    pub lag_secs: Option<i64>,
}

/// Snapshot from one probe evaluation. `structured_log` is a JSON event; the
/// metric is a counter incremented by `breach_count` when breaches exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationSloReport {
    pub breaches: Vec<ReplicationBreach>,
    pub structured_log: String,
    pub metric_name: String,
    pub breach_count: usize,
}

/// Evaluates the ledger through `latest_sequence`. Missing sequences are
/// reported only between the first and latest observed sequences, since no
/// earlier baseline is implied by this ledger. An empty ledger with a
/// non-negative `latest_sequence` is an `empty_ledger` breach, not a health
/// signal: no applied packet means no baseline, so a total stall must not
/// read as SLO-healthy (pass a negative `latest_sequence` when nothing has
/// been published yet). `now` is reserved for probes that later account for
/// published-but-not-yet-applied packets; until then such packets surface
/// only as `missing_sequence` breaches and are excluded from lag/p95.
/// `slo_secs` is the lag budget a packet must stay within; it defaults to
/// [`REPLICATION_LAG_SLO_SECS`] and exists as a field so callers (and tests)
/// can evaluate against a tighter budget without changing the ADR constant.
#[derive(Debug, Clone, Copy)]
pub struct ReplicationSloProbe {
    pub slo_secs: i64,
}

impl Default for ReplicationSloProbe {
    fn default() -> Self {
        Self {
            slo_secs: REPLICATION_LAG_SLO_SECS,
        }
    }
}

impl ReplicationSloProbe {
    pub fn evaluate(
        &self,
        ledger: &PacketLedger,
        latest_sequence: i64,
        _now: i64,
    ) -> ReplicationSloReport {
        let records = ledger.records();
        let mut breaches = Vec::new();
        for record in &records {
            if record.lag_secs > self.slo_secs {
                breaches.push(ReplicationBreach {
                    kind: "lag_exceeded",
                    sequence: Some(record.sequence),
                    lag_secs: Some(record.lag_secs),
                });
            }
        }

        if records.is_empty() {
            if latest_sequence >= 0 {
                breaches.push(ReplicationBreach {
                    kind: "empty_ledger",
                    sequence: None,
                    lag_secs: None,
                });
            }
        } else {
            // Enumerate gaps arithmetically from the sorted present
            // sequences instead of scanning first..=latest: the work is
            // bounded by the record count plus the reported-breaches cap,
            // never by a caller-supplied cursor span.
            let mut reported_missing = 0usize;
            let mut truncated = false;
            let mut report_gap = |start: i64, end: i64, breaches: &mut Vec<ReplicationBreach>| {
                let mut sequence = start;
                while sequence <= end {
                    if reported_missing >= MAX_MISSING_SEQUENCE_BREACHES {
                        truncated = true;
                        return;
                    }
                    breaches.push(ReplicationBreach {
                        kind: "missing_sequence",
                        sequence: Some(sequence),
                        lag_secs: None,
                    });
                    reported_missing += 1;
                    match sequence.checked_add(1) {
                        Some(next) => sequence = next,
                        None => return,
                    }
                }
            };
            let mut expected = records[0].sequence;
            for record in &records {
                if record.sequence > expected {
                    // Clip to latest_sequence: sequences past the reported
                    // latest are not yet missing.
                    let gap_end = (record.sequence - 1).min(latest_sequence);
                    if gap_end >= expected {
                        report_gap(expected, gap_end, &mut breaches);
                    }
                }
                expected = record.sequence.saturating_add(1);
            }
            if latest_sequence >= expected {
                report_gap(expected, latest_sequence, &mut breaches);
            }
            if truncated {
                breaches.push(ReplicationBreach {
                    kind: "missing_sequence_truncated",
                    sequence: None,
                    lag_secs: None,
                });
            }
        }

        let breach_count = breaches.len();
        let structured_log = serde_json::json!({
            "event": "mb_replication_slo_breach",
            "breach_count": breach_count,
            "breaches": breaches,
        })
        .to_string();
        ReplicationSloReport {
            breaches,
            structured_log,
            metric_name: BREACH_METRIC_NAME.to_string(),
            breach_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ledger_is_a_breach_not_a_pass() {
        let ledger = PacketLedger::default();
        let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 0);
        assert_eq!(report.breach_count, 1);
        assert_eq!(report.breaches[0].kind, "empty_ledger");
    }

    #[test]
    fn empty_ledger_with_nothing_published_passes() {
        let ledger = PacketLedger::default();
        let report = ReplicationSloProbe::default().evaluate(&ledger, -1, 0);
        assert_eq!(report.breach_count, 0);
    }

    #[test]
    fn huge_latest_sequence_truncates_instead_of_scanning() {
        let mut ledger = PacketLedger::default();
        ledger.record(0, 0, 0);
        let report = ReplicationSloProbe::default().evaluate(&ledger, i64::MAX, 0);
        assert_eq!(
            report.breach_count,
            MAX_MISSING_SEQUENCE_BREACHES + 1,
            "cap of enumerated missing sequences plus one truncation breach"
        );
        assert_eq!(
            report.breaches.last().map(|b| b.kind),
            Some("missing_sequence_truncated")
        );
    }

    #[test]
    fn gaps_beyond_latest_sequence_are_not_missing() {
        let mut ledger = PacketLedger::default();
        ledger.record(7, 0, 0);
        ledger.record(9, 0, 0);
        let report = ReplicationSloProbe::default().evaluate(&ledger, 7, 0);
        assert_eq!(report.breach_count, 0);
    }
}
