/// Stroke data model for the overlay sidecar.
/// Mirrors the LineData interface from host-app/src/OverlayWindow.tsx
/// with points stored as normalized 0-1 coordinates.
pub mod curve;

use std::time::Instant;

/// Hold strokes at full opacity for this duration after draw-end before fading.
/// 200ms shorter than viewer's 2.0s to mask video encode/decode latency.
const HOLD_DELAY_SECS: f64 = 1.8;
/// Fade duration after hold period ends. Down from 2.0s for snappier disappearance.
const FADE_DURATION_SECS: f64 = 0.5;

/// Minimum distance between consecutive points in normalized 0-1 space.
/// Prevents sub-pixel jitter from creating staircase artifacts on slow diagonal drawing.
/// At 1920px width: 0.002 = ~3.8 pixels.
const MIN_POINT_DISTANCE: f64 = 0.002;

#[derive(Debug, Clone)]
pub struct StrokeData {
    pub id: String,
    pub sender_id: String,
    pub points: Vec<(f64, f64)>,
    pub color: String,
    pub width: f64,
    pub opacity: f64,
    pub persist: bool,
    pub finished: bool,
    /// When the stroke finished (for fade timing). None if still drawing.
    pub finished_at: Option<Instant>,
}

impl StrokeData {
    pub fn new(
        id: String,
        sender_id: String,
        x: f64,
        y: f64,
        color: String,
        width: f64,
        persist: bool,
    ) -> Self {
        Self {
            id,
            sender_id,
            points: vec![(x, y)],
            color,
            width,
            opacity: 0.8, // Match frontend default opacity
            persist,
            finished: false,
            finished_at: None,
        }
    }

    pub fn add_point(&mut self, x: f64, y: f64) {
        self.points.push((x, y));
    }
}

/// Scale a normalized 0-1 coordinate to physical pixel dimensions.
pub fn scale_point(nx: f64, ny: f64, width: u32, height: u32) -> (f64, f64) {
    (nx * width as f64, ny * height as f64)
}

/// Parse a hex color string like "#FF0000" into (r, g, b) as f64 in 0.0-1.0 range.
/// Returns (1.0, 1.0, 1.0) white on parse failure.
pub fn parse_hex_color(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return (1.0, 1.0, 1.0);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255) as f64 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255) as f64 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255) as f64 / 255.0;
    (r, g, b)
}

/// Manages all active strokes, handling annotation packets.
#[derive(Debug, Default)]
pub struct StrokeManager {
    pub strokes: Vec<StrokeData>,
    needs_redraw: bool,
}

impl StrokeManager {
    pub fn new() -> Self {
        Self {
            strokes: Vec::new(),
            needs_redraw: false,
        }
    }

    /// Handle a draw-start (ds) packet.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_draw_start(
        &mut self,
        id: String,
        sender_id: String,
        x: f64,
        y: f64,
        color: String,
        width: f64,
        persist: bool,
    ) {
        tracing::debug!(
            "draw_start: stroke={} sender={} persist={} total_strokes={}",
            id,
            sender_id,
            persist,
            self.strokes.len() + 1
        );
        self.strokes
            .push(StrokeData::new(id, sender_id, x, y, color, width, persist));
        self.needs_redraw = true;
    }

    /// Handle a draw-move (dm) packet.
    /// Skips points closer than MIN_POINT_DISTANCE to the last accepted point
    /// (squared distance comparison, no sqrt per point).
    pub fn handle_draw_move(&mut self, line_id: &str, x: f64, y: f64) {
        if let Some(stroke) = self.strokes.iter_mut().find(|s| s.id == line_id) {
            if let Some(&(lx, ly)) = stroke.points.last() {
                let dx = x - lx;
                let dy = y - ly;
                if (dx * dx + dy * dy) < MIN_POINT_DISTANCE * MIN_POINT_DISTANCE {
                    return;
                }
            }
            stroke.add_point(x, y);
            self.needs_redraw = true;
        }
    }

    /// Handle a draw-end (de) packet.
    pub fn handle_draw_end(&mut self, line_id: &str) {
        if let Some(stroke) = self.strokes.iter_mut().find(|s| s.id == line_id) {
            stroke.finished = true;
            if !stroke.persist {
                stroke.finished_at = Some(Instant::now());
                tracing::debug!("draw_end: stroke={} fade started", stroke.id);
            } else {
                tracing::debug!("draw_end: stroke={} persistent, no fade", stroke.id);
            }
            self.needs_redraw = true;
        } else {
            tracing::warn!(
                "draw_end: stroke={} NOT FOUND in {} strokes",
                line_id,
                self.strokes.len()
            );
        }
    }

    /// Advance fade animations. Returns true if any strokes are still fading
    /// (caller should keep requesting frame callbacks).
    ///
    /// Strokes hold at full opacity for `HOLD_DELAY_SECS` after finishing,
    /// then fade to invisible over `FADE_DURATION_SECS` using cubic ease-out.
    /// During the hold period, `needs_redraw` is NOT set (no visual change).
    pub fn tick_fades(&mut self) -> bool {
        let now = Instant::now();
        let mut any_fading = false;

        for stroke in &mut self.strokes {
            if let Some(finished_at) = stroke.finished_at {
                let elapsed = now.duration_since(finished_at).as_secs_f64();

                if elapsed < HOLD_DELAY_SECS {
                    any_fading = true;
                    continue;
                }

                let fade_elapsed = elapsed - HOLD_DELAY_SECS;
                let t = (fade_elapsed / FADE_DURATION_SECS).min(1.0);
                let eased_t = 1.0 - (1.0 - t).powi(3); // cubic ease-out
                stroke.opacity = 0.8 * (1.0 - eased_t);
                self.needs_redraw = true;
                if t < 1.0 {
                    any_fading = true;
                }
            }
        }

        self.strokes
            .retain(|s| s.finished_at.is_none() || s.opacity > 0.001);

        any_fading
    }

    /// Handle a clear (cl) packet.
    /// If all=true, remove all strokes. Otherwise remove only strokes from sender_id.
    pub fn handle_clear(&mut self, sender_id: &str, all: bool) {
        if all {
            self.strokes.clear();
        } else {
            self.strokes.retain(|s| s.sender_id != sender_id);
        }
        self.needs_redraw = true;
    }

    pub fn take_redraw(&mut self) -> bool {
        let val = self.needs_redraw;
        self.needs_redraw = false;
        val
    }

    pub fn needs_redraw(&self) -> bool {
        self.needs_redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stroke_data_new() {
        let s = StrokeData::new(
            "l1".into(),
            "user1".into(),
            0.5,
            0.3,
            "#FF0000".into(),
            0.003,
            false,
        );
        assert_eq!(s.id, "l1");
        assert_eq!(s.sender_id, "user1");
        assert_eq!(s.points.len(), 1);
        assert_eq!(s.points[0], (0.5, 0.3));
        assert_eq!(s.color, "#FF0000");
        assert_eq!(s.width, 0.003);
        assert!(!s.persist);
        assert!(!s.finished);
        assert!((s.opacity - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_stroke_data_add_point() {
        let mut s = StrokeData::new(
            "l1".into(),
            "u".into(),
            0.0,
            0.0,
            "#000".into(),
            0.00185,
            false,
        );
        s.add_point(0.5, 0.5);
        s.add_point(1.0, 1.0);
        assert_eq!(s.points.len(), 3);
        assert_eq!(s.points[1], (0.5, 0.5));
        assert_eq!(s.points[2], (1.0, 1.0));
    }

    #[test]
    fn test_scale_point_center() {
        let (x, y) = scale_point(0.5, 0.3, 1920, 1080);
        assert!((x - 960.0).abs() < f64::EPSILON);
        assert!((y - 324.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_scale_point_origin() {
        let (x, y) = scale_point(0.0, 0.0, 1920, 1080);
        assert!((x - 0.0).abs() < f64::EPSILON);
        assert!((y - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_scale_point_max() {
        let (x, y) = scale_point(1.0, 1.0, 1920, 1080);
        assert!((x - 1920.0).abs() < f64::EPSILON);
        assert!((y - 1080.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_hex_color() {
        let (r, g, b) = parse_hex_color("#FF0000");
        assert!((r - 1.0).abs() < 0.01);
        assert!((g - 0.0).abs() < 0.01);
        assert!((b - 0.0).abs() < 0.01);

        let (r, g, b) = parse_hex_color("#00FF00");
        assert!((r - 0.0).abs() < 0.01);
        assert!((g - 1.0).abs() < 0.01);
        assert!((b - 0.0).abs() < 0.01);

        let (r, g, b) = parse_hex_color("#FFD60A");
        assert!((r - 1.0).abs() < 0.01);
        assert!((g - 0.839).abs() < 0.01);
        assert!((b - 0.039).abs() < 0.01);
    }

    #[test]
    fn test_parse_hex_color_invalid() {
        let (r, g, b) = parse_hex_color("bad");
        assert!((r - 1.0).abs() < f64::EPSILON);
        assert!((g - 1.0).abs() < f64::EPSILON);
        assert!((b - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_stroke_manager_draw_start() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.1,
            0.2,
            "#FF0000".into(),
            0.003,
            false,
        );
        assert_eq!(mgr.strokes.len(), 1);
        assert_eq!(mgr.strokes[0].id, "l1");
        assert!(mgr.needs_redraw());
    }

    #[test]
    fn test_stroke_manager_draw_move() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.1,
            0.2,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.take_redraw();
        mgr.handle_draw_move("l1", 0.3, 0.4);
        assert_eq!(mgr.strokes[0].points.len(), 2);
        assert!(mgr.needs_redraw());
    }

    #[test]
    fn test_stroke_manager_draw_end() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.1,
            0.2,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.handle_draw_end("l1");
        assert!(mgr.strokes[0].finished);
    }

    #[test]
    fn test_stroke_manager_clear_all() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.1,
            0.2,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.handle_draw_start(
            "l2".into(),
            "u2".into(),
            0.3,
            0.4,
            "#00FF00".into(),
            0.00185,
            false,
        );
        mgr.handle_clear("u1", true);
        assert!(mgr.strokes.is_empty());
        assert!(mgr.needs_redraw());
    }

    #[test]
    fn test_stroke_manager_clear_by_sender() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.1,
            0.2,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.handle_draw_start(
            "l2".into(),
            "u2".into(),
            0.3,
            0.4,
            "#00FF00".into(),
            0.00185,
            false,
        );
        mgr.handle_clear("u1", false);
        assert_eq!(mgr.strokes.len(), 1);
        assert_eq!(mgr.strokes[0].sender_id, "u2");
    }

    #[test]
    fn test_stroke_manager_take_redraw() {
        let mut mgr = StrokeManager::new();
        assert!(!mgr.take_redraw());
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.1,
            0.2,
            "#FF0000".into(),
            0.003,
            false,
        );
        assert!(mgr.take_redraw());
        assert!(!mgr.take_redraw()); // reset after take
    }

    // --- Fade timing tests (Phase 16-01) ---
    use std::time::Duration;

    /// Helper: create a StrokeManager with one non-persistent stroke that
    /// finished `elapsed_secs` ago.
    fn mgr_with_finished_stroke(elapsed_secs: f64) -> StrokeManager {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.5,
            0.5,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.handle_draw_end("l1");
        mgr.take_redraw(); // clear the redraw flag from draw_end
        // Backdate finished_at to simulate time passage
        mgr.strokes[0].finished_at = Some(Instant::now() - Duration::from_secs_f64(elapsed_secs));
        mgr
    }

    #[test]
    fn test_hold_delay_keeps_full_opacity() {
        // 1.0s after finish -- well within 2s hold period
        let mut mgr = mgr_with_finished_stroke(1.0);
        let fading = mgr.tick_fades();
        assert!(fading, "tick_fades should return true during hold");
        assert!(
            (mgr.strokes[0].opacity - 0.8).abs() < f64::EPSILON,
            "opacity should remain 0.8 during hold, got {}",
            mgr.strokes[0].opacity
        );
        assert!(
            !mgr.take_redraw(),
            "needs_redraw should NOT be set during hold"
        );
    }

    #[test]
    fn test_hold_delay_boundary() {
        // 1.799s after finish -- still in hold period (hold = 1.8s)
        let mut mgr = mgr_with_finished_stroke(1.799);
        mgr.tick_fades();
        assert!(
            (mgr.strokes[0].opacity - 0.8).abs() < f64::EPSILON,
            "opacity should remain 0.8 at hold boundary, got {}",
            mgr.strokes[0].opacity
        );
    }

    #[test]
    fn test_fade_starts_after_hold() {
        // 1.9s after finish -- 0.1s into the 0.5s fade (hold = 1.8s)
        let mut mgr = mgr_with_finished_stroke(1.9);
        mgr.tick_fades();
        assert!(
            mgr.strokes[0].opacity < 0.8,
            "opacity should be less than 0.8 after hold ends, got {}",
            mgr.strokes[0].opacity
        );
        assert!(
            mgr.strokes[0].opacity > 0.0,
            "opacity should still be positive, got {}",
            mgr.strokes[0].opacity
        );
        assert!(
            mgr.take_redraw(),
            "needs_redraw should be set during active fade"
        );
    }

    #[test]
    fn test_fade_midpoint_ease_out() {
        // 2.05s after finish -- fade t = 0.5 (halfway through 0.5s fade, hold = 1.8s)
        // eased_t = 1 - (1 - 0.5)^3 = 1 - 0.125 = 0.875
        // opacity = 0.8 * (1 - 0.875) = 0.8 * 0.125 = 0.1
        let mut mgr = mgr_with_finished_stroke(2.05);
        mgr.tick_fades();
        assert!(
            (mgr.strokes[0].opacity - 0.1).abs() < 0.02,
            "opacity at fade midpoint should be ~0.1 (ease-out), got {}",
            mgr.strokes[0].opacity
        );
    }

    #[test]
    fn test_fade_complete() {
        // 2.4s after finish -- 0.1s past the full fade window (hold 1.8 + fade 0.5)
        let mut mgr = mgr_with_finished_stroke(2.4);
        mgr.tick_fades();
        // After tick_fades, the stroke should be removed by the retain filter (opacity <= 0.001)
        assert!(
            mgr.strokes.is_empty(),
            "fully faded stroke should be removed, but {} strokes remain",
            mgr.strokes.len()
        );
    }

    #[test]
    fn test_persistent_stroke_no_fade() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.5,
            0.5,
            "#FF0000".into(),
            0.003,
            true,
        );
        mgr.handle_draw_end("l1");
        mgr.take_redraw();
        // Persistent strokes: finished_at should be None (no fade scheduled)
        assert!(
            mgr.strokes[0].finished_at.is_none(),
            "persistent stroke should not have finished_at set"
        );
        // Even after a long time, tick_fades should not affect it
        let fading = mgr.tick_fades();
        assert!(
            !fading,
            "tick_fades should return false with only persistent strokes"
        );
        assert!(
            (mgr.strokes[0].opacity - 0.8).abs() < f64::EPSILON,
            "persistent stroke opacity should remain 0.8, got {}",
            mgr.strokes[0].opacity
        );
    }

    #[test]
    fn test_clear_during_hold() {
        let mut mgr = mgr_with_finished_stroke(1.0); // in hold period
        mgr.handle_clear("u1", true);
        assert!(
            mgr.strokes.is_empty(),
            "clear-all during hold should remove all strokes"
        );
    }

    #[test]
    fn test_no_redraw_during_hold() {
        let mut mgr = mgr_with_finished_stroke(1.0);
        mgr.tick_fades();
        assert!(
            !mgr.take_redraw(),
            "needs_redraw should NOT be set during hold period"
        );
    }

    #[test]
    fn test_redraw_during_fade() {
        let mut mgr = mgr_with_finished_stroke(2.05); // mid-fade (hold 1.8 + 0.25)
        mgr.tick_fades();
        assert!(
            mgr.take_redraw(),
            "needs_redraw should be set during active fade"
        );
    }

    // --- Point deduplication tests (Phase 17-01) ---

    #[test]
    fn test_dedup_skips_close_point() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.0,
            0.0,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.take_redraw();
        // Distance = sqrt(0.001^2 + 0.001^2) = 0.00141 < 0.002
        mgr.handle_draw_move("l1", 0.001, 0.001);
        assert_eq!(
            mgr.strokes[0].points.len(),
            1,
            "point closer than MIN_POINT_DISTANCE should be skipped"
        );
        assert!(
            !mgr.needs_redraw(),
            "needs_redraw should NOT be set when point is skipped"
        );
    }

    #[test]
    fn test_dedup_accepts_distant_point() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.0,
            0.0,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.take_redraw();
        // Distance = 0.003 > 0.002
        mgr.handle_draw_move("l1", 0.003, 0.0);
        assert_eq!(
            mgr.strokes[0].points.len(),
            2,
            "point farther than MIN_POINT_DISTANCE should be accepted"
        );
        assert!(
            mgr.needs_redraw(),
            "needs_redraw should be set when point is accepted"
        );
    }

    #[test]
    fn test_dedup_boundary_exact_threshold() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.0,
            0.0,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.take_redraw();
        // Distance = 0.002 exactly equals threshold.
        // Comparison is strict less-than: dx*dx+dy*dy < threshold*threshold
        // 0.002^2 = 0.000004, threshold^2 = 0.000004 -- equal, NOT less than,
        // so the early return does NOT fire and the point is ACCEPTED.
        mgr.handle_draw_move("l1", 0.002, 0.0);
        assert_eq!(
            mgr.strokes[0].points.len(),
            2,
            "point at exactly MIN_POINT_DISTANCE should be accepted (strict less-than comparison)"
        );
    }

    #[test]
    fn test_dedup_preserves_first_move() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.5,
            0.5,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.take_redraw();
        // First move after start -- even if close, it has a valid last point to compare against
        // (0.5, 0.5) -> (0.5, 0.503) = distance 0.003 > 0.002
        mgr.handle_draw_move("l1", 0.5, 0.503);
        assert_eq!(
            mgr.strokes[0].points.len(),
            2,
            "first move should be accepted when far enough"
        );
    }

    #[test]
    fn test_dedup_sequential_accumulation() {
        let mut mgr = StrokeManager::new();
        mgr.handle_draw_start(
            "l1".into(),
            "u1".into(),
            0.0,
            0.0,
            "#FF0000".into(),
            0.003,
            false,
        );
        mgr.take_redraw();
        // Three moves: first far enough, second too close to first, third far enough from first
        mgr.handle_draw_move("l1", 0.005, 0.0); // accepted (0.005 > 0.002)
        mgr.handle_draw_move("l1", 0.006, 0.0); // skipped (0.001 from last accepted < 0.002)
        mgr.handle_draw_move("l1", 0.008, 0.0); // accepted (0.003 from last accepted > 0.002)
        assert_eq!(
            mgr.strokes[0].points.len(),
            3,
            "should have start + 2 accepted moves = 3 points"
        );
    }
}
