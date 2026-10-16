//! Per-packet replication accounting and an in-memory SLO probe.
//!
//! The applier persists the same fields in `mb_replication_packet` (migration
//! 0004); this module keeps the probe deterministic and independently testable.
use serde::Serialize;

/// Replication lag budget promised by ADR 0007.
pub const REPLICATION_LAG_SLO_SECS: i64 = 2 * 60 * 60;
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
/// earlier baseline is implied by this ledger. `now` is reserved for probes
/// that later account for published-but-not-yet-applied packets.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplicationSloProbe;

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
            if record.lag_secs > REPLICATION_LAG_SLO_SECS {
                breaches.push(ReplicationBreach {
                    kind: "lag_exceeded",
                    sequence: Some(record.sequence),
                    lag_secs: Some(record.lag_secs),
                });
            }
        }

        if let Some(first_sequence) = records.first().map(|record| record.sequence) {
            let present: std::collections::HashSet<i64> =
                records.iter().map(|record| record.sequence).collect();
            for sequence in first_sequence..=latest_sequence {
                if !present.contains(&sequence) {
                    breaches.push(ReplicationBreach {
                        kind: "missing_sequence",
                        sequence: Some(sequence),
                        lag_secs: None,
                    });
                }
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
