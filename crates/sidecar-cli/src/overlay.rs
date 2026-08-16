//! Overlay sidecar management for the TUI host.
//!
//! On Linux, spawns the overlay-sidecar process (the same Wayland layer-shell
//! overlay used by the Tauri app) and bridges messages between it and the
//! pipeline:
//!
//! - Viewer annotations from the pipeline → forwarded to the sidecar for rendering
//! - Overlay draw strokes from the sidecar → forwarded to the pipeline for broadcast to viewers
//! - Resolution override requests from the sidecar → forwarded to the pipeline's ABR controller
//! - Mode changes from the sidecar → forwarded to the TUI for state display

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command};
use std::sync::Mutex;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Events sent from the overlay sidecar reader thread back to the TUI.
pub enum OverlayEvent {
    /// Draw mode was changed (by the sidecar itself, e.g. user pressed a key on the overlay)
    ModeChanged { draw: bool },
}

/// Handle to a running overlay sidecar process.
///
/// Manages the child process lifetime and provides methods to send
/// commands and annotations to the sidecar. Drops gracefully by
/// sending a shutdown command before killing the process.
pub struct OverlayHandle {
    child: Child,
    writer: Mutex<Option<BufWriter<UnixStream>>>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl OverlayHandle {
    /// Send an annotation packet to the sidecar for rendering.
    pub fn send_annotation(&self, data: &str) {
        let msg = format!(
            r#"{{"cmd":"annotation","data":{}}}"#,
            serde_json::to_string(data).unwrap_or_default()
        );
        self.send_raw(&msg);
    }

    /// Toggle the overlay between draw mode and passthrough mode.
    pub fn send_set_mode(&self, draw: bool) {
        let msg = format!(r#"{{"cmd":"set_mode","draw":{}}}"#, draw);
        self.send_raw(&msg);
    }

    /// Toggle the overlay's remote-control active indicator.
    pub fn send_remote_control_active(&self, active: bool) {
        let msg = format!(r#"{{"cmd":"remote_control_active","active":{}}}"#, active);
        self.send_raw(&msg);
    }

    fn send_raw(&self, msg: &str) {
        if let Ok(mut guard) = self.writer.lock()
            && let Some(ref mut writer) = *guard
        {
            let _ = writeln!(writer, "{}", msg);
            let _ = writer.flush();
        }
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        // Ask the sidecar to exit cleanly, then force-kill if it lingers.
        self.send_raw(r#"{"cmd":"shutdown"}"#);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn the overlay sidecar and connect to it.
///
/// Returns an OverlayHandle and a receiver for overlay events (mode changes).
/// The reader thread forwards sidecar messages (annotations, resolution
/// requests) to the pipeline via the provided channels, and overlay state
/// changes back to the TUI via the returned receiver.
pub fn spawn_overlay(
    annotation_tx: mpsc::UnboundedSender<String>,
    resolution_override_tx: mpsc::UnboundedSender<u32>,
) -> Result<(OverlayHandle, mpsc::UnboundedReceiver<OverlayEvent>), String> {
    let socket_path = ipc::socket_path();

    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current executable: {}", e))?;
    info!(binary = %exe.display(), "Spawning overlay subprocess");

    let child = Command::new(&exe)
        .arg("overlay")
        .env(
            "WAYLAND_DISPLAY",
            std::env::var("WAYLAND_DISPLAY").unwrap_or_default(),
        )
        .env(
            "XDG_RUNTIME_DIR",
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_default(),
        )
        .spawn()
        .map_err(|e| format!("Failed to spawn overlay subprocess: {}", e))?;

    // Retry: the sidecar needs time to bind the socket after spawning.
    let stream = {
        let max_attempts = 50; // 50 * 100ms = 5s
        let mut attempt = 0;
        loop {
            match UnixStream::connect(&socket_path) {
                Ok(s) => break s,
                Err(e) => {
                    attempt += 1;
                    if attempt >= max_attempts {
                        return Err(format!(
                            "Failed to connect to sidecar socket {} after {} attempts: {}",
                            socket_path, max_attempts, e
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    };

    let reader_stream = stream
        .try_clone()
        .map_err(|e| format!("Failed to clone stream: {}", e))?;
    let writer = BufWriter::new(stream);

    let (overlay_event_tx, overlay_event_rx) = mpsc::unbounded_channel();

    // Reader thread: sidecar → pipeline + TUI bridge
    let reader_thread = std::thread::spawn(move || {
        let reader = BufReader::new(reader_stream);
        for line in reader.lines() {
            match line {
                Ok(line) if line.is_empty() => continue,
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                        match value.get("evt").and_then(|e| e.as_str()) {
                            Some("ready") => {
                                info!("Overlay sidecar ready");
                            }
                            Some("annotation") => {
                                if let Some(data) = value.get("data").and_then(|d| d.as_str())
                                    && annotation_tx.send(data.to_string()).is_err()
                                {
                                    warn!("Failed to forward overlay annotation: receiver dropped");
                                    break;
                                }
                            }
                            Some("set_resolution_request") => {
                                if let Some(height) = value.get("height").and_then(|h| h.as_u64()) {
                                    info!(height, "Overlay resolution override request");
                                    let _ = resolution_override_tx.send(height as u32);
                                }
                            }
                            // mode_changed: emitted on explicit draw mode toggle
                            // state_changed: periodic state sync (uses different field name)
                            // Both update the TUI's draw mode indicator.
                            Some("mode_changed") => {
                                if let Some(draw) = value.get("draw").and_then(|d| d.as_bool()) {
                                    debug!(draw, "Overlay mode changed");
                                    let _ =
                                        overlay_event_tx.send(OverlayEvent::ModeChanged { draw });
                                }
                            }
                            Some("state_changed") => {
                                if let Some(draw) = value.get("draw_mode").and_then(|d| d.as_bool())
                                {
                                    let _ =
                                        overlay_event_tx.send(OverlayEvent::ModeChanged { draw });
                                }
                            }
                            Some("error") => {
                                let msg = value
                                    .get("msg")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown");
                                error!("Overlay sidecar error: {}", msg);
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => {
                    info!("Overlay sidecar socket disconnected");
                    break;
                }
            }
        }
    });

    let handle = OverlayHandle {
        child,
        writer: Mutex::new(Some(writer)),
        _reader_thread: Some(reader_thread),
    };
    Ok((handle, overlay_event_rx))
}
