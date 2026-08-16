#[cfg(not(target_os = "linux"))]
compile_error!("sidecar-cli only supports Linux");

mod app;
mod overlay;
mod overlay_ctl;
mod tui;

use clap::{Parser, Subcommand};

/// Sidecar: low-latency peer-to-peer screen sharing from your terminal.
#[derive(Parser)]
#[command(name = "sidecar", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start a screen sharing session (TUI dashboard)
    Host,

    /// Open the web viewer in your browser for a room code
    Join {
        /// The room code to join (e.g. brave-parrot-42)
        room_code: String,
    },

    /// Control overlay draw mode [on|off, or toggle if omitted]
    Draw {
        /// "on" to enable, "off" to disable, omit to toggle
        mode: Option<String>,
    },

    /// Clear all annotations from the overlay
    Clear,

    /// Set output resolution height override (e.g. 720). Use 0 to clear.
    SetResolution {
        /// Target height in pixels (0 = clear override)
        height: u32,
    },

    /// Internal: run the overlay sidecar subprocess
    #[command(hide = true)]
    Overlay {
        /// Run in standalone test mode
        #[arg(long)]
        test: bool,

        /// Display/monitor index
        #[arg(long, default_value = "0")]
        display: u32,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Overlay { test, display }) => {
            init_logging();
            overlay_sidecar::run(test, display);
        }
        Some(Command::Host) | None => {
            init_logging();
            pipeline::fix_glibc_malloc();
            if let Err(e) = app::run() {
                eprintln!("{}", e);
            }
        }
        Some(Command::Join { room_code }) => {
            run_join(&room_code);
        }
        Some(Command::Draw { mode }) => match mode.as_deref() {
            None => overlay_ctl::send(ipc::HostToSidecar::ToggleDraw),
            Some("on") => overlay_ctl::send(ipc::HostToSidecar::SetMode { draw: true }),
            Some("off") => overlay_ctl::send(ipc::HostToSidecar::SetMode { draw: false }),
            Some(other) => {
                eprintln!("sidecar draw: unknown mode '{}' (use 'on' or 'off')", other);
                std::process::exit(1);
            }
        },
        Some(Command::Clear) => overlay_ctl::send(ipc::HostToSidecar::ClearAnnotations),
        Some(Command::SetResolution { height }) => {
            overlay_ctl::send(ipc::HostToSidecar::SetResolution { height })
        }
    }
}

fn init_logging() {
    let log_dir = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        format!("{}/.local/state", home)
    });
    let log_path = format!("{}/sidecar/sidecar.log", log_dir);
    let _ = std::fs::create_dir_all(format!("{}/sidecar", log_dir));

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    if let Ok(file) = log_file {
        let file_clone = file.try_clone().ok();

        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();

        if let Some(f) = file_clone {
            redirect_stderr(f);
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("off"))
            .init();
    }
}

pub fn run_join(room_code: &str) {
    let url = app::viewer_link(room_code);

    println!("Opening viewer: {}", url);

    let result = std::process::Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .or_else(|_| {
            if let Ok(browser) = std::env::var("BROWSER") {
                std::process::Command::new(browser).arg(&url).spawn()
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No browser found",
                ))
            }
        });

    match result {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Could not open browser: {}", e);
            eprintln!("Open this URL manually: {}", url);
            std::process::exit(1);
        }
    }
}

fn redirect_stderr(file: std::fs::File) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::dup2(file.as_raw_fd(), 2);
    }
    std::mem::forget(file);
}
