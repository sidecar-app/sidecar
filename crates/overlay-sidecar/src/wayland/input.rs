use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

use super::DragState;
use super::OverlayState;
use super::passthrough;
use super::render::schedule_redraw;
use super::toolbar::{ToolbarAction, clamp_offset};
use crate::ipc::protocol::SidecarToHost;

/// Left mouse button code
const BTN_LEFT: u32 = 272;

/// Minimum pointer movement (in logical pixels) before a grip press
/// transitions from PendingDrag to Dragging.  Prevents accidental
/// repositioning on click.
const DRAG_THRESHOLD: f64 = 4.0;

/// Handle pointer events for the overlay surface.
/// In draw mode, pointer events generate annotation packets.
pub fn handle_pointer_events(state: &mut OverlayState, events: &[PointerEvent]) {
    for event in events {
        // Only process events for our surface
        if let Some(ref wl_surf) = state.wl_surface
            && &event.surface != wl_surf
        {
            continue;
        }

        match event.kind {
            PointerEventKind::Enter { serial } => {
                state.pointer_x = event.position.0;
                state.pointer_y = event.position.1;
                state.pointer_enter_serial = Some(serial);
                if state.draw_mode {
                    set_cursor_shape(state, Shape::Crosshair);
                }
            }

            PointerEventKind::Leave { .. } => {
                if state.is_drawing {
                    finish_stroke(state);
                }
            }

            PointerEventKind::Motion { .. } => {
                state.pointer_x = event.position.0;
                state.pointer_y = event.position.1;

                // --- Drag state machine: Motion ---
                if state.draw_mode {
                    match state.drag_state.clone() {
                        DragState::Pending {
                            anchor_x,
                            anchor_y,
                            original_offset_x,
                            original_offset_y,
                        } => {
                            let dx = state.pointer_x - anchor_x;
                            let dy = state.pointer_y - anchor_y;
                            let dist = (dx * dx + dy * dy).sqrt();
                            if dist >= DRAG_THRESHOLD {
                                state.drag_state = DragState::Dragging {
                                    anchor_x,
                                    anchor_y,
                                    original_offset_x,
                                    original_offset_y,
                                };
                                // Update offset (clamped so internal state matches visual)
                                let (cx, cy) = clamp_offset(
                                    state.width,
                                    state.height,
                                    original_offset_x + dx,
                                    original_offset_y + dy,
                                );
                                state.toolbar_offset_x = cx;
                                state.toolbar_offset_y = cy;
                                schedule_redraw(state);
                            }
                            // While in PendingDrag or just transitioned to Dragging,
                            // skip normal hover tracking and drawing
                            continue;
                        }
                        DragState::Dragging {
                            anchor_x,
                            anchor_y,
                            original_offset_x,
                            original_offset_y,
                        } => {
                            let dx = state.pointer_x - anchor_x;
                            let dy = state.pointer_y - anchor_y;
                            let (cx, cy) = clamp_offset(
                                state.width,
                                state.height,
                                original_offset_x + dx,
                                original_offset_y + dy,
                            );
                            state.toolbar_offset_x = cx;
                            state.toolbar_offset_y = cy;
                            schedule_redraw(state);
                            // Skip hover tracking and drawing during drag
                            continue;
                        }
                        DragState::Idle => {
                            // Fall through to existing draw + hover logic
                        }
                    }
                }

                if state.draw_mode
                    && state.is_drawing
                    && let Some(ref line_id) = state.current_line_id.clone()
                {
                    let nx = state.pointer_x / state.width as f64;
                    let ny = state.pointer_y / state.height as f64;

                    state.stroke_manager.handle_draw_move(line_id, nx, ny);

                    // Send dm packet back to host for relay
                    if let Some(tx) = &state.host_tx {
                        let data = format!(
                            r#"{{"t":"dm","x":{},"y":{},"l":"{}","id":"host"}}"#,
                            nx, ny, line_id
                        );
                        let _ = tx.send(SidecarToHost::Annotation { data });
                    }

                    schedule_redraw(state);
                }

                // Track toolbar hover state (only redraw when hover changes)
                if state.draw_mode
                    && let Some(ref toolbar) = state.toolbar
                {
                    let new_hover = toolbar.element_at(state.pointer_x, state.pointer_y);
                    if new_hover != state.toolbar_hover_element {
                        let was_hovering = state.toolbar_hover_element.is_some();
                        let now_hovering = new_hover.is_some();
                        state.toolbar_hover_element = new_hover;
                        schedule_redraw(state);

                        if now_hovering {
                            if new_hover == Some(0) {
                                set_cursor_shape(state, Shape::Grab);
                            } else {
                                set_cursor_shape(state, Shape::Pointer);
                            }
                        } else if was_hovering && !now_hovering {
                            set_cursor_shape(state, Shape::Crosshair);
                        }
                    }
                }
            }

            PointerEventKind::Press { button, .. } => {
                if button == BTN_LEFT && state.draw_mode && !state.is_drawing {
                    // Check toolbar hit FIRST -- toolbar consumes clicks on its area
                    if let Some(ref toolbar) = state.toolbar
                        && toolbar.contains(state.pointer_x, state.pointer_y)
                    {
                        if let Some(action) = toolbar.hit_test(state.pointer_x, state.pointer_y) {
                            if matches!(action, ToolbarAction::DragHandle) {
                                // Enter PendingDrag -- don't dispatch, wait for motion threshold
                                state.drag_state = DragState::Pending {
                                    anchor_x: state.pointer_x,
                                    anchor_y: state.pointer_y,
                                    original_offset_x: state.toolbar_offset_x,
                                    original_offset_y: state.toolbar_offset_y,
                                };
                                // Immediate cursor feedback: closed hand on mouse-down
                                set_cursor_shape(state, Shape::Grabbing);
                            } else {
                                dispatch_toolbar_action(state, action);
                            }
                        }
                        // Click consumed by toolbar (even if on background) -- do NOT start stroke
                        continue;
                    }
                    start_stroke(state);
                }
            }

            PointerEventKind::Release { button, .. } => {
                if button == BTN_LEFT {
                    if !matches!(state.drag_state, DragState::Idle) {
                        state.drag_state = DragState::Idle;
                        if let Some(ref toolbar) = state.toolbar {
                            if toolbar.element_at(state.pointer_x, state.pointer_y) == Some(0) {
                                set_cursor_shape(state, Shape::Grab);
                            } else if toolbar.contains(state.pointer_x, state.pointer_y) {
                                set_cursor_shape(state, Shape::Pointer);
                            } else {
                                set_cursor_shape(state, Shape::Crosshair);
                            }
                            state.toolbar_hover_element =
                                toolbar.element_at(state.pointer_x, state.pointer_y);
                            schedule_redraw(state);
                        }
                        continue;
                    }
                    if state.is_drawing {
                        finish_stroke(state);
                    }
                }
            }

            PointerEventKind::Axis { .. } => {
                // Ignore scroll events
            }
        }
    }
}

/// Start a new stroke at the current pointer position.
fn start_stroke(state: &mut OverlayState) {
    state.draw_counter += 1;
    let line_id = format!("sidecar-{}", state.draw_counter);

    let nx = state.pointer_x / state.width as f64;
    let ny = state.pointer_y / state.height as f64;

    // Shift held → persistent stroke (no fade); otherwise fade after 2s
    let persist = state.shift_held;
    state.stroke_manager.handle_draw_start(
        line_id.clone(),
        "host".to_string(),
        nx,
        ny,
        state.host_color.clone(),
        state.host_brush_size,
        persist,
    );

    state.is_drawing = true;
    state.current_line_id = Some(line_id.clone());

    // Send ds packet back to host for relay to viewers
    if let Some(tx) = &state.host_tx {
        let persist_field = if persist { r#","p":true"# } else { "" };
        let data = format!(
            r#"{{"t":"ds","x":{},"y":{},"c":"{}","w":{},"l":"{}","id":"host"{}}}"#,
            nx, ny, state.host_color, state.host_brush_size, line_id, persist_field
        );
        let _ = tx.send(SidecarToHost::Annotation { data });
    }

    schedule_redraw(state);
}

/// Finish the current stroke.
fn finish_stroke(state: &mut OverlayState) {
    if let Some(ref line_id) = state.current_line_id.clone() {
        state.stroke_manager.handle_draw_end(line_id);

        // Send de packet back to host
        if let Some(tx) = &state.host_tx {
            let data = format!(r#"{{"t":"de","l":"{}","id":"host"}}"#, line_id);
            let _ = tx.send(SidecarToHost::Annotation { data });
        }

        schedule_redraw(state);
    }

    state.is_drawing = false;
    state.current_line_id = None;
}

/// Dispatch a toolbar click action, replicating the same state mutation
/// sequences used by handle_ipc_message in main.rs.
fn dispatch_toolbar_action(state: &mut OverlayState, action: ToolbarAction) {
    match action {
        ToolbarAction::SelectColor(color) => {
            state.host_color = color;
            state.update_state_snapshot();
            state.broadcast_state_changed();
            schedule_redraw(state);
            tracing::info!("Toolbar: color changed to {}", state.host_color);
        }
        ToolbarAction::SelectBrushSize(size) => {
            state.host_brush_size = size;
            state.update_state_snapshot();
            state.broadcast_state_changed();
            schedule_redraw(state);
            tracing::info!("Toolbar: brush size changed to {}", state.host_brush_size);
        }
        ToolbarAction::ClearAnnotations => {
            state.stroke_manager.handle_clear("", true);
            // Send cl packet to host for relay to viewers via WebRTC
            if let Some(tx) = &state.host_tx {
                let data = r#"{"t":"cl","id":"host","all":true}"#.to_string();
                let _ = tx.send(SidecarToHost::Annotation { data });
            }
            schedule_redraw(state);
            tracing::info!("Toolbar: all annotations cleared");
        }
        ToolbarAction::ExitDrawMode => {
            state.draw_mode = false;
            state.drag_state = DragState::Idle;
            passthrough::set_clickthrough(state, true);
            // Revert cursor to default since we're leaving draw mode
            set_cursor_shape(state, Shape::Default);
            schedule_redraw(state);
            state.update_state_snapshot();
            state.broadcast_state_changed();
            // Also notify host via calloop channel
            if let Some(tx) = &state.host_tx {
                let _ = tx.send(crate::ipc::protocol::SidecarToHost::ModeChanged { draw: false });
            }
            tracing::info!("Toolbar: draw mode disabled via exit button");
        }
        ToolbarAction::DragHandle => {
            // Drag handling is implemented in Plan 02 (pointer event handler).
            // This arm exists only so the match is exhaustive.
            tracing::debug!("Toolbar: DragHandle action (handled by pointer drag logic)");
        }
    }
}

/// Set the cursor shape via the wp-cursor-shape-v1 protocol.
/// Silently does nothing if the protocol is not available or no enter serial exists.
fn set_cursor_shape(state: &OverlayState, shape: Shape) {
    if let (Some(device), Some(serial)) = (&state.cursor_shape_device, state.pointer_enter_serial) {
        device.set_shape(serial, shape);
    }
}
