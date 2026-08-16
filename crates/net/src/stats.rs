use std::sync::Arc;
use std::time::Instant;

use webrtc::peer_connection::RTCPeerConnection;

/// Collects WebRTC stats from all peer connections and aggregates worst-viewer
/// metrics for adaptive bitrate control.
///
/// ABR uses packet loss for tier selection (webrtc-rs 0.17.1 has no bandwidth
/// estimation). Throughput is measured from ICETransportStats.bytes_sent deltas
/// for stats display only.
#[derive(Default)]
pub struct StatsCollector {
    /// Previous total bytes_sent across all peer connections (for throughput delta).
    prev_bytes_sent: u64,
    /// Timestamp of previous bytes_sent reading.
    prev_timestamp: Option<Instant>,
}

impl StatsCollector {
    /// Create a new StatsCollector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Detect if any peer connection is using a TURN relay candidate.
    /// Returns true if ANY active peer uses a relay for its nominated candidate pair.
    pub async fn detect_relay(peer_connections: &[Arc<RTCPeerConnection>]) -> bool {
        let mut all_stats = Vec::with_capacity(peer_connections.len());
        for pc in peer_connections {
            all_stats.push(pc.get_stats().await);
        }
        Self::detect_relay_from_stats(&all_stats)
    }

    /// Detect relay from pre-fetched stats snapshots (D-15).
    /// Avoids a second get_stats() call when used from collect_worst_stats.
    ///
    /// Returns true if EITHER endpoint of any peer's nominated candidate pair
    /// is a relay candidate. Checking only the local side misses asymmetric
    /// topologies where we reach the viewer via TURN but our own local
    /// candidate is host/srflx.
    fn detect_relay_from_stats(all_stats: &[webrtc::stats::StatsReport]) -> bool {
        use webrtc::ice::candidate::CandidateType;
        use webrtc::stats::StatsReportType;

        for stats in all_stats {
            let mut pair_ids: Option<(String, String)> = None;
            for report in stats.reports.values() {
                if let StatsReportType::CandidatePair(pair_stats) = report
                    && pair_stats.nominated
                {
                    pair_ids = Some((
                        pair_stats.local_candidate_id.clone(),
                        pair_stats.remote_candidate_id.clone(),
                    ));
                    break;
                }
            }
            let Some((local_id, remote_id)) = pair_ids else {
                continue;
            };
            for report in stats.reports.values() {
                match report {
                    StatsReportType::LocalCandidate(c)
                        if c.id == local_id && c.candidate_type == CandidateType::Relay =>
                    {
                        return true;
                    }
                    StatsReportType::RemoteCandidate(c)
                        if c.id == remote_id && c.candidate_type == CandidateType::Relay =>
                    {
                        return true;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// Collect the worst-case network stats across all peer connections.
    ///
    /// Returns `None` if no stats are available (e.g., no connections or
    /// connections not yet established). Uses the worst RTT and worst packet
    /// loss across all viewers to drive conservative ABR decisions.
    /// `throughput_kbps` is real measured throughput for stats display.
    pub async fn collect_worst_stats(
        &mut self,
        peer_connections: &[Arc<RTCPeerConnection>],
    ) -> Option<encode::NetworkStats> {
        if peer_connections.is_empty() {
            return None;
        }

        // Collect stats once per connection (D-15: was collected twice -- once for
        // worst-stats and once for relay detection)
        let mut all_stats = Vec::with_capacity(peer_connections.len());
        for pc in peer_connections {
            all_stats.push(pc.get_stats().await);
        }

        let mut worst_rtt: Option<f64> = None;
        let mut worst_loss: Option<f64> = None;
        let mut found_any = false;
        let mut total_bytes_sent: u64 = 0;

        for stats in &all_stats {
            for report in stats.reports.values() {
                use webrtc::stats::StatsReportType;
                match report {
                    StatsReportType::RemoteInboundRTP(rtp_stats) => {
                        found_any = true;
                        if let Some(rtt) = rtp_stats.round_trip_time {
                            worst_rtt = Some(worst_rtt.map_or(rtt, |prev: f64| prev.max(rtt)));
                        }
                        let loss = rtp_stats.fraction_lost;
                        worst_loss = Some(worst_loss.map_or(loss, |prev: f64| prev.max(loss)));
                    }
                    StatsReportType::Transport(transport_stats) => {
                        total_bytes_sent += transport_stats.bytes_sent as u64;
                    }
                    _ => {}
                }
            }
        }

        if !found_any {
            // Connections exist but no RTP stats yet: bail so ABR keeps its last
            // values rather than reacting to a half-established connection.
            tracing::debug!("No WebRTC stats available yet, returning conservative defaults");
            return None;
        }

        // Compute throughput from bytes_sent delta
        let now = Instant::now();
        let throughput_kbps = if let Some(prev_ts) = self.prev_timestamp {
            let elapsed = now.duration_since(prev_ts);
            let elapsed_secs = elapsed.as_secs_f64();
            if elapsed_secs > 0.0 && total_bytes_sent >= self.prev_bytes_sent {
                let delta_bytes = total_bytes_sent - self.prev_bytes_sent;
                // bytes -> kilobits: * 8 / 1000
                let kbps = (delta_bytes as f64 * 8.0) / (elapsed_secs * 1000.0);
                kbps as u32
            } else {
                0
            }
        } else {
            // First collection: no previous reading to delta against, report 0.
            0
        };
        self.prev_bytes_sent = total_bytes_sent;
        self.prev_timestamp = Some(now);

        let has_relay = Self::detect_relay_from_stats(&all_stats);
        let has_measured_rtt = worst_rtt.is_some();

        Some(encode::NetworkStats {
            // webrtc-rs round_trip_time is already in ms; keep conservative default for ABR
            rtt_ms: worst_rtt.unwrap_or(50.0),
            packet_loss_pct: worst_loss.unwrap_or(0.0) * 100.0,
            throughput_kbps,
            has_relay,
            has_measured_rtt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::time::Instant;
    use webrtc::ice::candidate::{CandidatePairState, CandidateType};
    use webrtc::ice::network_type::NetworkType;
    use webrtc::stats::{
        ICECandidatePairStats, ICECandidateStats, RTCStatsType, StatsReport, StatsReportType,
    };

    #[tokio::test]
    async fn collect_worst_stats_empty_returns_none() {
        let mut collector = StatsCollector::new();
        let result = collector.collect_worst_stats(&[]).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn detect_relay_empty_returns_false() {
        let result = StatsCollector::detect_relay(&[]).await;
        assert!(!result);
    }

    #[test]
    fn detect_relay_from_stats_empty() {
        assert!(!StatsCollector::detect_relay_from_stats(&[]));
    }

    fn make_pair(local_id: &str, remote_id: &str, nominated: bool) -> ICECandidatePairStats {
        ICECandidatePairStats {
            timestamp: Instant::now(),
            stats_type: RTCStatsType::CandidatePair,
            id: format!("{}-{}", local_id, remote_id),
            local_candidate_id: local_id.to_string(),
            remote_candidate_id: remote_id.to_string(),
            state: CandidatePairState::Succeeded,
            nominated,
            packets_sent: 0,
            packets_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
            last_packet_sent_timestamp: Instant::now(),
            last_packet_received_timestamp: Instant::now(),
            total_round_trip_time: 0.0,
            current_round_trip_time: 0.0,
            available_outgoing_bitrate: 0.0,
            available_incoming_bitrate: 0.0,
            requests_received: 0,
            requests_sent: 0,
            responses_received: 0,
            responses_sent: 0,
            consent_requests_sent: 0,
            circuit_breaker_trigger_count: 0,
            consent_expired_timestamp: Instant::now(),
            first_request_timestamp: Instant::now(),
            last_request_timestamp: Instant::now(),
            retransmissions_sent: 0,
        }
    }

    fn make_candidate(id: &str, ct: CandidateType, is_local: bool) -> ICECandidateStats {
        ICECandidateStats {
            timestamp: Instant::now(),
            stats_type: if is_local {
                RTCStatsType::LocalCandidate
            } else {
                RTCStatsType::RemoteCandidate
            },
            id: id.to_string(),
            candidate_type: ct,
            deleted: false,
            ip: "0.0.0.0".into(),
            network_type: NetworkType::Udp4,
            port: 0,
            priority: 0,
            relay_protocol: String::new(),
            url: String::new(),
        }
    }

    fn report_with(
        pair: ICECandidatePairStats,
        local: ICECandidateStats,
        remote: ICECandidateStats,
    ) -> StatsReport {
        let mut reports = HashMap::new();
        reports.insert("pair".into(), StatsReportType::CandidatePair(pair));
        reports.insert("local".into(), StatsReportType::LocalCandidate(local));
        reports.insert("remote".into(), StatsReportType::RemoteCandidate(remote));
        StatsReport { reports }
    }

    #[test]
    fn detect_relay_when_remote_is_relay_local_is_host() {
        // Regression: asymmetric ICE where we reach the viewer via TURN but
        // our local candidate is host/srflx. The path is still relayed.
        let report = report_with(
            make_pair("L1", "R1", true),
            make_candidate("L1", CandidateType::Host, true),
            make_candidate("R1", CandidateType::Relay, false),
        );
        assert!(StatsCollector::detect_relay_from_stats(&[report]));
    }

    #[test]
    fn detect_relay_when_local_is_relay_remote_is_host() {
        let report = report_with(
            make_pair("L1", "R1", true),
            make_candidate("L1", CandidateType::Relay, true),
            make_candidate("R1", CandidateType::Host, false),
        );
        assert!(StatsCollector::detect_relay_from_stats(&[report]));
    }

    #[test]
    fn no_relay_when_both_endpoints_direct() {
        let report = report_with(
            make_pair("L1", "R1", true),
            make_candidate("L1", CandidateType::ServerReflexive, true),
            make_candidate("R1", CandidateType::Host, false),
        );
        assert!(!StatsCollector::detect_relay_from_stats(&[report]));
    }

    #[test]
    fn non_nominated_relay_pair_ignored() {
        // A non-nominated pair with a relay candidate should not count.
        let report = report_with(
            make_pair("L1", "R1", false),
            make_candidate("L1", CandidateType::Host, true),
            make_candidate("R1", CandidateType::Relay, false),
        );
        assert!(!StatsCollector::detect_relay_from_stats(&[report]));
    }
}
