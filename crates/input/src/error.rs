use thiserror::Error;

#[derive(Error, Debug)]
pub enum InputError {
    /// Input injection is not implemented for this platform yet.
    #[error("remote control input is not supported on this platform")]
    Unsupported,
    /// The RemoteDesktop portal backend is absent (e.g. wlroots compositors).
    /// Signals the factory to try the next backend rather than surfacing an error.
    #[error("remote desktop portal not available")]
    PortalUnavailable,
    /// The user (or compositor) declined the remote-desktop portal.
    #[error("permission denied for remote control")]
    PermissionDenied,
    /// Failed to negotiate or drive the injection backend.
    #[error("failed to start remote control: {0}")]
    StartFailed(String),
    /// The injector's worker thread/channel is gone.
    #[error("injector is closed")]
    Closed,
}
