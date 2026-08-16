// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // On Linux, the overlay runs as a subprocess of the same binary.
    // Intercept the --overlay flag before Tauri starts.
    #[cfg(target_os = "linux")]
    if std::env::args().any(|a| a == "--overlay") {
        let display_id = std::env::args()
            .position(|a| a == "--display")
            .and_then(|i| std::env::args().nth(i + 1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        overlay_sidecar::run(false, display_id);
        return;
    }

    sidecar_lib::run()
}
