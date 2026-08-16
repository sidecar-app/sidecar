use capture::{CaptureOptions, Display, Frame};

#[test]
fn test_frame_new_computes_logical_dimensions() {
    let data = vec![0u8; 2560 * 1440 * 4];
    let frame = Frame::new(data, 2560, 1440, 2560 * 4, 123456789, 1, 2.0);
    assert_eq!(frame.logical_width, 1280);
    assert_eq!(frame.logical_height, 720);
    assert_eq!(frame.width, 2560);
    assert_eq!(frame.height, 1440);
    assert_eq!(frame.scale_factor, 2.0);
    assert_eq!(frame.timestamp_ns, 123456789);
    assert_eq!(frame.display_id, 1);
}

#[test]
fn test_frame_new_scale_factor_1() {
    let data = vec![0u8; 1920 * 1080 * 4];
    let frame = Frame::new(data, 1920, 1080, 7680, 0, 1, 1.0);
    assert_eq!(frame.logical_width, 1920);
    assert_eq!(frame.logical_height, 1080);
    assert_eq!(frame.scale_factor, 1.0);
}

#[test]
fn test_frame_new_zero_scale_factor_defaults_to_1() {
    let data = vec![0u8; 1920 * 1080 * 4];
    let frame = Frame::new(data, 1920, 1080, 7680, 0, 1, 0.0);
    assert_eq!(frame.scale_factor, 1.0);
    assert_eq!(frame.logical_width, 1920);
    assert_eq!(frame.logical_height, 1080);
}

#[test]
fn test_frame_new_negative_scale_factor_defaults_to_1() {
    let data = vec![0u8; 1920 * 1080 * 4];
    let frame = Frame::new(data, 1920, 1080, 7680, 0, 1, -1.0);
    assert_eq!(frame.scale_factor, 1.0);
    assert_eq!(frame.logical_width, 1920);
    assert_eq!(frame.logical_height, 1080);
}

#[test]
fn test_frame_data_length_matches_dimensions() {
    let stride: u32 = 400;
    let height: u32 = 100;
    let data = vec![0u8; (stride * height) as usize];
    let frame = Frame::new(data, 100, height, stride, 0, 1, 1.0);
    assert_eq!(frame.data.len(), (stride * height) as usize);
    assert_eq!(frame.stride, stride);
}

#[test]
fn test_capture_options_default() {
    let opts = CaptureOptions::default();
    assert_eq!(opts.fps, 60);
    assert!(opts.display_id.is_none());
}

#[test]
fn test_display_struct_fields() {
    let display = Display {
        id: 1,
        name: "Primary Monitor".to_string(),
        width: 3840,
        height: 2160,
        scale_factor: 2.0,
        is_primary: true,
    };
    assert_eq!(display.id, 1);
    assert_eq!(display.name, "Primary Monitor");
    assert_eq!(display.width, 3840);
    assert_eq!(display.height, 2160);
    assert_eq!(display.scale_factor, 2.0);
    assert!(display.is_primary);
}
