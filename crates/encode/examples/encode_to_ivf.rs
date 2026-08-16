//! IVF validation harness: encodes synthetic frames to VP9 IVF file.
//!
//! Usage: cargo run -p encode --example encode_to_ivf -- output.ivf [--frames 90] [--width 1920] [--height 1080]
//!
//! The output can be played with `ffplay output.ivf` or `mpv output.ivf`.

use encode::{EncoderBackend, EncoderConfig, VpxEncoder};
use std::fs::File;
use std::io::Write;
use std::time::Instant;

fn write_ivf_header(w: &mut impl Write, width: u16, height: u16, fps: u32, frame_count: u32) {
    w.write_all(b"DKIF").unwrap(); // signature
    w.write_all(&0u16.to_le_bytes()).unwrap(); // version
    w.write_all(&32u16.to_le_bytes()).unwrap(); // header length
    w.write_all(b"VP90").unwrap(); // codec fourcc
    w.write_all(&width.to_le_bytes()).unwrap();
    w.write_all(&height.to_le_bytes()).unwrap();
    w.write_all(&fps.to_le_bytes()).unwrap(); // timebase denominator
    w.write_all(&1u32.to_le_bytes()).unwrap(); // timebase numerator
    w.write_all(&frame_count.to_le_bytes()).unwrap();
    w.write_all(&0u32.to_le_bytes()).unwrap(); // unused
}

fn write_ivf_frame(w: &mut impl Write, data: &[u8], pts: u64) {
    w.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
    w.write_all(&pts.to_le_bytes()).unwrap();
    w.write_all(data).unwrap();
}

/// Generate a synthetic BGRA frame.
/// Alternates between solid dark blue (IDE background) and a pattern with
/// a white rectangle on dark background (code editor with text area).
fn make_frame(width: u32, height: u32, frame_idx: u32) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![0u8; (stride * height) as usize];

    let scene = (frame_idx / 30) % 2;
    if scene == 0 {
        // Solid dark blue (BGRA = [50, 40, 40, 255])
        for pixel in data.chunks_exact_mut(4) {
            pixel[0] = 50; // B
            pixel[1] = 40; // G
            pixel[2] = 40; // R
            pixel[3] = 255; // A
        }
    } else {
        // Dark background with white rectangle (simulating text area)
        let rect_x = width / 8;
        let rect_y = height / 8;
        let rect_w = width * 3 / 4;
        let rect_h = height * 3 / 4;

        for y in 0..height {
            for x in 0..width {
                let offset = ((y * stride) + x * 4) as usize;
                if x >= rect_x && x < rect_x + rect_w && y >= rect_y && y < rect_y + rect_h {
                    // White rectangle
                    data[offset] = 240; // B
                    data[offset + 1] = 240; // G
                    data[offset + 2] = 240; // R
                    data[offset + 3] = 255; // A
                } else {
                    // Dark background
                    data[offset] = 30; // B
                    data[offset + 1] = 30; // G
                    data[offset + 2] = 35; // R
                    data[offset + 3] = 255; // A
                }
            }
        }
    }

    data
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut output_path = "output.ivf".to_string();
    let mut frame_count: u32 = 90;
    let mut width: u32 = 1920;
    let mut height: u32 = 1080;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                i += 1;
                frame_count = args[i].parse().expect("invalid --frames value");
            }
            "--width" => {
                i += 1;
                width = args[i].parse().expect("invalid --width value");
            }
            "--height" => {
                i += 1;
                height = args[i].parse().expect("invalid --height value");
            }
            s if !s.starts_with("--") => {
                output_path = s.to_string();
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    println!(
        "Encoding {} frames at {}x{} to {}",
        frame_count, width, height, output_path
    );

    let config = EncoderConfig {
        width,
        height,
        ..Default::default()
    };

    let mut encoder = VpxEncoder::new(&config).expect("Failed to create encoder");
    let mut file = File::create(&output_path).expect("Failed to create output file");

    // Write the IVF header. The timebase is 1/1000 (milliseconds) to match the
    // encoder's fixed g_timebase. PTS values from encode_frame are in ms.
    write_ivf_header(&mut file, width as u16, height as u16, 1000, frame_count);

    let mut total_encode_time_us: u64 = 0;
    let mut total_packet_size: u64 = 0;

    let overall_start = Instant::now();

    for idx in 0..frame_count {
        let frame_data = make_frame(width, height, idx);
        let frame = capture::Frame::new(
            frame_data,
            width,
            height,
            width * 4,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            0,
            1.0,
        );

        let packet = encoder.encode_frame(&frame).expect("Encode failed");

        write_ivf_frame(&mut file, &packet.data, packet.pts);

        total_encode_time_us += packet.encode_duration_ns / 1000;
        total_packet_size += packet.data.len() as u64;
    }

    let overall_elapsed = overall_start.elapsed();
    let file_size = std::fs::metadata(&output_path).unwrap().len();

    println!("\n--- Encoding Summary ---");
    println!("Total frames:     {}", frame_count);
    println!(
        "File size:        {} bytes ({:.1} KB)",
        file_size,
        file_size as f64 / 1024.0
    );
    println!(
        "Avg encode time:  {:.2} ms/frame",
        total_encode_time_us as f64 / frame_count as f64 / 1000.0
    );
    println!(
        "Avg packet size:  {} bytes",
        total_packet_size / frame_count as u64
    );
    println!("Total wall time:  {:.2}s", overall_elapsed.as_secs_f64());
    println!(
        "\nValidate with: ffplay {} or mpv {}",
        output_path, output_path
    );
}
