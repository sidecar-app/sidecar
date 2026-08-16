use serde::{Deserialize, Serialize};

/// A word-based room code (e.g., "brave-parrot-42").
pub type RoomCode = String;

/// Events emitted when viewers join or leave the session.
///
/// Sent over an optional channel from HostSession to the pipeline
/// for UI event bridging.
#[derive(Debug, Clone)]
pub enum ViewerEvent {
    Joined {
        viewer_id: String,
        viewer_count: u32,
    },
    Left {
        viewer_id: String,
        viewer_count: u32,
    },
    /// Periodic presence sync from the signaling server.
    Presence {
        viewer_count: u32,
    },
    RoomExpired,
    /// Codec fallback was triggered because a viewer did not support the
    /// host's preferred codec. The encoder and track have been switched.
    CodecFallback {
        from_codec: String,
        to_codec: String,
    },
}

/// ICE server configuration for WebRTC connectivity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IceServerConfig {
    pub urls: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// Session configuration for connecting to the signaling server.
pub struct SessionConfig {
    pub signaling_url: String,
    pub max_viewers: u32,
    /// Video codec MIME type for the WebRTC track (e.g. `"video/VP9"`, `"video/AV1"`).
    pub codec_mime: String,
    /// Optional channel for viewer join/leave events (for UI bridging).
    pub viewer_events_tx: Option<tokio::sync::mpsc::UnboundedSender<ViewerEvent>>,
    /// Sender to signal the encode thread to switch codecs on fallback.
    pub codec_switch_tx: Option<std::sync::mpsc::SyncSender<encode::VideoCodec>>,
}

/// The hosted signaling server.
/// `SIDECAR_SIGNALING_URL` overrides it for local development and testing.
pub const DEFAULT_SIGNALING_URL: &str = "wss://ws.sidecar.rs";

impl Default for SessionConfig {
    fn default() -> Self {
        let signaling_url = std::env::var("SIDECAR_SIGNALING_URL")
            .unwrap_or_else(|_| DEFAULT_SIGNALING_URL.to_string());
        Self {
            signaling_url,
            max_viewers: 3,
            codec_mime: "video/VP9".to_string(),
            viewer_events_tx: None,
            codec_switch_tx: None,
        }
    }
}

impl PartialEq for SessionConfig {
    fn eq(&self, other: &Self) -> bool {
        self.signaling_url == other.signaling_url
            && self.max_viewers == other.max_viewers
            && self.codec_mime == other.codec_mime
    }
}

/// Viewer annotation message types for typed dispatch in the relay loop.
///
/// Only the types that require special handling are included.
/// If deserialization fails (e.g., drawing packets ds/dm/de), the message
/// is treated as a regular annotation and relayed as-is.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "t")]
pub enum AnnotationMessage {
    /// Color change
    #[serde(rename = "cc")]
    ColorChange { id: String, c: String },
    /// Viewport report
    #[serde(rename = "vp")]
    Viewport { h: u64 },
    /// Keyframe request
    #[serde(rename = "kf")]
    KeyframeRequest {},
    /// Clear all annotations
    #[serde(rename = "cl")]
    Clear {},
    /// Remote-control mode toggled by a viewer (control intent).
    #[serde(rename = "rc")]
    RemoteControl { on: bool },
    /// Remote-control mouse button. `b`: 0=left, 1=right, 2=middle. `p`: pressed.
    #[serde(rename = "mb")]
    MouseButton { b: u8, p: bool },
    /// Remote-control scroll deltas.
    #[serde(rename = "ms")]
    Scroll { dx: f64, dy: f64 },
    /// Remote-control key down. `k`: DOM KeyboardEvent.code.
    #[serde(rename = "kd")]
    KeyDown { k: String },
    /// Remote-control key up. `k`: DOM KeyboardEvent.code.
    #[serde(rename = "ku")]
    KeyUp { k: String },
}

/// All message types in the signaling protocol.
///
/// Uses `#[serde(tag = "type")]` for JSON like `{"type":"login","role":"host"}`.
/// Field names use camelCase to match the JavaScript signaling server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SignalingMessage {
    /// Host requests room creation.
    Login { role: String },
    /// Server confirms room created with code and ICE servers.
    LoginSuccess {
        #[serde(rename = "roomId")]
        room_id: RoomCode,
        #[serde(rename = "iceServers")]
        ice_servers: Vec<IceServerConfig>,
    },
    /// Viewer requests to join a room.
    Join {
        #[serde(rename = "roomId")]
        room_id: RoomCode,
    },
    /// Server confirms viewer joined with ICE servers.
    JoinSuccess {
        #[serde(rename = "roomId")]
        room_id: RoomCode,
        #[serde(rename = "iceServers")]
        ice_servers: Vec<IceServerConfig>,
    },
    /// Error response from the server.
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        message: String,
    },
    /// Notification that a viewer joined the room.
    ViewerJoined {
        #[serde(rename = "viewerId")]
        viewer_id: String,
        #[serde(rename = "viewerCount")]
        viewer_count: u32,
    },
    /// Notification that a viewer left the room.
    ViewerLeft {
        #[serde(rename = "viewerId")]
        viewer_id: String,
        #[serde(rename = "viewerCount")]
        viewer_count: u32,
    },
    /// Notification that the host left the room.
    HostLeft {},
    /// Presence update with current viewer count.
    Presence {
        #[serde(rename = "viewerCount")]
        viewer_count: u32,
    },
    /// Room expired due to inactivity.
    RoomExpired {},
    /// WebRTC SDP offer (relayed).
    Offer {
        sdp: String,
        #[serde(rename = "targetId", skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// WebRTC SDP answer (relayed).
    Answer {
        sdp: String,
        #[serde(rename = "targetId", skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// WebRTC ICE candidate (relayed).
    Candidate {
        candidate: String,
        #[serde(rename = "sdpMid", skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex", skip_serializing_if = "Option::is_none")]
        sdp_m_line_index: Option<u32>,
        #[serde(rename = "targetId", skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
}
