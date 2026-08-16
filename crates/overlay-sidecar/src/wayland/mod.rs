pub mod input;
pub mod passthrough;
pub mod render;
pub mod setup;
pub mod surface;
pub mod toolbar;

use crate::cursor::CursorManager;
use crate::drawing::StrokeManager;
use crate::ipc::protocol::SidecarToHost;
use calloop::channel::Sender as CalloopSender;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerHandler, cursor_shape::CursorShapeManager},
    },
    shell::wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::io::BufWriter;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;

/// Drag state machine for toolbar repositioning.
/// Idle -> PendingDrag (on grip press) -> Dragging (after threshold) -> Idle (on release).
#[derive(Debug, Clone)]
pub enum DragState {
    Idle,
    Pending {
        anchor_x: f64,
        anchor_y: f64,
        original_offset_x: f64,
        original_offset_y: f64,
    },
    Dragging {
        anchor_x: f64,
        anchor_y: f64,
        original_offset_x: f64,
        original_offset_y: f64,
    },
}

pub struct OverlayState {
    pub registry_state: RegistryState,
    pub compositor_state: CompositorState,
    pub layer_shell: LayerShell,
    pub shm: Shm,
    pub seat_state: SeatState,
    pub output_state: OutputState,

    // Surface state
    pub layer_surface: Option<LayerSurface>,
    pub wl_surface: Option<wl_surface::WlSurface>,
    pub pool: Option<SlotPool>,
    pub width: u32,
    pub height: u32,
    pub scale: i32,
    pub configured: bool,

    // Drawing state
    pub stroke_manager: StrokeManager,
    pub draw_mode: bool,

    // Remote-control state: controlling viewers' cursors (host-visible) and
    // whether the host master switch is on (drives the active indicator).
    pub cursor_manager: CursorManager,
    pub remote_control_active: bool,

    // Mutable host drawing settings (changed via IPC SetColor/SetBrushSize)
    pub host_color: String,
    pub host_brush_size: f64,

    // Pointer tracking for draw mode
    pub pointer_x: f64,
    pub pointer_y: f64,
    pub is_drawing: bool,
    pub current_line_id: Option<String>,
    pub draw_counter: u64,

    // Toolbar state (computed per frame, used by input.rs for hit-testing)
    pub toolbar: Option<toolbar::Toolbar>,
    pub toolbar_hover_element: Option<usize>,

    // Toolbar drag state (offset persists across draw mode toggles; drag_state resets)
    pub toolbar_offset_x: f64,
    pub toolbar_offset_y: f64,
    pub drag_state: DragState,

    // General redraw flag set by schedule_redraw() for non-stroke changes
    // (toolbar clicks, hover changes, mode toggles). Consumed by frame callback.
    pub pending_redraw: bool,

    // Modifier key tracking (for shift-persistent strokes)
    pub shift_held: bool,

    // IPC feedback channel (send events back to Tauri)
    pub host_tx: Option<CalloopSender<SidecarToHost>>,

    /// Shared client writer handles for broadcasting IPC events to all connected clients.
    /// Used by ipc::server module. None until socket server is started.
    pub client_writers: Option<Arc<Mutex<Vec<BufWriter<UnixStream>>>>>,

    /// Shared snapshot of drawing state for initial client sync.
    /// Updated by handle_ipc_message on color/brush/mode changes.
    /// Read by the accept loop to send StateChanged to new clients.
    pub state_snapshot: Option<Arc<Mutex<(String, f64, bool)>>>,

    // QueueHandle for requesting frame callbacks from render
    pub qh: Option<QueueHandle<OverlayState>>,

    // Control flags
    pub should_exit: bool,
    pub first_configure: bool,
    pub frame_callback_pending: bool,

    // Seat peripherals
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
    pub pointer: Option<wl_pointer::WlPointer>,

    // Cursor shape protocol support
    pub cursor_shape_manager: Option<CursorShapeManager>,
    pub cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    /// Serial from the most recent pointer Enter event, needed for set_shape().
    pub pointer_enter_serial: Option<u32>,
}

// --- Compositor Handler ---

impl CompositorHandler for OverlayState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        let scale = new_factor.max(1);
        tracing::info!("Scale factor changed to {}", scale);
        self.scale = scale;
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.frame_callback_pending = false;

        // Advance fade animations (may set needs_redraw)
        let still_fading = self.stroke_manager.tick_fades();
        // Expire stale remote-control cursors (may set needs_redraw)
        self.cursor_manager.tick_expiry();

        // Render if strokes/cursors changed OR if a non-stroke redraw was
        // requested (toolbar clicks, hover changes, mode toggles)
        if self.stroke_manager.needs_redraw()
            || self.cursor_manager.needs_redraw()
            || self.pending_redraw
        {
            self.pending_redraw = false;
            render::render_frame(self);
        } else if still_fading {
            // Fades in progress but no visual change yet: keep the callback chain alive
            render::schedule_redraw(self);
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

// --- Output Handler ---

impl OutputHandler for OverlayState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        tracing::debug!("New output detected");
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        tracing::debug!("Output updated");
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        tracing::debug!("Output destroyed");
    }
}

// --- Layer Shell Handler ---

impl LayerShellHandler for OverlayState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        tracing::info!("Layer surface closed by compositor");
        self.should_exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let new_w = configure.new_size.0;
        let new_h = configure.new_size.1;

        if new_w > 0 && new_h > 0 {
            let size_changed = new_w != self.width || new_h != self.height;
            self.width = new_w;
            self.height = new_h;

            if size_changed || self.pool.is_none() {
                let phys_w = self.width * self.scale as u32;
                let phys_h = self.height * self.scale as u32;
                let pool_size = (phys_w * phys_h * 4) as usize;
                match SlotPool::new(pool_size, &self.shm) {
                    Ok(pool) => {
                        self.pool = Some(pool);
                        tracing::info!(
                            "SlotPool created: {}x{} logical, {}x{} physical",
                            self.width,
                            self.height,
                            phys_w,
                            phys_h
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to create SlotPool: {}", e);
                    }
                }
            }
        }

        self.configured = true;

        if !self.first_configure {
            self.first_configure = true;
            tracing::info!(
                "First configure: {}x{} scale={}",
                self.width,
                self.height,
                self.scale
            );

            // In test mode draw_mode starts true, so the overlay accepts input.
            passthrough::set_clickthrough(self, !self.draw_mode);

            if let Some(tx) = &self.host_tx {
                let _ = tx.send(SidecarToHost::Ready);
            }
        }

        render::render_frame(self);
    }
}

// --- Seat Handler ---

impl SeatHandler for OverlayState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
        tracing::debug!("New seat available");
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            tracing::info!("Keyboard capability available");
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => {
                    self.keyboard = Some(keyboard);
                }
                Err(e) => {
                    tracing::warn!("Failed to create keyboard: {}", e);
                }
            }
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            tracing::info!("Pointer capability available");
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    // Create cursor shape device if manager was bound
                    if let Some(ref mgr) = self.cursor_shape_manager {
                        let device = mgr.get_shape_device(&pointer, qh);
                        self.cursor_shape_device = Some(device);
                        tracing::info!("Cursor shape device created");
                    }
                    self.pointer = Some(pointer);
                }
                Err(e) => {
                    tracing::warn!("Failed to create pointer: {}", e);
                }
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            tracing::info!("Keyboard capability removed");
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
        if capability == Capability::Pointer {
            tracing::info!("Pointer capability removed");
            self.pointer.take();
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
        tracing::debug!("Seat removed");
    }
}

// --- SHM Handler ---

impl ShmHandler for OverlayState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

// --- Keyboard Handler ---

impl KeyboardHandler for OverlayState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        tracing::debug!("Keyboard focus entered");
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        tracing::debug!("Keyboard focus left");
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        // Escape: in test mode (no host_tx) → exit; in normal mode → disable draw mode
        if event.keysym == Keysym::Escape {
            if self.host_tx.is_none() {
                tracing::info!("Escape pressed in test mode, exiting");
                self.should_exit = true;
            } else if self.draw_mode {
                self.draw_mode = false;
                self.drag_state = DragState::Idle;
                passthrough::set_clickthrough(self, true);
                render::schedule_redraw(self);
                self.toolbar_hover_element = None;
                if let Some(tx) = &self.host_tx {
                    let _ = tx.send(SidecarToHost::ModeChanged { draw: false });
                }
                tracing::info!("Draw mode disabled via Escape");
            }
        }
        if event.keysym == Keysym::d || event.keysym == Keysym::D {
            self.draw_mode = !self.draw_mode;
            passthrough::set_clickthrough(self, !self.draw_mode);
            render::schedule_redraw(self);
            if !self.draw_mode {
                self.toolbar_hover_element = None;
                self.drag_state = DragState::Idle;
            }
            if let Some(tx) = &self.host_tx {
                let _ = tx.send(SidecarToHost::ModeChanged {
                    draw: self.draw_mode,
                });
            }
            tracing::info!("Draw mode toggled to {} via 'd' key", self.draw_mode);
        }
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.shift_held = modifiers.shift;
    }
}

// --- Pointer Handler ---

impl PointerHandler for OverlayState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        input::handle_pointer_events(self, events);
    }
}

// --- Registry ---

impl ProvidesRegistryState for OverlayState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

// --- Delegate macros ---

delegate_compositor!(OverlayState);
delegate_output!(OverlayState);
delegate_shm!(OverlayState);
delegate_seat!(OverlayState);
delegate_keyboard!(OverlayState);
delegate_pointer!(OverlayState);
delegate_layer!(OverlayState);
delegate_registry!(OverlayState);
