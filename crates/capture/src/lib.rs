#[cfg(target_os = "linux")]
mod backend_linux;
#[cfg(target_os = "macos")]
mod backend_macos;
#[cfg(target_os = "windows")]
mod backend_windows;
pub mod error;

pub use error::CaptureError;
use std::sync::mpsc;

/// Compute actual stride for a 4-bpp buffer that may include row padding.
#[allow(dead_code)] // Used by backend_linux (conditionally compiled)
pub(crate) fn compute_stride_4bpp(data_len: usize, width: u32, height: u32) -> u32 {
    let min_stride = width * 4;
    let expected = (min_stride as usize) * (height as usize);
    if height > 0 && data_len > expected {
        (data_len / height as usize) as u32
    } else {
        min_stride
    }
}

/// Strip row padding from a 4-bpp buffer, producing clean data with stride = width * 4.
pub(crate) fn strip_row_padding(
    data: Vec<u8>,
    width: u32,
    height: u32,
    actual_stride: u32,
) -> (Vec<u8>, u32) {
    let min_stride = width * 4;
    if actual_stride <= min_stride {
        return (data, min_stride);
    }
    let mut clean = Vec::with_capacity((min_stride as usize) * (height as usize));
    for row in 0..height as usize {
        let start = row * actual_stride as usize;
        let end = start + min_stride as usize;
        if end <= data.len() {
            clean.extend_from_slice(&data[start..end]);
        }
    }
    (clean, min_stride)
}

/// A single captured frame in BGRA pixel format.
pub struct Frame {
    /// Raw pixel data in BGRA format
    pub data: Vec<u8>,
    /// Physical pixel width
    pub width: u32,
    /// Physical pixel height
    pub height: u32,
    /// Bytes per row (may include padding)
    pub stride: u32,
    /// Monotonic timestamp in nanoseconds
    pub timestamp_ns: u64,
    /// Source display identifier
    pub display_id: u32,
    /// Display scale factor (e.g., 2.0 for Retina)
    pub scale_factor: f64,
    /// Logical width (width as f64 / scale_factor, rounded)
    pub logical_width: u32,
    /// Logical height (height as f64 / scale_factor, rounded)
    pub logical_height: u32,
}

impl Frame {
    /// Create a new Frame, computing logical dimensions from physical dimensions and scale factor.
    pub fn new(
        data: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        timestamp_ns: u64,
        display_id: u32,
        scale_factor: f64,
    ) -> Self {
        let scale = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        Self {
            data,
            width,
            height,
            stride,
            timestamp_ns,
            display_id,
            scale_factor: scale,
            logical_width: (width as f64 / scale).round() as u32,
            logical_height: (height as f64 / scale).round() as u32,
        }
    }
}

/// Information about an available display/monitor.
pub struct Display {
    pub id: u32,
    pub name: String,
    /// Physical pixel width
    pub width: u32,
    /// Physical pixel height
    pub height: u32,
    pub scale_factor: f64,
    pub is_primary: bool,
}

/// Options for starting a capture session.
pub struct CaptureOptions {
    /// Target frames per second (default: 60)
    pub fps: u32,
    /// Specific display to capture. None = use OS native picker.
    pub display_id: Option<u32>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            fps: 60,
            display_id: None,
        }
    }
}

/// Handle to a running capture session. Dropping stops capture.
pub struct CaptureHandle {
    /// Receive captured frames from this channel
    pub frames: mpsc::Receiver<Frame>,
    _stop_tx: Option<std::sync::mpsc::SyncSender<()>>,
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        if let Some(tx) = self._stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// List all available displays/monitors.
pub fn list_displays() -> Result<Vec<Display>, CaptureError> {
    #[cfg(target_os = "linux")]
    {
        backend_linux::list_displays()
    }
    #[cfg(target_os = "macos")]
    {
        backend_macos::list_displays()
    }
    #[cfg(target_os = "windows")]
    {
        backend_windows::list_displays()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(CaptureError::NotSupported)
    }
}

/// Start capturing frames from the specified display.
///
/// Spawns a dedicated OS thread running the platform capture backend
/// and delivers BGRA-normalized frames via the `CaptureHandle.frames` channel.
/// Dropping the `CaptureHandle` signals the capture thread to stop cleanly.
pub async fn start_capture(
    opts: CaptureOptions,
) -> Result<(CaptureHandle, Option<String>), CaptureError> {
    #[cfg(target_os = "linux")]
    {
        let (frame_rx, stop_tx, warning) = backend_linux::spawn_capture_thread(opts).await?;
        Ok((
            CaptureHandle {
                frames: frame_rx,
                _stop_tx: Some(stop_tx),
            },
            warning,
        ))
    }
    #[cfg(target_os = "macos")]
    {
        let (frame_rx, stop_tx, warning) = backend_macos::spawn_capture_thread(opts).await?;
        Ok((
            CaptureHandle {
                frames: frame_rx,
                _stop_tx: Some(stop_tx),
            },
            warning,
        ))
    }
    #[cfg(target_os = "windows")]
    {
        let (frame_rx, stop_tx, warning) = backend_windows::spawn_capture_thread(opts).await?;
        Ok((
            CaptureHandle {
                frames: frame_rx,
                _stop_tx: Some(stop_tx),
            },
            warning,
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(CaptureError::NotSupported)
    }
}
