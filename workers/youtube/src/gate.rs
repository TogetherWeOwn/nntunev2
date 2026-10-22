//! GO/NO-GO gate: byte-fetch failure rate from Worker egress.
//!
//! The spike's gate question: are label-video stream URLs bound to the
//! extraction IP? If more than 5% of byte fetches from Worker egress fail,
//! bytes would need the proxy too, which is uneconomic — that is a NO-GO.
//! An empty measurement (zero fetches) fails closed: no data, no GO.

/// Maximum tolerable byte-fetch failure rate: 5%.
pub const GATE_FAILURE_THRESHOLD: f64 = 0.05;

/// The gate verdict over one measurement run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateVerdict {
    /// Failed byte fetches.
    pub failures: u64,
    /// Total byte fetches attempted.
    pub total: u64,
    /// `failures / total` (1.0 when `total == 0`: fail closed).
    pub failure_rate: f64,
    /// True when `failure_rate <= 0.05` with at least one fetch.
    pub go: bool,
}

/// Evaluate the gate over `failures` failed fetches out of `total`.
pub fn evaluate_gate(failures: u64, total: u64) -> GateVerdict {
    if total == 0 {
        return GateVerdict {
            failures,
            total,
            failure_rate: 1.0,
            go: false,
        };
    }
    let failure_rate = failures as f64 / total as f64;
    GateVerdict {
        failures,
        total,
        failure_rate,
        go: failure_rate <= GATE_FAILURE_THRESHOLD,
    }
}

/// Day-1 corpus class of a measured video. Classes are OMV/ATV
/// membership proven by `musicVideoType` plus the allowlist/badge gate
/// (see [`crate::select`]); the serialized names stay stable for the
/// JSONL measurement record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCategory {
    /// Official music video (`musicVideoType` OMV).
    LabelMusic,
    /// Official audio track (`musicVideoType` ATV).
    AudioTrack,
}

/// One byte-probe result (`kind` is `head`, `mid` or `full`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProbeRecord {
    pub kind: String,
    pub status: u16,
    pub bytes: u64,
    pub ok: bool,
}

/// The per-video JSONL measurement record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VideoMeasurement {
    pub video_id: String,
    pub category: VideoCategory,
    pub player_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default)]
    pub probes: Vec<ProbeRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_percent_is_go() {
        let verdict = evaluate_gate(5, 100);
        assert!((verdict.failure_rate - 0.05).abs() < f64::EPSILON);
        assert!(verdict.go);
    }

    #[test]
    fn over_five_percent_is_no_go() {
        let verdict = evaluate_gate(6, 100);
        assert!(!verdict.go);
    }

    #[test]
    fn empty_measurement_fails_closed() {
        let verdict = evaluate_gate(0, 0);
        assert!(!verdict.go);
    }

    #[test]
    fn measurement_serde_roundtrip() {
        let measurement = VideoMeasurement {
            video_id: "dQw4w9WgXcQ".to_string(),
            category: VideoCategory::LabelMusic,
            player_status: "OK".to_string(),
            ip: Some("203.0.113.7".to_string()),
            probes: vec![ProbeRecord {
                kind: "head".to_string(),
                status: 206,
                bytes: 262_144,
                ok: true,
            }],
        };
        let json = serde_json::to_value(&measurement).expect("serialize");
        assert_eq!(json["video_id"], "dQw4w9WgXcQ");
        assert_eq!(json["category"], "label_music");
        assert_eq!(json["probes"][0]["status"], 206);
        let back: VideoMeasurement = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, measurement);
    }
}
