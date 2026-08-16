use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("capture not supported on this system")]
    NotSupported,
    #[error("permission denied for screen capture")]
    PermissionDenied,
    #[error("failed to start capture: {0}")]
    StartFailed(String),
    #[error("capture session ended unexpectedly: {0}")]
    SessionError(String),
    #[error("no displays found")]
    NoDisplays,
}
