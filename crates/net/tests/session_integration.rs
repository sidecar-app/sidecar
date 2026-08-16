//! Integration tests for session lifecycle and signaling protocol.
//!
//! These tests require a running signaling server. Set the `SIGNALING_URL`
//! environment variable to the server's WebSocket URL before running:
//!
//!     SIGNALING_URL=ws://localhost:8080 cargo test -p net -- --include-ignored

use std::time::Duration;

use net::SignalingClient;
use net::types::SignalingMessage;

/// Returns the signaling server URL from the `SIGNALING_URL` env var,
/// panicking with a helpful message if it is not set.
fn signaling_url() -> String {
    std::env::var("SIGNALING_URL").expect(
        "SIGNALING_URL not set — point it at a running signaling server \
         (e.g. SIGNALING_URL=ws://localhost:8080)",
    )
}

/// Host logs in and receives a word-based room code with ICE servers.
#[ignore] // requires a running signaling server (SIGNALING_URL); run with --include-ignored
#[tokio::test]
async fn test_host_login_returns_word_room_code() {
    let url = signaling_url();

    let mut client = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect");

    client
        .send(SignalingMessage::Login {
            role: "host".to_string(),
        })
        .await
        .expect("Failed to send login");

    let msg = tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("Timeout waiting for LoginSuccess")
        .expect("Recv error");

    match msg {
        Some(SignalingMessage::LoginSuccess {
            room_id,
            ice_servers,
        }) => {
            assert!(!room_id.is_empty(), "Room ID should not be empty");

            // Word-based room codes contain hyphens (e.g., "brave-parrot-42").
            assert!(
                room_id.contains('-'),
                "Room code should be word-based (contain hyphens): got '{}'",
                room_id
            );

            assert!(!ice_servers.is_empty(), "ICE servers should not be empty");

            let has_stun = ice_servers.iter().any(|s| s.urls.contains("stun:"));
            assert!(has_stun, "Should have at least one STUN server");
        }
        other => panic!("Expected LoginSuccess, got: {:?}", other),
    }

    client.close().await.ok();
}

/// 5th viewer is rejected with ROOM_FULL error.
#[ignore] // requires a running signaling server (SIGNALING_URL); run with --include-ignored
#[tokio::test]
async fn test_viewer_limit_enforcement() {
    let url = signaling_url();

    let mut host = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect host");

    host.send(SignalingMessage::Login {
        role: "host".to_string(),
    })
    .await
    .expect("Failed to send host login");

    let room_code = match tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("Timeout")
        .expect("Recv error")
    {
        Some(SignalingMessage::LoginSuccess { room_id, .. }) => room_id,
        other => panic!("Expected LoginSuccess, got: {:?}", other),
    };

    let mut viewers = Vec::new();
    for i in 0..4 {
        let mut viewer = SignalingClient::connect(&url)
            .await
            .unwrap_or_else(|_| panic!("Failed to connect viewer {}", i));

        viewer
            .send(SignalingMessage::Join {
                room_id: room_code.clone(),
            })
            .await
            .unwrap_or_else(|_| panic!("Failed to send join for viewer {}", i));

        let msg = tokio::time::timeout(Duration::from_secs(5), viewer.recv())
            .await
            .unwrap_or_else(|_| panic!("Timeout for viewer {} join", i))
            .unwrap_or_else(|_| panic!("Recv error for viewer {}", i));

        match msg {
            Some(SignalingMessage::JoinSuccess { .. }) => {}
            other => panic!("Viewer {} expected JoinSuccess, got: {:?}", i, other),
        }

        viewers.push(viewer);
    }

    // Clear the queued ViewerJoined notifications so they don't interfere with
    // the 5th viewer's rejection below.
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(2), host.recv())
            .await
            .ok();
    }

    // The 5th viewer exceeds the room cap and must be rejected.
    let mut viewer5 = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect 5th viewer");

    viewer5
        .send(SignalingMessage::Join {
            room_id: room_code.clone(),
        })
        .await
        .expect("Failed to send join for 5th viewer");

    let msg = tokio::time::timeout(Duration::from_secs(5), viewer5.recv())
        .await
        .expect("Timeout waiting for 5th viewer response")
        .expect("Recv error for 5th viewer");

    match msg {
        Some(SignalingMessage::Error { code, message }) => {
            assert_eq!(
                code.as_deref(),
                Some("ROOM_FULL"),
                "Expected ROOM_FULL error code, got: {:?} (message: {})",
                code,
                message
            );
        }
        other => panic!("5th viewer expected Error(ROOM_FULL), got: {:?}", other),
    }

    viewer5.close().await.ok();
    for mut v in viewers {
        v.close().await.ok();
    }
    host.close().await.ok();
}

/// Viewer joins and host receives ViewerJoined with correct count.
#[ignore]
#[tokio::test]
async fn test_viewer_join_and_presence() {
    let url = signaling_url();

    let mut host = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect host");

    host.send(SignalingMessage::Login {
        role: "host".to_string(),
    })
    .await
    .expect("Failed to send host login");

    let room_code = match tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("Timeout")
        .expect("Recv error")
    {
        Some(SignalingMessage::LoginSuccess { room_id, .. }) => room_id,
        other => panic!("Expected LoginSuccess, got: {:?}", other),
    };

    let mut viewer = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect viewer");

    viewer
        .send(SignalingMessage::Join {
            room_id: room_code.clone(),
        })
        .await
        .expect("Failed to send viewer join");

    let msg = tokio::time::timeout(Duration::from_secs(5), viewer.recv())
        .await
        .expect("Timeout")
        .expect("Recv error");

    match msg {
        Some(SignalingMessage::JoinSuccess { ice_servers, .. }) => {
            assert!(!ice_servers.is_empty(), "Viewer should receive ICE servers");
        }
        other => panic!("Expected JoinSuccess, got: {:?}", other),
    }

    let msg = tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("Timeout waiting for ViewerJoined")
        .expect("Recv error");

    match msg {
        Some(SignalingMessage::ViewerJoined {
            viewer_id,
            viewer_count,
        }) => {
            assert!(!viewer_id.is_empty(), "Viewer ID should not be empty");
            assert_eq!(viewer_count, 1, "Should be 1 viewer");
        }
        other => panic!("Expected ViewerJoined, got: {:?}", other),
    }

    viewer.close().await.ok();
    host.close().await.ok();
}

/// SDP offer/answer relay between host and viewer.
#[ignore]
#[tokio::test]
async fn test_sdp_relay() {
    let url = signaling_url();

    let mut host = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect host");

    host.send(SignalingMessage::Login {
        role: "host".to_string(),
    })
    .await
    .expect("Failed to send host login");

    let room_code = match tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("Timeout")
        .expect("Recv error")
    {
        Some(SignalingMessage::LoginSuccess { room_id, .. }) => room_id,
        other => panic!("Expected LoginSuccess, got: {:?}", other),
    };

    let mut viewer = SignalingClient::connect(&url)
        .await
        .expect("Failed to connect viewer");

    viewer
        .send(SignalingMessage::Join {
            room_id: room_code.clone(),
        })
        .await
        .expect("Failed to send viewer join");

    let _join = tokio::time::timeout(Duration::from_secs(5), viewer.recv())
        .await
        .expect("Timeout")
        .expect("Recv error");

    let viewer_id = match tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("Timeout")
        .expect("Recv error")
    {
        Some(SignalingMessage::ViewerJoined { viewer_id, .. }) => viewer_id,
        other => panic!("Expected ViewerJoined, got: {:?}", other),
    };

    host.send(SignalingMessage::Offer {
        sdp: "test-offer-sdp".to_string(),
        target_id: Some(viewer_id.clone()),
        from: None,
    })
    .await
    .expect("Failed to send offer");

    let msg = tokio::time::timeout(Duration::from_secs(5), viewer.recv())
        .await
        .expect("Timeout waiting for Offer")
        .expect("Recv error");

    match msg {
        Some(SignalingMessage::Offer { sdp, .. }) => {
            assert_eq!(sdp, "test-offer-sdp", "Offer SDP should match");
        }
        other => panic!("Expected Offer, got: {:?}", other),
    }

    viewer
        .send(SignalingMessage::Answer {
            sdp: "test-answer-sdp".to_string(),
            target_id: None,
            from: None,
        })
        .await
        .expect("Failed to send answer");

    let msg = tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("Timeout waiting for Answer")
        .expect("Recv error");

    match msg {
        Some(SignalingMessage::Answer { sdp, from, .. }) => {
            assert_eq!(sdp, "test-answer-sdp", "Answer SDP should match");
            assert_eq!(
                from.as_deref(),
                Some(viewer_id.as_str()),
                "Answer should have viewer's ID in `from`"
            );
        }
        other => panic!("Expected Answer, got: {:?}", other),
    }

    viewer.close().await.ok();
    host.close().await.ok();
}
