use thiserror::Error;

#[derive(Error, Debug)]
pub enum EncodeError {
    #[error("encoder initialization failed: {0}")]
    InitFailed(String),
    #[error("encoding frame failed: {0}")]
    EncodeFailed(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("colorspace conversion failed: {0}")]
    ColorspaceError(String),
    #[error("encoder thread spawn failed: {0}")]
    ThreadSpawnFailed(String),
}
