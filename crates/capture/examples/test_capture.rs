//! Standalone capture test that runs outside Tauri to isolate PipeWire issues.
use std::path::Path;

#[tokio::main]
async fn main() {
    println!("=== Capture Test ===");

    println!("\n--- list_displays ---");
    match capture::list_displays() {
        Ok(displays) => {
            for d in &displays {
                println!(
                    "  Display {}: {} ({}x{}, scale={}, primary={})",
                    d.id, d.name, d.width, d.height, d.scale_factor, d.is_primary
                );
            }
        }
        Err(e) => {
            eprintln!("  Error: {e}");
            return;
        }
    }

    println!("\n--- start_capture (5 seconds) ---");
    let opts = capture::CaptureOptions {
        fps: 10,
        display_id: None,
    };

    let (handle, warning) = match capture::start_capture(opts).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("  Error starting capture: {e}");
            return;
        }
    };
    if let Some(w) = warning {
        eprintln!("  Warning: {w}");
    }

    let out_dir = std::env::var("CAPTURE_TEST_OUT")
        .unwrap_or_else(|_| "/tmp/screenshare-test-standalone".to_string());
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut count = 0u32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

    while std::time::Instant::now() < deadline {
        match handle
            .frames
            .recv_timeout(deadline.duration_since(std::time::Instant::now()))
        {
            Ok(frame) => {
                println!(
                    "  Frame {count}: {}x{} stride={} ts={} data_len={}",
                    frame.width,
                    frame.height,
                    frame.stride,
                    frame.timestamp_ns,
                    frame.data.len()
                );

                if count < 3 {
                    let path = Path::new(&out_dir).join(format!("frame_{count:03}.png"));
                    match save_png(&frame, &path) {
                        Ok(_) => println!("  Saved: {}", path.display()),
                        Err(e) => eprintln!("  PNG save error: {e}"),
                    }
                }

                count += 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                println!("  Channel closed");
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
        }
    }

    println!("\n--- Done: {count} frames captured ---");
    println!("  Output: {out_dir}");
}

fn save_png(frame: &capture::Frame, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut rgba = frame.data.clone();
    for chunk in rgba.as_chunks_mut::<4>().0 {
        chunk.swap(0, 2);
    }

    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(file, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    Ok(())
}
