use encode::abr::{AbrConfig, RuleBasedAbrController};
use encode::colorspace::{I420Buffer, bgra_to_i420};
use encode::{AdaptiveBitrateController, NetworkStats};

#[test]
fn test_bgra_to_i420_solid_red() {
    // 4x2 solid red BGRA image (B=0, G=0, R=255, A=255)
    let width = 4u32;
    let height = 2u32;
    let stride = width * 4;
    let pixel: [u8; 4] = [0, 0, 255, 255]; // BGRA red
    let bgra: Vec<u8> = pixel.repeat((width * height) as usize);

    let mut buf = I420Buffer::new(width, height);
    bgra_to_i420(&bgra, width, height, stride, &mut buf).unwrap();

    // BT.601 luma for pure red (R=255, G=0, B=0):
    // Y ~= 81 (with 16 offset)
    // Only check the actual pixel area, not stride padding
    for row in 0..height {
        for col in 0..width {
            let y_val = buf.y[(row * buf.y_stride + col) as usize];
            assert!(
                (y_val as i32 - 81).unsigned_abs() < 5,
                "Y plane value {} not close to expected ~81 for red",
                y_val
            );
        }
    }
}

#[test]
fn test_bgra_to_i420_1920x1080() {
    // Performance sanity: converting a full HD buffer should not error
    let width = 1920u32;
    let height = 1080u32;
    let stride = width * 4;
    let bgra = vec![128u8; (stride * height) as usize];

    let mut buf = I420Buffer::new(width, height);
    bgra_to_i420(&bgra, width, height, stride, &mut buf).unwrap();

    assert_eq!(buf.width, 1920);
    assert_eq!(buf.height, 1080);
    // Planes use aligned strides (>= width)
    assert!(buf.y_stride >= 1920);
    assert!(buf.uv_stride >= 960);
    assert_eq!(buf.y.len(), (buf.y_stride * 1080) as usize);
    assert_eq!(buf.u.len(), (buf.uv_stride * 540) as usize);
    assert_eq!(buf.v.len(), (buf.uv_stride * 540) as usize);
}

#[test]
fn test_bgra_to_i420_odd_dimensions() {
    // Odd dimensions (1367x769) should round up to even (1368x770)
    let width = 1367u32;
    let height = 769u32;
    let stride = width * 4;
    let bgra = vec![100u8; (stride * height) as usize];

    let mut buf = I420Buffer::new(width, height);
    assert_eq!(buf.width, 1368, "Width should be rounded up to even");
    assert_eq!(buf.height, 770, "Height should be rounded up to even");

    bgra_to_i420(&bgra, width, height, stride, &mut buf).unwrap();

    // Planes use aligned strides
    assert!(buf.y_stride >= 1368);
    assert_eq!(buf.y.len(), (buf.y_stride * 770) as usize);
    assert_eq!(buf.u.len(), (buf.uv_stride * 385) as usize);
    assert_eq!(buf.v.len(), (buf.uv_stride * 385) as usize);
}

#[test]
fn test_bgra_to_i420_with_stride_padding() {
    // 4x2 image with stride = 20 (4*4=16 pixels + 4 bytes padding per row)
    let width = 4u32;
    let height = 2u32;
    let stride = 20u32; // 4 bytes padding per row
    let pixel: [u8; 4] = [255, 255, 255, 255]; // BGRA white

    let mut bgra = Vec::new();
    for _row in 0..height {
        for _col in 0..width {
            bgra.extend_from_slice(&pixel);
        }
        // 4 bytes padding
        bgra.extend_from_slice(&[0, 0, 0, 0]);
    }
    assert_eq!(bgra.len(), (stride * height) as usize);

    let mut buf = I420Buffer::new(width, height);
    bgra_to_i420(&bgra, width, height, stride, &mut buf).unwrap();

    // BT.601 luma for white (R=255, G=255, B=255): Y ~= 235
    // Only check actual pixel area
    for row in 0..height {
        for col in 0..width {
            let y_val = buf.y[(row * buf.y_stride + col) as usize];
            assert!(
                (y_val as i32 - 235).unsigned_abs() < 5,
                "Y plane value {} not close to expected ~235 for white",
                y_val
            );
        }
    }
}

#[test]
fn test_i420buffer_new_plane_sizes() {
    let buf = I420Buffer::new(640, 480);
    assert_eq!(buf.width, 640);
    assert_eq!(buf.height, 480);
    // Strides are 32-byte aligned
    assert!(buf.y_stride >= 640);
    assert_eq!(buf.y_stride % 32, 0);
    assert!(buf.uv_stride >= 320);
    assert_eq!(buf.uv_stride % 32, 0);
    assert_eq!(buf.y.len(), (buf.y_stride * 480) as usize);
    assert_eq!(buf.u.len(), (buf.uv_stride * 240) as usize);
    assert_eq!(buf.v.len(), (buf.uv_stride * 240) as usize);

    // Odd dimensions should round up
    let buf_odd = I420Buffer::new(641, 481);
    assert_eq!(buf_odd.width, 642);
    assert_eq!(buf_odd.height, 482);
    assert!(buf_odd.y_stride >= 642);
    assert_eq!(buf_odd.y_stride % 32, 0);
}

// ======== ABR Controller Tests (step-based degradation) ========
//
// The RuleBasedAbrController degrades one step per on_network_stats call
// (not jumping to a tier). Without set_capture_dimensions or viewer
// viewport, default height is 1080p. Bitrate is computed dynamically
// from resolution, FPS, and quality level.
//
// Default config: base_rate=4000, fps_steps=[60,30,15], threshold=2%
//
// Degradation steps (level → fps, quality):
//   0: 60fps High    → bitrate 8000
//   1: 30fps High    → bitrate 4000
//   2: 30fps Medium  → bitrate 2800
//   3: 15fps Medium  → bitrate 1400
//   4: 15fps Low     → bitrate 900
//   5: 15fps Low + resolution stepdown → height 720, bitrate 500

fn stats_loss(loss_pct: f64) -> NetworkStats {
    NetworkStats {
        rtt_ms: 20.0,
        packet_loss_pct: loss_pct,
        throughput_kbps: 0,
        has_relay: false,
        has_measured_rtt: true,
    }
}

#[test]
fn test_abr_no_loss() {
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());
    let p = abr.on_network_stats(&stats_loss(0.0));
    // Level 0: 60fps, High quality, 1080p
    assert_eq!(p.target_bitrate_kbps, 8000);
    assert_eq!(p.target_fps, 60);
    assert_eq!(p.max_quantizer, 33);
    assert_eq!(p.target_height, 1080);
}

#[test]
fn test_abr_light_loss() {
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());
    // 3% > 2% threshold → degrade one step to level 1
    let p = abr.on_network_stats(&stats_loss(3.0));
    assert_eq!(p.target_bitrate_kbps, 4000);
    assert_eq!(p.target_fps, 30);
    assert_eq!(p.max_quantizer, 33);
    assert_eq!(p.target_height, 1080);
}

#[test]
fn test_abr_moderate_loss() {
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());
    // Three consecutive loss events → level 3
    abr.on_network_stats(&stats_loss(10.0)); // level 1
    abr.on_network_stats(&stats_loss(10.0)); // level 2
    let p = abr.on_network_stats(&stats_loss(10.0)); // level 3
    assert_eq!(p.target_bitrate_kbps, 1400);
    assert_eq!(p.target_fps, 15);
    assert_eq!(p.max_quantizer, 40);
    assert_eq!(p.target_height, 1080);
}

#[test]
fn test_abr_severe_loss() {
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());
    // Five consecutive loss events → level 5 (resolution stepdown)
    for _ in 0..5 {
        abr.on_network_stats(&stats_loss(20.0));
    }
    let p = abr.on_network_stats(&stats_loss(0.0)); // stays at level 5 (1 good, need 3)
    assert_eq!(p.target_bitrate_kbps, 500);
    assert_eq!(p.target_fps, 15);
    assert_eq!(p.max_quantizer, 48);
    assert_eq!(p.target_height, 720);
}

#[test]
fn test_abr_recovery_requires_hysteresis() {
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());

    // Degrade to level 2 (30fps, Medium)
    abr.on_network_stats(&stats_loss(5.0)); // level 1
    abr.on_network_stats(&stats_loss(5.0)); // level 2
    assert_eq!(abr.current_params().max_quantizer, 40);

    // Recovery needs 3 consecutive good samples
    abr.on_network_stats(&stats_loss(0.0));
    assert_eq!(abr.current_params().max_quantizer, 40, "1st good: hold");
    abr.on_network_stats(&stats_loss(0.0));
    assert_eq!(abr.current_params().max_quantizer, 40, "2nd good: hold");
    let p = abr.on_network_stats(&stats_loss(0.0));
    // Recovered one step: level 2 → 1 (30fps, High)
    assert_eq!(p.max_quantizer, 33, "3rd good: should recover quality");
    assert_eq!(p.target_fps, 30);
}

#[test]
fn test_abr_tier_degradation_progression() {
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());

    // Level 0: 60fps, High quality
    let p = abr.on_network_stats(&stats_loss(0.0));
    assert_eq!(p.target_fps, 60);
    assert_eq!(p.max_quantizer, 33);
    assert_eq!(p.target_height, 1080);

    // Level 1: FPS drops to 30
    let p = abr.on_network_stats(&stats_loss(3.0));
    assert_eq!(p.target_fps, 30);
    assert_eq!(p.max_quantizer, 33);
    assert_eq!(p.target_height, 1080);

    // Level 2: quality drops to Medium
    let p = abr.on_network_stats(&stats_loss(5.0));
    assert_eq!(p.target_fps, 30);
    assert_eq!(p.max_quantizer, 40);
    assert_eq!(p.target_height, 1080);

    // Level 3: FPS drops to 15
    let p = abr.on_network_stats(&stats_loss(5.0));
    assert_eq!(p.target_fps, 15);
    assert_eq!(p.max_quantizer, 40);
    assert_eq!(p.target_height, 1080);

    // Level 4: quality drops to Low
    let p = abr.on_network_stats(&stats_loss(5.0));
    assert_eq!(p.target_fps, 15);
    assert_eq!(p.max_quantizer, 48);
    assert_eq!(p.target_height, 1080);

    // Level 5: resolution stepdown (last resort)
    let p = abr.on_network_stats(&stats_loss(5.0));
    assert_eq!(p.target_fps, 15);
    assert_eq!(p.max_quantizer, 48);
    assert_eq!(p.target_height, 720);
}
