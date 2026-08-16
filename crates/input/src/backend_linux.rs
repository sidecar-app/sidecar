//! Linux/Wayland input injection via the XDG **RemoteDesktop** portal (ashpd).
//!
//! The portal's absolute pointer positioning (`NotifyPointerMotionAbsolute`)
//! requires a ScreenCast stream bound to the *same* session, so we negotiate a
//! combined RemoteDesktop + ScreenCast session (`RemoteDesktop` implements
//! `IsScreencastSession`). This is intentionally a **separate** portal session
//! from `crates/capture`: remote control is activated lazily, after capture has
//! already started, and the portal does not allow adding devices to an
//! already-started session. A persisted restore token keeps the second consent
//! dialog from reappearing on later sessions.
//!
//! The ashpd session is `!Send` in practice, so it never leaves the worker
//! thread: a dedicated thread owns the session and a current-thread Tokio
//! runtime, receiving [`InputEvent`]s over a channel. The returned [`Injector`]
//! only holds the channel sender and is freely `Send + Sync`.

use std::fs;
use std::path::PathBuf;

use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::enumflags2::BitFlags;

use crate::{
    BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Injector, InputError, InputEvent, MouseButton, keymap,
};

/// Negotiate the portal on a dedicated thread and return a handle once ready.
pub(crate) async fn spawn() -> Result<Injector, InputError> {
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<InputEvent>();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), InputError>>();

    std::thread::Builder::new()
        .name("sidecar-input".into())
        .spawn(move || run_worker(event_rx, ready_tx))
        .map_err(|e| InputError::StartFailed(format!("spawn input thread: {e}")))?;

    // Propagate the negotiation result (Ok, denial, or thread death).
    match ready_rx.await {
        Ok(Ok(())) => Ok(Injector::from_fn(move |ev| {
            event_tx.send(ev).map_err(|_| InputError::Closed)
        })),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(InputError::StartFailed("input thread exited early".into())),
    }
}

fn run_worker(
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<InputEvent>,
    ready_tx: tokio::sync::oneshot::Sender<Result<(), InputError>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready_tx.send(Err(InputError::StartFailed(format!("runtime: {e}"))));
            return;
        }
    };

    rt.block_on(async move {
        let backend = match Backend::negotiate().await {
            Ok(b) => b,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        if ready_tx.send(Ok(())).is_err() {
            return; // caller gave up
        }
        tracing::info!(
            node = backend.stream_node,
            width = backend.width,
            height = backend.height,
            "remote-control input ready"
        );

        // Drain events until the last Injector is dropped (channel closes).
        while let Some(event) = event_rx.recv().await {
            if let Err(e) = backend.handle(event).await {
                tracing::warn!(error = %e, "failed to inject input event");
            }
        }
        tracing::info!("remote-control input stopped");
    });
    // Backend (and its portal session) drops here, releasing the OS permission.
}

struct Backend {
    proxy: RemoteDesktop,
    _screencast: Screencast,
    session: Session<RemoteDesktop>,
    stream_node: u32,
    width: f64,
    height: f64,
}

impl Backend {
    async fn negotiate() -> Result<Self, InputError> {
        let proxy = RemoteDesktop::new().await.map_err(|e| match e {
            // No RemoteDesktop backend (wlroots compositors) -> let the factory
            // fall back to the virtual-input protocols.
            ashpd::Error::PortalNotFound(_) => InputError::PortalUnavailable,
            other => InputError::StartFailed(format!("portal connect: {other}")),
        })?;
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(|e| InputError::StartFailed(format!("create_session: {e}")))?;

        // Request keyboard + pointer control, reusing a restore token if present.
        let restore = load_restore_token();
        let mut dev_opts = SelectDevicesOptions::default()
            .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
            .set_persist_mode(PersistMode::ExplicitlyRevoked);
        if let Some(token) = restore.as_deref() {
            dev_opts = dev_opts.set_restore_token(token);
        }
        proxy
            .select_devices(&session, dev_opts)
            .await
            .map_err(|e| InputError::StartFailed(format!("select_devices: {e}")))?
            .response()
            .map_err(map_response_err)?;

        // Associate a monitor ScreenCast on the SAME session so we get a stream
        // reference for absolute pointer positioning. We never open a PipeWire
        // stream from it -- only its node id + size are used as a coordinate
        // reference.
        let screencast = Screencast::new()
            .await
            .map_err(|e| InputError::StartFailed(format!("screencast connect: {e}")))?;
        let src_opts = SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::Embedded)
            .set_sources(BitFlags::from(SourceType::Monitor))
            .set_multiple(false)
            .set_persist_mode(PersistMode::ExplicitlyRevoked);
        screencast
            .select_sources(&session, src_opts)
            .await
            .map_err(|e| InputError::StartFailed(format!("select_sources: {e}")))?
            .response()
            .map_err(map_response_err)?;

        let selected = proxy
            .start(&session, None, Default::default())
            .await
            .map_err(|e| InputError::StartFailed(format!("start: {e}")))?
            .response()
            .map_err(map_response_err)?;

        if let Some(token) = selected.restore_token() {
            save_restore_token(token);
        }

        let stream = selected
            .streams()
            .first()
            .ok_or_else(|| InputError::StartFailed("no screencast stream for input".into()))?;
        let stream_node = stream.pipe_wire_node_id();
        let (w, h) = stream.size().unwrap_or((0, 0));
        if w <= 0 || h <= 0 {
            return Err(InputError::StartFailed(format!(
                "invalid stream size {w}x{h}"
            )));
        }

        Ok(Self {
            proxy,
            _screencast: screencast,
            session,
            stream_node,
            width: w as f64,
            height: h as f64,
        })
    }

    async fn handle(&self, event: InputEvent) -> Result<(), ashpd::Error> {
        match event {
            InputEvent::PointerMotionAbsolute { x, y } => {
                let px = x.clamp(0.0, 1.0) * self.width;
                let py = y.clamp(0.0, 1.0) * self.height;
                self.proxy
                    .notify_pointer_motion_absolute(
                        &self.session,
                        self.stream_node,
                        px,
                        py,
                        Default::default(),
                    )
                    .await
            }
            InputEvent::PointerButton { button, pressed } => {
                let code = match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Middle => BTN_MIDDLE,
                } as i32;
                self.proxy
                    .notify_pointer_button(
                        &self.session,
                        code,
                        key_state(pressed),
                        Default::default(),
                    )
                    .await
            }
            InputEvent::PointerAxis { dx, dy } => {
                self.proxy
                    .notify_pointer_axis(&self.session, dx, dy, Default::default())
                    .await
            }
            InputEvent::Key { code, pressed } => match keymap::dom_code_to_evdev(&code) {
                Some(keycode) => {
                    self.proxy
                        .notify_keyboard_keycode(
                            &self.session,
                            keycode,
                            key_state(pressed),
                            Default::default(),
                        )
                        .await
                }
                None => {
                    tracing::debug!(code = %code, "ignoring unmapped key");
                    Ok(())
                }
            },
        }
    }
}

fn key_state(pressed: bool) -> KeyState {
    if pressed {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}

fn map_response_err(e: ashpd::Error) -> InputError {
    // A `Response` error means the request completed but was cancelled/denied
    // by the user or compositor; anything else is a hard failure.
    match e {
        ashpd::Error::Response(_) => InputError::PermissionDenied,
        other => InputError::StartFailed(other.to_string()),
    }
}

// --- Restore token persistence (mirrors crates/capture/src/backend_linux.rs) ---

fn xdg_cache_dir() -> Option<PathBuf> {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
}

fn token_path() -> Option<PathBuf> {
    Some(
        xdg_cache_dir()?
            .join("sidecar")
            .join("remote_desktop_restore_token"),
    )
}

fn load_restore_token() -> Option<String> {
    let token = fs::read_to_string(token_path()?).ok()?;
    if token.is_empty() { None } else { Some(token) }
}

fn save_restore_token(token: &str) {
    if let Some(path) = token_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, token);
    }
}
