pub mod abr;
pub mod colorspace;
pub mod error;
pub mod h264;
pub mod profiles;
pub mod resize;
#[cfg(target_os = "linux")]
pub mod vaapi;
#[cfg(target_os = "macos")]
pub mod videotoolbox;
mod vpx;

pub use abr::{AbrConfig, RuleBasedAbrController};
pub use profiles::{EncodeProfile, ProfileCatalog};

pub use error::EncodeError;
#[cfg(target_os = "linux")]
pub use vaapi::VaapiEncoder;
#[cfg(target_os = "macos")]
pub use videotoolbox::VideotoolboxEncoder;
pub use vpx::VpxEncoder;

/// Video codec selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    VP9,
    AV1,
    H264,
}

impl VideoCodec {
    /// MIME type string for WebRTC track registration.
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::VP9 => "video/VP9",
            Self::AV1 => "video/AV1",
            Self::H264 => "video/H264",
        }
    }

    /// FFmpeg VAAPI encoder name for this codec.
    #[cfg(target_os = "linux")]
    pub fn vaapi_encoder_name(&self) -> &'static str {
        match self {
            Self::VP9 => "vp9_vaapi",
            Self::AV1 => "av1_vaapi",
            Self::H264 => "h264_vaapi",
        }
    }

    /// FFmpeg VideoToolbox encoder name for this codec.
    #[cfg(target_os = "macos")]
    pub fn videotoolbox_encoder_name(&self) -> &'static str {
        match self {
            Self::AV1 => "av1_videotoolbox",
            Self::H264 => "h264_videotoolbox",
            Self::VP9 => unreachable!("VP9 not available via VideoToolbox"),
        }
    }
}

/// Configuration for initializing an encoder.
#[derive(Clone, Copy)]
pub struct EncoderConfig {
    /// Which video codec to use.
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    /// Target bitrate in kbps
    pub target_bitrate_kbps: u32,
    /// Maximum bitrate in kbps
    pub max_bitrate_kbps: u32,
    /// Minimum bitrate in kbps
    pub min_bitrate_kbps: u32,
    /// Target frames per second
    pub target_fps: u32,
    /// Number of encoding threads
    pub threads: u32,
    /// Maximum quantizer value (higher = lower quality)
    pub max_quantizer: u32,
    /// Minimum quantizer value (lower = higher quality)
    pub min_quantizer: u32,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::VP9,
            width: 1920,
            height: 1080,
            target_bitrate_kbps: 16_000,
            max_bitrate_kbps: 16_000,
            min_bitrate_kbps: 500,
            target_fps: 60,
            threads: 8,
            max_quantizer: 33,
            min_quantizer: 10,
        }
    }
}

/// Runtime-adjustable encoder parameters for adaptive bitrate control.
#[derive(Clone, PartialEq)]
pub struct EncoderParams {
    pub target_bitrate_kbps: u32,
    pub max_quantizer: u32,
    pub min_quantizer: u32,
    pub target_fps: u32,
    /// Target resolution height for downsampling (0 = use source height)
    pub target_height: u32,
}

/// Network statistics used by the adaptive bitrate controller.
pub struct NetworkStats {
    /// Round-trip time in milliseconds
    pub rtt_ms: f64,
    /// Packet loss percentage (0.0 - 100.0)
    pub packet_loss_pct: f64,
    /// Measured throughput in kbps (real bytes_sent deltas, for stats display).
    ///
    /// Computed from ICETransportStats.bytes_sent over the collection interval.
    /// Reflects actual data rate, which varies with encoding activity.
    /// Not used by ABR, which selects tiers from packet loss instead.
    pub throughput_kbps: u32,
    /// True if any peer connection is using a TURN relay candidate
    pub has_relay: bool,
    /// True only when WebRTC provided a real RTT measurement (not a fallback default)
    pub has_measured_rtt: bool,
}

/// An encoded video packet ready for transmission.
pub struct EncodedPacket {
    /// Encoded bitstream data (codec determined by encoder)
    pub data: Vec<u8>,
    /// Presentation timestamp
    pub pts: u64,
    /// Whether this packet is a keyframe
    pub is_keyframe: bool,
    /// Time spent encoding this frame in nanoseconds
    pub encode_duration_ns: u64,
    /// Original capture timestamp from the source frame
    pub capture_timestamp_ns: u64,
    /// Width of the encoded frame
    pub width: u32,
    /// Height of the encoded frame
    pub height: u32,
}

/// Trait for video encoder backends (VP9, AV1, future HW encoders).
pub trait EncoderBackend: Send {
    /// Initialize or reconfigure the encoder with the given configuration.
    fn configure(&mut self, config: &EncoderConfig) -> Result<(), EncodeError>;

    /// Encode a single captured frame, returning the encoded packet.
    fn encode_frame(&mut self, frame: &capture::Frame) -> Result<EncodedPacket, EncodeError>;

    /// Encode a BGRA slice directly, avoiding frame struct construction.
    /// Default implementation wraps the data in a Frame and calls encode_frame.
    fn encode_bgra_slice(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        let frame = capture::Frame::new(
            bgra_data.to_vec(),
            width,
            height,
            stride,
            capture_timestamp_ns,
            0,
            1.0,
        );
        self.encode_frame(&frame)
    }

    /// Encode a BGRA slice with a pipelined backend that may overlap this
    /// frame's GPU upload with the previous frame's encode. Returns `None` when
    /// no packet is ready yet (the hardware pipeline is still filling); a later
    /// call (or `flush`) returns it. The default is synchronous (always `Some`).
    fn encode_pipelined(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        Ok(Some(self.encode_bgra_slice(
            bgra_data,
            width,
            height,
            stride,
            capture_timestamp_ns,
        )?))
    }

    /// Request the next frame be encoded as a keyframe.
    fn request_keyframe(&mut self) {}

    /// Flush any buffered frames from the encoder.
    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError>;

    /// Update encoder parameters at runtime (for adaptive bitrate).
    fn update_params(&mut self, params: &EncoderParams) -> Result<(), EncodeError>;
}

/// Trait for adaptive bitrate controllers.
pub trait AdaptiveBitrateController: Send {
    /// Process network statistics and return updated encoder parameters.
    fn on_network_stats(&mut self, stats: &NetworkStats) -> EncoderParams;

    /// Get the current encoder parameters.
    fn current_params(&self) -> &EncoderParams;
}

/// Probe available hardware encoders and return the best codec for this system.
///
/// On Linux, checks for VAAPI AV1 first (best efficiency on RDNA 3+ / Intel Arc+),
/// then VP9 VAAPI, falling back to software VP9.
#[cfg(target_os = "linux")]
pub fn detect_best_codec() -> VideoCodec {
    use ffmpeg_next as ffmpeg;
    if ffmpeg::init().is_err() {
        return VideoCodec::VP9;
    }
    if vaapi::is_vaapi_available() {
        for codec in [VideoCodec::AV1, VideoCodec::VP9] {
            if ffmpeg::encoder::find_by_name(codec.vaapi_encoder_name()).is_some() {
                return codec;
            }
        }
    }
    VideoCodec::VP9
}

#[cfg(target_os = "macos")]
pub fn detect_best_codec() -> VideoCodec {
    if videotoolbox::is_av1_available() {
        tracing::info!("VideoToolbox AV1 hardware encoder available");
        return VideoCodec::AV1;
    }
    if videotoolbox::is_h264_available() {
        tracing::info!("VideoToolbox H264 hardware encoder available");
        return VideoCodec::H264;
    }
    VideoCodec::VP9
}

#[cfg(target_os = "windows")]
pub fn detect_best_codec() -> VideoCodec {
    VideoCodec::VP9
}

/// Create the best available encoder for the current platform.
///
/// On Linux, tries VAAPI hardware encoding first, falling back to software VP9.
/// On macOS, tries VideoToolbox AV1 first, falling back to software VP9.
/// On Windows, uses software VP9.
fn create_encoder(config: &EncoderConfig) -> Box<dyn EncoderBackend> {
    #[cfg(target_os = "linux")]
    {
        match vaapi::VaapiEncoder::new(config) {
            Ok(enc) => {
                tracing::info!(
                    encoder = config.codec.vaapi_encoder_name(),
                    "using VAAPI hardware encoder"
                );
                return Box::new(enc);
            }
            Err(e) => {
                tracing::warn!(error = %e, "VAAPI unavailable, falling back to software VP9");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if matches!(config.codec, VideoCodec::AV1 | VideoCodec::H264) {
            match videotoolbox::VideotoolboxEncoder::new(config) {
                Ok(enc) => {
                    tracing::info!(codec = ?config.codec, "using VideoToolbox hardware encoder");
                    return Box::new(enc);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "VideoToolbox unavailable, falling back to software VP9");
                }
            }
        }
    }
    Box::new(VpxEncoder::new(config).expect("VPX encoder init"))
}

/// Spawn a dedicated OS thread for VP9 encoding.
///
/// Mirrors the capture thread pattern: reads frames from the input channel,
/// encodes each with VpxEncoder, and sends packets on the output channel.
/// Returns the packet receiver and a stop signal sender.
///
/// The thread exits when either:
/// - The frame channel closes (capture stopped)
/// - A stop signal is received
/// - The packet receiver is dropped
pub fn spawn_encode_thread(
    config: EncoderConfig,
    frame_rx: std::sync::mpsc::Receiver<capture::Frame>,
    params_rx: Option<tokio::sync::watch::Receiver<EncoderParams>>,
    keyframe_request_rx: Option<std::sync::mpsc::Receiver<()>>,
    has_viewers_rx: Option<tokio::sync::watch::Receiver<bool>>,
    capture_dims_tx: Option<std::sync::mpsc::SyncSender<(u32, u32)>>,
    codec_switch_rx: Option<std::sync::mpsc::Receiver<VideoCodec>>,
) -> Result<
    (
        tokio::sync::mpsc::Receiver<EncodedPacket>,
        std::sync::mpsc::SyncSender<()>,
    ),
    EncodeError,
> {
    let (packet_tx, packet_rx) = tokio::sync::mpsc::channel::<EncodedPacket>(2);
    let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<()>(1);

    std::thread::Builder::new()
        .name("encode-vpx".into())
        .spawn(move || {
            // Defer encoder init until first frame so we know the actual resolution.
            let mut encoder: Option<Box<dyn EncoderBackend>> = None;
            let mut config = config;
            let mut params_rx = params_rx;
            let mut resize_buf = resize::ResizeBuffer::new();
            let mut last_target_height: u32 = 1080;
            let keyframe_request_rx = keyframe_request_rx;
            let has_viewers_rx = has_viewers_rx;
            let mut capture_dims_tx = capture_dims_tx;
            let codec_switch_rx = codec_switch_rx;
            let mut was_paused = false;
            let mut current_target_fps: u32 = config.target_fps;
            // Track last applied params to avoid redundant vpx_codec_enc_config_set
            // calls. tokio watch::has_changed() fires even for identical values,
            // and repeated config_set calls may disturb the VP9 rate controller's
            // internal buffer state.
            let mut last_applied_params: Option<EncoderParams> = None;
            let mut last_frame: Option<capture::Frame> = None;
            let mut next_encode_at = std::time::Instant::now();

            'encode_loop: loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                let has_viewers = if let Some(ref rx) = has_viewers_rx {
                    *rx.borrow()
                } else {
                    true
                };

                if !has_viewers {
                    if !was_paused {
                        tracing::info!("pausing encoding (no viewers)");
                        was_paused = true;
                    }
                    match frame_rx.recv() {
                        Ok(_) => {
                            while frame_rx.try_recv().is_ok() {}
                            continue;
                        }
                        Err(_) => break,
                    }
                }

                if was_paused {
                    tracing::info!("resuming encoding (viewers connected)");
                    if let Some(ref mut enc) = encoder {
                        enc.request_keyframe();
                    }
                    was_paused = false;
                }

                // --- Single deadline-based rate limiter ---
                // Encode exactly once per frame interval. Between deadlines,
                // collect capture frames (keeping the latest). On deadline,
                // encode the latest capture frame or re-encode the last frame
                // (keepalive for static screens). Deadline advances by a fixed
                // interval so OS timer jitter self-corrects.
                let frame_interval =
                    std::time::Duration::from_micros(1_000_000 / current_target_fps.max(1) as u64);
                let now = std::time::Instant::now();

                if now < next_encode_at {
                    // Not time to encode yet, so collect frames until the deadline.
                    let timeout = next_encode_at - now;
                    match frame_rx.recv_timeout(timeout) {
                        Ok(f) => {
                            last_frame = Some(f);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            break 'encode_loop;
                        }
                    }
                    while let Ok(f) = frame_rx.try_recv() {
                        last_frame = Some(f);
                    }
                    // If still before deadline (got a frame early), loop back
                    if std::time::Instant::now() < next_encode_at {
                        continue;
                    }
                }

                // Deadline reached, so grab any last-moment captures.
                while let Ok(f) = frame_rx.try_recv() {
                    last_frame = Some(f);
                }

                // Need at least one frame to encode
                let Some(ref mut frame) = last_frame else {
                    match frame_rx.recv() {
                        Ok(f) => {
                            last_frame = Some(f);
                            next_encode_at = std::time::Instant::now() + frame_interval;
                        }
                        Err(_) => break 'encode_loop,
                    }
                    continue;
                };

                // Advance deadline (self-correcting for OS timer jitter)
                next_encode_at += frame_interval;
                let now = std::time::Instant::now();
                if next_encode_at < now {
                    next_encode_at = now;
                }

                // Skip zero-dimension frames (macOS display transitions)
                if frame.width == 0 || frame.height == 0 {
                    continue;
                }

                // --- Codec switch ---
                if let Some(ref rx) = codec_switch_rx
                    && let Ok(new_codec) = rx.try_recv()
                {
                    tracing::info!(from = ?config.codec, to = ?new_codec, "switching codec");
                    encoder = None;
                    config.codec = new_codec;
                }

                // Lazily init encoder from first frame's actual dimensions
                let enc = match &mut encoder {
                    Some(e) => e,
                    None => {
                        let init_config = EncoderConfig {
                            width: frame.width,
                            height: frame.height,
                            ..config
                        };
                        let init_enc = create_encoder(&init_config);
                        tracing::info!(
                            width = frame.width,
                            height = frame.height,
                            "encoder initialized"
                        );
                        if let Some(tx) = capture_dims_tx.take() {
                            let _ = tx.send((frame.width, frame.height));
                        }
                        encoder = Some(init_enc);
                        encoder.as_mut().unwrap()
                    }
                };

                // Keyframe requests (PLI forwarding)
                if let Some(ref rx) = keyframe_request_rx
                    && rx.try_recv().is_ok()
                {
                    while rx.try_recv().is_ok() {}
                    enc.request_keyframe();
                    tracing::info!("keyframe requested by viewer");
                }

                if let Some(ref mut rx) = params_rx
                    && rx.has_changed().unwrap_or(false)
                {
                    let new_params = rx.borrow_and_update().clone();
                    let actually_changed = last_applied_params.as_ref() != Some(&new_params);
                    if actually_changed {
                        if new_params.target_fps != current_target_fps && new_params.target_fps > 0
                        {
                            tracing::info!(
                                old_fps = current_target_fps,
                                new_fps = new_params.target_fps,
                                "target fps changed"
                            );
                            current_target_fps = new_params.target_fps;
                        }
                        if let Err(e) = enc.update_params(&new_params) {
                            tracing::warn!(error = %e, "failed to update params");
                        }
                        last_applied_params = Some(new_params);
                    }
                }

                // --- Resize + Encode ---
                let current_target_height = if let Some(ref rx) = params_rx {
                    rx.borrow().target_height
                } else {
                    1080
                };
                let effective_target_height = if current_target_height == 0 {
                    1080
                } else {
                    current_target_height
                };
                let resize_target =
                    resize::compute_target(frame.width, frame.height, effective_target_height);

                let encode_result = match resize_target {
                    Some((dst_w, dst_h)) => {
                        if effective_target_height != last_target_height {
                            tracing::info!(
                                width = dst_w,
                                height = dst_h,
                                target_height = effective_target_height,
                                "resize target changed"
                            );
                            last_target_height = effective_target_height;
                        }
                        let resized = resize_buf.resize_bgra(
                            &mut frame.data,
                            frame.width,
                            frame.height,
                            dst_w,
                            dst_h,
                        );
                        enc.encode_pipelined(resized, dst_w, dst_h, dst_w * 4, frame.timestamp_ns)
                    }
                    None => {
                        if effective_target_height != last_target_height {
                            tracing::info!(
                                width = frame.width,
                                height = frame.height,
                                target_height = effective_target_height,
                                "no resize needed"
                            );
                            last_target_height = effective_target_height;
                        }
                        enc.encode_pipelined(
                            &frame.data,
                            frame.width,
                            frame.height,
                            frame.stride,
                            frame.timestamp_ns,
                        )
                    }
                };

                match encode_result {
                    // Pipelined backends return None while the hardware pipeline
                    // fills; the packet arrives on a later tick or at flush.
                    Ok(None) => {}
                    Ok(Some(packet)) => match packet_tx.try_send(packet) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "encode failed");
                    }
                }
            }

            if let Some(ref mut enc) = encoder
                && let Ok(remaining) = enc.flush()
            {
                for packet in remaining {
                    if packet_tx.blocking_send(packet).is_err() {
                        break;
                    }
                }
            }
        })
        .map_err(|e| EncodeError::ThreadSpawnFailed(e.to_string()))?;

    Ok((packet_rx, stop_tx))
}
