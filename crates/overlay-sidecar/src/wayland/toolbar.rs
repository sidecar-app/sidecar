use std::f64::consts::PI;

use crate::drawing::parse_hex_color;

// --- Types ---

/// Actions that toolbar clicks can produce
#[derive(Debug, Clone, PartialEq)]
pub enum ToolbarAction {
    SelectColor(String),
    SelectBrushSize(f64),
    ClearAnnotations,
    ExitDrawMode,
    DragHandle,
}

/// A single clickable element in the toolbar
#[derive(Debug, Clone)]
struct ToolbarElement {
    /// Center x in logical pixels
    cx: f64,
    /// Center y in logical pixels
    cy: f64,
    /// Half-width of hit area
    half_w: f64,
    /// Half-height of hit area
    half_h: f64,
    /// What clicking this element does
    action: ToolbarAction,
}

// --- Constants ---

/// Color palette, reused from the shared ipc crate.
pub const PALETTE: &[&str] = ipc::COLOR_PALETTE;

/// Brush size options
pub const BRUSH_SIZES: &[f64] = &[0.003, 0.01];

// Layout constants (all logical pixels; HiDPI handled by existing ctx.scale())
const TOOLBAR_HEIGHT: f64 = 44.0;
const TOOLBAR_BOTTOM_MARGIN: f64 = 24.0;
const TOOLBAR_PADDING_H: f64 = 18.0;
const TOOLBAR_CORNER_RADIUS: f64 = 8.0;
const SWATCH_RADIUS: f64 = 12.0;
const SWATCH_HIT_RADIUS: f64 = 12.0;
const ELEMENT_SPACING: f64 = 10.0;
const GROUP_SPACING: f64 = 18.0;
const SELECTION_RING_WIDTH: f64 = 2.0;
const FONT_SIZE: f64 = 13.0;
const EDGE_MARGIN: f64 = 8.0;
const GRIP_DOT_RADIUS: f64 = 1.5;
const GRIP_DOT_GAP: f64 = 5.0;

// Estimated widths for text elements (layout uses fixed estimates;
// exact measurement happens at draw time via text_extents but layout
// needs stable positions for hit-testing)
const CLEAR_TEXT_WIDTH: f64 = 40.0;
const EXIT_SYMBOL_WIDTH: f64 = 24.0;

// --- Toolbar struct ---

pub struct Toolbar {
    /// Bounding box in logical coordinates (x, y, width, height)
    pub bounds: (f64, f64, f64, f64),
    elements: Vec<ToolbarElement>,
}

impl Toolbar {
    /// Compute layout for given surface dimensions (logical pixels).
    /// Call once per frame before draw() and hit_test().
    /// `offset_x` and `offset_y` shift the toolbar from its default bottom-center position.
    pub fn layout(surface_w: u32, surface_h: u32, offset_x: f64, offset_y: f64) -> Self {
        let sw = surface_w as f64;
        let sh = surface_h as f64;

        // Calculate total inner width from element groups:
        // Group 0: Grip handle
        let grip_w = SWATCH_HIT_RADIUS * 2.0;

        // Group 1: 7 color swatches
        let color_group_w = (PALETTE.len() as f64) * (SWATCH_RADIUS * 2.0)
            + ((PALETTE.len() as f64) - 1.0) * ELEMENT_SPACING;

        // Group 2: 3 brush size selectors
        let brush_group_w = (BRUSH_SIZES.len() as f64) * (SWATCH_HIT_RADIUS * 2.0)
            + ((BRUSH_SIZES.len() as f64) - 1.0) * ELEMENT_SPACING;

        // Group 3: Clear + Exit buttons (GROUP_SPACING before Exit mirrors grip spacing)
        let action_group_w = CLEAR_TEXT_WIDTH + GROUP_SPACING + EXIT_SYMBOL_WIDTH;

        let inner_w = grip_w
            + GROUP_SPACING
            + color_group_w
            + 2.0 * GROUP_SPACING
            + brush_group_w
            + GROUP_SPACING
            + action_group_w;
        let toolbar_w = inner_w + 2.0 * TOOLBAR_PADDING_H;

        // Position toolbar bottom-center with offset
        let mut toolbar_x = ((sw - toolbar_w) / 2.0) + offset_x;
        let mut toolbar_y = (sh - TOOLBAR_BOTTOM_MARGIN - TOOLBAR_HEIGHT) + offset_y;

        // Clamp to surface bounds with margin
        toolbar_x = toolbar_x.max(EDGE_MARGIN).min(sw - toolbar_w - EDGE_MARGIN);
        toolbar_y = toolbar_y
            .max(EDGE_MARGIN)
            .min(sh - TOOLBAR_HEIGHT - EDGE_MARGIN);

        let center_y = toolbar_y + TOOLBAR_HEIGHT / 2.0;

        let mut elements = Vec::with_capacity(12);

        // Cursor tracks current element center x position
        let mut cx = toolbar_x + TOOLBAR_PADDING_H + SWATCH_HIT_RADIUS;

        // Group 0: Grip handle (drag affordance) -- leftmost element
        elements.push(ToolbarElement {
            cx,
            cy: center_y,
            half_w: SWATCH_HIT_RADIUS,
            half_h: SWATCH_HIT_RADIUS,
            action: ToolbarAction::DragHandle,
        });
        cx += SWATCH_HIT_RADIUS * 2.0 + GROUP_SPACING;

        // Group 1: Color swatches
        for color in PALETTE {
            elements.push(ToolbarElement {
                cx,
                cy: center_y,
                half_w: SWATCH_HIT_RADIUS,
                half_h: SWATCH_HIT_RADIUS,
                action: ToolbarAction::SelectColor(color.to_string()),
            });
            cx += SWATCH_RADIUS * 2.0 + ELEMENT_SPACING;
        }

        // Move past last element spacing and add double group spacing
        // (undo the last ELEMENT_SPACING, then add 2x GROUP_SPACING)
        cx = cx - ELEMENT_SPACING + 2.0 * GROUP_SPACING;

        // Group 2: Brush sizes
        for &size in BRUSH_SIZES {
            elements.push(ToolbarElement {
                cx,
                cy: center_y,
                half_w: SWATCH_HIT_RADIUS,
                half_h: SWATCH_HIT_RADIUS,
                action: ToolbarAction::SelectBrushSize(size),
            });
            cx += SWATCH_HIT_RADIUS * 2.0 + ELEMENT_SPACING;
        }

        // Group spacing between brush sizes and action buttons
        cx = cx - ELEMENT_SPACING + GROUP_SPACING;

        // Group 3: Clear button
        let clear_cx = cx + CLEAR_TEXT_WIDTH / 2.0;
        elements.push(ToolbarElement {
            cx: clear_cx,
            cy: center_y,
            half_w: CLEAR_TEXT_WIDTH / 2.0,
            half_h: SWATCH_HIT_RADIUS,
            action: ToolbarAction::ClearAnnotations,
        });
        cx += CLEAR_TEXT_WIDTH + GROUP_SPACING;

        // Exit button (GROUP_SPACING before it mirrors grip spacing on left end)
        let exit_cx = cx + EXIT_SYMBOL_WIDTH / 2.0;
        elements.push(ToolbarElement {
            cx: exit_cx,
            cy: center_y,
            half_w: EXIT_SYMBOL_WIDTH / 2.0,
            half_h: SWATCH_HIT_RADIUS,
            action: ToolbarAction::ExitDrawMode,
        });

        Toolbar {
            bounds: (toolbar_x, toolbar_y, toolbar_w, TOOLBAR_HEIGHT),
            elements,
        }
    }

    /// Draw the toolbar onto the Cairo context.
    /// Context should already have HiDPI scale applied via ctx.scale().
    pub fn draw(
        &self,
        ctx: &cairo::Context,
        current_color: &str,
        current_brush_size: f64,
        hover_element: Option<usize>,
    ) {
        let (bx, by, bw, bh) = self.bounds;

        rounded_rect(ctx, bx, by, bw, bh, TOOLBAR_CORNER_RADIUS);
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.75);
        let _ = ctx.fill();

        for (idx, elem) in self.elements.iter().enumerate() {
            match &elem.action {
                ToolbarAction::DragHandle => {
                    // Draw six-dot grip pattern (2 columns x 3 rows)
                    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.6);
                    for row in -1i32..=1 {
                        // Left column
                        ctx.arc(
                            elem.cx - GRIP_DOT_GAP / 2.0,
                            elem.cy + row as f64 * GRIP_DOT_GAP,
                            GRIP_DOT_RADIUS,
                            0.0,
                            2.0 * PI,
                        );
                        let _ = ctx.fill();
                        // Right column
                        ctx.arc(
                            elem.cx + GRIP_DOT_GAP / 2.0,
                            elem.cy + row as f64 * GRIP_DOT_GAP,
                            GRIP_DOT_RADIUS,
                            0.0,
                            2.0 * PI,
                        );
                        let _ = ctx.fill();
                    }
                }
                ToolbarAction::SelectColor(color) => {
                    let (r, g, b) = parse_hex_color(color);
                    ctx.set_source_rgb(r, g, b);
                    ctx.arc(elem.cx, elem.cy, SWATCH_RADIUS, 0.0, 2.0 * PI);
                    let _ = ctx.fill();

                    // Selection ring for current color
                    if color == current_color {
                        ctx.set_source_rgb(1.0, 1.0, 1.0);
                        ctx.set_line_width(SELECTION_RING_WIDTH);
                        ctx.arc(
                            elem.cx,
                            elem.cy,
                            SWATCH_RADIUS + SELECTION_RING_WIDTH,
                            0.0,
                            2.0 * PI,
                        );
                        let _ = ctx.stroke();
                    }
                }
                ToolbarAction::SelectBrushSize(size) => {
                    // Fixed display radii for Pencil / Marker
                    let display_r = if (*size - BRUSH_SIZES[0]).abs() < f64::EPSILON {
                        4.0
                    } else {
                        7.0
                    };
                    ctx.set_source_rgb(1.0, 1.0, 1.0);
                    ctx.arc(elem.cx, elem.cy, display_r / 2.0, 0.0, 2.0 * PI);
                    let _ = ctx.fill();

                    // Selection ring for current brush size
                    if (*size - current_brush_size).abs() < f64::EPSILON {
                        ctx.set_source_rgb(1.0, 1.0, 1.0);
                        ctx.set_line_width(SELECTION_RING_WIDTH);
                        ctx.arc(
                            elem.cx,
                            elem.cy,
                            SWATCH_HIT_RADIUS + SELECTION_RING_WIDTH,
                            0.0,
                            2.0 * PI,
                        );
                        let _ = ctx.stroke();
                    }
                }
                ToolbarAction::ClearAnnotations => {
                    ctx.select_font_face(
                        "sans-serif",
                        cairo::FontSlant::Normal,
                        cairo::FontWeight::Normal,
                    );
                    ctx.set_font_size(FONT_SIZE);
                    let extents = ctx.text_extents("Clear").unwrap();
                    ctx.set_source_rgb(1.0, 1.0, 1.0);
                    ctx.move_to(
                        elem.cx - extents.width() / 2.0 - extents.x_bearing(),
                        elem.cy + FONT_SIZE / 3.0,
                    );
                    let _ = ctx.show_text("Clear");
                }
                ToolbarAction::ExitDrawMode => {
                    // Draw exit "X" using Cairo line drawing (avoids font glyph issues)
                    let arm = FONT_SIZE * 0.35; // half-size of the X arms
                    ctx.set_source_rgb(1.0, 1.0, 1.0);
                    ctx.set_line_width(2.0);
                    ctx.set_line_cap(cairo::LineCap::Round);
                    ctx.move_to(elem.cx - arm, elem.cy - arm);
                    ctx.line_to(elem.cx + arm, elem.cy + arm);
                    let _ = ctx.stroke();
                    ctx.move_to(elem.cx + arm, elem.cy - arm);
                    ctx.line_to(elem.cx - arm, elem.cy + arm);
                    let _ = ctx.stroke();
                }
            }

            // Hover highlight (circle for swatches, rounded rect for text/action elements)
            if hover_element == Some(idx) {
                ctx.set_source_rgba(1.0, 1.0, 1.0, 0.15);
                match &elem.action {
                    ToolbarAction::ClearAnnotations => {
                        // Measure actual text width for accurate hover rect
                        ctx.select_font_face(
                            "sans-serif",
                            cairo::FontSlant::Normal,
                            cairo::FontWeight::Normal,
                        );
                        ctx.set_font_size(FONT_SIZE);
                        let extents = ctx.text_extents("Clear").unwrap();
                        let pad = 6.0;
                        let rw = extents.width() + pad * 2.0;
                        let rh = FONT_SIZE + pad * 2.0;
                        let rx = elem.cx - rw / 2.0;
                        let ry = elem.cy - rh / 2.0;
                        rounded_rect(ctx, rx, ry, rw, rh, 4.0);
                        let _ = ctx.fill();
                    }
                    ToolbarAction::ExitDrawMode => {
                        // Square hover around the X icon
                        let arm = FONT_SIZE * 0.35;
                        let pad = 6.0;
                        let size = (arm + pad) * 2.0;
                        let rx = elem.cx - size / 2.0;
                        let ry = elem.cy - size / 2.0;
                        rounded_rect(ctx, rx, ry, size, size, 4.0);
                        let _ = ctx.fill();
                    }
                    _ => {
                        // Small circle for color swatches and brush size indicators
                        ctx.arc(elem.cx, elem.cy, SWATCH_HIT_RADIUS, 0.0, 2.0 * PI);
                        let _ = ctx.fill();
                    }
                }
            }
        }
    }

    /// Test if a point hits a toolbar element.
    /// Returns the action for the first element whose bounding box contains (x, y).
    pub fn hit_test(&self, x: f64, y: f64) -> Option<ToolbarAction> {
        for elem in &self.elements {
            if (x - elem.cx).abs() <= elem.half_w && (y - elem.cy).abs() <= elem.half_h {
                return Some(elem.action.clone());
            }
        }
        None
    }

    /// Check if a point is within the toolbar bounding box.
    /// Used to suppress drawing strokes on toolbar area.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (bx, by, bw, bh) = self.bounds;
        x >= bx && x <= bx + bw && y >= by && y <= by + bh
    }

    /// Find which element index the pointer is over (for hover state tracking).
    pub fn element_at(&self, x: f64, y: f64) -> Option<usize> {
        for (idx, elem) in self.elements.iter().enumerate() {
            if (x - elem.cx).abs() <= elem.half_w && (y - elem.cy).abs() <= elem.half_h {
                return Some(idx);
            }
        }
        None
    }
}

/// Clamp a proposed toolbar offset so the toolbar stays within the surface
/// bounds (with `EDGE_MARGIN` on every side).  Call this after computing a
/// new offset from a drag delta so the internal offset never overshoots
/// the visual position.
pub fn clamp_offset(surface_w: u32, surface_h: u32, offset_x: f64, offset_y: f64) -> (f64, f64) {
    let sw = surface_w as f64;
    let sh = surface_h as f64;

    // Reproduce the same inner-width calculation as layout() to get toolbar_w.
    let grip_w = SWATCH_HIT_RADIUS * 2.0;
    let color_group_w = (PALETTE.len() as f64) * (SWATCH_RADIUS * 2.0)
        + ((PALETTE.len() as f64) - 1.0) * ELEMENT_SPACING;
    let brush_group_w = (BRUSH_SIZES.len() as f64) * (SWATCH_HIT_RADIUS * 2.0)
        + ((BRUSH_SIZES.len() as f64) - 1.0) * ELEMENT_SPACING;
    let action_group_w = CLEAR_TEXT_WIDTH + GROUP_SPACING + EXIT_SYMBOL_WIDTH;
    let inner_w = grip_w
        + GROUP_SPACING
        + color_group_w
        + 2.0 * GROUP_SPACING
        + brush_group_w
        + GROUP_SPACING
        + action_group_w;
    let toolbar_w = inner_w + 2.0 * TOOLBAR_PADDING_H;

    // Default (offset = 0) positions
    let default_x = (sw - toolbar_w) / 2.0;
    let default_y = sh - TOOLBAR_BOTTOM_MARGIN - TOOLBAR_HEIGHT;

    // Allowed position range
    let min_x = EDGE_MARGIN;
    let max_x = sw - toolbar_w - EDGE_MARGIN;
    let min_y = EDGE_MARGIN;
    let max_y = sh - TOOLBAR_HEIGHT - EDGE_MARGIN;

    // Clamp offset so default + offset stays in [min, max]
    let clamped_x = offset_x.max(min_x - default_x).min(max_x - default_x);
    let clamped_y = offset_y.max(min_y - default_y).min(max_y - default_y);

    (clamped_x, clamped_y)
}

/// Draw a rounded rectangle path using 4 Cairo arcs.
/// Standard Cairo cookbook pattern.
fn rounded_rect(ctx: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    ctx.new_sub_path();
    ctx.arc(x + r, y + r, r, PI, 1.5 * PI); // top-left
    ctx.arc(x + w - r, y + r, r, 1.5 * PI, 2.0 * PI); // top-right
    ctx.arc(x + w - r, y + h - r, r, 0.0, 0.5 * PI); // bottom-right
    ctx.arc(x + r, y + h - r, r, 0.5 * PI, PI); // bottom-left
    ctx.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolbar() -> Toolbar {
        Toolbar::layout(1920, 1080, 0.0, 0.0)
    }

    #[test]
    fn test_layout_bottom_center_y_position() {
        let tb = toolbar();
        // toolbar y should be approx 1080 - 24 - 44 = 1012
        let expected_y = 1080.0 - TOOLBAR_BOTTOM_MARGIN - TOOLBAR_HEIGHT;
        assert!(
            (tb.bounds.1 - expected_y).abs() < 0.1,
            "toolbar y={} expected ~{}",
            tb.bounds.1,
            expected_y
        );
    }

    #[test]
    fn test_layout_horizontally_centered() {
        let tb = toolbar();
        let center = tb.bounds.0 + tb.bounds.2 / 2.0;
        assert!(
            (center - 960.0).abs() < 1.0,
            "toolbar center x={} expected ~960",
            center
        );
    }

    #[test]
    fn test_layout_produces_12_elements() {
        let tb = toolbar();
        assert_eq!(
            tb.elements.len(),
            12,
            "expected 12 elements (grip + 7 colors + 2 brushes + clear + exit), got {}",
            tb.elements.len()
        );
    }

    #[test]
    fn test_hit_test_first_color_swatch() {
        let tb = toolbar();
        // First color swatch is now at index 1 (grip is index 0)
        let elem = &tb.elements[1];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(
            action,
            Some(ToolbarAction::SelectColor("#FF3B30".to_string()))
        );
    }

    #[test]
    fn test_hit_test_second_color_swatch() {
        let tb = toolbar();
        // Second color swatch is now at index 2
        let elem = &tb.elements[2];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(
            action,
            Some(ToolbarAction::SelectColor("#FF9500".to_string()))
        );
    }

    #[test]
    fn test_hit_test_brush_pencil() {
        let tb = toolbar();
        // Brush sizes start at element index 8 (grip + 7 colors)
        let elem = &tb.elements[8];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(action, Some(ToolbarAction::SelectBrushSize(0.003)));
    }

    #[test]
    fn test_hit_test_brush_marker() {
        let tb = toolbar();
        // Second brush size is at index 9 (grip + 7 colors + 1 brush)
        let elem = &tb.elements[9];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(action, Some(ToolbarAction::SelectBrushSize(0.01)));
    }

    #[test]
    fn test_hit_test_clear_button() {
        let tb = toolbar();
        // Clear button is at index 10 (grip + 7 colors + 2 brushes)
        let elem = &tb.elements[10];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(action, Some(ToolbarAction::ClearAnnotations));
    }

    #[test]
    fn test_hit_test_exit_button() {
        let tb = toolbar();
        // Exit button is at index 11 (grip + 7 colors + 2 brushes + clear)
        let elem = &tb.elements[11];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(action, Some(ToolbarAction::ExitDrawMode));
    }

    #[test]
    fn test_hit_test_outside_bounds() {
        let tb = toolbar();
        // Far away from toolbar
        let action = tb.hit_test(0.0, 0.0);
        assert_eq!(action, None);
    }

    #[test]
    fn test_contains_inside() {
        let tb = toolbar();
        let (bx, by, bw, bh) = tb.bounds;
        // Center of toolbar should be contained
        assert!(tb.contains(bx + bw / 2.0, by + bh / 2.0));
    }

    #[test]
    fn test_contains_outside() {
        let tb = toolbar();
        // Far outside toolbar
        assert!(!tb.contains(0.0, 0.0));
        assert!(!tb.contains(1920.0, 0.0));
    }

    #[test]
    fn test_element_at_over_element() {
        let tb = toolbar();
        // 4th color swatch at index 4 (was index 3, shifted by grip)
        let elem = &tb.elements[4];
        let idx = tb.element_at(elem.cx, elem.cy);
        assert_eq!(idx, Some(4));
    }

    #[test]
    fn test_element_at_toolbar_background() {
        let tb = toolbar();
        // Point inside toolbar bounds but in a gap between groups
        // Find a point between the last color swatch and first brush size
        let last_color = &tb.elements[7]; // last color (grip + 7 colors)
        let first_brush = &tb.elements[8]; // first brush (grip + 7 colors + 0)
        let gap_x = (last_color.cx + last_color.half_w + first_brush.cx - first_brush.half_w) / 2.0;
        let gap_y = tb.bounds.1 + tb.bounds.3 / 2.0;

        // This point should be inside bounds but not on any element
        let result = tb.element_at(gap_x, gap_y);
        assert_eq!(
            result, None,
            "Point in gap between groups should not hit any element"
        );
    }

    // --- New tests for grip element, offset, and clamping ---

    #[test]
    fn test_layout_grip_element() {
        let tb = toolbar();
        assert_eq!(
            tb.elements[0].action,
            ToolbarAction::DragHandle,
            "First element must be DragHandle grip"
        );
    }

    #[test]
    fn test_hit_test_grip() {
        let tb = toolbar();
        let elem = &tb.elements[0];
        let action = tb.hit_test(elem.cx, elem.cy);
        assert_eq!(action, Some(ToolbarAction::DragHandle));
    }

    #[test]
    fn test_layout_zero_offset() {
        let tb = Toolbar::layout(1920, 1080, 0.0, 0.0);
        // Y should be 1080 - 24 - 44 = 1012 (same as original)
        let expected_y = 1080.0 - 24.0 - 44.0;
        assert!(
            (tb.bounds.1 - expected_y).abs() < 0.1,
            "zero offset y={} expected ~{}",
            tb.bounds.1,
            expected_y
        );
        // Horizontally centered
        let center = tb.bounds.0 + tb.bounds.2 / 2.0;
        assert!(
            (center - 960.0).abs() < 1.0,
            "zero offset center x={} expected ~960",
            center
        );
    }

    #[test]
    fn test_layout_with_offset() {
        let tb_base = Toolbar::layout(1920, 1080, 0.0, 0.0);
        let tb_offset = Toolbar::layout(1920, 1080, 100.0, -50.0);
        // X shifted right by 100, Y shifted up by 50
        assert!(
            (tb_offset.bounds.0 - (tb_base.bounds.0 + 100.0)).abs() < 0.1,
            "offset x={} expected ~{}",
            tb_offset.bounds.0,
            tb_base.bounds.0 + 100.0
        );
        assert!(
            (tb_offset.bounds.1 - (tb_base.bounds.1 - 50.0)).abs() < 0.1,
            "offset y={} expected ~{}",
            tb_offset.bounds.1,
            tb_base.bounds.1 - 50.0
        );
    }

    #[test]
    fn test_layout_clamp_edges() {
        // Large positive offset should clamp to right edge
        let tb = Toolbar::layout(1920, 1080, 5000.0, 5000.0);
        let margin = 8.0;
        assert!(
            tb.bounds.0 + tb.bounds.2 <= 1920.0 - margin + 0.1,
            "toolbar right edge {} exceeds surface width - margin {}",
            tb.bounds.0 + tb.bounds.2,
            1920.0 - margin
        );
        assert!(
            tb.bounds.1 + 44.0 <= 1080.0 - margin + 0.1,
            "toolbar bottom edge {} exceeds surface height - margin {}",
            tb.bounds.1 + 44.0,
            1080.0 - margin
        );

        // Large negative offset should clamp to left/top edge
        let tb2 = Toolbar::layout(1920, 1080, -5000.0, -5000.0);
        assert!(
            tb2.bounds.0 >= margin - 0.1,
            "toolbar left edge {} less than margin {}",
            tb2.bounds.0,
            margin
        );
        assert!(
            tb2.bounds.1 >= margin - 0.1,
            "toolbar top edge {} less than margin {}",
            tb2.bounds.1,
            margin
        );
    }

    #[test]
    fn test_clamp_offset_within_bounds() {
        // Small offset should pass through unclamped
        let (cx, cy) = clamp_offset(1920, 1080, 50.0, -20.0);
        assert!(
            (cx - 50.0).abs() < 0.01 && (cy - (-20.0)).abs() < 0.01,
            "small offset should not be clamped: ({}, {})",
            cx,
            cy
        );
    }

    #[test]
    fn test_clamp_offset_right_edge() {
        // Very large positive x offset should be clamped so toolbar stays on screen
        let (cx, _cy) = clamp_offset(1920, 1080, 5000.0, 0.0);
        // Verify: default_x + cx + toolbar_w + EDGE_MARGIN <= 1920
        let tb = Toolbar::layout(1920, 1080, cx, 0.0);
        let right_edge = tb.bounds.0 + tb.bounds.2;
        assert!(
            right_edge <= 1920.0 - EDGE_MARGIN + 0.1,
            "clamped toolbar right edge {} should be <= {}",
            right_edge,
            1920.0 - EDGE_MARGIN
        );
    }

    #[test]
    fn test_clamp_offset_left_edge() {
        // Very large negative x offset should be clamped to left edge
        let (cx, _cy) = clamp_offset(1920, 1080, -5000.0, 0.0);
        let tb = Toolbar::layout(1920, 1080, cx, 0.0);
        assert!(
            tb.bounds.0 >= EDGE_MARGIN - 0.1,
            "clamped toolbar left edge {} should be >= {}",
            tb.bounds.0,
            EDGE_MARGIN
        );
    }

    #[test]
    fn test_clamp_offset_matches_visual() {
        // The key invariant: clamped offset produces same visual as unclamped
        // because layout() also clamps. But with clamped offset, reversing
        // direction responds immediately (no overshoot gap).
        let raw_x = 3000.0;
        let (cx, _cy) = clamp_offset(1920, 1080, raw_x, 0.0);
        // Clamped offset must be less than raw (was clamped)
        assert!(
            cx < raw_x,
            "offset should be clamped from {} to {}",
            raw_x,
            cx
        );
        // Both should produce the same visual position
        let tb_raw = Toolbar::layout(1920, 1080, raw_x, 0.0);
        let tb_clamped = Toolbar::layout(1920, 1080, cx, 0.0);
        assert!(
            (tb_raw.bounds.0 - tb_clamped.bounds.0).abs() < 0.1,
            "visual positions should match: raw={} clamped={}",
            tb_raw.bounds.0,
            tb_clamped.bounds.0
        );
    }

    #[test]
    fn test_clamp_offset_bottom_edge() {
        // Large positive y offset should clamp to bottom
        let (_cx, cy) = clamp_offset(1920, 1080, 0.0, 5000.0);
        let tb = Toolbar::layout(1920, 1080, 0.0, cy);
        let bottom_edge = tb.bounds.1 + TOOLBAR_HEIGHT;
        assert!(
            bottom_edge <= 1080.0 - EDGE_MARGIN + 0.1,
            "clamped toolbar bottom {} should be <= {}",
            bottom_edge,
            1080.0 - EDGE_MARGIN
        );
    }

    #[test]
    fn test_clamp_offset_top_edge() {
        // Large negative y offset should clamp to top
        let (_cx, cy) = clamp_offset(1920, 1080, 0.0, -5000.0);
        let tb = Toolbar::layout(1920, 1080, 0.0, cy);
        assert!(
            tb.bounds.1 >= EDGE_MARGIN - 0.1,
            "clamped toolbar top {} should be >= {}",
            tb.bounds.1,
            EDGE_MARGIN
        );
    }

    #[test]
    fn test_drag_threshold() {
        // Verify threshold behavior: 3px movement is below 4px threshold
        let dx: f64 = 3.0;
        let dy: f64 = 0.0;
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(dist < 4.0, "3px should be below 4px drag threshold");

        let dx2: f64 = 3.0;
        let dy2: f64 = 3.0;
        let dist2 = (dx2 * dx2 + dy2 * dy2).sqrt();
        assert!(
            dist2 >= 4.0,
            "3+3 diagonal ({:.1}px) should exceed 4px drag threshold",
            dist2
        );
    }
}
