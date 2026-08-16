//! Windows screen capture via Windows.Graphics.Capture (WGC).
//!
//! Uses the `windows-capture` crate (a thin wrapper over the WinRT WGC API +
//! D3D11 staging-texture readback). Delivers BGRA frames over the same
//! `std::sync::mpsc::sync_channel` contract as the Linux/macOS backends.
//!
//! WGC delivers frames on change (not at a steady FPS); the encode thread
//! handles rate-limiting and keepalive re-encoding for static screens, so
//! the change-only delivery is acceptable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WgcFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::{CaptureError, CaptureOptions, Display, Frame, strip_row_padding};

// ---------------------------------------------------------------------------
// Display enumeration
// ---------------------------------------------------------------------------

pub fn list_displays() -> Result<Vec<Display>, CaptureError> {
    let monitors = Monitor::enumerate()
        .map_err(|e| CaptureError::StartFailed(format!("enumerate monitors: {e}")))?;
    if monitors.is_empty() {
        return Err(CaptureError::NoDisplays);
    }
    let primary_hmon = Monitor::primary().ok().map(|m| m.as_raw_hmonitor());

    let mut out = Vec::with_capacity(monitors.len());
    for (i, m) in monitors.into_iter().enumerate() {
        let width = m.width().unwrap_or(0);
        let height = m.height().unwrap_or(0);
        let name = m
            .device_string()
            .or_else(|_| m.device_name())
            .or_else(|_| m.name())
            .unwrap_or_else(|_| format!("Display {}", i + 1));
        out.push(Display {
            id: (i as u32) + 1, // 1-based to match Monitor::from_index
            name,
            width,
            height,
            scale_factor: 1.0, // TODO: query GetDpiForMonitor when needed
            is_primary: primary_hmon == Some(m.as_raw_hmonitor()),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Capture handler
// ---------------------------------------------------------------------------

struct CaptureCtx {
    frame_tx: std_mpsc::SyncSender<Frame>,
    init_tx: Option<std_mpsc::SyncSender<Result<(), String>>>,
    display_id: u32,
    scale_factor: f64,
    start_instant: Instant,
    stop_flag: Arc<AtomicBool>,
}

struct SidecarHandler {
    frame_tx: std_mpsc::SyncSender<Frame>,
    display_id: u32,
    scale_factor: f64,
    start_instant: Instant,
    stop_flag: Arc<AtomicBool>,
}

impl GraphicsCaptureApiHandler for SidecarHandler {
    type Flags = CaptureCtx;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let mut flags = ctx.flags;
        if let Some(tx) = flags.init_tx.take() {
            let _ = tx.send(Ok(()));
        }
        Ok(Self {
            frame_tx: flags.frame_tx,
            display_id: flags.display_id,
            scale_factor: flags.scale_factor,
            start_instant: flags.start_instant,
            stop_flag: flags.stop_flag,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WgcFrame<'_>,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop_flag.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }

        // Stage the GPU texture to CPU and map. BGRA, 256-byte aligned row pitch.
        let mut buffer = frame.buffer()?;
        let width = buffer.width();
        let height = buffer.height();
        let row_pitch = buffer.row_pitch();
        let raw = buffer.as_raw_buffer();

        let ts_ns = self.start_instant.elapsed().as_nanos() as u64;

        let (data, stride) = strip_row_padding(raw.to_vec(), width, height, row_pitch);

        let out = Frame::new(
            data,
            width,
            height,
            stride,
            ts_ns,
            self.display_id,
            self.scale_factor,
        );

        // Non-blocking; drop frame if encoder is behind (matches Linux/mac).
        let _ = self.frame_tx.try_send(out);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub async fn spawn_capture_thread(
    opts: CaptureOptions,
) -> Result<
    (
        std_mpsc::Receiver<Frame>,
        std_mpsc::SyncSender<()>,
        Option<String>,
    ),
    CaptureError,
> {
    let monitor = match opts.display_id {
        Some(id) => Monitor::from_index(id as usize)
            .map_err(|e| CaptureError::StartFailed(format!("monitor index {id}: {e}")))?,
        None => Monitor::primary().map_err(|_| CaptureError::NoDisplays)?,
    };
    let display_id = opts.display_id.unwrap_or(1);
    let scale_factor = 1.0; // TODO: GetDpiForMonitor

    // Same channel capacities as the Linux backend.
    let (frame_tx, frame_rx) = std_mpsc::sync_channel::<Frame>(2);
    let (stop_tx, stop_rx) = std_mpsc::sync_channel::<()>(1);
    let (init_tx, init_rx) = std_mpsc::sync_channel::<Result<(), String>>(1);
    let stop_flag = Arc::new(AtomicBool::new(false));

    let ctx = CaptureCtx {
        frame_tx,
        init_tx: Some(init_tx),
        display_id,
        scale_factor,
        start_instant: Instant::now(),
        stop_flag: stop_flag.clone(),
    };
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        ctx,
    );

    // Start capture on the windows-capture crate's own thread.
    let control = SidecarHandler::start_free_threaded(settings)
        .map_err(|e| CaptureError::StartFailed(format!("WGC start: {e}")))?;

    match init_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return Err(CaptureError::StartFailed(msg)),
        Err(_) => return Err(CaptureError::StartFailed("WGC init timeout".into())),
    }

    // Stop watcher: flips stop_flag (read in on_frame_arrived) and joins
    // the WGC thread via control.stop().
    std::thread::Builder::new()
        .name("capture-windows-stop".into())
        .spawn(move || {
            let _ = stop_rx.recv();
            stop_flag.store(true, Ordering::Relaxed);
            let _ = control.stop();
        })
        .map_err(|e| CaptureError::StartFailed(format!("stop watcher spawn failed: {e}")))?;

    Ok((frame_rx, stop_tx, None))
}
