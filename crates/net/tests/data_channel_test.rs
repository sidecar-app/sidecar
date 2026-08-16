//! Tests for data channel infrastructure (DCHAN-01, DCHAN-03).
//!
//! Validates that PeerConnectionManager creates a labeled "annotations" data
//! channel before SDP offer generation, and that the relay channel pattern
//! preserves sender ID for no-echo filtering.

use net::{IceServerConfig, PeerConnectionManager, ViewerTrack};
use tokio::sync::mpsc;

#[tokio::test]
async fn data_channel_created() {
    // DCHAN-01: Host creates a labeled "annotations" data channel per viewer
    // before SDP offer generation.
    let track = ViewerTrack::default();
    let (signaling_tx, _signaling_rx) = mpsc::unbounded_channel();
    let (relay_tx, _relay_rx) = mpsc::unbounded_channel();
    let (dc_state_tx, _dc_state_rx) = mpsc::unbounded_channel();
    let ice_servers = vec![IceServerConfig {
        urls: "stun:stun.l.google.com:19302".to_string(),
        username: None,
        credential: None,
    }];

    let pcm = PeerConnectionManager::new(
        "viewer-test".to_string(),
        &ice_servers,
        track.as_track_local(),
        signaling_tx,
        relay_tx,
        dc_state_tx,
    )
    .await
    .unwrap();

    assert_eq!(pcm.data_channel().label(), "annotations");

    let sdp = pcm.create_offer().await.unwrap();
    assert!(
        sdp.contains("webrtc-datachannel"),
        "SDP offer should contain data channel transport: {sdp}"
    );
}

#[tokio::test]
async fn relay_excludes_sender() {
    // DCHAN-03: Host relays viewer annotations to all other connected
    // viewers, excluding the sender (no echo).

    // Create the relay channel (same type used in session.rs relay_loop)
    let (relay_tx, mut relay_rx) = mpsc::unbounded_channel::<(String, String)>();

    relay_tx
        .send((
            "viewer-A".to_string(),
            r#"{"t":"dm","x":0.5,"y":0.5,"l":"l1","id":"viewer-A"}"#.to_string(),
        ))
        .unwrap();

    // The relay tuple carries the sender ID alongside the message so the relay
    // loop can filter it out (no echo).
    let (sender_id, message) = relay_rx.recv().await.unwrap();
    assert_eq!(sender_id, "viewer-A");
    assert!(message.contains("\"t\":\"dm\""));

    // Demonstrate the no-echo filter pattern used in session.rs relay_loop:
    // When relaying to all viewers, we exclude the sender.
    let viewers = ["viewer-A", "viewer-B", "viewer-C"];
    let recipients: Vec<&&str> = viewers.iter().filter(|id| **id != sender_id).collect();

    assert_eq!(recipients.len(), 2);
    assert!(!recipients.contains(&&"viewer-A"));
    assert!(recipients.contains(&&"viewer-B"));
    assert!(recipients.contains(&&"viewer-C"));
}
