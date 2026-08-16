use anyhow::Context;
use smithay_client_toolkit::{
    compositor::CompositorState,
    output::OutputState,
    registry::RegistryState,
    seat::{SeatState, pointer::cursor_shape::CursorShapeManager},
    shell::wlr_layer::LayerShell,
    shm::Shm,
};
use wayland_client::{Connection, EventQueue, globals::registry_queue_init};

use super::OverlayState;
use crate::drawing::StrokeManager;

/// Connect to the Wayland compositor and bind all required globals.
/// Returns the connection, event queue, and initialized OverlayState.
pub fn connect() -> anyhow::Result<(Connection, EventQueue<OverlayState>, OverlayState)> {
    let conn = Connection::connect_to_env()
        .context("Failed to connect to Wayland compositor. Is WAYLAND_DISPLAY set?")?;
    tracing::info!("Connected to Wayland display");

    let (globals, event_queue) =
        registry_queue_init(&conn).context("Failed to initialize Wayland registry")?;
    let qh = event_queue.handle();

    let compositor_state =
        CompositorState::bind(&globals, &qh).context("wl_compositor not available")?;
    tracing::debug!("Bound compositor");

    let layer_shell = LayerShell::bind(&globals, &qh).context(
        "wlr-layer-shell not available. This compositor may not support overlay surfaces. \
             GNOME/Mutter does not support wlr-layer-shell.",
    )?;
    tracing::debug!("Bound layer shell");

    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;
    tracing::debug!("Bound shared memory");

    let output_state = OutputState::new(&globals, &qh);
    tracing::debug!("Initialized output state");

    let seat_state = SeatState::new(&globals, &qh);
    tracing::debug!("Initialized seat state");

    // Bind cursor shape manager (optional: not all compositors support it)
    let cursor_shape_manager = match CursorShapeManager::bind(&globals, &qh) {
        Ok(mgr) => {
            tracing::info!("Bound wp-cursor-shape-manager-v1");
            Some(mgr)
        }
        Err(e) => {
            tracing::info!(
                "wp-cursor-shape-manager-v1 not available ({}), cursor changes disabled",
                e
            );
            None
        }
    };

    let registry_state = RegistryState::new(&globals);

    let state = OverlayState {
        registry_state,
        compositor_state,
        layer_shell,
        shm,
        seat_state,
        output_state,

        // Surface state -- set during create_layer_surface
        layer_surface: None,
        wl_surface: None,
        pool: None,
        width: 0,
        height: 0,
        scale: 1,
        configured: false,

        // Drawing state
        stroke_manager: StrokeManager::new(),
        draw_mode: false,

        // Remote-control state
        cursor_manager: crate::cursor::CursorManager::new(),
        remote_control_active: false,

        // Mutable host drawing state (defaults per CONTEXT.md)
        host_color: ipc::HOST_DEFAULT_COLOR.to_string(),
        host_brush_size: 0.003,

        // Pointer tracking
        pointer_x: 0.0,
        pointer_y: 0.0,
        is_drawing: false,
        current_line_id: None,
        draw_counter: 0,

        // Toolbar state
        toolbar: None,
        toolbar_hover_element: None,

        // Toolbar drag
        toolbar_offset_x: 0.0,
        toolbar_offset_y: 0.0,
        drag_state: super::DragState::Idle,

        // Redraw and modifier tracking
        pending_redraw: false,
        shift_held: false,

        // IPC -- set before event loop starts
        host_tx: None,

        // IPC server infrastructure -- set before event loop starts
        client_writers: None,
        state_snapshot: None,

        // QueueHandle -- set after event loop creation
        qh: None,

        // Control flags
        should_exit: false,
        first_configure: false,
        frame_callback_pending: false,

        // Seat peripherals
        keyboard: None,
        pointer: None,

        // Cursor shape protocol
        cursor_shape_manager,
        cursor_shape_device: None,
        pointer_enter_serial: None,
    };

    Ok((conn, event_queue, state))
}
