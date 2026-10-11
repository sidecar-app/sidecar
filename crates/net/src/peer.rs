use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::track::track_local::TrackLocal;
use webrtc_ice::mdns::MulticastDnsMode;

use crate::error::NetError;
use crate::types::{IceServerConfig, SignalingMessage};

/// Drop TURN servers that aren't reachable from this host.
///
/// An unreachable TURN server causes a ~5s ICE gather stall that blocks
/// host/srflx candidates on LAN, so it's better left out. Probes run
/// concurrently (500ms TCP connect, or a DNS lookup for UDP-only servers);
/// non-TURN entries are kept as-is. Call once per session: the result is
/// reused for every viewer's peer connection.
pub async fn filter_reachable_ice_servers(ice_servers: &[IceServerConfig]) -> Vec<IceServerConfig> {
    let probes = ice_servers.iter().map(|config| async move {
        let Some(addr) = turn_probe_addr(&config.urls) else {
            return true;
        };
        let reachable = if config.urls.contains("transport=udp") {
            match tokio::net::lookup_host(addr.as_str()).await {
                Ok(mut addrs) => addrs.next().is_some(),
                Err(_) => false,
            }
        } else {
            matches!(
                tokio::time::timeout(
                    Duration::from_millis(500),
                    tokio::net::TcpStream::connect(addr.as_str()),
                )
                .await,
                Ok(Ok(_))
            )
        };
        if reachable {
            info!(url = %config.urls, "TURN server reachable, including");
        } else {
            warn!(url = %config.urls, "TURN server unreachable, skipping");
        }
        reachable
    });
    let reachable = futures_util::future::join_all(probes).await;
    ice_servers
        .iter()
        .zip(reachable)
        .filter(|(_, ok)| *ok)
        .map(|(config, _)| config.clone())
        .collect()
}

/// `host:port` to probe for a `turn:`/`turns:` URL, or `None` for other
/// schemes. Fills in the default port (3478, or 5349 for `turns:`) when the
/// URL omits it, since `host` alone isn't a connectable socket address.
fn turn_probe_addr(url: &str) -> Option<String> {
    let (rest, default_port) = match url.strip_prefix("turns:") {
        Some(rest) => (rest, 5349),
        None => (url.strip_prefix("turn:")?, 3478),
    };
    let host_port = rest.split('?').next().unwrap_or(rest);
    // An IPv6 literal (`[::1]`) contains colons itself; only a colon after
    // the closing bracket is a port separator.
    let has_port = match host_port.rfind(']') {
        Some(end) => host_port[end..].contains(':'),
        None => host_port.contains(':'),
    };
    Some(if has_port {
        host_port.to_string()
    } else {
        format!("{host_port}:{default_port}")
    })
}

/// Manages a single viewer's WebRTC peer connection lifecycle.
///
/// Handles SDP offer/answer exchange, ICE candidate gathering and buffering,
/// and connection state monitoring. One `PeerConnectionManager` is created
/// per viewer that joins the session.
pub struct PeerConnectionManager {
    peer_connection: Arc<RTCPeerConnection>,
    viewer_id: String,
    /// Channel to send ICE candidates and SDP back to the signaling task.
    #[allow(dead_code)]
    signaling_tx: mpsc::UnboundedSender<SignalingMessage>,
    /// Queued ICE candidates received before remote description is set.
    pending_candidates: Vec<RTCIceCandidateInit>,
    remote_description_set: bool,
    /// Data channel for annotation messages (created before SDP offer).
    data_channel: Arc<RTCDataChannel>,
}

impl PeerConnectionManager {
    /// Create a new peer connection for a viewer.
    ///
    /// Sets up the WebRTC API with default codecs and interceptors, creates a
    /// peer connection with the provided ICE servers, adds the video track, and
    /// registers callbacks for ICE candidate gathering and connection state changes.
    pub async fn new(
        viewer_id: String,
        ice_servers: &[IceServerConfig],
        video_track: Arc<dyn TrackLocal + Send + Sync>,
        signaling_tx: mpsc::UnboundedSender<SignalingMessage>,
        relay_tx: mpsc::UnboundedSender<(String, String)>,
        dc_state_tx: mpsc::UnboundedSender<(String, String)>,
    ) -> Result<Self, NetError> {
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        // Disable mDNS to avoid "No route to host" errors on networks without
        // multicast support (VPNs, some macOS configs).
        let mut setting_engine = SettingEngine::default();
        setting_engine.set_ice_multicast_dns_mode(MulticastDnsMode::Disabled);

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(setting_engine)
            .build();

        // TURN reachability is probed once per session (see
        // `filter_reachable_ice_servers`), not here: this runs per viewer while
        // the session lock is held.
        let rtc_ice_servers: Vec<RTCIceServer> = ice_servers
            .iter()
            .map(|config| RTCIceServer {
                urls: vec![config.urls.clone()],
                username: config.username.clone().unwrap_or_default(),
                credential: config.credential.clone().unwrap_or_default(),
            })
            .collect();

        let peer_connection = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: rtc_ice_servers,
                ..Default::default()
            })
            .await?,
        );

        // Keep the sender: the RTCP reader below reads PLI from it.
        let rtp_sender = peer_connection.add_track(video_track).await?;

        // Must create the data channel before create_offer so it appears in the SDP.
        let dc_init = RTCDataChannelInit {
            ordered: Some(true),
            ..Default::default()
        };
        let data_channel = peer_connection
            .create_data_channel("annotations", Some(dc_init))
            .await
            .map_err(NetError::WebRtc)?;

        // dc_state_tx carries open/close state up to the UI.
        let dc_label = data_channel.label().to_string();
        let vid_open = viewer_id.clone();
        let state_tx_open = dc_state_tx.clone();
        data_channel.on_open(Box::new(move || {
            let label = dc_label.clone();
            let vid = vid_open.clone();
            let tx = state_tx_open.clone();
            Box::pin(async move {
                info!(viewer_id = %vid, label = %label, "Data channel opened");
                let _ = tx.send((vid, "open".to_string()));
            })
        }));

        let relay = relay_tx.clone();
        let vid_msg = viewer_id.clone();
        data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let tx = relay.clone();
            let vid = vid_msg.clone();
            Box::pin(async move {
                if msg.is_string {
                    let text = String::from_utf8_lossy(&msg.data).to_string();
                    let _ = tx.send((vid, text));
                }
            })
        }));

        let vid_close = viewer_id.clone();
        let state_tx_close = dc_state_tx.clone();
        data_channel.on_close(Box::new(move || {
            let vid = vid_close.clone();
            let tx = state_tx_close.clone();
            Box::pin(async move {
                info!(viewer_id = %vid, "Data channel closed");
                let _ = tx.send((vid, "closed".to_string()));
            })
        }));

        // Forward viewer PLI to the encoder, rate-limited to max 3 PLI per 10s
        // per viewer. Browsers send PLI every ~60ms when they can't decode;
        // without a cap a stalled viewer would force keyframes forever, degrading
        // the stream for other viewers. A sliding window (not a permanent cap)
        // allows recovery after the stall window passes.
        {
            let relay = relay_tx.clone();
            let vid = viewer_id.clone();
            let sender = rtp_sender.clone();
            tokio::spawn(async move {
                let mut pli_timestamps: VecDeque<tokio::time::Instant> = VecDeque::new();
                const MAX_PLI_IN_WINDOW: usize = 3;
                const PLI_WINDOW: Duration = Duration::from_secs(10);
                while let Ok((packets, _)) = sender.read_rtcp().await {
                    for pkt in &packets {
                        let h = pkt.header();
                        // PayloadSpecificFeedback (206), FMT 1 = PLI
                        if h.packet_type == rtcp::header::PacketType::PayloadSpecificFeedback
                            && h.count == 1
                        {
                            let now = tokio::time::Instant::now();
                            while pli_timestamps
                                .front()
                                .is_some_and(|t| now.duration_since(*t) > PLI_WINDOW)
                            {
                                pli_timestamps.pop_front();
                            }
                            if pli_timestamps.len() < MAX_PLI_IN_WINDOW {
                                pli_timestamps.push_back(now);
                                info!(viewer_id = %vid, pli_count = pli_timestamps.len(), max = MAX_PLI_IN_WINDOW, window_secs = PLI_WINDOW.as_secs(), "Forwarding RTCP PLI keyframe request");
                                let _ = relay.send((vid.clone(), r#"{"t":"kf"}"#.to_string()));
                            } else {
                                debug!(viewer_id = %vid, "PLI request dropped (sliding window cap: {} in {}s)", MAX_PLI_IN_WINDOW, PLI_WINDOW.as_secs());
                            }
                        }
                    }
                }
            });
        }

        let tx_ice = signaling_tx.clone();
        let vid = viewer_id.clone();
        peer_connection.on_ice_candidate(Box::new(move |candidate| {
            let tx = tx_ice.clone();
            let target = vid.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    match candidate.to_json() {
                        Ok(init) => {
                            let msg = SignalingMessage::Candidate {
                                candidate: init.candidate,
                                sdp_mid: init.sdp_mid,
                                sdp_m_line_index: init.sdp_mline_index.map(|v| v as u32),
                                target_id: Some(target),
                                from: None,
                            };
                            if let Err(e) = tx.send(msg) {
                                warn!("Failed to send ICE candidate to signaling: {e}");
                            }
                        }
                        Err(e) => {
                            warn!("Failed to serialize ICE candidate: {e}");
                        }
                    }
                }
            })
        }));

        let vid_state = viewer_id.clone();
        peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            let viewer = vid_state.clone();
            Box::pin(async move {
                match state {
                    RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Closed => {
                        warn!(viewer_id = %viewer, state = ?state, "Peer connection state changed");
                    }
                    _ => {
                        info!(viewer_id = %viewer, state = ?state, "Peer connection state changed");
                    }
                }
            })
        }));

        Ok(Self {
            peer_connection,
            viewer_id,
            signaling_tx,
            pending_candidates: Vec::new(),
            remote_description_set: false,
            data_channel,
        })
    }

    /// Create an SDP offer and set it as the local description.
    ///
    /// Returns the SDP string to be sent to the viewer via signaling.
    pub async fn create_offer(&self) -> Result<String, NetError> {
        let offer = self.peer_connection.create_offer(None).await?;
        self.peer_connection
            .set_local_description(offer.clone())
            .await?;
        Ok(offer.sdp)
    }

    /// Set the remote SDP answer from the viewer.
    ///
    /// After setting the remote description, any queued ICE candidates
    /// are applied (prevents the race condition where candidates arrive
    /// before the remote description).
    ///
    /// `encoding_codec_mime` is the MIME type the host is currently encoding
    /// (e.g. `"video/AV1"`). Returns `true` if the viewer's SDP answer
    /// includes that codec, `false` if the viewer stripped it (codec
    /// fallback needed). When `false`, the remote description is NOT set:
    /// the caller should close this peer connection and re-negotiate
    /// with a different codec.
    pub async fn set_remote_answer(
        &mut self,
        sdp: String,
        encoding_codec_mime: &str,
    ) -> Result<bool, NetError> {
        let codec_accepted = sdp_contains_codec(&sdp, encoding_codec_mime);
        if !codec_accepted {
            // Don't call set_remote_description: webrtc-rs would reject the
            // answer with "unable to start track, codec is not supported by
            // remote" and kill the session.
            return Ok(false);
        }

        let answer = RTCSessionDescription::answer(sdp)?;
        self.peer_connection.set_remote_description(answer).await?;
        self.remote_description_set = true;

        let candidates: Vec<RTCIceCandidateInit> = self.pending_candidates.drain(..).collect();
        for candidate in candidates {
            self.peer_connection.add_ice_candidate(candidate).await?;
        }

        Ok(true)
    }

    /// Add a remote ICE candidate from the viewer.
    ///
    /// If the remote description has not been set yet, the candidate is
    /// queued and will be applied when [`set_remote_answer`] is called.
    pub async fn add_ice_candidate(
        &mut self,
        candidate: String,
        sdp_mline_index: Option<u16>,
        sdp_mid: Option<String>,
    ) -> Result<(), NetError> {
        let init = RTCIceCandidateInit {
            candidate,
            sdp_mline_index,
            sdp_mid,
            ..Default::default()
        };

        if self.remote_description_set {
            self.peer_connection.add_ice_candidate(init).await?;
        } else {
            self.pending_candidates.push(init);
        }

        Ok(())
    }

    /// Close the peer connection.
    pub async fn close(&self) -> Result<(), NetError> {
        self.peer_connection.close().await?;
        Ok(())
    }

    /// Get the current connection state.
    pub fn connection_state(&self) -> RTCPeerConnectionState {
        self.peer_connection.connection_state()
    }

    /// Get a reference to the underlying peer connection (for stats collection).
    pub fn peer_connection(&self) -> &Arc<RTCPeerConnection> {
        &self.peer_connection
    }

    /// Get the viewer ID associated with this peer connection.
    pub fn viewer_id(&self) -> &str {
        &self.viewer_id
    }

    /// Send a text message on the annotations data channel.
    pub async fn send_annotation(&self, text: &str) -> Result<(), NetError> {
        self.data_channel
            .send_text(text)
            .await
            .map(|_| ())
            .map_err(NetError::WebRtc)
    }

    /// Get a reference to the data channel.
    pub fn data_channel(&self) -> &Arc<RTCDataChannel> {
        &self.data_channel
    }
}

/// Check whether an SDP answer includes a specific video codec.
///
/// Scans `a=rtpmap:` lines within the `m=video` section for the codec name
/// extracted from a MIME type (e.g. `"video/AV1"` → searches for `"AV1/"`).
///
/// Returns `true` if the codec appears in the answer (viewer supports it),
/// `false` if the viewer stripped it during negotiation.
/// Returns `true` on parse failure (conservative, avoids unnecessary fallback).
pub fn sdp_contains_codec(sdp: &str, codec_mime: &str) -> bool {
    let codec_name = match codec_mime.split('/').nth(1) {
        Some(name) => name.to_uppercase(),
        None => return true, // Unparseable MIME, assume accepted
    };

    let needle = format!("{}/", codec_name);
    let mut in_video = false;
    for line in sdp.lines() {
        if line.starts_with("m=video") {
            in_video = true;
            continue;
        }
        if in_video && line.starts_with("m=") {
            break;
        }
        if in_video && line.starts_with("a=rtpmap:") && line.to_uppercase().contains(&needle) {
            return true;
        }
    }

    // If we never found an m=video section, assume accepted (not a video SDP?)
    if !in_video {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::ViewerTrack;
    use crate::types::IceServerConfig;

    #[test]
    fn turn_probe_addr_keeps_explicit_port() {
        assert_eq!(
            turn_probe_addr("turn:turn.example.com:443?transport=tcp").as_deref(),
            Some("turn.example.com:443")
        );
        assert_eq!(
            turn_probe_addr("turn:[::1]:3479").as_deref(),
            Some("[::1]:3479")
        );
    }

    #[test]
    fn turn_probe_addr_adds_default_port() {
        assert_eq!(
            turn_probe_addr("turn:turn.example.com?transport=tcp").as_deref(),
            Some("turn.example.com:3478")
        );
        assert_eq!(
            turn_probe_addr("turns:turn.example.com").as_deref(),
            Some("turn.example.com:5349")
        );
        assert_eq!(turn_probe_addr("turn:[::1]").as_deref(), Some("[::1]:3478"));
    }

    #[test]
    fn turn_probe_addr_ignores_non_turn() {
        assert_eq!(turn_probe_addr("stun:stun.l.google.com:19302"), None);
    }

    #[tokio::test]
    async fn peer_connection_manager_new_succeeds() {
        let track = ViewerTrack::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (relay_tx, _relay_rx) = mpsc::unbounded_channel();
        let (dc_state_tx, _dc_state_rx) = mpsc::unbounded_channel();
        let ice_servers = vec![IceServerConfig {
            urls: "stun:stun.l.google.com:19302".to_string(),
            username: None,
            credential: None,
        }];
        let pcm = PeerConnectionManager::new(
            "viewer-1".to_string(),
            &ice_servers,
            track.as_track_local(),
            tx,
            relay_tx,
            dc_state_tx,
        )
        .await;
        assert!(pcm.is_ok());
        let pcm = pcm.unwrap();
        assert!(!pcm.remote_description_set);
        assert!(pcm.pending_candidates.is_empty());
    }

    #[tokio::test]
    async fn create_offer_returns_sdp() {
        let track = ViewerTrack::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (relay_tx, _relay_rx) = mpsc::unbounded_channel();
        let (dc_state_tx, _dc_state_rx) = mpsc::unbounded_channel();
        let ice_servers = vec![IceServerConfig {
            urls: "stun:stun.l.google.com:19302".to_string(),
            username: None,
            credential: None,
        }];
        let pcm = PeerConnectionManager::new(
            "viewer-1".to_string(),
            &ice_servers,
            track.as_track_local(),
            tx,
            relay_tx,
            dc_state_tx,
        )
        .await
        .unwrap();
        let sdp = pcm.create_offer().await.unwrap();
        assert!(sdp.contains("v=0"), "SDP should contain v=0 session line");
        assert!(!sdp.is_empty());
    }

    #[tokio::test]
    async fn sdp_offer_contains_vp9_for_default_track() {
        let track = ViewerTrack::default(); // VP9
        let (tx, _rx) = mpsc::unbounded_channel();
        let (relay_tx, _relay_rx) = mpsc::unbounded_channel();
        let (dc_state_tx, _dc_state_rx) = mpsc::unbounded_channel();
        let ice_servers = vec![IceServerConfig {
            urls: "stun:stun.l.google.com:19302".to_string(),
            username: None,
            credential: None,
        }];
        let pcm = PeerConnectionManager::new(
            "viewer-1".to_string(),
            &ice_servers,
            track.as_track_local(),
            tx,
            relay_tx,
            dc_state_tx,
        )
        .await
        .unwrap();
        let sdp = pcm.create_offer().await.unwrap();
        assert!(
            sdp_contains_codec(&sdp, "video/VP9"),
            "VP9 track SDP offer must contain VP9 in rtpmap"
        );
    }

    #[test]
    fn sdp_contains_codec_finds_av1() {
        let sdp = "\
v=0\r\n\
o=- 0 0 IN IP4 0.0.0.0\r\n\
s=-\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 41 98\r\n\
a=rtpmap:41 AV1/90000\r\n\
a=rtpmap:98 VP9/90000\r\n";
        assert!(sdp_contains_codec(sdp, "video/AV1"));
        assert!(sdp_contains_codec(sdp, "video/VP9"));
    }

    #[test]
    fn sdp_contains_codec_av1_absent() {
        let sdp = "\
v=0\r\n\
o=- 0 0 IN IP4 0.0.0.0\r\n\
s=-\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 98\r\n\
a=rtpmap:98 VP9/90000\r\n";
        assert!(!sdp_contains_codec(sdp, "video/AV1"));
        assert!(sdp_contains_codec(sdp, "video/VP9"));
    }

    #[test]
    fn sdp_contains_codec_case_insensitive() {
        let sdp = "\
v=0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 41\r\n\
a=rtpmap:41 av1/90000\r\n";
        assert!(sdp_contains_codec(sdp, "video/AV1"));
    }

    #[test]
    fn sdp_contains_codec_no_video_section() {
        let sdp = "\
v=0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=rtpmap:111 opus/48000/2\r\n";
        // No m=video section, so conservatively assume it was accepted.
        assert!(sdp_contains_codec(sdp, "video/AV1"));
    }

    #[test]
    fn sdp_contains_codec_empty_sdp() {
        assert!(sdp_contains_codec("", "video/AV1"));
    }

    #[test]
    fn sdp_contains_codec_bad_mime() {
        assert!(sdp_contains_codec("m=video 9\r\n", "badmime"));
    }
}
