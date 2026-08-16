//! Capture-only memory leak isolation test.
//! Run: cargo run -p capture --example mem_test
//! Or with malloc fix: MALLOC_MMAP_THRESHOLD_=65536 cargo run -p capture --example mem_test

fn get_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    eprintln!("=== Memory Leak Test (capture-only) ===");
    eprintln!("PID: {}", std::process::id());

    let opts = capture::CaptureOptions {
        fps: 30,
        display_id: None,
    };
    let (handle, _warning) = match capture::start_capture(opts).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Error: {e}");
            return;
        }
    };

    let mut count = 0u64;
    let start = std::time::Instant::now();
    let mut last_report = start;

    while let Ok(frame) = handle.frames.recv() {
        let data_len = frame.data.len();
        let data_cap = frame.data.capacity();
        count += 1;
        drop(frame);

        let now = std::time::Instant::now();
        if now.duration_since(last_report).as_secs() >= 2 {
            let rss_kb = get_rss_kb();
            let elapsed = now.duration_since(start).as_secs();
            eprintln!(
                "[{elapsed:3}s] frames={count:5} len={data_len:10} cap={data_cap:10} RSS={:6}MB",
                rss_kb / 1024
            );
            last_report = now;
            if rss_kb > 4_000_000 {
                eprintln!("RSS > 4GB, aborting!");
                break;
            }
        }
    }
}
