use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Serialize)]
pub struct ViewerCountPayload {
    pub count: u32,
    pub viewer_id: String,
    pub action: String, // "joined" or "left"
}

#[derive(Clone, Serialize)]
pub struct LatencyPayload {
    pub rtt_ms: f64,
    pub packet_loss_pct: f64,
    pub status: String, // "connected" | "degraded" | "disconnected"
    pub has_measured_rtt: bool,
}

pub fn emit_viewer_count(app: &AppHandle, viewer_id: String, count: u32, action: &str) {
    let _ = app.emit(
        "viewer-count",
        ViewerCountPayload {
            count,
            viewer_id,
            action: action.to_string(),
        },
    );
}

pub fn emit_latency(
    app: &AppHandle,
    rtt_ms: f64,
    packet_loss_pct: f64,
    has_measured_rtt: bool,
    status: &str,
) {
    let _ = app.emit(
        "latency-update",
        LatencyPayload {
            rtt_ms,
            packet_loss_pct,
            status: status.to_string(),
            has_measured_rtt,
        },
    );
}

#[derive(Clone, Serialize)]
pub struct DataChannelStatePayload {
    pub state: String, // "open" | "closed" | "connecting"
}

pub fn emit_data_channel_state(app: &AppHandle, state: &str) {
    let _ = app.emit(
        "data-channel-state",
        DataChannelStatePayload {
            state: state.to_string(),
        },
    );
}

#[derive(Clone, Serialize)]
pub struct AnnotationPayload {
    pub data: String,
}

pub fn emit_viewer_annotation(app: &AppHandle, data: &str) {
    let _ = app.emit(
        "viewer-annotation",
        AnnotationPayload {
            data: data.to_string(),
        },
    );
}

#[derive(Clone, Serialize)]
pub struct ResolutionPayload {
    pub source_width: u32,
    pub source_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    /// Target bitrate from ABR controller (kbps)
    pub target_bitrate_kbps: u32,
    /// Max quantizer from ABR controller (higher = lower quality)
    pub max_quantizer: u32,
    /// Target FPS from ABR controller
    pub target_fps: u32,
    /// Measured throughput from network stats (kbps)
    pub bandwidth_kbps: u32,
    /// Whether the connection is relayed through a TURN server
    pub is_relay: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn emit_resolution(
    app: &AppHandle,
    source_w: u32,
    source_h: u32,
    out_w: u32,
    out_h: u32,
    target_bitrate_kbps: u32,
    max_quantizer: u32,
    target_fps: u32,
    bandwidth_kbps: u32,
    is_relay: bool,
) {
    let _ = app.emit(
        "resolution-update",
        ResolutionPayload {
            source_width: source_w,
            source_height: source_h,
            output_width: out_w,
            output_height: out_h,
            target_bitrate_kbps,
            max_quantizer,
            target_fps,
            bandwidth_kbps,
            is_relay,
        },
    );
}

pub fn emit_session_ended(app: &AppHandle) {
    let _ = app.emit("session-ended", ());
}

pub fn emit_room_expired(app: &AppHandle) {
    let _ = app.emit("room-expired", ());
}

#[derive(Clone, Serialize)]
pub struct WarningPayload {
    pub message: String,
}

pub fn emit_warning(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "pipeline-warning",
        WarningPayload {
            message: message.to_string(),
        },
    );
}

#[derive(Clone, Serialize)]
pub struct RemoteControlRequestedPayload {
    pub viewer_id: String,
}

/// A viewer asked to control the host while the master switch was off.
pub fn emit_remote_control_requested(app: &AppHandle, viewer_id: &str) {
    let _ = app.emit(
        "remote-control-requested",
        RemoteControlRequestedPayload {
            viewer_id: viewer_id.to_string(),
        },
    );
}

#[derive(Clone, Serialize)]
pub struct RemoteControlActivePayload {
    pub enabled: bool,
    pub controllers: u32,
}

/// Remote-control master-switch state + active controller count changed.
pub fn emit_remote_control_active(app: &AppHandle, enabled: bool, controllers: u32) {
    let _ = app.emit(
        "remote-control-active",
        RemoteControlActivePayload {
            enabled,
            controllers,
        },
    );
}
