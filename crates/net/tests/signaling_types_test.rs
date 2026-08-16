use net::types::{IceServerConfig, SessionConfig, SignalingMessage};

#[test]
fn signaling_message_login_serializes() {
    let msg = SignalingMessage::Login {
        role: "host".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"login\""));
    assert!(json.contains("\"role\":\"host\""));
}

#[test]
fn signaling_message_login_success_deserializes() {
    let json = r#"{"type":"login-success","roomId":"blue-tiger-42","iceServers":[{"urls":"stun:stun.l.google.com:19302"}]}"#;
    let msg: SignalingMessage = serde_json::from_str(json).unwrap();
    match msg {
        SignalingMessage::LoginSuccess {
            room_id,
            ice_servers,
        } => {
            assert_eq!(room_id, "blue-tiger-42");
            assert_eq!(ice_servers.len(), 1);
        }
        _ => panic!("Expected LoginSuccess"),
    }
}

#[test]
fn signaling_message_round_trip_error() {
    let msg = SignalingMessage::Error {
        code: Some("ROOM_FULL".into()),
        message: "Full".into(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let decoded: SignalingMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        SignalingMessage::Error { code, message } => {
            assert_eq!(code, Some("ROOM_FULL".into()));
            assert_eq!(message, "Full");
        }
        _ => panic!("Expected Error"),
    }
}

#[test]
fn signaling_message_round_trip_viewer_joined() {
    let msg = SignalingMessage::ViewerJoined {
        viewer_id: "v1".into(),
        viewer_count: 2,
    };
    let json = serde_json::to_string(&msg).unwrap();
    let decoded: SignalingMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        SignalingMessage::ViewerJoined {
            viewer_id,
            viewer_count,
        } => {
            assert_eq!(viewer_id, "v1");
            assert_eq!(viewer_count, 2);
        }
        _ => panic!("Expected ViewerJoined"),
    }
}

#[test]
fn ice_server_config_skips_none_fields() {
    let cfg = IceServerConfig {
        urls: "stun:example.com".into(),
        username: None,
        credential: None,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(!json.contains("username"));
    assert!(!json.contains("credential"));
}

#[test]
fn session_config_default() {
    let cfg = SessionConfig::default();
    assert_eq!(cfg.signaling_url, net::DEFAULT_SIGNALING_URL);
    assert_eq!(cfg.max_viewers, 3);
}
