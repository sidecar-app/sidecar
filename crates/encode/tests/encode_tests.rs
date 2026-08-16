//! Integration tests for VP9 encoding at multiple resolutions.

use encode::{EncoderBackend, EncoderConfig, EncoderParams, VpxEncoder};

/// Create a synthetic BGRA frame with a solid color.
fn make_synthetic_frame(width: u32, height: u32, color: [u8; 4]) -> capture::Frame {
    let stride = width * 4;
    let data_len = (stride * height) as usize;
    let mut data = Vec::with_capacity(data_len);
    for _ in 0..(width * height) {
        data.extend_from_slice(&color);
    }
    capture::Frame::new(
        data,
        width,
        height,
        stride,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64,
        0,   // display_id
        1.0, // scale_factor
    )
}

fn default_config(width: u32, height: u32) -> EncoderConfig {
    EncoderConfig {
        width,
        height,
        ..Default::default()
    }
}

#[test]
fn test_encode_single_frame() {
    let config = default_config(1920, 1080);
    let mut encoder = VpxEncoder::new(&config).expect("encoder init");

    let frame = make_synthetic_frame(1920, 1080, [255, 0, 0, 255]); // Blue in BGRA
    let packet = encoder.encode_frame(&frame).expect("encode");

    assert!(!packet.data.is_empty(), "encoded data should not be empty");
    assert!(packet.is_keyframe, "first frame should be a keyframe");
    assert_eq!(packet.width, 1920);
    assert_eq!(packet.height, 1080);
    println!(
        "Single frame: {} bytes, keyframe={}, encode_time={}us",
        packet.data.len(),
        packet.is_keyframe,
        packet.encode_duration_ns / 1000
    );
}

#[test]
fn test_encode_multi_resolution() {
    let resolutions = [(1280, 720), (1920, 1080), (2560, 1440)];

    for (width, height) in &resolutions {
        let config = default_config(*width, *height);
        let mut encoder = VpxEncoder::new(&config).expect("encoder init");

        for i in 0..3 {
            let frame = make_synthetic_frame(*width, *height, [0, (i * 80) as u8, 0, 255]);
            let packet = encoder.encode_frame(&frame).expect("encode");
            assert!(
                !packet.data.is_empty(),
                "frame {} at {}x{} should produce data",
                i,
                width,
                height
            );
        }
        println!("Resolution {}x{}: 3 frames encoded OK", width, height);
    }
}

#[test]
fn test_encode_odd_dimensions() {
    // Odd dimensions that should be rounded to even internally
    let config = default_config(1367, 769);
    let mut encoder = VpxEncoder::new(&config).expect("encoder init");

    let frame = make_synthetic_frame(1367, 769, [128, 128, 128, 255]);
    let packet = encoder.encode_frame(&frame).expect("encode");

    assert!(!packet.data.is_empty(), "odd-dimension frame should encode");
    assert_eq!(packet.width, 1368);
    assert_eq!(packet.height, 770);
    println!(
        "Odd dimensions 1367x769 -> {}x{}: {} bytes",
        packet.width,
        packet.height,
        packet.data.len()
    );
}

#[test]
fn test_encode_sequence() {
    let config = default_config(1920, 1080);
    let mut encoder = VpxEncoder::new(&config).expect("encoder init");

    let mut had_keyframe = false;
    let mut prev_pts = 0u64;
    for i in 0..10u64 {
        let frame = make_synthetic_frame(1920, 1080, [0, 0, (i * 25) as u8, 255]);
        let packet = encoder.encode_frame(&frame).expect("encode");
        // PTS is now milliseconds since encoder creation (g_timebase = 1/1000).
        // First frame should be near 0, subsequent frames should be monotonically
        // increasing. We can't check exact values since they depend on wall time.
        if i > 0 {
            assert!(
                packet.pts >= prev_pts,
                "pts should be monotonically increasing: frame {} pts {} < prev {}",
                i,
                packet.pts,
                prev_pts
            );
        }
        prev_pts = packet.pts;
        if packet.is_keyframe {
            had_keyframe = true;
        }
    }
    assert!(had_keyframe, "at least first frame should be a keyframe");
    println!("Sequence: 10 frames encoded with monotonically increasing pts");
}

#[test]
fn test_update_params() {
    let config = default_config(1920, 1080);
    let mut encoder = VpxEncoder::new(&config).expect("encoder init");

    // Encode a frame with default params
    let frame = make_synthetic_frame(1920, 1080, [0, 0, 255, 255]);
    let _packet = encoder.encode_frame(&frame).expect("encode");

    // Update to lower bitrate (keep quantizer within original range)
    let params = EncoderParams {
        target_bitrate_kbps: 500,
        max_quantizer: 40,
        min_quantizer: 2,
        target_fps: 30,
        target_height: 1080,
    };
    encoder.update_params(&params).expect("update params");

    // Encode another frame with updated params
    let frame2 = make_synthetic_frame(1920, 1080, [0, 255, 0, 255]);
    let packet2 = encoder.encode_frame(&frame2).expect("encode after update");
    assert!(
        !packet2.data.is_empty(),
        "encoding after param update should work"
    );
    println!("Param update: bitrate changed to 500kbps, encoding OK");
}

#[test]
#[ignore] // requires bare-metal perf; too flaky on CI runners
fn test_encode_latency_budget() {
    let config = default_config(1920, 1080);
    let mut encoder = VpxEncoder::new(&config).expect("encoder init");

    let mut durations: Vec<u64> = Vec::with_capacity(30);
    for i in 0..30u64 {
        let frame = make_synthetic_frame(1920, 1080, [0, (i * 8) as u8, (i * 4) as u8, 255]);
        let packet = encoder.encode_frame(&frame).expect("encode");
        durations.push(packet.encode_duration_ns);
    }

    durations.sort();
    let p50 = durations[14];
    let p95 = durations[28];
    let p99 = durations[29];

    println!(
        "Encode latency (1080p, 30 frames):\n  P50: {:.2}ms\n  P95: {:.2}ms\n  P99: {:.2}ms",
        p50 as f64 / 1_000_000.0,
        p95 as f64 / 1_000_000.0,
        p99 as f64 / 1_000_000.0
    );

    // In debug mode, encoding is 5-10x slower due to unoptimized colorspace
    // conversion and libvpx wrapper overhead. Only enforce the 20ms budget
    // in release builds where SIMD optimizations are active.
    if cfg!(not(debug_assertions)) {
        assert!(
            p95 < 20_000_000,
            "P95 encode time ({:.2}ms) should be under 20ms",
            p95 as f64 / 1_000_000.0
        );
    } else {
        println!(
            "(debug mode: skipping 20ms assertion, P95={:.2}ms)",
            p95 as f64 / 1_000_000.0
        );
    }
}
