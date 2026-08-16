//! HostSession orchestrator -- ties together signaling, peer connections, shared
//! video track, stats collection, and clipboard copy into a complete multi-viewer
//! session.
//!
//! The caller creates mpsc channels and passes `Receiver<EncodedPacket>` and
//! `UnboundedSender<NetworkStats>` to [`HostSession::start`]. The caller owns
//! the send side of the packet channel (fed by the encode thread) and the
//! receive side of the stats channel (consumed by the ABR controller).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Mutex, broadcast, mpsc};
use tracing::{debug, error, info, warn};
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::clipboard;
use crate::error::NetError;
use crate::peer::PeerConnectionManager;
use crate::signaling::SignalingClient;
use crate::stats::StatsCollector;
use crate::track::ViewerTrack;
use crate::types::{
    AnnotationMessage, IceServerConfig, RoomCode, SessionConfig, SignalingMessage, ViewerEvent,
};

/// Pool of viewer annotation colors. Assigns unique colors from a vivid palette
/// on viewer connect and recycles them on disconnect.
struct ColorPool {
    /// Full palette for wraparound when pool is exhausted.
    palette: Vec<String>,
    /// Colors currently available for assignment.
    available: VecDeque<String>,
    /// Map of viewer_id -> assigned color.
    assigned: HashMap<String, String>,
}

impl ColorPool {
    fn new() -> Self {
        let palette: Vec<String> = ipc::COLOR_PALETTE.iter().map(|s| s.to_string()).collect();
        let available = VecDeque::from(palette.clone());
        Self {
            palette,
            available,
            assigned: HashMap::new(),
        }
    }

    /// Assign a color to a viewer. Returns the color hex string.
    /// If all colors are in use, wraps around the palette.
    fn assign(&mut self, viewer_id: &str) -> String {
        let color = self.available.pop_front().unwrap_or_else(|| {
            // Wrap around: use palette index based on total assigned count
            self.palette[self.assigned.len() % self.palette.len()].clone()
        });
        self.assigned.insert(viewer_id.to_string(), color.clone());
        color
    }

    /// Release a viewer's color back to the available pool.
    /// No-op if the viewer_id is not in the assigned map.
    fn release(&mut self, viewer_id: &str) {
        if let Some(color) = self.assigned.remove(viewer_id) {
            self.available.push_back(color);
        }
    }

    /// Update the assigned color for a viewer (for color picker changes).
    fn update_color(&mut self, viewer_id: &str, new_color: String) {
        self.assigned.insert(viewer_id.to_string(), new_color);
    }
}

/// One encoded video frame ready to be written to viewer tracks.
///
/// Broadcast to every viewer's writer task. `data` is reference-counted so each
/// viewer clones cheaply; `duration` is the real elapsed time since the previous
/// frame (computed once centrally so all viewers share consistent RTP timing).
#[derive(Clone)]
struct FanoutFrame {
    data: Bytes,
    duration: Duration,
}

/// Per-viewer state tracked by the session.
struct ViewerState {
    peer: PeerConnectionManager,
    /// Task that writes the broadcast frame stream to this viewer's own track.
    /// Aborted when the viewer leaves or during codec fallback teardown.
    writer: tokio::task::JoinHandle<()>,
}

/// Shared mutable state between the session's async tasks.
struct SessionInner {
    viewers: HashMap<String, ViewerState>,
    ice_servers: Vec<IceServerConfig>,
    stats_tx: mpsc::UnboundedSender<encode::NetworkStats>,
    max_viewers: u32,
    viewer_events_tx: Option<mpsc::UnboundedSender<ViewerEvent>>,
    color_pool: ColorPool,
    /// MIME type of the codec the encoder is currently producing (e.g. `"video/AV1"`).
    encoding_codec_mime: String,
    /// Set to `true` after a codec fallback to prevent re-triggering.
    codec_fallen_back: bool,
    /// Sender to tell the encode thread to switch codecs.
    codec_switch_tx: Option<std::sync::mpsc::SyncSender<encode::VideoCodec>>,
    /// Broadcast of encoded frames to each viewer's writer task. Each viewer owns
    /// its own track (its own RTP packetizer / payload type / header extensions),
    /// so a client that negotiated the codec differently (e.g. Firefox vs Chrome
    /// on AV1) cannot corrupt the shared stream or stall other viewers.
    packet_broadcast_tx: broadcast::Sender<FanoutFrame>,
}

/// Orchestrates a complete screen-sharing session.
///
/// Connects to the signaling server, creates a room, copies the room code to
/// the clipboard, and spawns tasks for the signaling loop, packet forwarding,
/// and stats collection. Up to [`SessionConfig::max_viewers`] viewers can join
/// simultaneously.
pub struct HostSession;

impl HostSession {
    /// Start a new hosting session.
    ///
    /// Connects to the signaling server, creates a room, copies the room code
    /// to the host's clipboard (best-effort), and spawns the session event loop.
    ///
    /// Returns `(room_code, join_handle, annotation_inject_tx, dc_state_rx, annotation_forward_rx)` where:
    /// - `join_handle` represents the long-running session task
    /// - `annotation_inject_tx` sends host-originated annotations to all viewers
    /// - `dc_state_rx` receives data channel state changes (viewer_id, "open"|"closed")
    /// - `annotation_forward_rx` receives viewer annotations forwarded to host frontend
    ///
    /// The task runs until the room expires, the packet channel closes, or an
    /// unrecoverable error occurs.
    ///
    /// # Arguments
    /// * `config` - Session configuration (signaling URL, max viewers)
    /// * `packet_rx` - Receives encoded VP9 packets from the encode thread
    /// * `stats_tx` - Sends worst-viewer network stats for ABR consumption
    pub async fn start(
        config: SessionConfig,
        packet_rx: mpsc::Receiver<encode::EncodedPacket>,
        stats_tx: mpsc::UnboundedSender<encode::NetworkStats>,
    ) -> Result<
        (
            RoomCode,
            tokio::task::JoinHandle<Result<(), NetError>>,
            mpsc::UnboundedSender<String>,
            mpsc::UnboundedReceiver<(String, String)>,
            mpsc::UnboundedReceiver<String>,
        ),
        NetError,
    > {
        let mut signaling = SignalingClient::connect(&config.signaling_url).await?;
        info!(url = %config.signaling_url, "Connected to signaling server");

        signaling
            .send(SignalingMessage::Login {
                role: "host".to_string(),
            })
            .await?;

        let (room_code, ice_servers) = loop {
            match signaling.recv().await? {
                Some(SignalingMessage::LoginSuccess {
                    room_id,
                    ice_servers,
                }) => {
                    break (room_id, ice_servers);
                }
                Some(SignalingMessage::Error { code, message }) => {
                    return Err(NetError::Signaling(format!(
                        "Login failed: {} (code: {:?})",
                        message, code
                    )));
                }
                Some(_) => continue, // Ignore unexpected messages during login
                None => {
                    return Err(NetError::Signaling(
                        "Connection closed during login".to_string(),
                    ));
                }
            }
        };

        // NOTE: Do not log ice_servers because IceServerConfig contains TURN credentials (D-12)
        info!(
            room_code = %room_code,
            ice_server_count = ice_servers.len(),
            "Room created with ICE servers"
        );

        // Best-effort: a clipboard failure must not abort the session.
        match clipboard::copy_to_clipboard(&room_code) {
            Ok(()) => info!(room_code = %room_code, "Room code copied to clipboard"),
            Err(e) => {
                warn!(room_code = %room_code, error = %e, "Failed to copy room code to clipboard (non-fatal)")
            }
        }

        // Per-viewer frame fan-out channel. Each viewer owns its own track and
        // writer task; this broadcast carries encoded frames to all of them.
        // Capacity bounds how far a slow viewer may lag before it drops frames
        // (isolated to that viewer, never blocking others).
        let (packet_broadcast_tx, _) = broadcast::channel::<FanoutFrame>(16);

        let inner = Arc::new(Mutex::new(SessionInner {
            viewers: HashMap::new(),
            ice_servers,
            stats_tx,
            max_viewers: config.max_viewers,
            viewer_events_tx: config.viewer_events_tx,
            color_pool: ColorPool::new(),
            encoding_codec_mime: config.codec_mime,
            codec_fallen_back: false,
            codec_switch_tx: config.codec_switch_tx,
            packet_broadcast_tx: packet_broadcast_tx.clone(),
        }));

        // Outbound signaling for ICE candidates produced by peer connection callbacks.
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<SignalingMessage>();

        // Annotation relay: viewer -> host -> other viewers.
        let (relay_tx, mut relay_rx) = mpsc::unbounded_channel::<(String, String)>();

        // Host annotation inject: host frontend -> all viewers.
        let (annotation_inject_tx, mut annotation_inject_rx) = mpsc::unbounded_channel::<String>();

        // Data channel state: on_open/on_close -> session -> pipeline -> UI.
        let (dc_state_tx, mut dc_state_rx) = mpsc::unbounded_channel::<(String, String)>();

        // dc state events are processed in the session (color assignment) before
        // being forwarded on this second channel to the pipeline.
        let (dc_state_forward_tx, dc_state_forward_rx) =
            mpsc::unbounded_channel::<(String, String)>();

        // Viewer annotations forwarded to host frontend: relay_loop -> pipeline -> host React.
        let (annotation_forward_tx, annotation_forward_rx) = mpsc::unbounded_channel::<String>();

        let room_code_ret = room_code.clone();

        let handle = tokio::spawn(async move {
            let inner_sig = Arc::clone(&inner);
            let inner_stats = Arc::clone(&inner);
            let inner_relay = Arc::clone(&inner);
            let mut packet_rx = packet_rx;

            tokio::select! {
                result = Self::signaling_loop(signaling, &inner_sig, outbound_tx.clone(), &mut outbound_rx, relay_tx, dc_state_tx) => {
                    if let Err(e) = &result {
                        error!(error = %e, "Signaling loop ended with error");
                    }
                    result
                }

                result = Self::packet_fanout_loop(&mut packet_rx, &packet_broadcast_tx) => {
                    if let Err(e) = &result {
                        error!(error = %e, "Packet fan-out loop ended with error");
                    }
                    result
                }

                result = Self::stats_loop(&inner_stats) => {
                    if let Err(e) = &result {
                        error!(error = %e, "Stats collection loop ended with error");
                    }
                    result
                }

                result = Self::relay_loop(&mut relay_rx, &mut annotation_inject_rx, &mut dc_state_rx, dc_state_forward_tx, annotation_forward_tx.clone(), &inner_relay) => {
                    if let Err(e) = &result {
                        error!(error = %e, "Relay loop ended with error");
                    }
                    result
                }
            }
        });

        Ok((
            room_code_ret,
            handle,
            annotation_inject_tx,
            dc_state_forward_rx,
            annotation_forward_rx,
        ))
    }

    /// Main signaling loop: dispatches incoming messages and forwards outbound ones.
    async fn signaling_loop(
        mut signaling: SignalingClient,
        inner: &Arc<Mutex<SessionInner>>,
        outbound_tx: mpsc::UnboundedSender<SignalingMessage>,
        outbound_rx: &mut mpsc::UnboundedReceiver<SignalingMessage>,
        relay_tx: mpsc::UnboundedSender<(String, String)>,
        dc_state_tx: mpsc::UnboundedSender<(String, String)>,
    ) -> Result<(), NetError> {
        loop {
            tokio::select! {
                msg = signaling.recv() => {
                    match msg? {
                        Some(msg) => {
                            Self::handle_signaling_message(msg, inner, &mut signaling, &outbound_tx, &relay_tx, &dc_state_tx).await?;
                        }
                        None => {
                            info!("Signaling connection closed");
                            break;
                        }
                    }
                }

                // Outbound messages from peer connection callbacks (ICE candidates)
                Some(msg) = outbound_rx.recv() => {
                    signaling.send(msg).await?;
                }
            }
        }

        let mut locked = inner.lock().await;
        let viewers: Vec<(String, ViewerState)> = locked.viewers.drain().collect();
        for (viewer_id, viewer_state) in viewers {
            locked.color_pool.release(&viewer_id);
            debug!(viewer_id = %viewer_id, "Closing peer connection on session end");
            if let Err(e) = viewer_state.peer.close().await {
                warn!(viewer_id = %viewer_id, error = %e, "Error closing peer connection");
            }
        }

        Ok(())
    }

    /// Handle a single signaling message.
    async fn handle_signaling_message(
        msg: SignalingMessage,
        inner: &Arc<Mutex<SessionInner>>,
        signaling: &mut SignalingClient,
        outbound_tx: &mpsc::UnboundedSender<SignalingMessage>,
        relay_tx: &mpsc::UnboundedSender<(String, String)>,
        dc_state_tx: &mpsc::UnboundedSender<(String, String)>,
    ) -> Result<(), NetError> {
        match msg {
            SignalingMessage::ViewerJoined {
                viewer_id,
                viewer_count,
            } => {
                let mut locked = inner.lock().await;

                // Emit before the capacity check so the UI count stays
                // in sync even when the viewer is rejected.
                if let Some(ref tx) = locked.viewer_events_tx {
                    let _ = tx.send(ViewerEvent::Joined {
                        viewer_id: viewer_id.clone(),
                        viewer_count,
                    });
                }

                if locked.viewers.len() as u32 >= locked.max_viewers {
                    warn!(
                        viewer_id = %viewer_id,
                        max = locked.max_viewers,
                        "Viewer joined but session is full, ignoring"
                    );
                    return Ok(());
                }

                info!(
                    viewer_id = %viewer_id,
                    viewer_count = viewer_count,
                    "Viewer joined"
                );

                Self::create_viewer_peer(
                    &mut locked,
                    viewer_id,
                    signaling,
                    outbound_tx,
                    relay_tx,
                    dc_state_tx,
                )
                .await?;
            }

            SignalingMessage::Answer {
                sdp,
                from: Some(viewer_id),
                ..
            } => {
                let mut locked = inner.lock().await;
                let encoding_mime = locked.encoding_codec_mime.clone();
                if let Some(viewer) = locked.viewers.get_mut(&viewer_id) {
                    let codec_accepted = viewer.peer.set_remote_answer(sdp, &encoding_mime).await?;
                    info!(
                        viewer_id = %viewer_id,
                        codec_accepted,
                        encoding = %encoding_mime,
                        "Set remote answer, codec negotiation result"
                    );

                    if !codec_accepted && !locked.codec_fallen_back {
                        warn!(
                            viewer_id = %viewer_id,
                            encoding = %locked.encoding_codec_mime,
                            "Viewer does not support codec, falling back to VP9"
                        );

                        Self::codec_fallback(
                            &mut locked,
                            signaling,
                            outbound_tx,
                            relay_tx,
                            dc_state_tx,
                        )
                        .await?;
                    }
                } else {
                    warn!(viewer_id = %viewer_id, "Answer from unknown viewer");
                }
            }

            SignalingMessage::Candidate {
                candidate,
                sdp_mid,
                sdp_m_line_index,
                from: Some(viewer_id),
                ..
            } => {
                let mut locked = inner.lock().await;
                if let Some(viewer) = locked.viewers.get_mut(&viewer_id) {
                    viewer
                        .peer
                        .add_ice_candidate(candidate, sdp_m_line_index.map(|v| v as u16), sdp_mid)
                        .await?;
                    debug!(viewer_id = %viewer_id, "Added ICE candidate");
                } else {
                    warn!(viewer_id = %viewer_id, "ICE candidate from unknown viewer");
                }
            }

            SignalingMessage::ViewerLeft {
                viewer_id,
                viewer_count,
            } => {
                let mut locked = inner.lock().await;

                // Emit before remove so the UI gets the count even for unknown viewers.
                if let Some(ref tx) = locked.viewer_events_tx {
                    let _ = tx.send(ViewerEvent::Left {
                        viewer_id: viewer_id.clone(),
                        viewer_count,
                    });
                }

                if let Some(viewer_state) = locked.viewers.remove(&viewer_id) {
                    locked.color_pool.release(&viewer_id);
                    info!(
                        viewer_id = %viewer_id,
                        viewer_count = viewer_count,
                        "Viewer left, cleaning up peer connection"
                    );
                    Self::teardown_viewer(viewer_state).await;
                }
            }

            SignalingMessage::Presence { viewer_count } => {
                let locked = inner.lock().await;
                debug!(viewer_count = viewer_count, "Presence update");
                if let Some(ref tx) = locked.viewer_events_tx {
                    let _ = tx.send(ViewerEvent::Presence { viewer_count });
                }
            }

            SignalingMessage::RoomExpired {} => {
                info!("Room expired, ending session");
                let locked = inner.lock().await;
                if let Some(ref tx) = locked.viewer_events_tx {
                    let _ = tx.send(ViewerEvent::RoomExpired);
                }
                return Err(NetError::Signaling("Room expired".to_string()));
            }

            _ => {
                debug!(msg = ?msg, "Ignoring unhandled signaling message");
            }
        }

        Ok(())
    }

    /// Create a peer connection for a viewer, send an SDP offer, and store the state.
    async fn create_viewer_peer(
        locked: &mut SessionInner,
        viewer_id: String,
        signaling: &mut SignalingClient,
        outbound_tx: &mpsc::UnboundedSender<SignalingMessage>,
        relay_tx: &mpsc::UnboundedSender<(String, String)>,
        dc_state_tx: &mpsc::UnboundedSender<(String, String)>,
    ) -> Result<(), NetError> {
        // Each viewer gets its OWN track, so its RTP packetizer, payload type, and
        // header extensions match its own SDP negotiation. A shared track cannot
        // serve clients that negotiate the codec differently (e.g. Firefox vs Chrome
        // on AV1), which corrupts the stream for everyone.
        let viewer_track = ViewerTrack::new(&locked.encoding_codec_mime);
        let track_arc = viewer_track.track();
        let track_local = viewer_track.as_track_local();
        let ice_servers = locked.ice_servers.clone();
        let pcm = PeerConnectionManager::new(
            viewer_id.clone(),
            &ice_servers,
            track_local,
            outbound_tx.clone(),
            relay_tx.clone(),
            dc_state_tx.clone(),
        )
        .await?;

        let sdp = pcm.create_offer().await?;
        signaling
            .send(SignalingMessage::Offer {
                sdp,
                target_id: Some(viewer_id.clone()),
                from: None,
            })
            .await?;

        // Spawn this viewer's writer task: it pulls frames from the shared fan-out
        // broadcast and writes them to this viewer's own track. Frames the viewer
        // can't keep up with are dropped for it alone (broadcast Lagged), never
        // stalling other viewers.
        let writer = Self::spawn_viewer_writer(
            viewer_id.clone(),
            track_arc,
            locked.packet_broadcast_tx.subscribe(),
        );

        locked
            .viewers
            .insert(viewer_id.clone(), ViewerState { peer: pcm, writer });

        Ok(())
    }

    /// Spawn the per-viewer writer task that drains the broadcast frame stream and
    /// writes each frame to this viewer's own track.
    fn spawn_viewer_writer(
        viewer_id: String,
        track: Arc<TrackLocalStaticSample>,
        mut frame_rx: broadcast::Receiver<FanoutFrame>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match frame_rx.recv().await {
                    Ok(frame) => {
                        let sample = Sample {
                            data: frame.data,
                            duration: frame.duration,
                            ..Default::default()
                        };
                        if let Err(e) = track.write_sample(&sample).await {
                            // The peer is closing/gone; stop writing for this viewer.
                            debug!(viewer_id = %viewer_id, error = %e, "viewer writer stopping");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(viewer_id = %viewer_id, skipped, "viewer fell behind, dropped frames");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Stop a viewer's writer task and close its peer connection. Called from
    /// every viewer-removal site (viewer leave, codec fallback) so the teardown
    /// sequence stays in one place.
    async fn teardown_viewer(viewer: ViewerState) {
        viewer.writer.abort();
        if let Err(e) = viewer.peer.close().await {
            warn!(viewer_id = %viewer.peer.viewer_id(), error = %e, "Error closing viewer peer connection");
        }
    }

    /// Switch the encoder to VP9 and re-negotiate with all connected viewers.
    ///
    /// Called when a viewer's SDP answer reveals it doesn't support the host's
    /// preferred codec (e.g. AV1). This:
    /// 1. Signals the encode thread to switch to VP9
    /// 2. Closes and re-creates all viewer peer connections (each gets a fresh VP9
    ///    track matching the encoder's new codec)
    /// 3. Sends new SDP offers to all viewers
    async fn codec_fallback(
        locked: &mut SessionInner,
        signaling: &mut SignalingClient,
        outbound_tx: &mpsc::UnboundedSender<SignalingMessage>,
        relay_tx: &mpsc::UnboundedSender<(String, String)>,
        dc_state_tx: &mpsc::UnboundedSender<(String, String)>,
    ) -> Result<(), NetError> {
        let old_codec = locked.encoding_codec_mime.clone();

        if let Some(ref tx) = locked.codec_switch_tx {
            let _ = tx.send(encode::VideoCodec::VP9);
        }

        // Subsequently created viewer tracks pick up this codec.
        locked.encoding_codec_mime = "video/VP9".to_string();
        locked.codec_fallen_back = true;

        // Close and re-create every peer: create_viewer_peer builds a fresh VP9
        // track per viewer matching the encoder's new codec.
        let viewer_ids: Vec<String> = locked.viewers.keys().cloned().collect();
        for vid in &viewer_ids {
            if let Some(old_viewer) = locked.viewers.remove(vid) {
                Self::teardown_viewer(old_viewer).await;
            }
        }

        for vid in viewer_ids {
            Self::create_viewer_peer(locked, vid, signaling, outbound_tx, relay_tx, dc_state_tx)
                .await?;
        }

        if let Some(ref tx) = locked.viewer_events_tx {
            let _ = tx.send(ViewerEvent::CodecFallback {
                from_codec: old_codec,
                to_codec: "video/VP9".to_string(),
            });
        }

        info!("Codec fallback complete: all viewers re-negotiated with VP9");
        Ok(())
    }

    /// Packet fan-out loop: reads encoded packets and broadcasts each as a
    /// [`FanoutFrame`] to every viewer's writer task (each writes to its own track).
    ///
    /// Broadcasting (rather than writing one shared track) means each viewer's RTP
    /// state is independent, so heterogeneous clients (e.g. Firefox + Chrome on AV1)
    /// don't corrupt each other, and a slow viewer drops its own frames instead of
    /// stalling the pipeline. The broadcast still drains `packet_rx` when there are
    /// zero viewers, so the encoder never backs up.
    ///
    /// Uses actual elapsed time between packets for the sample duration so that
    /// RTP timestamps accurately reflect real wall clock time. A hardcoded
    /// duration (e.g., 33ms for 30fps) causes progressive RTP timestamp drift
    /// when frame intervals vary (ABR FPS changes, frame skipping, encode
    /// timing variance), which makes the viewer's jitter buffer accumulate
    /// latency over minutes.
    async fn packet_fanout_loop(
        packet_rx: &mut mpsc::Receiver<encode::EncodedPacket>,
        broadcast_tx: &broadcast::Sender<FanoutFrame>,
    ) -> Result<(), NetError> {
        let default_duration = Duration::from_millis(1000 / 30);
        let mut last_send_time: Option<tokio::time::Instant> = None;

        while let Some(packet) = packet_rx.recv().await {
            let now = tokio::time::Instant::now();
            let duration = if let Some(prev) = last_send_time {
                let elapsed = now.duration_since(prev);
                // Clamp minimum only: below 1ms suggests burst delivery.
                // No upper cap: large gaps (static screen where PipeWire
                // pauses delivery, or encoding paused) must advance the RTP
                // timestamp to match wall clock. Capping at 1s caused
                // progressive RTP/NTP clock divergence: after a 30s gap,
                // RTP was 29s behind NTP, making the browser's jitter
                // buffer add latency to reconcile the clocks.
                if elapsed < Duration::from_millis(1) {
                    default_duration
                } else {
                    elapsed
                }
            } else {
                default_duration
            };
            last_send_time = Some(now);

            // Move the encoder's Vec into Bytes (no copy); each viewer's writer
            // then clones the Bytes by bumping its refcount.
            let frame = FanoutFrame {
                data: Bytes::from(packet.data),
                duration,
            };

            // Err only means there are currently no viewers; drop the frame and keep
            // draining so the encoder never blocks.
            let _ = broadcast_tx.send(frame);
        }

        info!("Packet channel closed, ending fan-out loop");
        Ok(())
    }

    /// Validate that a viewer annotation payload is well-formed.
    /// Rejects oversized messages and payloads that aren't valid JSON.
    fn validate_annotation(message: &str) -> bool {
        const MAX_ANNOTATION_SIZE: usize = 4096;
        if message.len() > MAX_ANNOTATION_SIZE {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(message).is_ok()
    }

    /// Annotation relay loop: receives from individual viewer data channels
    /// and from host frontend injection, broadcasts to appropriate targets.
    /// Also handles data channel state events for color assignment before
    /// forwarding them to the pipeline for UI event emission.
    async fn relay_loop(
        relay_rx: &mut mpsc::UnboundedReceiver<(String, String)>,
        inject_rx: &mut mpsc::UnboundedReceiver<String>,
        dc_state_rx: &mut mpsc::UnboundedReceiver<(String, String)>,
        dc_state_forward_tx: mpsc::UnboundedSender<(String, String)>,
        annotation_forward_tx: mpsc::UnboundedSender<String>,
        inner: &Arc<Mutex<SessionInner>>,
    ) -> Result<(), NetError> {
        // Global sliding window cap: forward at most 5 keyframe requests per 10s,
        // regardless of how many viewers are requesting. Protects encoder from spam.
        // Uses a sliding window (not a permanent cap) so recovery is possible after
        // the window passes.
        let mut kf_timestamps: VecDeque<tokio::time::Instant> = VecDeque::new();
        const MAX_KF_IN_WINDOW: usize = 5;
        const KF_WINDOW: Duration = Duration::from_secs(10);
        loop {
            tokio::select! {
                // Viewer annotation: typed dispatch via AnnotationMessage enum (D-14)
                Some((sender_id, message)) = relay_rx.recv() => {
                    if !Self::validate_annotation(&message) {
                        tracing::warn!(viewer_id = %sender_id, len = message.len(), "Rejected invalid annotation payload");
                        continue;
                    }
                    match serde_json::from_str::<AnnotationMessage>(&message) {
                        Ok(AnnotationMessage::KeyframeRequest { .. }) => {
                            let now = tokio::time::Instant::now();
                            while kf_timestamps.front().is_some_and(|t| now.duration_since(*t) > KF_WINDOW) {
                                kf_timestamps.pop_front();
                            }
                            if kf_timestamps.len() < MAX_KF_IN_WINDOW {
                                kf_timestamps.push_back(now);
                                tracing::debug!(viewer_id = %sender_id, kf_count = kf_timestamps.len(), max = MAX_KF_IN_WINDOW, window_secs = KF_WINDOW.as_secs(), "Forwarding keyframe request to encoder");
                                let _ = annotation_forward_tx.send(r#"{"_keyframe_request":true}"#.to_string());
                            } else {
                                tracing::debug!(viewer_id = %sender_id, "Keyframe request dropped (sliding window cap: {} in {}s)", MAX_KF_IN_WINDOW, KF_WINDOW.as_secs());
                            }
                        }
                        Ok(AnnotationMessage::Viewport { h }) => {
                            tracing::debug!(viewer_id = %sender_id, viewport_height = h, "Received viewport report");
                            tracing::info!(viewer_id = %sender_id, viewport_height = h, "Viewer viewport height");
                            let viewport_msg = format!(r#"{{"_viewport_height":{h}}}"#);
                            let _ = annotation_forward_tx.send(viewport_msg);
                        }
                        Ok(AnnotationMessage::ColorChange { ref id, ref c }) => {
                            {
                                let mut locked = inner.lock().await;
                                locked.color_pool.update_color(id, c.to_string());
                                info!(viewer_id = %id, color = %c, "Updated viewer color");
                            }
                            // Broadcast to ALL viewers (including the sender) so everyone
                            // sees the new color.
                            let ca_json = format!(r#"{{"t":"ca","id":"{id}","c":"{c}"}}"#);
                            let targets: Vec<(String, Arc<webrtc::data_channel::RTCDataChannel>)> = {
                                let locked = inner.lock().await;
                                locked.viewers.iter()
                                    .map(|(id, vs)| (id.clone(), vs.peer.data_channel().clone()))
                                    .collect()
                            };
                            for (vid, dc) in targets {
                                if let Err(e) = dc.send_text(ca_json.clone()).await {
                                    warn!(viewer_id = %vid, error = %e, "Failed to send color update");
                                }
                            }
                            // Also forward to host frontend so host sees updated color
                            let _ = annotation_forward_tx.send(ca_json);
                        }
                        Ok(AnnotationMessage::Clear { .. }) => {
                            let _ = annotation_forward_tx.send(message.clone());

                            // Relay to every viewer except the sender (no echo).
                            let targets: Vec<(String, Arc<webrtc::data_channel::RTCDataChannel>)> = {
                                let locked = inner.lock().await;
                                locked.viewers.iter()
                                    .filter(|(id, _)| **id != sender_id)
                                    .map(|(id, vs)| (id.clone(), vs.peer.data_channel().clone()))
                                    .collect()
                            };
                            for (vid, dc) in targets {
                                if let Err(e) = dc.send_text(message.clone()).await {
                                    warn!(viewer_id = %vid, error = %e, "Failed to relay annotation");
                                }
                            }
                        }
                        Ok(AnnotationMessage::RemoteControl { .. })
                        | Ok(AnnotationMessage::MouseButton { .. })
                        | Ok(AnnotationMessage::Scroll { .. })
                        | Ok(AnnotationMessage::KeyDown { .. })
                        | Ok(AnnotationMessage::KeyUp { .. }) => {
                            // Remote-control intent + input. Forward to the host pipeline
                            // ONLY -- never relay to other viewers, so keystrokes (e.g.
                            // passwords typed during remote control) can't leak to peers.
                            // Each packet carries the viewer's own id for controller tracking.
                            let _ = annotation_forward_tx.send(message.clone());
                        }
                        Err(_) => {
                            // Unknown or drawing type (ds, dm, de, c): relay as-is.
                            let _ = annotation_forward_tx.send(message.clone());

                            let targets: Vec<(String, Arc<webrtc::data_channel::RTCDataChannel>)> = {
                                let locked = inner.lock().await;
                                locked.viewers.iter()
                                    .filter(|(id, _)| **id != sender_id)
                                    .map(|(id, vs)| (id.clone(), vs.peer.data_channel().clone()))
                                    .collect()
                            };
                            for (vid, dc) in targets {
                                if let Err(e) = dc.send_text(message.clone()).await {
                                    warn!(viewer_id = %vid, error = %e, "Failed to relay annotation");
                                }
                            }
                        }
                    }
                }
                // Host annotation: broadcast to ALL viewers
                Some(message) = inject_rx.recv() => {
                    let is_clear = matches!(
                        serde_json::from_str::<AnnotationMessage>(&message),
                        Ok(AnnotationMessage::Clear { .. })
                    );
                    let targets: Vec<(String, Arc<webrtc::data_channel::RTCDataChannel>)> = {
                        let locked = inner.lock().await;
                        locked.viewers.iter()
                            .map(|(id, vs)| (id.clone(), vs.peer.data_channel().clone()))
                            .collect()
                    };
                    if is_clear {
                        info!(
                            viewer_count = targets.len(),
                            message = %message,
                            "Broadcasting cl packet to all viewers"
                        );
                    }
                    for (id, dc) in targets {
                        if let Err(e) = dc.send_text(message.clone()).await {
                            warn!(viewer_id = %id, error = %e, "Failed to send host annotation");
                        } else if is_clear {
                            debug!(viewer_id = %id, "cl packet sent successfully");
                        }
                    }
                }
                // Data channel state: assign color on "open", forward all events to pipeline
                Some((viewer_id, state)) = dc_state_rx.recv() => {
                    if state == "open" {
                        let ca_json = {
                            let mut locked = inner.lock().await;
                            let color = locked.color_pool.assign(&viewer_id);
                            info!(viewer_id = %viewer_id, color = %color, "Assigned annotation color");
                            format!(r#"{{"t":"ca","id":"{}","c":"{}"}}"#, viewer_id, color)
                        };
                        let dc = {
                            let locked = inner.lock().await;
                            locked.viewers.get(&viewer_id).map(|vs| vs.peer.data_channel().clone())
                        };
                        if let Some(dc) = dc {
                            if let Err(e) = dc.send_text(ca_json).await {
                                warn!(viewer_id = %viewer_id, error = %e, "Failed to send color assignment");
                            } else {
                                info!(viewer_id = %viewer_id, "Sent color assignment");
                            }
                        }
                    }
                    // Forward to pipeline for UI event emission
                    let _ = dc_state_forward_tx.send((viewer_id, state));
                }
                else => {
                    info!("Relay channels closed");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Stats collection loop: periodically gathers worst-viewer stats.
    async fn stats_loop(inner: &Arc<Mutex<SessionInner>>) -> Result<(), NetError> {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut collector = StatsCollector::new();

        loop {
            interval.tick().await;

            let (pcs, stats_tx) = {
                let locked = inner.lock().await;
                let pcs: Vec<_> = locked
                    .viewers
                    .values()
                    .map(|v| Arc::clone(v.peer.peer_connection()))
                    .collect();
                (pcs, locked.stats_tx.clone())
            };

            if pcs.is_empty() {
                continue;
            }

            if let Some(stats) = collector.collect_worst_stats(&pcs).await
                && stats_tx.send(stats).is_err()
            {
                debug!("Stats receiver dropped, stopping stats collection");
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_pool_new_has_seven_colors() {
        let pool = ColorPool::new();
        assert_eq!(pool.available.len(), 7);
        assert!(pool.assigned.is_empty());
    }

    #[test]
    fn color_pool_assign_returns_first_color() {
        let mut pool = ColorPool::new();
        let color = pool.assign("v1");
        assert_eq!(color, "#FF3B30");
    }

    #[test]
    fn color_pool_sequential_assigns_return_different_colors() {
        let mut pool = ColorPool::new();
        let c1 = pool.assign("v1");
        let c2 = pool.assign("v2");
        let c3 = pool.assign("v3");
        assert_ne!(c1, c2);
        assert_ne!(c2, c3);
        assert_ne!(c1, c3);
    }

    #[test]
    fn color_pool_release_returns_color_to_pool() {
        let mut pool = ColorPool::new();
        let c1 = pool.assign("v1");
        assert_eq!(pool.available.len(), 6);
        pool.release("v1");
        assert_eq!(pool.available.len(), 7);
        // Released color goes to back of queue
        assert_eq!(pool.available.back().unwrap(), &c1);
    }

    #[test]
    fn color_pool_released_color_reused() {
        let mut pool = ColorPool::new();
        // Assign all 7
        for i in 0..7 {
            pool.assign(&format!("v{}", i));
        }
        assert!(pool.available.is_empty());
        // Release first viewer
        pool.release("v0");
        assert_eq!(pool.available.len(), 1);
        // Next assign gets the released color
        let reused = pool.assign("v7");
        assert_eq!(reused, "#FF3B30");
    }

    #[test]
    fn color_pool_wraparound_on_exhaustion() {
        let mut pool = ColorPool::new();
        // Assign all 7
        for i in 0..7 {
            pool.assign(&format!("v{}", i));
        }
        // 8th viewer wraps around (no panic)
        let c7 = pool.assign("v7");
        assert!(!c7.is_empty());
    }

    #[test]
    fn color_pool_release_unknown_viewer_is_noop() {
        let mut pool = ColorPool::new();
        pool.release("nonexistent"); // should not panic
        assert_eq!(pool.available.len(), 7);
    }

    #[test]
    fn color_pool_update_color_changes_assigned() {
        let mut pool = ColorPool::new();
        let original = pool.assign("v1");
        assert_eq!(original, "#FF3B30");
        pool.update_color("v1", "#00C7BE".to_string());
        assert_eq!(pool.assigned.get("v1").unwrap(), "#00C7BE");
    }
}
