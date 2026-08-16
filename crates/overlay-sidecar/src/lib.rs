mod cursor;
pub mod drawing;
mod ipc;
mod wayland;

use std::sync::{Arc, Mutex};

use calloop::EventLoop;
use calloop::channel::{Channel, Event as ChannelEvent, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;

use crate::ipc::server::{self, ClientWriters};
use ::ipc::{HostToSidecar, SidecarToHost, is_valid_hex_color};

/// Run the overlay sidecar process.
///
/// This is the main entry point, called either from `sidecar overlay`
/// (CLI) or from the Tauri host-app via `--overlay`.
///
/// The function runs a blocking Wayland event loop and never returns
/// under normal operation; it exits via `std::process::exit`.
pub fn run(test_mode: bool, display_id: u32) {
    // Set parent death signal so sidecar dies if host crashes
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }

    if test_mode {
        tracing::info!("Running in TEST mode (standalone, draw mode enabled)");
        tracing::info!(
            "Draw on the overlay with your mouse. Press Escape to exit, D to toggle draw mode."
        );
        eprintln!(
            "overlay-sidecar test mode: draw with mouse, Escape to exit, D to toggle draw/passthrough"
        );
    }

    let _ = display_id; // reserved for multi-monitor support

    let sock_path = ::ipc::socket_path();
    tracing::info!(display_id, socket = %sock_path, "overlay-sidecar starting");

    let (conn, event_queue, mut state) = match wayland::setup::connect() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to connect to Wayland: {}", e);
            report_error_and_exit(&format!("Wayland connection failed: {e}"));
        }
    };

    let qh = event_queue.handle();
    wayland::surface::create_layer_surface(&mut state, &qh);
    state.qh = Some(qh);

    if test_mode {
        state.draw_mode = true;
    }

    let mut event_loop: EventLoop<wayland::OverlayState> =
        EventLoop::try_new().expect("Failed to create calloop event loop");
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .expect("Failed to insert Wayland source into calloop");

    let (ipc_tx, ipc_rx): (
        calloop::channel::Sender<HostToSidecar>,
        Channel<HostToSidecar>,
    ) = channel();
    let (host_event_tx, host_event_rx): (
        calloop::channel::Sender<SidecarToHost>,
        Channel<SidecarToHost>,
    ) = channel();
    state.host_tx = Some(host_event_tx);

    let client_writers: ClientWriters = Arc::new(Mutex::new(Vec::new()));
    state.client_writers = Some(client_writers.clone());

    // Register IPC command channel (host -> sidecar)
    loop_handle
        .insert_source(ipc_rx, |event, _, state: &mut wayland::OverlayState| {
            if let ChannelEvent::Msg(msg) = event {
                handle_ipc_message(state, msg);
            }
        })
        .expect("Failed to register IPC source");

    // Register host event channel -- broadcasts SidecarToHost events to all clients
    let broadcast_writers = client_writers.clone();
    loop_handle
        .insert_source(
            host_event_rx,
            move |event, _, _state: &mut wayland::OverlayState| {
                if let ChannelEvent::Msg(msg) = event {
                    server::broadcast(&broadcast_writers, &msg);
                }
            },
        )
        .expect("Failed to register host event broadcast source");

    // Shared state snapshot for initial client sync
    let state_snapshot_data: Arc<Mutex<(String, f64, bool)>> = Arc::new(Mutex::new((
        state.host_color.clone(),
        state.host_brush_size,
        state.draw_mode,
    )));
    state.state_snapshot = Some(state_snapshot_data.clone());

    let snap = state_snapshot_data.clone();
    let state_snapshot: Arc<dyn Fn() -> SidecarToHost + Send + Sync> = Arc::new(move || {
        let guard = snap.lock().unwrap();
        SidecarToHost::StateChanged {
            color: guard.0.clone(),
            brush_size: guard.1,
            draw_mode: guard.2,
        }
    });

    match server::start_server(
        &sock_path,
        ipc_tx.clone(),
        client_writers.clone(),
        state_snapshot,
    ) {
        Ok(()) => {}
        Err(e) => {
            if test_mode {
                tracing::warn!("Failed to start IPC server in test mode: {}", e);
                // Continue without server in test mode
            } else {
                tracing::error!("Failed to start IPC server: {}", e);
                std::process::exit(1);
            }
        }
    }

    tracing::info!("Entering event loop");
    loop {
        if state.should_exit {
            tracing::info!("Exiting event loop");
            break;
        }
        if let Err(e) = event_loop.dispatch(std::time::Duration::from_millis(16), &mut state) {
            tracing::error!("Event loop dispatch error: {}", e);
            break;
        }
        // Drive fade animations from the event loop.  The frame callback
        // handler (CompositorHandler::frame) is the primary render driver.
        // This fallback only renders when no frame callback is pending,
        // which avoids frame-callback accumulation and redundant renders
        // while still covering compositors that don't fire callbacks for
        // bufferless commits from schedule_redraw.
        let fading = state.stroke_manager.tick_fades();
        state.cursor_manager.tick_expiry();
        let needs = state.stroke_manager.needs_redraw()
            || state.cursor_manager.needs_redraw()
            || state.pending_redraw;
        if (fading || needs) && !state.frame_callback_pending {
            state.pending_redraw = false;
            wayland::render::render_frame(&mut state);
        }
    }

    server::cleanup_socket(&sock_path);
    tracing::info!("overlay-sidecar exiting");
    if test_mode {
        eprintln!("overlay-sidecar test mode exited");
    }
}

/// Handle an IPC message from the host app.
fn handle_ipc_message(state: &mut wayland::OverlayState, msg: HostToSidecar) {
    match msg {
        HostToSidecar::Annotation { data } => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
                match value.get("t").and_then(|t| t.as_str()) {
                    Some("ds") => {
                        let x = value["x"].as_f64().unwrap_or(0.0);
                        let y = value["y"].as_f64().unwrap_or(0.0);
                        let c = value["c"].as_str().unwrap_or("#FFFFFF").to_string();
                        let w = value["w"].as_f64().unwrap_or(0.003);
                        let l = value["l"].as_str().unwrap_or("").to_string();
                        let id = value["id"].as_str().unwrap_or("").to_string();
                        let p = value["p"].as_bool().unwrap_or(false);
                        state.stroke_manager.handle_draw_start(l, id, x, y, c, w, p);
                    }
                    Some("dm") => {
                        let x = value["x"].as_f64().unwrap_or(0.0);
                        let y = value["y"].as_f64().unwrap_or(0.0);
                        let l = value["l"].as_str().unwrap_or("");
                        state.stroke_manager.handle_draw_move(l, x, y);
                    }
                    Some("de") => {
                        let l = value["l"].as_str().unwrap_or("");
                        state.stroke_manager.handle_draw_end(l);
                    }
                    Some("cl") => {
                        let id = value["id"].as_str().unwrap_or("");
                        let all = value["all"].as_bool().unwrap_or(false);
                        state.stroke_manager.handle_clear(id, all);
                    }
                    Some("c") => {
                        // A viewer's pointer, tagged with their mode ("m"). Draw-mode
                        // pointers (pencil) always render so the host can see who is
                        // annotating; control-mode pointers (arrow) only render while
                        // remote control is active.
                        let is_draw = value["m"].as_str() == Some("d");
                        if is_draw || state.remote_control_active {
                            let id = value["id"].as_str().unwrap_or("").to_string();
                            let x = value["x"].as_f64().unwrap_or(0.0);
                            let y = value["y"].as_f64().unwrap_or(0.0);
                            let color = value["c"].as_str().map(|s| s.to_string());
                            let mode = if is_draw {
                                crate::cursor::CursorMode::Draw
                            } else {
                                crate::cursor::CursorMode::Control
                            };
                            if !id.is_empty() {
                                state.cursor_manager.update(id, x, y, color, mode);
                            }
                        }
                    }
                    _ => { /* ignore unknown packet types */ }
                }
            }
            // Schedule re-render on next frame callback (not immediately)
            wayland::render::schedule_redraw(state);
        }
        HostToSidecar::SetMode { draw } => {
            state.draw_mode = draw;
            if !draw {
                state.drag_state = wayland::DragState::Idle;
            }
            wayland::passthrough::set_clickthrough(state, !draw);
            wayland::render::schedule_redraw(state);
            // Update snapshot and broadcast
            state.update_state_snapshot();
            state.broadcast_state_changed();
        }
        HostToSidecar::Shutdown => {
            state.should_exit = true;
        }
        HostToSidecar::SetColor { color } => {
            if is_valid_hex_color(&color) {
                state.host_color = color;
                tracing::info!("Host color changed to {}", state.host_color);
                state.update_state_snapshot();
                state.broadcast_state_changed();
            }
            // Invalid hex silently ignored per CONTEXT.md
        }
        HostToSidecar::SetBrushSize { size } => {
            state.host_brush_size = size.clamp(0.001, 0.02);
            tracing::info!("Host brush size changed to {}", state.host_brush_size);
            state.update_state_snapshot();
            state.broadcast_state_changed();
        }
        HostToSidecar::ToggleDraw => {
            state.draw_mode = !state.draw_mode;
            if !state.draw_mode {
                state.drag_state = wayland::DragState::Idle;
            }
            wayland::passthrough::set_clickthrough(state, !state.draw_mode);
            wayland::render::schedule_redraw(state);
            state.update_state_snapshot();
            state.broadcast_state_changed();
            tracing::info!(
                "Draw mode toggled to {} via IPC ToggleDraw",
                state.draw_mode
            );
        }
        HostToSidecar::ClearAnnotations => {
            state.stroke_manager.handle_clear("", true);
            // Send cl packet to host for relay to viewers via WebRTC
            if let Some(tx) = &state.host_tx {
                let data = r#"{"t":"cl","id":"host","all":true}"#.to_string();
                let _ = tx.send(::ipc::SidecarToHost::Annotation { data });
            }
            wayland::render::schedule_redraw(state);
            tracing::info!("All annotations cleared via IPC ClearAnnotations");
        }
        HostToSidecar::SetResolution { height } => {
            tracing::info!("Received set-resolution command: height={}", height);
            // Forward to host-app via SidecarToHost channel.
            // The overlay-sidecar cannot control ABR directly -- it relays to host-app.
            if let Some(tx) = &state.host_tx {
                let _ = tx.send(::ipc::SidecarToHost::SetResolutionRequest { height });
            }
        }
        HostToSidecar::RemoteControlActive { active } => {
            tracing::info!("Remote control active: {}", active);
            state.remote_control_active = active;
            if !active {
                // Stop tracking controllers' pointers once control ends, but keep
                // draw-mode pencils (they render regardless of remote control).
                state.cursor_manager.clear_controllers();
            }
            wayland::render::schedule_redraw(state);
        }
    }
}

impl wayland::OverlayState {
    /// Update the shared state snapshot (used for initial client sync).
    pub fn update_state_snapshot(&self) {
        if let Some(ref snapshot) = self.state_snapshot {
            let mut guard = snapshot.lock().unwrap();
            guard.0 = self.host_color.clone();
            guard.1 = self.host_brush_size;
            guard.2 = self.draw_mode;
        }
    }

    /// Broadcast a StateChanged event to all connected clients.
    pub fn broadcast_state_changed(&self) {
        if let Some(ref writers) = self.client_writers {
            let msg = ::ipc::SidecarToHost::StateChanged {
                color: self.host_color.clone(),
                brush_size: self.host_brush_size,
                draw_mode: self.draw_mode,
            };
            crate::ipc::server::broadcast(writers, &msg);
        }
    }
}

/// Report an error and exit.
fn report_error_and_exit(msg: &str) -> ! {
    tracing::error!("Fatal: {}", msg);
    std::process::exit(1);
}
