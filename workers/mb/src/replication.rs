//! Per-packet replication accounting and an in-memory SLO probe.
//!
//! The applier persists the same fields in `mb_replication_packet` (migration
//! 0017); this module keeps the probe deterministic and independently testable.
use serde::Serialize;

/// Replication lag budget promised by ADR 0007.
pub const REPLICATION_LAG_SLO_SECS: i64 = 2 * 60 * 60;
/// Cap on individually enumerated `missing_sequence` breaches per evaluation.
/// A corrupt `latest_sequence` cursor must not turn one probe call into an
/// unbounded scan; past the cap the report carries one
/// `missing_sequence_truncated` breach instead of silently stopping.
pub const MAX_MISSING_SEQUENCE_BREACHES: usize = 10_000;
/// Cap on entries in the structured log's `breaches` array, after compressing
/// contiguous unknown `missing_sequence` runs into ranges. The full breach
/// list stays on [`ReplicationSloReport::breaches`] for callers; the log is
/// bounded so one event stays well under the 256 KB Workers per-request log
/// limit and its `truncated` marker is never the part that gets cut off.
pub const MAX_STRUCTURED_LOG_BREACHES: usize = 100;
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

/// One published-but-not-yet-applied packet, with its publication time
/// (Unix epoch seconds). The probe uses it to tell an in-flight packet
/// (still inside its lag budget) from a stalled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPacket {
    pub sequence: i64,
    pub published_at: i64,
}

/// Per-packet ledger. Recording a sequence again replaces its prior record,
/// making a retried applier write idempotent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PacketLedger {
    records: Vec<PacketRecord>,
    pending: Vec<PendingPacket>,
}

impl PacketLedger {
    /// Record an applied packet and calculate its lag from the timestamps.
    pub fn record(&mut self, sequence: i64, published_at: i64, applied_at: i64) {
        self.record_packet(sequence.to_string(), sequence, published_at, applied_at);
    }

    /// Record a packet with its upstream identifier and sequence number.
    ///
    /// Callers must satisfy the durable schema's constraints (migration
    /// 0017): `sequence >= 0`, `applied_at >= published_at`, and `packet_id`
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
        self.pending.retain(|p| p.sequence != sequence);
    }

    /// Record a packet the publication feed has published but the applier
    /// has not applied yet. Recording the same sequence again replaces its
    /// publication time; applying the packet (via [`Self::record`]) clears
    /// the pending entry.
    pub fn record_published(&mut self, sequence: i64, published_at: i64) {
        let pending = PendingPacket {
            sequence,
            published_at,
        };
        if let Some(existing) = self.pending.iter_mut().find(|p| p.sequence == sequence) {
            *existing = pending;
        } else {
            self.pending.push(pending);
        }
    }

    /// Publication time of a pending packet, if the feed reported one.
    pub fn pending_published_at(&self, sequence: i64) -> Option<i64> {
        self.pending
            .iter()
            .find(|p| p.sequence == sequence)
            .map(|p| p.published_at)
    }

    /// Return records in packet-sequence order, independent of insertion order.
    pub fn records(&self) -> Vec<PacketRecord> {
        let mut records = self.records.clone();
        records.sort_by_key(|record| record.sequence);
        records
    }

    /// Return pending packets in packet-sequence order.
    pub fn pending(&self) -> Vec<PendingPacket> {
        let mut pending = self.pending.clone();
        pending.sort_by_key(|p| p.sequence);
        pending
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
    /// Missing sequences known to be published and still inside their lag
    /// budget at evaluation time: in flight, not breaches.
    pub pending_count: usize,
}

/// Evaluates the ledger through `latest_sequence`. Missing sequences are
/// reported only between the first and latest observed sequences, since no
/// earlier baseline is implied by this ledger. An empty ledger with a
/// non-negative `latest_sequence` is an `empty_ledger` breach, not a health
/// signal: no applied packet means no baseline, so a total stall must not
/// read as SLO-healthy (pass a negative `latest_sequence` when nothing has
/// been published yet). A missing sequence the ledger knows as pending
/// (see [`PacketLedger::record_published`]) breaches only once
/// `now - published_at > slo_secs` (ADR 0007: applied within the budget of
/// publication); inside the budget it counts toward `pending_count`
/// instead. A missing sequence with unknown publication time still
/// breaches as `missing_sequence`: without evidence it is in flight, a gap
/// must not read as healthy.
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
        now: i64,
    ) -> ReplicationSloReport {
        let records = ledger.records();
        let pending = ledger.pending();
        let mut breaches = Vec::new();
        let mut pending_count = 0usize;
        for record in &records {
            if record.lag_secs > self.slo_secs {
                breaches.push(ReplicationBreach {
                    kind: "lag_exceeded",
                    sequence: Some(record.sequence),
                    lag_secs: Some(record.lag_secs),
                });
            }
        }

        let mut truncated = false;
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
            // bounded by the record + pending counts plus the
            // reported-breaches cap, never by a caller-supplied cursor span.
            let mut reported_missing = 0usize;
            let mut report_gap = |start: i64,
                                  end: i64,
                                  breaches: &mut Vec<ReplicationBreach>,
                                  pending_count: &mut usize| {
                // Pending packets inside this gap, in sequence order.
                let in_gap: Vec<&PendingPacket> = pending
                    .iter()
                    .filter(|p| p.sequence >= start && p.sequence <= end)
                    .collect();
                let mut cursor = start;
                for packet in &in_gap {
                    if packet.sequence > cursor {
                        enumerate_unknown(
                            cursor,
                            packet.sequence - 1,
                            breaches,
                            &mut reported_missing,
                            &mut truncated,
                        );
                    }
                    let age = now.saturating_sub(packet.published_at);
                    if age > self.slo_secs {
                        push_missing(
                            packet.sequence,
                            Some(age),
                            breaches,
                            &mut reported_missing,
                            &mut truncated,
                        );
                    } else {
                        *pending_count += 1;
                    }
                    match packet.sequence.checked_add(1) {
                        Some(next) => cursor = next,
                        None => return,
                    }
                }
                if end >= cursor {
                    enumerate_unknown(cursor, end, breaches, &mut reported_missing, &mut truncated);
                }
            };
            let mut expected = records[0].sequence;
            for record in &records {
                if record.sequence > expected {
                    // Clip to latest_sequence: sequences past the reported
                    // latest are not yet missing.
                    let gap_end = (record.sequence - 1).min(latest_sequence);
                    if gap_end >= expected {
                        report_gap(expected, gap_end, &mut breaches, &mut pending_count);
                    }
                }
                expected = record.sequence.saturating_add(1);
            }
            if latest_sequence >= expected {
                report_gap(expected, latest_sequence, &mut breaches, &mut pending_count);
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
        let structured_log = build_structured_log(&breaches, pending_count, truncated);
        ReplicationSloReport {
            breaches,
            structured_log,
            metric_name: BREACH_METRIC_NAME.to_string(),
            breach_count,
            pending_count,
        }
    }
}

fn push_missing(
    sequence: i64,
    lag_secs: Option<i64>,
    breaches: &mut Vec<ReplicationBreach>,
    reported_missing: &mut usize,
    truncated: &mut bool,
) {
    if *reported_missing >= MAX_MISSING_SEQUENCE_BREACHES {
        *truncated = true;
        return;
    }
    breaches.push(ReplicationBreach {
        kind: "missing_sequence",
        sequence: Some(sequence),
        lag_secs,
    });
    *reported_missing += 1;
}

fn enumerate_unknown(
    from: i64,
    to: i64,
    breaches: &mut Vec<ReplicationBreach>,
    reported_missing: &mut usize,
    truncated: &mut bool,
) {
    let mut sequence = from;
    while sequence <= to {
        if *reported_missing >= MAX_MISSING_SEQUENCE_BREACHES {
            *truncated = true;
            return;
        }
        push_missing(sequence, None, breaches, reported_missing, truncated);
        match sequence.checked_add(1) {
            Some(next) => sequence = next,
            None => return,
        }
    }
}

/// Build the bounded JSON log event. Contiguous unknown-publication
/// `missing_sequence` breaches compress into one
/// `missing_sequence_range` entry (`first`/`last`/`count`); overdue
/// pending breaches keep their per-packet age and stay individual. After
/// compression at most [`MAX_STRUCTURED_LOG_BREACHES`] entries are logged,
/// with `missing_count`, `pending_count` and `truncated` making any
/// elision explicit. `breach_count` always equals the full breach list.
fn build_structured_log(
    breaches: &[ReplicationBreach],
    pending_count: usize,
    enumeration_truncated: bool,
) -> String {
    let missing_count = breaches
        .iter()
        .filter(|b| b.kind == "missing_sequence")
        .count();
    let mut logged: Vec<serde_json::Value> = Vec::new();
    let mut log_truncated = enumeration_truncated;
    let mut iter = breaches.iter().peekable();
    while let Some(breach) = iter.next() {
        if logged.len() >= MAX_STRUCTURED_LOG_BREACHES {
            log_truncated = true;
            break;
        }
        if breach.kind == "missing_sequence" && breach.lag_secs.is_none() {
            if let Some(first) = breach.sequence {
                let mut last = first;
                let mut count: u64 = 1;
                while let Some(next) = iter.peek() {
                    if next.kind == "missing_sequence"
                        && next.lag_secs.is_none()
                        && next.sequence == last.checked_add(1)
                    {
                        last = next.sequence.unwrap_or(last);
                        count += 1;
                        iter.next();
                    } else {
                        break;
                    }
                }
                if count > 1 {
                    logged.push(serde_json::json!({
                        "kind": "missing_sequence_range",
                        "first": first,
                        "last": last,
                        "count": count,
                    }));
                    continue;
                }
            }
        }
        logged.push(serde_json::json!({
            "kind": breach.kind,
            "sequence": breach.sequence,
            "lag_secs": breach.lag_secs,
        }));
    }
    // The enumeration-truncation breach is the evidence that the missing
    // list was capped; make sure it survives the log-entry cap too.
    if enumeration_truncated
        && !logged
            .iter()
            .any(|entry| entry["kind"] == "missing_sequence_truncated")
    {
        if logged.len() >= MAX_STRUCTURED_LOG_BREACHES {
            logged.pop();
        }
        logged.push(serde_json::json!({
            "kind": "missing_sequence_truncated",
            "sequence": null,
            "lag_secs": null,
        }));
    }
    serde_json::json!({
        "event": "mb_replication_slo_breach",
        "breach_count": breaches.len(),
        "missing_count": missing_count,
        "pending_count": pending_count,
        "truncated": log_truncated,
        "logged_breach_count": logged.len(),
        "breaches": logged,
    })
    .to_string()
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

    #[test]
    fn in_flight_packet_inside_budget_is_pending_not_a_breach() {
        let mut ledger = PacketLedger::default();
        for sequence in 1..=9 {
            ledger.record(sequence, 0, 600);
        }
        // Packet 10 published at T=10_000, probe runs 60 s later.
        ledger.record_published(10, 10_000);
        let report = ReplicationSloProbe::default().evaluate(&ledger, 10, 10_060);
        assert_eq!(report.breach_count, 0);
        assert_eq!(report.pending_count, 1);
        let event: serde_json::Value = serde_json::from_str(&report.structured_log).unwrap();
        assert_eq!(event["pending_count"], 1);
        assert_eq!(event["breach_count"], 0);
    }

    #[test]
    fn pending_packet_breaches_only_after_the_budget_from_publication() {
        let mut ledger = PacketLedger::default();
        ledger.record(1, 0, 600);
        ledger.record_published(2, 10_000);
        let probe = ReplicationSloProbe::default();
        // Exactly at the budget passes; one second past it breaches.
        let at_budget = probe.evaluate(&ledger, 2, 10_000 + REPLICATION_LAG_SLO_SECS);
        assert_eq!(at_budget.breach_count, 0);
        assert_eq!(at_budget.pending_count, 1);
        let stalled = probe.evaluate(&ledger, 2, 10_000 + 3 * 60 * 60);
        assert_eq!(stalled.breach_count, 1);
        assert_eq!(stalled.breaches[0].kind, "missing_sequence");
        assert_eq!(stalled.breaches[0].sequence, Some(2));
        assert_eq!(stalled.breaches[0].lag_secs, Some(3 * 60 * 60));
        assert_eq!(stalled.pending_count, 0);
    }

    #[test]
    fn unknown_publication_gap_still_breaches_next_to_a_pending_packet() {
        let mut ledger = PacketLedger::default();
        ledger.record(7, 0, 0);
        ledger.record(10, 0, 0);
        // 8 is in flight (published 60 s ago); 9's publication is unknown.
        ledger.record_published(8, 1_000);
        let report = ReplicationSloProbe::default().evaluate(&ledger, 10, 1_060);
        assert_eq!(report.pending_count, 1);
        assert_eq!(report.breach_count, 1);
        assert_eq!(report.breaches[0].sequence, Some(9));
        assert_eq!(report.breaches[0].lag_secs, None);
    }

    #[test]
    fn applying_a_packet_clears_its_pending_entry() {
        let mut ledger = PacketLedger::default();
        ledger.record_published(5, 100);
        assert_eq!(ledger.pending_published_at(5), Some(100));
        ledger.record(5, 100, 200);
        assert_eq!(ledger.pending_published_at(5), None);
        assert!(ledger.pending().is_empty());
    }

    #[test]
    fn worst_case_structured_log_stays_well_under_the_workers_log_limit() {
        let mut ledger = PacketLedger::default();
        ledger.record(0, 0, 0);
        let report = ReplicationSloProbe::default().evaluate(&ledger, i64::MAX, 0);
        assert!(
            report.structured_log.len() < 256 * 1024,
            "structured log is {} bytes",
            report.structured_log.len()
        );
        let event: serde_json::Value = serde_json::from_str(&report.structured_log).unwrap();
        assert_eq!(event["event"], "mb_replication_slo_breach");
        assert_eq!(event["breach_count"], report.breaches.len());
        assert_eq!(event["missing_count"], MAX_MISSING_SEQUENCE_BREACHES);
        assert_eq!(event["truncated"], true);
        let logged = event["breaches"].as_array().unwrap();
        assert!(logged.len() <= MAX_STRUCTURED_LOG_BREACHES);
        // The 10,000 enumerated unknowns compress into one range entry,
        // and the truncation breach survives to the end of the log.
        assert_eq!(logged[0]["kind"], "missing_sequence_range");
        assert_eq!(logged[0]["first"], 1);
        assert_eq!(logged[0]["count"], MAX_MISSING_SEQUENCE_BREACHES);
        assert!(logged
            .iter()
            .any(|entry| entry["kind"] == "missing_sequence_truncated"));
    }

    #[test]
    fn structured_log_caps_many_individual_breaches() {
        let mut ledger = PacketLedger::default();
        for sequence in 0..(MAX_STRUCTURED_LOG_BREACHES as i64 + 50) {
            ledger.record(sequence, 0, REPLICATION_LAG_SLO_SECS + 1);
        }
        let latest = MAX_STRUCTURED_LOG_BREACHES as i64 + 49;
        let report = ReplicationSloProbe::default().evaluate(&ledger, latest, 0);
        assert_eq!(report.breach_count, MAX_STRUCTURED_LOG_BREACHES + 50);
        let event: serde_json::Value = serde_json::from_str(&report.structured_log).unwrap();
        assert_eq!(event["truncated"], true);
        assert_eq!(event["logged_breach_count"], MAX_STRUCTURED_LOG_BREACHES);
        assert!(report.structured_log.len() < 256 * 1024);
    }
}
