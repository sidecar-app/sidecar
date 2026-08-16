//! Synthetic input injection for the remote-control feature.
//!
//! Mirrors the `capture` crate's `#[cfg(target_os)]` backend layout: a small
//! platform-neutral API (`InputEvent`, `Injector`) with one backend module per
//! OS. Only Linux/Wayland (XDG RemoteDesktop portal) is implemented today;
//! macOS (CGEvent) and Windows (SendInput) are stubs that return
//! [`InputError::Unsupported`] so the workspace builds on every platform.
//!
//! Coordinates are normalized `0.0..=1.0` relative to the shared frame, so the
//! transport and the viewer never need to know the host's resolution; each
//! backend maps them to its own screen geometry.

mod error;
pub use error::InputError;

#[cfg(target_os = "linux")]
mod backend_linux;
#[cfg(target_os = "macos")]
mod backend_macos;
#[cfg(target_os = "windows")]
mod backend_windows;
#[cfg(target_os = "linux")]
mod backend_wlr;
#[cfg(target_os = "linux")]
mod keymap;

/// evdev mouse button codes (`BTN_LEFT/RIGHT/MIDDLE` from
/// `<linux/input-event-codes.h>`), shared by both Linux input backends.
#[cfg(target_os = "linux")]
pub(crate) const BTN_LEFT: u32 = 0x110;
#[cfg(target_os = "linux")]
pub(crate) const BTN_RIGHT: u32 = 0x111;
#[cfg(target_os = "linux")]
pub(crate) const BTN_MIDDLE: u32 = 0x112;

/// A mouse button, mapped to the platform's native button code by the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// A single input action to inject on the host.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// Move the pointer to an absolute, normalized position (`0.0..=1.0`).
    PointerMotionAbsolute { x: f64, y: f64 },
    /// Press or release a mouse button.
    PointerButton { button: MouseButton, pressed: bool },
    /// Scroll. `dx`/`dy` are continuous deltas (positive `dy` scrolls down).
    PointerAxis { dx: f64, dy: f64 },
    /// Press or release a key, identified by its DOM `KeyboardEvent.code`
    /// (e.g. `"KeyA"`, `"Enter"`). The backend maps it to a physical keycode;
    /// the host's keymap then decides the produced character.
    Key { code: String, pressed: bool },
}

/// A handle to a running injection backend.
///
/// Holds only a send closure over the backend's event channel, so it is `Send +
/// Sync` regardless of the (often `!Send`) session/connection it drives, and
/// each backend can use whatever channel type fits its event loop. Dropping the
/// last `Injector` stops the backend and releases the OS permission.
pub struct Injector {
    send: Box<dyn Fn(InputEvent) -> Result<(), InputError> + Send + Sync>,
}

impl Injector {
    /// Negotiate the platform input backend, prompting for permission if
    /// needed. Resolves once the backend is ready (or the user declines).
    ///
    /// On Linux this tries the XDG RemoteDesktop portal first and falls back to
    /// the wlroots virtual-input protocols when the portal lacks RemoteDesktop
    /// (Sway, Hyprland, etc.). Must be called from within a Tokio runtime.
    pub async fn create() -> Result<Self, InputError> {
        #[cfg(target_os = "linux")]
        {
            match backend_linux::spawn().await {
                Err(InputError::PortalUnavailable) => {
                    tracing::info!(
                        "RemoteDesktop portal unavailable; falling back to wlroots virtual input"
                    );
                    backend_wlr::spawn().await
                }
                other => other,
            }
        }
        #[cfg(target_os = "macos")]
        {
            backend_macos::spawn().await
        }
        #[cfg(target_os = "windows")]
        {
            backend_windows::spawn().await
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(InputError::Unsupported)
        }
    }

    /// Queue an event for injection. Non-blocking and fire-and-forget; returns
    /// [`InputError::Closed`] only if the backend has stopped.
    pub fn inject(&self, event: InputEvent) -> Result<(), InputError> {
        (self.send)(event)
    }

    /// Construct a handle from a backend's send function. Used by backends.
    pub(crate) fn from_fn(
        send: impl Fn(InputEvent) -> Result<(), InputError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            send: Box::new(send),
        }
    }
}

#[cfg(test)]
mod smoke {
    use super::*;

    /// Negotiates a real injection backend (portal or wlroots fallback) and
    /// drops it. Creates/destroys virtual devices but injects nothing, so it
    /// has no visible effect. Requires a live Wayland session, hence `#[ignore]`.
    /// Run on the target machine with:
    ///   cargo test -p input -- --ignored --nocapture create_injector_on_this_session
    #[test]
    #[ignore = "requires a live Wayland session with RemoteDesktop portal or wlroots virtual input"]
    fn create_injector_on_this_session() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(Injector::create());
        match &result {
            Ok(_) => eprintln!("injector created OK"),
            Err(e) => eprintln!("injector failed: {e}"),
        }
        assert!(result.is_ok(), "create() failed: {:?}", result.err());
    }
}
