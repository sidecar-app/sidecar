//! Host-side rendering of controlling viewers' pointers during remote control.
//!
//! Cursor positions arrive as the same `{t:"c",x,y,id,c}` packets used for the
//! client-side cursor dots, so the host overlay shows a color-coded pointer per
//! controlling viewer. Stale cursors expire after [`CURSOR_TIMEOUT_SECS`],
//! matching the viewer-side timeout.

use std::collections::HashMap;
use std::time::Instant;

/// Remove a cursor that hasn't moved within this window (matches the viewer's
/// 3s `CURSOR_TIMEOUT`).
const CURSOR_TIMEOUT_SECS: f64 = 3.0;

/// Fallback color when a cursor packet omits one.
const DEFAULT_CURSOR_COLOR: &str = "#FF3B30";

/// Which mode a viewer is in, selecting the glyph drawn for their pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    /// Annotating: drawn as a pencil.
    Draw,
    /// Remote-controlling: drawn as an arrow pointer.
    Control,
}

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    /// Normalized 0-1 position.
    pub x: f64,
    pub y: f64,
    pub color: String,
    pub mode: CursorMode,
    pub last_seen: Instant,
}

/// Tracks the latest pointer position per controlling viewer.
#[derive(Debug, Default)]
pub struct CursorManager {
    pub cursors: HashMap<String, RemoteCursor>,
    needs_redraw: bool,
}

impl CursorManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a viewer's pointer position (normalized 0-1) and mode.
    pub fn update(&mut self, id: String, x: f64, y: f64, color: Option<String>, mode: CursorMode) {
        match self.cursors.get_mut(&id) {
            Some(c) => {
                c.x = x;
                c.y = y;
                if let Some(color) = color {
                    c.color = color;
                }
                c.mode = mode;
                c.last_seen = Instant::now();
            }
            None => {
                self.cursors.insert(
                    id,
                    RemoteCursor {
                        x,
                        y,
                        color: color.unwrap_or_else(|| DEFAULT_CURSOR_COLOR.to_string()),
                        mode,
                        last_seen: Instant::now(),
                    },
                );
            }
        }
        self.needs_redraw = true;
    }

    /// Drop cursors past the inactivity timeout. Sets `needs_redraw` if any were
    /// removed so the overlay repaints without them.
    pub fn tick_expiry(&mut self) {
        let now = Instant::now();
        let before = self.cursors.len();
        self.cursors
            .retain(|_, c| now.duration_since(c.last_seen).as_secs_f64() < CURSOR_TIMEOUT_SECS);
        if self.cursors.len() != before {
            self.needs_redraw = true;
        }
    }

    /// Drop control-mode (pointer) cursors, keeping draw-mode (pencil) ones.
    /// Used when remote control ends: draw cursors are shown regardless of RC,
    /// so only the controllers' pointers should disappear.
    pub fn clear_controllers(&mut self) {
        let before = self.cursors.len();
        self.cursors.retain(|_, c| c.mode == CursorMode::Draw);
        if self.cursors.len() != before {
            self.needs_redraw = true;
        }
    }

    pub fn take_redraw(&mut self) -> bool {
        let v = self.needs_redraw;
        self.needs_redraw = false;
        v
    }

    pub fn needs_redraw(&self) -> bool {
        self.needs_redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn update_inserts_and_moves() {
        let mut m = CursorManager::new();
        m.update(
            "a".into(),
            0.1,
            0.2,
            Some("#FF0000".into()),
            CursorMode::Draw,
        );
        assert_eq!(m.cursors.len(), 1);
        assert!(m.needs_redraw());
        m.update("a".into(), 0.5, 0.6, None, CursorMode::Draw);
        let c = &m.cursors["a"];
        assert_eq!((c.x, c.y), (0.5, 0.6));
        // Color is retained when the packet omits it.
        assert_eq!(c.color, "#FF0000");
    }

    #[test]
    fn expiry_removes_stale_cursor() {
        let mut m = CursorManager::new();
        m.update("a".into(), 0.1, 0.2, None, CursorMode::Control);
        m.take_redraw();
        m.cursors.get_mut("a").unwrap().last_seen =
            Instant::now() - Duration::from_secs_f64(CURSOR_TIMEOUT_SECS + 0.5);
        m.tick_expiry();
        assert!(m.cursors.is_empty());
        assert!(m.needs_redraw());
    }

    #[test]
    fn clear_controllers_keeps_draw_cursors() {
        let mut m = CursorManager::new();
        m.update("drawer".into(), 0.1, 0.2, None, CursorMode::Draw);
        m.update("controller".into(), 0.3, 0.4, None, CursorMode::Control);
        m.clear_controllers();
        assert!(m.cursors.contains_key("drawer"));
        assert!(!m.cursors.contains_key("controller"));
    }
}
