use serde::{Deserialize, Serialize};

/// Messages sent from Tauri host app to the overlay sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd")]
pub enum HostToSidecar {
    /// Forward raw annotation JSON (ds/dm/de/cl packets from viewers or host UI)
    #[serde(rename = "annotation")]
    Annotation { data: String },

    /// Toggle between draw mode and passthrough mode
    #[serde(rename = "set_mode")]
    SetMode { draw: bool },

    /// Graceful shutdown request
    #[serde(rename = "shutdown")]
    Shutdown,

    /// Change the host drawing color. Color must be #RRGGBB hex.
    #[serde(rename = "set_color")]
    SetColor { color: String },

    /// Change the host brush size (will be clamped to 1.0-20.0)
    #[serde(rename = "set_brush_size")]
    SetBrushSize { size: f64 },

    /// Toggle draw mode atomically (sidecar flips its own state)
    #[serde(rename = "toggle_draw")]
    ToggleDraw,

    /// Clear all annotations from the overlay
    #[serde(rename = "clear_annotations")]
    ClearAnnotations,

    /// Set manual resolution override. Height 0 clears override.
    #[serde(rename = "set_resolution")]
    SetResolution { height: u32 },

    /// Toggle the remote-control active indicator on the overlay. When active,
    /// the overlay draws a border so the host knows their input can be driven
    /// remotely. Does not gate injection (that lives in the pipeline) -- this
    /// is purely the host-visible indicator.
    #[serde(rename = "remote_control_active")]
    RemoteControlActive { active: bool },
}

/// Messages sent from the overlay sidecar back to Tauri host app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "evt")]
pub enum SidecarToHost {
    /// Sidecar successfully created layer surface and is ready
    #[serde(rename = "ready")]
    Ready,

    /// User toggled draw/passthrough mode via keyboard in the sidecar
    #[serde(rename = "mode_changed")]
    ModeChanged { draw: bool },

    /// Host drew a stroke on the sidecar overlay; forward to viewers via data channel
    #[serde(rename = "annotation")]
    Annotation { data: String },

    /// Fatal error in the sidecar
    #[serde(rename = "error")]
    Error { msg: String },

    /// Current drawing state broadcast to all clients.
    /// Emitted after SetColor/SetBrushSize and on new client connect.
    #[serde(rename = "state_changed")]
    StateChanged {
        color: String,
        brush_size: f64,
        draw_mode: bool,
    },

    /// Forward resolution override request from CLI to host-app pipeline.
    /// The overlay-sidecar itself cannot control ABR -- it relays to host-app.
    #[serde(rename = "set_resolution_request")]
    SetResolutionRequest { height: u32 },
}

/// Preset annotation color palette, kept consistent across host, viewer, and server.
pub const COLOR_PALETTE: &[&str] = &[
    "#FF3B30", // red
    "#FF9500", // orange
    "#FFD60A", // yellow
    "#34C759", // green
    "#007AFF", // blue
    "#AF52DE", // purple
    "#00C7BE", // teal
];

/// Default host drawing color (last color in the palette).
pub const HOST_DEFAULT_COLOR: &str = COLOR_PALETTE[COLOR_PALETTE.len() - 1];

/// Validate a `#RRGGBB` hex color string.
/// Returns true if exactly 7 chars: '#' followed by 6 hex digits.
pub fn is_valid_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Construct the well-known socket path.
/// Prefers $XDG_RUNTIME_DIR (standard Wayland/systemd convention),
/// falls back to /tmp with UID for environments without systemd.
/// On Windows, uses the TEMP directory.
pub fn socket_path() -> String {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) => format!("{}/sidecar-overlay.sock", dir),
        Err(_) => {
            #[cfg(unix)]
            {
                let uid = unsafe { libc::getuid() };
                log::warn!("XDG_RUNTIME_DIR not set, using /tmp fallback");
                format!("/tmp/sidecar-overlay-{}.sock", uid)
            }
            #[cfg(windows)]
            {
                let tmp = std::env::temp_dir();
                log::warn!("XDG_RUNTIME_DIR not set, using TEMP fallback");
                format!("{}\\sidecar-overlay.sock", tmp.display())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_color_serialize() {
        let msg = HostToSidecar::SetColor {
            color: "#FF3B30".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r##"{"cmd":"set_color","color":"#FF3B30"}"##);
        let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_set_brush_size_serialize() {
        let msg = HostToSidecar::SetBrushSize { size: 0.005 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"set_brush_size","size":0.005}"#);
        let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_state_changed_serialize() {
        let msg = SidecarToHost::StateChanged {
            color: "#FF3B30".to_string(),
            brush_size: 0.003,
            draw_mode: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r##"{"evt":"state_changed","color":"#FF3B30","brush_size":0.003,"draw_mode":false}"##
        );
        let decoded: SidecarToHost = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_valid_hex_colors() {
        assert!(is_valid_hex_color("#FF3B30"));
        assert!(is_valid_hex_color("#007AFF"));
        assert!(is_valid_hex_color("#000000"));
        assert!(is_valid_hex_color("#ffffff"));
        assert!(is_valid_hex_color("#aAbBcC"));
    }

    #[test]
    fn test_invalid_hex_colors() {
        assert!(!is_valid_hex_color("FF3B30")); // missing #
        assert!(!is_valid_hex_color("#FF3B3")); // too short
        assert!(!is_valid_hex_color("#FF3B300")); // too long
        assert!(!is_valid_hex_color("#GGHHII")); // non-hex
        assert!(!is_valid_hex_color("")); // empty
        assert!(!is_valid_hex_color("red")); // named color
    }

    #[test]
    fn test_host_to_sidecar_annotation_serialize() {
        let msg = HostToSidecar::Annotation {
            data: r##"{"t":"ds","x":0.5,"y":0.5,"c":"#FF0000","w":0.003,"l":"abc","id":"host1"}"##
                .to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""cmd":"annotation""#));
        assert!(json.contains(r#""data":"#));
    }

    #[test]
    fn test_host_to_sidecar_set_mode_serialize() {
        let msg = HostToSidecar::SetMode { draw: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"set_mode","draw":true}"#);

        let msg2 = HostToSidecar::SetMode { draw: false };
        let json2 = serde_json::to_string(&msg2).unwrap();
        assert_eq!(json2, r#"{"cmd":"set_mode","draw":false}"#);
    }

    #[test]
    fn test_host_to_sidecar_shutdown_serialize() {
        let msg = HostToSidecar::Shutdown;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"shutdown"}"#);
    }

    #[test]
    fn test_sidecar_to_host_ready_serialize() {
        let msg = SidecarToHost::Ready;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"evt":"ready"}"#);
    }

    #[test]
    fn test_sidecar_to_host_mode_changed_serialize() {
        let msg = SidecarToHost::ModeChanged { draw: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"evt":"mode_changed","draw":true}"#);
    }

    #[test]
    fn test_sidecar_to_host_annotation_serialize() {
        let msg = SidecarToHost::Annotation {
            data: r##"{"t":"ds","x":0.1,"y":0.2,"c":"#FFD60A","w":0.003,"l":"line1","id":"host"}"##
                .to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""evt":"annotation""#));
    }

    #[test]
    fn test_sidecar_to_host_error_serialize() {
        let msg = SidecarToHost::Error {
            msg: "no layer_shell global".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""evt":"error""#));
        assert!(json.contains("no layer_shell global"));
    }

    #[test]
    fn test_host_to_sidecar_roundtrip() {
        let variants = vec![
            HostToSidecar::Annotation {
                data: "test".to_string(),
            },
            HostToSidecar::SetMode { draw: true },
            HostToSidecar::SetMode { draw: false },
            HostToSidecar::Shutdown,
            HostToSidecar::SetColor {
                color: "#FF3B30".to_string(),
            },
            HostToSidecar::SetBrushSize { size: 0.005 },
            HostToSidecar::ToggleDraw,
            HostToSidecar::ClearAnnotations,
            HostToSidecar::SetResolution { height: 720 },
            HostToSidecar::RemoteControlActive { active: true },
            HostToSidecar::RemoteControlActive { active: false },
        ];
        for original in variants {
            let json = serde_json::to_string(&original).unwrap();
            let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
            assert_eq!(original, decoded);
        }
    }

    #[test]
    fn test_remote_control_active_serialize() {
        let msg = HostToSidecar::RemoteControlActive { active: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"remote_control_active","active":true}"#);
        let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_sidecar_to_host_roundtrip() {
        let variants = vec![
            SidecarToHost::Ready,
            SidecarToHost::ModeChanged { draw: true },
            SidecarToHost::ModeChanged { draw: false },
            SidecarToHost::Annotation {
                data: "test".to_string(),
            },
            SidecarToHost::Error {
                msg: "fail".to_string(),
            },
            SidecarToHost::StateChanged {
                color: "#FF3B30".to_string(),
                brush_size: 0.003,
                draw_mode: false,
            },
            SidecarToHost::SetResolutionRequest { height: 720 },
        ];
        for original in variants {
            let json = serde_json::to_string(&original).unwrap();
            let decoded: SidecarToHost = serde_json::from_str(&json).unwrap();
            assert_eq!(original, decoded);
        }
    }

    #[test]
    fn test_annotation_data_contains_ds_packet() {
        let ds_json =
            r##"{"t":"ds","x":0.5,"y":0.5,"c":"#FF0000","w":0.003,"l":"abc","id":"host1"}"##;
        let msg = HostToSidecar::Annotation {
            data: ds_json.to_string(),
        };
        let serialized = serde_json::to_string(&msg).unwrap();
        let deserialized: HostToSidecar = serde_json::from_str(&serialized).unwrap();
        if let HostToSidecar::Annotation { data } = deserialized {
            assert_eq!(data, ds_json);
            let inner: serde_json::Value = serde_json::from_str(&data).unwrap();
            assert_eq!(inner["t"], "ds");
            assert_eq!(inner["x"], 0.5);
        } else {
            panic!("Expected Annotation variant");
        }
    }

    #[test]
    fn test_toggle_draw_serialize() {
        let msg = HostToSidecar::ToggleDraw;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"toggle_draw"}"#);
        let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_clear_annotations_serialize() {
        let msg = HostToSidecar::ClearAnnotations;
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"clear_annotations"}"#);
        let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_set_resolution_serialize() {
        let msg = HostToSidecar::SetResolution { height: 720 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"cmd":"set_resolution","height":720}"#);
        let decoded: HostToSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_set_resolution_request_serialize() {
        let msg = SidecarToHost::SetResolutionRequest { height: 720 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""evt":"set_resolution_request""#));
        assert!(json.contains(r#""height":720"#));
        let decoded: SidecarToHost = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_socket_path_with_xdg() {
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: This test is not run in parallel with other tests that access this env var.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        let path = socket_path();
        assert_eq!(path, "/run/user/1000/sidecar-overlay.sock");
        match original {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
    }
}
