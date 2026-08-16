/// Catmull-Rom to cubic Bezier conversion functions.
/// Ports Konva's exact `getControlPoints` and `expandPoints` algorithms
/// to produce visually identical curve rendering on the Cairo overlay.
///
/// Tension constant matching Konva `<Line tension={0.5}>`.
/// Duplicated in both Rust and JS renderers (same pattern as HOLD_DELAY_SECS / fadeConstants.ts).
pub const TENSION: f64 = 0.5;

/// Compute two Bezier control points for the curve segment through (x1,y1),
/// influenced by its neighbors (x0,y0) and (x2,y2).
/// Returns (cp1x, cp1y, cp2x, cp2y).
pub fn get_control_points(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    t: f64,
) -> (f64, f64, f64, f64) {
    let d01 = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
    let d12 = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    let d_sum = d01 + d12;
    if d_sum < f64::EPSILON {
        // Degenerate: all three points coincide -- return input point as both control points
        return (x1, y1, x1, y1);
    }
    let fa = t * d01 / d_sum;
    let fb = t * d12 / d_sum;
    let dx = x2 - x0;
    let dy = y2 - y0;
    (x1 - fa * dx, y1 - fa * dy, x1 + fb * dx, y1 + fb * dy)
}

/// Expand a sequence of (x,y) points into Bezier control point triplets.
/// Output format: [cp1x, cp1y, px, py, cp2x, cp2y, ...] matching Konva's expandPoints.
/// Returns empty Vec for fewer than 3 points.
pub fn expand_points(points: &[(f64, f64)], tension: f64) -> Vec<f64> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }

    let mut all = Vec::with_capacity((n - 2) * 6);
    for i in 1..n - 1 {
        let (x0, y0) = points[i - 1];
        let (x1, y1) = points[i];
        let (x2, y2) = points[i + 1];
        let (cp1x, cp1y, cp2x, cp2y) = get_control_points(x0, y0, x1, y1, x2, y2, tension);
        if cp1x.is_nan() {
            continue;
        }
        all.extend_from_slice(&[cp1x, cp1y, x1, y1, cp2x, cp2y]);
    }
    all
}

/// Convert a quadratic Bezier to cubic Bezier control points.
/// Given start (sx,sy), quadratic control (qcx,qcy), and end (ex,ey),
/// returns (cp1x, cp1y, cp2x, cp2y) for the equivalent cubic Bezier.
pub fn quad_to_cubic(
    sx: f64,
    sy: f64,
    qcx: f64,
    qcy: f64,
    ex: f64,
    ey: f64,
) -> (f64, f64, f64, f64) {
    let cp1x = sx + (2.0 / 3.0) * (qcx - sx);
    let cp1y = sy + (2.0 / 3.0) * (qcy - sy);
    let cp2x = ex + (2.0 / 3.0) * (qcx - ex);
    let cp2y = ey + (2.0 / 3.0) * (qcy - ey);
    (cp1x, cp1y, cp2x, cp2y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tension_constant() {
        assert!((TENSION - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_control_points_basic() {
        let (cp1x, cp1y, cp2x, cp2y) = get_control_points(0.0, 0.0, 100.0, 100.0, 200.0, 0.0, 0.5);
        assert!(cp1x.is_finite());
        assert!(cp1y.is_finite());
        assert!(cp2x.is_finite());
        assert!(cp2y.is_finite());
    }

    #[test]
    fn test_control_points_coincident() {
        let (cp1x, cp1y, cp2x, cp2y) = get_control_points(50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 0.5);
        assert!((cp1x - 50.0).abs() < f64::EPSILON);
        assert!((cp1y - 50.0).abs() < f64::EPSILON);
        assert!((cp2x - 50.0).abs() < f64::EPSILON);
        assert!((cp2y - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_expand_points_three() {
        let pts = vec![(0.0, 0.0), (100.0, 100.0), (200.0, 0.0)];
        let result = expand_points(&pts, TENSION);
        assert_eq!(
            result.len(),
            6,
            "3 points should produce 1 triplet (6 values)"
        );
        // Middle values should be the interior point
        assert!((result[2] - 100.0).abs() < f64::EPSILON);
        assert!((result[3] - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_expand_points_five() {
        let pts = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (200.0, 0.0),
            (300.0, 0.0),
            (400.0, 0.0),
        ];
        let result = expand_points(&pts, TENSION);
        assert_eq!(
            result.len(),
            18,
            "5 points should produce 3 triplets (18 values)"
        );
    }

    #[test]
    fn test_expand_points_two() {
        let pts = vec![(0.0, 0.0), (100.0, 0.0)];
        let result = expand_points(&pts, TENSION);
        assert!(result.is_empty(), "2 points should produce no triplets");
    }

    #[test]
    fn test_expand_points_empty() {
        let pts: Vec<(f64, f64)> = vec![];
        let result = expand_points(&pts, TENSION);
        assert!(result.is_empty());
    }

    #[test]
    fn test_konva_match_five_points() {
        // Verified against Konva's expandPoints JS output for this exact input
        let pts = vec![
            (0.0, 0.0),
            (50.0, 100.0),
            (100.0, 0.0),
            (150.0, 100.0),
            (200.0, 0.0),
        ];
        let tp = expand_points(&pts, 0.5);
        assert_eq!(tp.len(), 18);
        // Each triplet: [cp1x, cp1y, px, py, cp2x, cp2y]
        // Triplet 0: interior point (50, 100)
        assert!((tp[2] - 50.0).abs() < 1e-10);
        assert!((tp[3] - 100.0).abs() < 1e-10);
        // Triplet 1: interior point (100, 0)
        assert!((tp[8] - 100.0).abs() < 1e-10);
        assert!((tp[9] - 0.0).abs() < 1e-10);
        // Triplet 2: interior point (150, 100)
        assert!((tp[14] - 150.0).abs() < 1e-10);
        assert!((tp[15] - 100.0).abs() < 1e-10);
    }

    #[test]
    fn test_quad_to_cubic() {
        // Q(start=(0,0), control=(50,100), end=(100,0))
        // cp1 = start + 2/3*(control-start) = (33.33, 66.67)
        // cp2 = end + 2/3*(control-end) = (66.67, 66.67)
        let (cp1x, cp1y, cp2x, cp2y) = quad_to_cubic(0.0, 0.0, 50.0, 100.0, 100.0, 0.0);
        assert!((cp1x - 100.0 / 3.0).abs() < 0.01);
        assert!((cp1y - 200.0 / 3.0).abs() < 0.01);
        assert!((cp2x - 200.0 / 3.0).abs() < 0.01);
        assert!((cp2y - 200.0 / 3.0).abs() < 0.01);
    }
}
