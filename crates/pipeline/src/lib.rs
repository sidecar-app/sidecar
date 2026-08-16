use tokio::sync::{mpsc, watch};
use tracing::{debug, info};

/// Fix glibc's dynamic mmap threshold behavior that causes OOM.
///
/// glibc raises the mmap threshold after freeing large mmap'd allocations.
/// Once the threshold exceeds our frame size (~33MB for 4K), subsequent
/// allocations use arena/brk memory that is NEVER returned to the OS.
/// At 30fps x 33MB, this leaks ~1GB/s.
///
/// Disabling the dynamic threshold and setting a fixed 64KB threshold ensures
/// all large allocations (frames, I420 buffers) always use mmap, which is
/// properly returned to the OS via munmap on free.
pub fn fix_glibc_malloc() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 64 * 1024);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 128 * 1024);
    }
}

/// Events emitted by the pipeline to any consumer (Tauri UI, TUI, etc.)
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    ViewerJoined {
        viewer_id: String,
        viewer_count: u32,
    },
    ViewerLeft {
        viewer_id: String,
        viewer_count: u32,
    },
    RoomExpired,
    LatencyUpdate {
        rtt_ms: f64,
        packet_loss_pct: f64,
        status: String,
        has_measured_rtt: bool,
    },
    ResolutionUpdate {
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
        target_bitrate_kbps: u32,
        max_quantizer: u32,
        target_fps: u32,
        bandwidth_kbps: u32,
        is_relay: bool,
    },
    DataChannelState {
        state: String,
    },
    /// A viewer annotation forwarded from the session relay.
    /// The consumer decides what to do with it (emit to UI, forward to overlay, etc.)
    ViewerAnnotation {
        data: String,
    },
    /// Periodic viewer count sync from the signaling server (not a real join/leave).
    ViewerCountSync {
        viewer_count: u32,
    },
    /// Non-fatal warning for the user (e.g. window/region capture detected).
    Warning {
        message: String,
    },
    /// A viewer activated remote-control mode while the host master switch was
    /// off. The frontend should prompt the host to allow control.
    RemoteControlRequested {
        viewer_id: String,
    },
    /// Remote-control master-switch state and the number of viewers currently
    /// in remote-control mode. Emitted when either changes.
    RemoteControlActive {
        enabled: bool,
        controllers: u32,
    },
}

/// Derive connection status from network stats.
/// Degraded: packet_loss_pct > 5.0 OR rtt_ms > 100.0
/// Otherwise: connected.
pub fn derive_status(rtt_ms: f64, packet_loss_pct: f64) -> &'static str {
    if packet_loss_pct > 5.0 || rtt_ms > 100.0 {
        "degraded"
    } else {
        "connected"
    }
}

/// Handle to a running capture->encode->net pipeline.
///
/// Holds all the resources needed to run and tear down the pipeline.
/// Use `stop()` for ordered teardown: network -> encode -> capture.
pub struct PipelineHandle {
    /// HostSession task handle
    session_handle: tokio::task::JoinHandle<Result<(), net::NetError>>,
    /// Encode thread stop signal
    encode_stop: std::sync::mpsc::SyncSender<()>,
    /// Capture handle (dropping stops capture)
    capture_handle: capture::CaptureHandle,
    /// Event monitoring task handle
    event_handle: tokio::task::JoinHandle<()>,
    /// Room code for this session
    room_code: String,
    /// Channel to send host-originated annotations to all viewers
    annotation_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Channel for resolution override commands -> ABR controller
    pub resolution_override_tx: mpsc::UnboundedSender<u32>,
    /// Channel for encoder params override commands -> pipeline (bypasses ABR)
    pub encoder_params_override_tx: mpsc::UnboundedSender<encode::EncoderParams>,
    /// Channel for the remote-control master switch (true = allow viewer input).
    remote_control_tx: mpsc::UnboundedSender<bool>,
}

impl PipelineHandle {
    /// Get the room code for this session.
    pub fn room_code(&self) -> &str {
        &self.room_code
    }

    /// Get a clone of the annotation channel sender.
    pub fn annotation_tx(&self) -> tokio::sync::mpsc::UnboundedSender<String> {
        self.annotation_tx.clone()
    }

    /// Set the remote-control master switch. When enabled, the pipeline lazily
    /// negotiates the input portal (prompting for permission) and injects
    /// viewer input; when disabled, injection stops and the portal is released.
    pub fn set_remote_control(&self, enabled: bool) {
        let _ = self.remote_control_tx.send(enabled);
    }

    /// Tear down the pipeline in order: network -> encode -> capture.
    pub async fn stop(self) {
        info!("Stopping pipeline...");

        self.session_handle.abort();
        let _ = self.session_handle.await;
        debug!("Session task stopped");

        let _ = self.encode_stop.send(());
        debug!("Encode thread stop signal sent");

        self.event_handle.abort();
        let _ = self.event_handle.await;
        debug!("Event monitoring task stopped");

        // Dropping the capture handle stops capture via its Drop impl.
        drop(self.capture_handle);
        debug!("Capture handle dropped");

        info!("Pipeline stopped");
    }
}

/// Start the full capture -> encode -> net pipeline.
///
/// Chains all three upstream crates into a single pipeline:
/// 1. Capture: grabs screen frames
/// 2. Encode: VP9-encodes frames into packets
/// 3. Net: sends packets to viewers via WebRTC
///
/// Also spawns ABR feedback and event monitoring tasks.
/// Pipeline events are sent to `event_tx` for the consumer to handle
/// (Tauri UI emits them as app events, TUI renders them in the dashboard).
pub async fn start_pipeline(
    event_tx: mpsc::UnboundedSender<PipelineEvent>,
) -> Result<PipelineHandle, Box<dyn std::error::Error + Send + Sync>> {
    info!("Starting capture...");
    let (mut capture_handle, capture_warning) =
        capture::start_capture(capture::CaptureOptions::default()).await?;

    if let Some(msg) = capture_warning {
        let _ = event_tx.send(PipelineEvent::Warning { message: msg });
    }

    // Swap out the frame receiver: the handle keeps a dummy so it stays
    // Drop-safe, while we own the live receiver for the encode thread.
    let (_, dummy_rx) = std::sync::mpsc::sync_channel(1);
    let frame_rx = std::mem::replace(&mut capture_handle.frames, dummy_rx);

    let max_bitrate_kbps: u32 = std::env::var("SIDECAR_MAX_BITRATE_KBPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16_000);
    let abr_config = encode::AbrConfig {
        max_bitrate_kbps,
        ..encode::AbrConfig::default()
    };
    let initial_params = encode::EncoderParams {
        target_bitrate_kbps: max_bitrate_kbps,
        max_quantizer: 33,
        min_quantizer: 2,
        target_fps: 60,
        target_height: 0,
    };
    let (params_tx, params_rx) = watch::channel(initial_params);

    let (keyframe_request_tx, keyframe_request_rx) = std::sync::mpsc::channel::<()>();
    let (has_viewers_tx, has_viewers_rx) = watch::channel(false);

    let codec = encode::detect_best_codec();
    info!("Starting encoder (codec: {:?})...", codec);
    let (capture_dims_tx, capture_dims_rx) = std::sync::mpsc::sync_channel::<(u32, u32)>(1);
    let (codec_switch_tx, codec_switch_rx) = std::sync::mpsc::sync_channel::<encode::VideoCodec>(1);
    let (packet_rx, encode_stop) = encode::spawn_encode_thread(
        encode::EncoderConfig {
            codec,
            ..Default::default()
        },
        frame_rx,
        Some(params_rx),
        Some(keyframe_request_rx),
        Some(has_viewers_rx),
        Some(capture_dims_tx),
        Some(codec_switch_rx),
    )?;

    let (stats_tx, mut stats_rx) = mpsc::unbounded_channel::<encode::NetworkStats>();
    let (viewer_events_tx, mut viewer_events_rx) = mpsc::unbounded_channel::<net::ViewerEvent>();

    info!("Starting network session...");
    let session_config = net::SessionConfig {
        codec_mime: codec.mime_type().to_string(),
        viewer_events_tx: Some(viewer_events_tx),
        codec_switch_tx: Some(codec_switch_tx),
        ..Default::default()
    };
    let (room_code, session_handle, annotation_tx, mut dc_state_rx, mut annotation_forward_rx) =
        net::HostSession::start(session_config, packet_rx, stats_tx.clone()).await?;
    info!(room_code = %room_code, "Session started");

    let (resolution_override_tx, mut resolution_override_rx) = mpsc::unbounded_channel::<u32>();
    let (encoder_params_override_tx, mut encoder_params_override_rx) =
        mpsc::unbounded_channel::<encode::EncoderParams>();

    let (remote_control_tx, mut remote_control_rx) = mpsc::unbounded_channel::<bool>();

    let resolution_override_tx_ret = resolution_override_tx.clone();
    let encoder_params_override_tx_ret = encoder_params_override_tx.clone();
    let remote_control_tx_ret = remote_control_tx.clone();

    let event_handle = tokio::spawn(async move {
        use encode::AdaptiveBitrateController;
        let mut abr = encode::RuleBasedAbrController::new(abr_config);

        let mut capture_dims_rx = Some(capture_dims_rx);
        let mut capture_dims_set = false;
        let mut capture_width: u32 = 0;
        let mut capture_height: u32 = 0;
        let mut abr_override: Option<encode::EncoderParams> = None;

        // Remote-control state. The host master switch (`rc_enabled`) is the
        // overall gate; the injector is created lazily on first enable (portal
        // consent). Only a single `active_controller` viewer's input is injected
        // at a time -- a mouse-button press claims the slot (tap-to-take-over),
        // and the first viewer to enter RC mode is auto-granted it. All RC-mode
        // viewers' pointers still render as presence cursors regardless.
        let mut rc_enabled = false;
        let mut injector: Option<input::Injector> = None;
        let mut injector_pending = false;
        let mut rc_viewers: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut active_controller: Option<String> = None;
        // Latest pointer position per viewer (normalized), used to warp the host
        // cursor to a controller's spot on click/scroll.
        let mut cursor_pos: std::collections::HashMap<String, (f64, f64)> =
            std::collections::HashMap::new();
        // Inputs the host currently believes the active controller holds. Motion
        // is injected only while a button is held (dragging), so plain hovering
        // never moves the host cursor. These also let us release everything if
        // control ends before the matching "up" arrives (disconnect, focus loss).
        let mut held_buttons: std::collections::HashSet<input::MouseButton> =
            std::collections::HashSet::new();
        let mut held_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        let (injector_ready_tx, mut injector_ready_rx) =
            mpsc::unbounded_channel::<Result<input::Injector, input::InputError>>();

        loop {
            tokio::select! {
                Some(viewer_event) = viewer_events_rx.recv() => {
                    match viewer_event {
                        net::ViewerEvent::Joined { viewer_id, viewer_count } => {
                            let _ = event_tx.send(PipelineEvent::ViewerJoined {
                                viewer_id,
                                viewer_count,
                            });
                            let _ = has_viewers_tx.send(true);
                        }
                        net::ViewerEvent::Left { viewer_id, viewer_count } => {
                            // If the departing viewer was driving remote control,
                            // release anything it held: a disconnect/crash never
                            // sends the matching key/button "up", so it would
                            // otherwise stay stuck down on the host.
                            let was_controller =
                                active_controller.as_deref() == Some(viewer_id.as_str());
                            if was_controller {
                                release_held_input(
                                    injector.as_ref(),
                                    &mut held_keys,
                                    &mut held_buttons,
                                );
                                active_controller = None;
                            }
                            cursor_pos.remove(&viewer_id);
                            let was_rc = rc_viewers.remove(&viewer_id);
                            if was_rc || was_controller {
                                let _ = event_tx.send(PipelineEvent::RemoteControlActive {
                                    enabled: rc_enabled,
                                    controllers: rc_viewers.len() as u32,
                                });
                            }
                            let _ = event_tx.send(PipelineEvent::ViewerLeft {
                                viewer_id,
                                viewer_count,
                            });
                            if viewer_count == 0 {
                                let _ = has_viewers_tx.send(false);
                            }
                        }
                        net::ViewerEvent::Presence { viewer_count } => {
                            let _ = event_tx.send(PipelineEvent::ViewerCountSync { viewer_count });
                            let has = viewer_count > 0;
                            if *has_viewers_tx.borrow() != has {
                                let _ = has_viewers_tx.send(has);
                            }
                        }
                        net::ViewerEvent::RoomExpired => {
                            let _ = event_tx.send(PipelineEvent::RoomExpired);
                        }
                        net::ViewerEvent::CodecFallback { from_codec, to_codec } => {
                            info!(from = %from_codec, to = %to_codec, "Codec fallback triggered");
                            let _ = event_tx.send(PipelineEvent::Warning {
                                message: format!(
                                    "Switched from {} to {} for browser compatibility",
                                    from_codec, to_codec
                                ),
                            });
                        }
                    }
                }
                Some(stats) = stats_rx.recv() => {
                    if !capture_dims_set
                        && let Some(ref rx) = capture_dims_rx
                        && let Ok((w, h)) = rx.try_recv()
                    {
                        info!(width = w, height = h, "Capture dimensions received, building encode profile catalog");
                        abr.set_capture_dimensions(w, h);
                        capture_width = w;
                        capture_height = h;
                        capture_dims_set = true;
                        capture_dims_rx = None;
                    }

                    let status = derive_status(stats.rtt_ms, stats.packet_loss_pct).to_string();
                    let _ = event_tx.send(PipelineEvent::LatencyUpdate {
                        rtt_ms: stats.rtt_ms,
                        packet_loss_pct: stats.packet_loss_pct,
                        status,
                        has_measured_rtt: stats.has_measured_rtt,
                    });

                    let params = if let Some(ref ovr) = abr_override {
                        ovr.clone()
                    } else {
                        abr.set_turn_relay(stats.has_relay);
                        abr.on_network_stats(&stats)
                    };
                    debug!(
                        loss_pct = stats.packet_loss_pct,
                        rtt_ms = stats.rtt_ms,
                        has_rtt = stats.has_measured_rtt,
                        bitrate_kbps = params.target_bitrate_kbps,
                        max_q = params.max_quantizer,
                        fps = params.target_fps,
                        target_height = params.target_height,
                        override_active = abr_override.is_some(),
                        "ABR params updated"
                    );
                    let _ = params_tx.send(params.clone());

                    let effective_height = if params.target_height == 0 { 1080 } else { params.target_height };
                    let (out_w, out_h) = if capture_height > 0 && capture_height > effective_height {
                        let aspect = capture_width as f64 / capture_height as f64;
                        let dst_h = effective_height & !1;
                        let dst_w = ((dst_h as f64 * aspect).round() as u32) & !1;
                        (dst_w, dst_h)
                    } else {
                        (capture_width, capture_height)
                    };

                    let _ = event_tx.send(PipelineEvent::ResolutionUpdate {
                        source_width: capture_width,
                        source_height: capture_height,
                        output_width: out_w,
                        output_height: out_h,
                        target_bitrate_kbps: params.target_bitrate_kbps,
                        max_quantizer: params.max_quantizer,
                        target_fps: params.target_fps,
                        bandwidth_kbps: stats.throughput_kbps,
                        is_relay: stats.has_relay,
                    });
                }
                Some((viewer_id, state)) = dc_state_rx.recv() => {
                    debug!(viewer_id = %viewer_id, state = %state, "Data channel state changed");
                    let _ = event_tx.send(PipelineEvent::DataChannelState {
                        state,
                    });
                }
                Some(annotation_data) = annotation_forward_rx.recv() => {
                    if annotation_data.starts_with(r#"{"_keyframe_request""#) {
                        debug!("Forwarding viewer keyframe request to encoder");
                        let _ = keyframe_request_tx.send(());
                    } else if annotation_data.starts_with(r#"{"_viewport_height":"#) {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&annotation_data)
                            && let Some(h) = parsed.get("_viewport_height").and_then(|v| v.as_u64())
                        {
                            debug!(viewport_height = h, "Setting viewer max height on ABR");
                            abr.set_viewer_max_height(Some(h as u32));
                        }
                    } else {
                        // Parse once; dispatch on the packet type tag. Non-RC
                        // packets (drawing, cursor) still flow to ViewerAnnotation.
                        let parsed = serde_json::from_str::<serde_json::Value>(&annotation_data).ok();
                        let t = parsed.as_ref().and_then(|v| v.get("t")).and_then(|t| t.as_str());
                        match t {
                            // Remote-control mode toggle: track who is in RC mode,
                            // prompt the host if the master switch is off, and
                            // auto-grant the controller slot to the first viewer in.
                            Some("rc") => {
                                let v = parsed.as_ref().unwrap();
                                let on = v.get("on").and_then(|x| x.as_bool()).unwrap_or(false);
                                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let changed = if on { rc_viewers.insert(id.clone()) } else { rc_viewers.remove(&id) };
                                if on {
                                    if !rc_enabled {
                                        let _ = event_tx.send(PipelineEvent::RemoteControlRequested { viewer_id: id.clone() });
                                    } else if active_controller.is_none() {
                                        active_controller = Some(id.clone());
                                    }
                                } else if active_controller.as_deref() == Some(id.as_str()) {
                                    // The active controller left RC mode; free the slot
                                    // and release anything it still held.
                                    release_held_input(
                                        injector.as_ref(),
                                        &mut held_keys,
                                        &mut held_buttons,
                                    );
                                    active_controller = None;
                                }
                                if changed {
                                    let _ = event_tx.send(PipelineEvent::RemoteControlActive {
                                        enabled: rc_enabled,
                                        controllers: rc_viewers.len() as u32,
                                    });
                                }
                            }
                            // Mouse/scroll/key input: injected only from the active
                            // controller. A button PRESS claims the slot first.
                            Some(tag @ ("mb" | "ms" | "kd" | "ku")) => {
                                let v = parsed.as_ref().unwrap();
                                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                let is_press = tag == "mb"
                                    && v.get("p").and_then(|x| x.as_bool()).unwrap_or(false);
                                if is_press
                                    && rc_enabled
                                    && !id.is_empty()
                                    && active_controller.as_deref() != Some(id)
                                {
                                    // New controller takes the slot; release whatever
                                    // the previous one held so it doesn't stick.
                                    release_held_input(
                                        injector.as_ref(),
                                        &mut held_keys,
                                        &mut held_buttons,
                                    );
                                    active_controller = Some(id.to_string());
                                    rc_viewers.insert(id.to_string());
                                    info!(controller = %id, "Remote control taken over");
                                }
                                if rc_enabled
                                    && active_controller.as_deref() == Some(id)
                                    && let Some(inj) = &injector
                                {
                                    let mut dead = false;
                                    // Warp the host pointer to the controller's spot before
                                    // a click/scroll, so it lands where they point. (Hover
                                    // motion is presence-only and never warps the cursor.)
                                    if matches!(tag, "mb" | "ms")
                                        && let Some(&(x, y)) = cursor_pos.get(id)
                                    {
                                        dead |= inj
                                            .inject(input::InputEvent::PointerMotionAbsolute { x, y })
                                            .is_err();
                                    }
                                    if let Some(ev) = parse_input_event(tag, v) {
                                        // Track what's now held so it can be released
                                        // if the matching "up" never arrives.
                                        match &ev {
                                            input::InputEvent::PointerButton { button, pressed } => {
                                                if *pressed {
                                                    held_buttons.insert(*button);
                                                } else {
                                                    held_buttons.remove(button);
                                                }
                                            }
                                            input::InputEvent::Key { code, pressed } => {
                                                if *pressed {
                                                    held_keys.insert(code.clone());
                                                } else {
                                                    held_keys.remove(code);
                                                }
                                            }
                                            _ => {}
                                        }
                                        dead |= inj.inject(ev).is_err();
                                    }
                                    if dead {
                                        // Worker died: revert to a safe disabled state.
                                        injector = None;
                                        rc_enabled = false;
                                        active_controller = None;
                                        held_keys.clear();
                                        held_buttons.clear();
                                        let _ = event_tx.send(PipelineEvent::RemoteControlActive {
                                            enabled: false,
                                            controllers: rc_viewers.len() as u32,
                                        });
                                    }
                                }
                            }
                            // Cursor packet: records each viewer's position (used to warp
                            // the host pointer on click/scroll) and, while the active
                            // controller drags (a button is held), injects motion so the
                            // drag tracks their pointer. Plain hovering never moves the
                            // host cursor, so it doesn't fight the host's own mouse. The
                            // host's real cursor is captured in the video directly.
                            // Always forwarded to the overlay.
                            Some("c") => {
                                let v = parsed.as_ref().unwrap();
                                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                if let (Some(x), Some(y)) = (
                                    v.get("x").and_then(|n| n.as_f64()),
                                    v.get("y").and_then(|n| n.as_f64()),
                                ) {
                                    if !id.is_empty() {
                                        cursor_pos.insert(id.to_string(), (x, y));
                                    }
                                    // Inject motion only while the active controller drags
                                    // (a button is held) -- click-to-warp.
                                    if rc_enabled
                                        && active_controller.as_deref() == Some(id)
                                        && !held_buttons.is_empty()
                                        && let Some(inj) = &injector
                                    {
                                        let _ = inj.inject(
                                            input::InputEvent::PointerMotionAbsolute { x, y },
                                        );
                                    }
                                }
                                let _ = event_tx.send(PipelineEvent::ViewerAnnotation { data: annotation_data });
                            }
                            _ => {
                                let _ = event_tx.send(PipelineEvent::ViewerAnnotation { data: annotation_data });
                            }
                        }
                    }
                }
                Some(enabled) = remote_control_rx.recv() => {
                    rc_enabled = enabled;
                    if !enabled {
                        // Disabling stops control; release anything held before the
                        // reconcile below drops the injector (destroying the virtual
                        // pointer), so nothing stays stuck down on the host.
                        release_held_input(
                            injector.as_ref(),
                            &mut held_keys,
                            &mut held_buttons,
                        );
                        active_controller = None;
                    } else if active_controller.is_none() {
                        // If a viewer already requested control, grant it now.
                        active_controller = rc_viewers.iter().next().cloned();
                    }
                    let _ = event_tx.send(PipelineEvent::RemoteControlActive {
                        enabled: rc_enabled,
                        controllers: rc_viewers.len() as u32,
                    });
                }
                Some(result) = injector_ready_rx.recv() => {
                    injector_pending = false;
                    match result {
                        Ok(inj) => {
                            info!("Remote-control input ready");
                            // If control was released during negotiation, the reconcile
                            // below drops this again.
                            injector = Some(inj);
                        }
                        Err(e) => {
                            rc_enabled = false;
                            active_controller = None;
                            let _ = event_tx.send(PipelineEvent::Warning {
                                message: format!("Remote control unavailable: {e}"),
                            });
                            let _ = event_tx.send(PipelineEvent::RemoteControlActive {
                                enabled: false,
                                controllers: rc_viewers.len() as u32,
                            });
                        }
                    }
                }
                Some(override_params) = encoder_params_override_rx.recv() => {
                    if override_params.target_fps == 0 {
                        info!("Clearing encoder params override, returning to ABR");
                        abr_override = None;
                    } else {
                        info!(
                            bitrate = override_params.target_bitrate_kbps,
                            qp = override_params.max_quantizer,
                            fps = override_params.target_fps,
                            height = override_params.target_height,
                            "Setting encoder params override (ABR bypassed)"
                        );
                        abr_override = Some(override_params.clone());
                        let _ = params_tx.send(override_params);
                    }
                }
                Some(height) = resolution_override_rx.recv() => {
                    if height == 0 {
                        info!("Clearing manual resolution override");
                        abr.set_manual_override(None);
                    } else {
                        info!(height = height, "Setting manual resolution override");
                        abr.set_manual_override(Some(height));
                    }
                    let params = abr.current_params().clone();
                    let _ = params_tx.send(params);
                }
                else => {
                    info!("Event channels closed, stopping event monitor");
                    break;
                }
            }

            // Reconcile the injector with the desired state: it exists only while
            // a viewer is actively controlling. The virtual pointer disables the
            // compositor's hardware cursor (breaking the embedded screencast
            // cursor), so it's created on demand and dropped the moment control
            // ends, letting the real cursor be captured again the rest of the time.
            let want_injector = rc_enabled && active_controller.is_some();
            if want_injector && injector.is_none() && !injector_pending {
                injector_pending = true;
                let ready_tx = injector_ready_tx.clone();
                info!("Negotiating remote-control input backend");
                tokio::spawn(async move {
                    let _ = ready_tx.send(input::Injector::create().await);
                });
            } else if !want_injector && injector.is_some() {
                info!("Releasing remote-control input backend (virtual pointer destroyed)");
                injector = None;
            }
        }
    });

    Ok(PipelineHandle {
        session_handle,
        encode_stop,
        capture_handle,
        event_handle,
        room_code,
        annotation_tx,
        resolution_override_tx: resolution_override_tx_ret,
        encoder_params_override_tx: encoder_params_override_tx_ret,
        remote_control_tx: remote_control_tx_ret,
    })
}

/// Release every key and mouse button the host currently believes is held.
///
/// Used when control ends without the matching "up" events arriving -- the
/// active controller disconnects, leaves RC mode, is taken over, or RC is
/// disabled. Without this a press whose release never came (viewer crash,
/// network drop, browser focus loss) stays stuck down on the host. Best-effort:
/// injection errors are ignored since the injector is being torn down anyway.
/// Always clears the tracking sets, even when no injector is present.
fn release_held_input(
    injector: Option<&input::Injector>,
    held_keys: &mut std::collections::HashSet<String>,
    held_buttons: &mut std::collections::HashSet<input::MouseButton>,
) {
    if let Some(inj) = injector {
        for &button in held_buttons.iter() {
            let _ = inj.inject(input::InputEvent::PointerButton {
                button,
                pressed: false,
            });
        }
        for code in held_keys.iter() {
            let _ = inj.inject(input::InputEvent::Key {
                code: code.clone(),
                pressed: false,
            });
        }
    }
    held_buttons.clear();
    held_keys.clear();
}

/// Translate a parsed remote-control input packet into an [`input::InputEvent`].
fn parse_input_event(t: &str, v: &serde_json::Value) -> Option<input::InputEvent> {
    match t {
        "mb" => {
            let button = match v.get("b").and_then(|n| n.as_u64()).unwrap_or(0) {
                0 => input::MouseButton::Left,
                1 => input::MouseButton::Right,
                2 => input::MouseButton::Middle,
                _ => return None,
            };
            let pressed = v.get("p").and_then(|x| x.as_bool()).unwrap_or(false);
            Some(input::InputEvent::PointerButton { button, pressed })
        }
        "ms" => {
            let dx = v.get("dx").and_then(|n| n.as_f64()).unwrap_or(0.0);
            let dy = v.get("dy").and_then(|n| n.as_f64()).unwrap_or(0.0);
            Some(input::InputEvent::PointerAxis { dx, dy })
        }
        "kd" | "ku" => {
            let code = v.get("k").and_then(|x| x.as_str())?.to_string();
            Some(input::InputEvent::Key {
                code,
                pressed: t == "kd",
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_status_connected() {
        assert_eq!(derive_status(50.0, 1.0), "connected");
        assert_eq!(derive_status(0.0, 0.0), "connected");
        assert_eq!(derive_status(99.9, 4.9), "connected");
    }

    #[test]
    fn test_derive_status_degraded_high_loss() {
        assert_eq!(derive_status(50.0, 5.1), "degraded");
        assert_eq!(derive_status(50.0, 10.0), "degraded");
    }

    #[test]
    fn test_derive_status_degraded_high_rtt() {
        assert_eq!(derive_status(101.0, 1.0), "degraded");
        assert_eq!(derive_status(200.0, 0.0), "degraded");
    }

    #[test]
    fn test_derive_status_degraded_both() {
        assert_eq!(derive_status(150.0, 10.0), "degraded");
    }
}
