use smithay_client_toolkit::shell::WaylandSurface;
use wayland_client::protocol::wl_shm;

use super::OverlayState;
use crate::drawing::curve::{self, TENSION};
use crate::drawing::{parse_hex_color, scale_point};

/// Request a redraw on the next frame callback. Does NOT render immediately.
/// This is the function input handlers and IPC handlers should call instead
/// of render_frame() to avoid allocating SHM buffers at pointer-event rate.
pub fn schedule_redraw(state: &mut OverlayState) {
    // Mark that a non-stroke redraw is pending (toolbar, hover, mode changes).
    // The frame callback checks this flag alongside stroke_manager.needs_redraw().
    state.pending_redraw = true;

    if state.frame_callback_pending || !state.configured {
        return; // already scheduled or not ready
    }
    if let (Some(ls), Some(qh)) = (&state.layer_surface, &state.qh) {
        let wl_surface = ls.wl_surface();
        wl_surface.frame(qh, wl_surface.clone());
        wl_surface.commit();
        state.frame_callback_pending = true;
    }
}

/// Render all current strokes onto an ARGB8888 SHM buffer using Cairo.
/// Attaches the buffer to the wl_surface and commits.
pub fn render_frame(state: &mut OverlayState) {
    if !state.configured {
        return;
    }

    let pool = match state.pool.as_mut() {
        Some(p) => p,
        None => return,
    };

    let width = state.width;
    let height = state.height;
    let scale = state.scale as u32;

    if width == 0 || height == 0 {
        return;
    }

    let phys_w = width * scale;
    let phys_h = height * scale;
    let stride = (phys_w * 4) as i32;

    let (buffer, canvas) = match pool.create_buffer(
        phys_w as i32,
        phys_h as i32,
        stride,
        wl_shm::Format::Argb8888,
    ) {
        Ok(bc) => bc,
        Err(e) => {
            tracing::error!("Failed to create buffer: {}", e);
            return;
        }
    };

    // SAFETY: canvas is a valid mutable slice from SlotPool that lives
    // for the duration of this function. The Cairo surface borrows it
    // and is dropped before the buffer is attached.
    let cairo_surface = unsafe {
        cairo::ImageSurface::create_for_data_unsafe(
            canvas.as_mut_ptr(),
            cairo::Format::ARgb32,
            phys_w as i32,
            phys_h as i32,
            stride,
        )
    };

    let cairo_surface = match cairo_surface {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to create Cairo surface: {}", e);
            return;
        }
    };

    let ctx = match cairo::Context::new(&cairo_surface) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create Cairo context: {}", e);
            return;
        }
    };

    // Scale context for HiDPI
    ctx.scale(state.scale as f64, state.scale as f64);
    ctx.set_antialias(cairo::Antialias::Best);

    // Clear to fully transparent
    ctx.set_operator(cairo::Operator::Clear);
    if let Err(e) = ctx.paint() {
        tracing::error!("Cairo clear paint failed: {}", e);
        return;
    }
    ctx.set_operator(cairo::Operator::Over);

    for stroke in &state.stroke_manager.strokes {
        if stroke.points.is_empty() {
            continue;
        }

        let (r, g, b) = parse_hex_color(&stroke.color);
        ctx.set_source_rgba(r, g, b, stroke.opacity);
        ctx.set_line_width(stroke.width * height as f64);
        ctx.set_line_cap(cairo::LineCap::Round);
        ctx.set_line_join(cairo::LineJoin::Round);

        // Scale all points from normalized 0-1 to pixel coordinates.
        // Curve fitting MUST operate on pixel coords for correct distance weighting.
        let scaled: Vec<(f64, f64)> = stroke
            .points
            .iter()
            .map(|&(nx, ny)| scale_point(nx, ny, width, height))
            .collect();

        let n = scaled.len();
        ctx.move_to(scaled[0].0, scaled[0].1);

        if n == 1 {
            // Single-point: draw a tiny dot
            ctx.line_to(scaled[0].0 + 0.1, scaled[0].1 + 0.1);
        } else if n == 2 {
            // Two points: straight line (no curve fitting possible)
            ctx.line_to(scaled[1].0, scaled[1].1);
        } else {
            // 3+ points: Catmull-Rom to cubic Bezier (matches Konva tension=0.5)
            let tp = curve::expand_points(&scaled, TENSION);
            let tp_len = tp.len();

            if tp_len >= 4 {
                // First segment: quadratic (converted to cubic) from first point
                // to first expanded midpoint, using first control point.
                // Konva: quadraticCurveTo(tp[0], tp[1], tp[2], tp[3])
                let (cp1x, cp1y, cp2x, cp2y) =
                    curve::quad_to_cubic(scaled[0].0, scaled[0].1, tp[0], tp[1], tp[2], tp[3]);
                ctx.curve_to(cp1x, cp1y, cp2x, cp2y, tp[2], tp[3]);

                // Middle segments: cubic Bezier from expanded points.
                // Each 6-tuple starting at index 4: [cp1x, cp1y, px, py, cp2x, cp2y]
                // Konva: bezierCurveTo(allPoints[n], allPoints[n+1], allPoints[n+2],
                //                      allPoints[n+3], allPoints[n+4], allPoints[n+5])
                let mut i = 4;
                while i + 5 < tp_len {
                    ctx.curve_to(tp[i], tp[i + 1], tp[i + 2], tp[i + 3], tp[i + 4], tp[i + 5]);
                    i += 6;
                }

                // Last segment: quadratic (converted to cubic) to actual last point.
                // Konva: quadraticCurveTo(allPoints[len-2], allPoints[len-1], lastX, lastY)
                let (cp1x, cp1y, cp2x, cp2y) = curve::quad_to_cubic(
                    tp[tp_len - 4],
                    tp[tp_len - 3],
                    tp[tp_len - 2],
                    tp[tp_len - 1],
                    scaled[n - 1].0,
                    scaled[n - 1].1,
                );
                ctx.curve_to(cp1x, cp1y, cp2x, cp2y, scaled[n - 1].0, scaled[n - 1].1);
            } else {
                // Fallback: not enough tension points, use line_to
                for &(sx, sy) in &scaled[1..n] {
                    ctx.line_to(sx, sy);
                }
            }
        }

        if let Err(e) = ctx.stroke() {
            tracing::warn!("Cairo stroke failed: {}", e);
        }
    }

    // Render viewer pointers (color-coded per viewer) on top of strokes: a
    // pencil for viewers who are annotating, an arrow for those controlling.
    // These are captured into the video, so all viewers see them too.
    for cursor in state.cursor_manager.cursors.values() {
        let (cx, cy) = scale_point(cursor.x, cursor.y, width, height);
        let (r, g, b) = parse_hex_color(&cursor.color);
        match cursor.mode {
            crate::cursor::CursorMode::Draw => draw_pencil(&ctx, cx, cy, r, g, b),
            crate::cursor::CursorMode::Control => draw_pointer(&ctx, cx, cy, r, g, b),
        }
    }

    // Remote-control active indicator: a border so the host always knows their
    // input can be driven remotely.
    if state.remote_control_active {
        ctx.set_source_rgba(0.0, 0.78, 0.745, 0.9); // teal (#00C7BE)
        ctx.set_line_width(4.0);
        ctx.rectangle(2.0, 2.0, width as f64 - 4.0, height as f64 - 4.0);
        let _ = ctx.stroke();
    }

    if state.draw_mode {
        let toolbar = super::toolbar::Toolbar::layout(
            width,
            height,
            state.toolbar_offset_x,
            state.toolbar_offset_y,
        );
        toolbar.draw(
            &ctx,
            &state.host_color,
            state.host_brush_size,
            state.toolbar_hover_element,
        );
        state.toolbar = Some(toolbar);
    } else {
        state.toolbar = None;
        state.toolbar_hover_element = None;
    }

    // Flush and drop Cairo objects before using the buffer
    cairo_surface.flush();
    drop(ctx);
    drop(cairo_surface);

    state.stroke_manager.take_redraw();
    state.cursor_manager.take_redraw();

    let layer_surface = match &state.layer_surface {
        Some(ls) => ls,
        None => return,
    };
    let wl_surface = layer_surface.wl_surface();

    wl_surface.set_buffer_scale(state.scale);
    wl_surface.damage_buffer(0, 0, phys_w as i32, phys_h as i32);

    if let Err(e) = buffer.attach_to(wl_surface) {
        tracing::error!("Failed to attach buffer: {}", e);
        return;
    }

    // Request frame callback only if one isn't already pending.
    // This prevents frame callback accumulation when render_frame is
    // called from both the CompositorHandler::frame callback and the
    // event loop body in the same iteration.
    if !state.frame_callback_pending
        && let Some(ref qh) = state.qh
    {
        wl_surface.frame(qh, wl_surface.clone());
        state.frame_callback_pending = true;
    }

    layer_surface.commit();
}

/// Fill a closed polygon glyph whose points are relative to the tip at `(x, y)`
/// (logical pixels), with the viewer `color` and a white outline for contrast on
/// any background.
fn fill_glyph(ctx: &cairo::Context, x: f64, y: f64, pts: &[(f64, f64)], r: f64, g: f64, b: f64) {
    ctx.new_path();
    ctx.move_to(x + pts[0].0, y + pts[0].1);
    for &(dx, dy) in &pts[1..] {
        ctx.line_to(x + dx, y + dy);
    }
    ctx.close_path();

    // White outline first so the fill sits inside it.
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    ctx.set_line_width(1.5);
    ctx.set_line_join(cairo::LineJoin::Round);
    let _ = ctx.stroke_preserve();

    ctx.set_source_rgba(r, g, b, 1.0);
    let _ = ctx.fill();
}

/// Draw a classic arrow pointer with its tip at `(x, y)` (logical pixels),
/// filled with the viewer color and outlined white for contrast on any
/// background. Shape is a standard NW-pointing cursor glyph.
fn draw_pointer(ctx: &cairo::Context, x: f64, y: f64, r: f64, g: f64, b: f64) {
    // Arrow outline relative to the tip at (0, 0), in logical pixels.
    const PTS: &[(f64, f64)] = &[
        (0.0, 0.0),
        (0.0, 16.0),
        (4.0, 12.5),
        (6.5, 18.0),
        (8.5, 17.0),
        (6.0, 11.5),
        (11.0, 11.5),
    ];
    fill_glyph(ctx, x, y, PTS, r, g, b);
}

/// Draw a pencil glyph with its writing tip at `(x, y)` (logical pixels), filled
/// with the viewer color and outlined white. Marks a viewer who is annotating.
/// Shape and coordinates mirror the viewer-side `CanvasOverlay` pencil so the
/// captured overlay matches what viewers render client-side.
fn draw_pencil(ctx: &cairo::Context, x: f64, y: f64, r: f64, g: f64, b: f64) {
    // Pencil silhouette relative to the tip at (0, 0), in logical pixels:
    // a pointed tip at the origin with the body extending toward the SE.
    const PTS: &[(f64, f64)] = &[
        (0.0, 0.0),
        (5.5, 2.2),
        (18.0, 14.7),
        (14.7, 18.0),
        (2.2, 5.5),
    ];
    fill_glyph(ctx, x, y, PTS, r, g, b);

    // Collar line separating the graphite tip from the body.
    ctx.new_path();
    ctx.move_to(x + 5.5, y + 2.2);
    ctx.line_to(x + 2.2, y + 5.5);
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    ctx.set_line_width(1.2);
    ctx.set_line_cap(cairo::LineCap::Round);
    let _ = ctx.stroke();
}
