use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetError {
    #[error("WebRTC error: {0}")]
    WebRtc(#[from] webrtc::Error),

    #[error("Signaling error: {0}")]
    Signaling(String),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Session full: {0}")]
    SessionFull(String),

    #[error("Room not found: {0}")]
    RoomNotFound(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Thread spawn failed: {0}")]
    ThreadSpawnFailed(String),

    #[error("Clipboard error: {0}")]
    Clipboard(String),
}
