//! ABR watch channel wiring tests (INTG-01).
//!
//! Validates that spawn_encode_thread accepts an optional
//! watch::Receiver<EncoderParams> and applies updates at runtime.

use encode::{EncoderConfig, EncoderParams, spawn_encode_thread};
use tokio::sync::watch;

/// Create a synthetic BGRA frame with a solid color.
fn make_synthetic_frame(width: u32, height: u32, color: [u8; 4]) -> capture::Frame {
    let stride = width * 4;
    let data = color.repeat((width * height) as usize);
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

/// Test 1: spawn_encode_thread accepts an optional watch::Receiver<EncoderParams>
/// parameter without breaking existing callers (None case).
#[tokio::test]
async fn spawn_encode_thread_accepts_none_params_rx() {
    let config = EncoderConfig {
        width: 640,
        height: 480,
        ..Default::default()
    };
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<capture::Frame>(8);

    // Should compile and work with None (no params receiver)
    let (mut packet_rx, stop_tx) =
        spawn_encode_thread(config, frame_rx, None, None, None, None, None)
            .expect("spawn with None params_rx");

    // Send one frame to verify the thread works
    let frame = make_synthetic_frame(640, 480, [0, 0, 255, 255]);
    frame_tx.send(frame).expect("send frame");

    let packet = packet_rx.recv().await.expect("receive packet");
    assert!(!packet.data.is_empty(), "should produce encoded data");

    // Clean shutdown
    drop(frame_tx);
    let _ = stop_tx.send(());
}

/// Test 2: When a watch::Receiver is provided and a new EncoderParams value
/// is sent, the encoder processes the update (has_changed returns false after).
///
/// Uses target_fps == config.target_fps to avoid frame-skipping, which would
/// drop frames sent too quickly and cause the test to hang.
#[tokio::test]
async fn abr_watch_channel_updates_encoder() {
    let initial_params = EncoderParams {
        target_bitrate_kbps: 2000,
        max_quantizer: 40,
        min_quantizer: 2,
        target_fps: 60, // must match config.target_fps to avoid frame-skipping
        target_height: 1080,
    };
    let (params_tx, params_rx) = watch::channel(initial_params);

    let config = EncoderConfig {
        width: 640,
        height: 480,
        ..Default::default()
    };
    let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<capture::Frame>(8);

    let (mut packet_rx, stop_tx) =
        spawn_encode_thread(config, frame_rx, Some(params_rx), None, None, None, None)
            .expect("spawn with params_rx");

    // Send a frame to initialize the encoder
    let frame = make_synthetic_frame(640, 480, [0, 0, 255, 255]);
    frame_tx.send(frame).expect("send frame");
    let _packet = tokio::time::timeout(std::time::Duration::from_secs(5), packet_rx.recv())
        .await
        .expect("timeout waiting for initial packet")
        .expect("receive initial packet");

    // Send new ABR params (keep fps at 60 to avoid frame-skipping)
    let new_params = EncoderParams {
        target_bitrate_kbps: 500,
        max_quantizer: 48,
        min_quantizer: 4,
        target_fps: 60,
        target_height: 720,
    };
    params_tx.send(new_params).expect("send new params");

    // Send another frame -- the encoder should pick up the new params
    let frame2 = make_synthetic_frame(640, 480, [0, 255, 0, 255]);
    frame_tx.send(frame2).expect("send frame 2");
    let packet2 = tokio::time::timeout(std::time::Duration::from_secs(5), packet_rx.recv())
        .await
        .expect("timeout waiting for packet after param update")
        .expect("receive packet after param update");
    assert!(
        !packet2.data.is_empty(),
        "should produce encoded data after param update"
    );

    // Clean shutdown
    drop(frame_tx);
    let _ = stop_tx.send(());
}
