//! End-to-end latency and ABR integration tests for the encode pipeline.

use encode::abr::{AbrConfig, RuleBasedAbrController};
use encode::{
    AdaptiveBitrateController, EncoderBackend, EncoderConfig, NetworkStats, VpxEncoder,
    spawn_encode_thread,
};

/// Create a synthetic BGRA frame with a solid color.
fn make_synthetic_frame(width: u32, height: u32, color: [u8; 4]) -> capture::Frame {
    let stride = width * 4;
    let data = color.repeat((width * height) as usize);
    capture::Frame::new(
        data,
        width,
        height,
        stride,
        // Use a monotonic-ish timestamp based on current time
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64,
        0,   // display_id
        1.0, // scale_factor
    )
}

#[tokio::test]
#[ignore] // requires bare-metal perf; too flaky on CI runners
async fn test_encode_thread_pipeline_latency() {
    let config = EncoderConfig {
        width: 1920,
        height: 1080,
        ..Default::default()
    };

    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<capture::Frame>(32);

    let (mut packet_rx, stop_tx) =
        spawn_encode_thread(config, frame_rx, None, None, None, None, None)
            .expect("spawn encode thread");

    let num_frames = 30u64;
    let mut latencies_ns: Vec<u64> = Vec::with_capacity(num_frames as usize);

    for i in 0..num_frames {
        let color = [0u8, 0, (i * 8) as u8, 255];
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let frame = capture::Frame::new(
            color.repeat((1920 * 1080) as usize),
            1920,
            1080,
            1920 * 4,
            now_ns,
            0,
            1.0,
        );

        frame_tx.send(frame).expect("send frame");

        let packet = packet_rx.recv().await.expect("receive packet");

        let recv_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let latency = recv_ns.saturating_sub(packet.capture_timestamp_ns);
        latencies_ns.push(latency);
    }

    // Drop frame sender to signal end of stream
    drop(frame_tx);

    let _ = stop_tx.send(());

    latencies_ns.sort();
    let p50 = latencies_ns[(num_frames as f64 * 0.50) as usize];
    let p95 = latencies_ns[(num_frames as f64 * 0.95) as usize - 1];
    let p99 = latencies_ns[num_frames as usize - 1];

    println!(
        "\nPipeline latency (capture->encode->output), 1080p, {} frames:",
        num_frames
    );
    println!("  P50: {:.2} ms", p50 as f64 / 1_000_000.0);
    println!("  P95: {:.2} ms", p95 as f64 / 1_000_000.0);
    println!("  P99: {:.2} ms", p99 as f64 / 1_000_000.0);

    // In debug mode, encoding is much slower due to unoptimized code.
    // Only enforce the 20ms budget in release builds.
    if cfg!(not(debug_assertions)) {
        assert!(
            p95 < 20_000_000,
            "P95 pipeline latency ({:.2}ms) should be under 20ms",
            p95 as f64 / 1_000_000.0
        );
    } else {
        println!(
            "  (debug mode: skipping 20ms assertion, P95={:.2}ms)",
            p95 as f64 / 1_000_000.0
        );
    }
}

#[test]
fn test_abr_integration_with_encoder() {
    let config = EncoderConfig {
        width: 1920,
        height: 1080,
        ..Default::default()
    };

    let mut encoder = VpxEncoder::new(&config).expect("encoder init");
    let mut abr = RuleBasedAbrController::new(AbrConfig::default());

    // Encode 5 frames at default settings
    for i in 0..5u64 {
        let frame = make_synthetic_frame(1920, 1080, [0, 0, (i * 50) as u8, 255]);
        let packet = encoder
            .encode_frame(&frame)
            .expect("encode at default settings");
        assert!(!packet.data.is_empty(), "frame {} should produce data", i);
    }

    // Simulate congestion
    let congested = NetworkStats {
        rtt_ms: 150.0,
        packet_loss_pct: 3.0,

        throughput_kbps: 0,
        has_relay: false,
        has_measured_rtt: false,
    };
    let new_params = abr.on_network_stats(&congested);
    encoder
        .update_params(&new_params)
        .expect("update params with congested settings");

    println!(
        "After congestion: bitrate={}kbps, max_q={}, fps={}",
        new_params.target_bitrate_kbps, new_params.max_quantizer, new_params.target_fps
    );

    // Encode 5 more frames under congestion
    for i in 0..5u64 {
        let frame = make_synthetic_frame(1920, 1080, [0, (i * 50) as u8, 0, 255]);
        let packet = encoder
            .encode_frame(&frame)
            .expect("encode under congestion");
        assert!(
            !packet.data.is_empty(),
            "congested frame {} should produce data",
            i
        );
    }

    // Simulate recovery
    let recovered = NetworkStats {
        rtt_ms: 20.0,
        packet_loss_pct: 0.0,

        throughput_kbps: 0,
        has_relay: false,
        has_measured_rtt: false,
    };
    let recovered_params = abr.on_network_stats(&recovered);
    encoder
        .update_params(&recovered_params)
        .expect("update params with recovered settings");

    println!(
        "After recovery: bitrate={}kbps, max_q={}, fps={}",
        recovered_params.target_bitrate_kbps,
        recovered_params.max_quantizer,
        recovered_params.target_fps
    );

    // Encode 5 more frames after recovery
    for i in 0..5u64 {
        let frame = make_synthetic_frame(1920, 1080, [(i * 50) as u8, 0, 0, 255]);
        let packet = encoder.encode_frame(&frame).expect("encode after recovery");
        assert!(
            !packet.data.is_empty(),
            "recovered frame {} should produce data",
            i
        );
    }

    println!("ABR integration test: 15 frames across 3 conditions - all OK");
}
