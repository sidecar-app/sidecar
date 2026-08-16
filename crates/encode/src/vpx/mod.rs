//! VP9 encoder backend using libvpx with screen-content optimization.

mod sys;

use crate::colorspace::{self, I420Buffer};
use crate::{EncodeError, EncodedPacket, EncoderBackend, EncoderConfig, EncoderParams};

/// VP9 encoder implementing `EncoderBackend` with screen-content mode and realtime deadline.
///
/// Uses libvpx directly via the safe wrappers in `sys.rs`. Maintains a reusable I420 buffer
/// to avoid per-frame allocations during colorspace conversion.
pub struct VpxEncoder {
    ctx: sys::VpxCodecCtx,
    cfg: sys::EncCfg,
    i420_buf: I420Buffer,
    frame_count: u64,
    width: u32,
    height: u32,
    force_keyframe: bool,
    current_config: EncoderConfig,
    start_time: std::time::Instant,
}

// Safety: VpxEncoder is used exclusively from a single dedicated encode thread.
// The raw pointers in the config buffer are unused in single-pass CBR mode
// and never dereferenced across threads.
unsafe impl Send for VpxEncoder {}

impl VpxEncoder {
    /// Create a new VP9 encoder with the given configuration.
    ///
    /// Width and height are rounded up to even numbers (VP9/I420 requirement).
    pub fn new(config: &EncoderConfig) -> Result<Self, EncodeError> {
        let even_w = (config.width + 1) & !1;
        let even_h = (config.height + 1) & !1;

        let (ctx, cfg) = sys::init_encoder(even_w, even_h, config)?;
        let i420_buf = I420Buffer::new(even_w, even_h);

        Ok(Self {
            ctx,
            cfg,
            i420_buf,
            frame_count: 0,
            width: even_w,
            height: even_h,
            force_keyframe: false,
            current_config: EncoderConfig {
                width: even_w,
                height: even_h,
                ..*config
            },
            start_time: std::time::Instant::now(),
        })
    }
}

impl VpxEncoder {
    /// Request the next frame be encoded as a keyframe.
    /// Used for viewer-initiated keyframe requests and post-reconfigure recovery.
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Encode a BGRA slice directly, without creating a capture::Frame.
    ///
    /// This avoids per-frame heap copies: the caller can pass a borrowed slice
    /// (e.g., from the resize buffer) instead of an owned Vec. The encoder
    /// handles colorspace conversion (BGRA -> I420) and VP9 encoding internally.
    pub fn encode_bgra_slice(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        let start = std::time::Instant::now();

        let even_w = (width + 1) & !1;
        let even_h = (height + 1) & !1;
        if even_w != self.width || even_h != self.height {
            tracing::info!(
                old_width = self.width,
                old_height = self.height,
                new_width = even_w,
                new_height = even_h,
                "resolution changed, reinitializing encoder"
            );
            // Full reinit instead of resize_encoder: libvpx's internal reference
            // frame buffers don't survive dimension changes via config_set, causing
            // dark artifact zones from corrupted inter-frame prediction.
            let new_config = EncoderConfig {
                width: even_w,
                height: even_h,
                ..self.current_config
            };
            let (new_ctx, new_cfg) = sys::init_encoder(even_w, even_h, &new_config)?;
            self.ctx = new_ctx; // Drop old context (calls vpx_codec_destroy)
            self.cfg = new_cfg;
            self.width = even_w;
            self.height = even_h;
            self.i420_buf = I420Buffer::new(even_w, even_h);
            self.force_keyframe = true;
            self.current_config = new_config;
        }

        colorspace::bgra_to_i420(bgra_data, width, height, stride, &mut self.i420_buf)?;

        let pts_ms = self.start_time.elapsed().as_millis() as i64;
        let force_kf = self.force_keyframe;
        if force_kf {
            tracing::debug!("forcing keyframe");
            self.force_keyframe = false;
        }
        let (data, is_keyframe) = sys::encode_frame_raw(
            &mut self.ctx,
            &self.i420_buf.y,
            &self.i420_buf.u,
            &self.i420_buf.v,
            self.width,
            self.height,
            self.i420_buf.y_stride,
            self.i420_buf.uv_stride,
            pts_ms,
            force_kf,
        )?;

        self.frame_count += 1;

        let elapsed = start.elapsed();

        Ok(EncodedPacket {
            data,
            pts: pts_ms as u64,
            is_keyframe,
            encode_duration_ns: elapsed.as_nanos() as u64,
            capture_timestamp_ns,
            width: self.width,
            height: self.height,
        })
    }
}

impl EncoderBackend for VpxEncoder {
    fn encode_bgra_slice(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        VpxEncoder::encode_bgra_slice(self, bgra_data, width, height, stride, capture_timestamp_ns)
    }

    fn request_keyframe(&mut self) {
        VpxEncoder::request_keyframe(self);
    }

    fn configure(&mut self, config: &EncoderConfig) -> Result<(), EncodeError> {
        let even_w = (config.width + 1) & !1;
        let even_h = (config.height + 1) & !1;

        let (ctx, cfg) = sys::init_encoder(even_w, even_h, config)?;
        self.ctx = ctx;
        self.cfg = cfg;
        self.i420_buf = I420Buffer::new(even_w, even_h);
        self.frame_count = 0;
        self.width = even_w;
        self.height = even_h;
        self.force_keyframe = true;
        self.start_time = std::time::Instant::now();
        self.current_config = EncoderConfig {
            width: even_w,
            height: even_h,
            ..*config
        };
        Ok(())
    }

    fn encode_frame(&mut self, frame: &capture::Frame) -> Result<EncodedPacket, EncodeError> {
        let start = std::time::Instant::now();

        let even_w = (frame.width + 1) & !1;
        let even_h = (frame.height + 1) & !1;
        if even_w != self.width || even_h != self.height {
            tracing::info!(
                old_width = self.width,
                old_height = self.height,
                new_width = even_w,
                new_height = even_h,
                "resolution changed, reinitializing encoder"
            );
            let new_config = EncoderConfig {
                width: even_w,
                height: even_h,
                ..self.current_config
            };
            let (new_ctx, new_cfg) = sys::init_encoder(even_w, even_h, &new_config)?;
            self.ctx = new_ctx;
            self.cfg = new_cfg;
            self.width = even_w;
            self.height = even_h;
            self.i420_buf = I420Buffer::new(even_w, even_h);
            self.force_keyframe = true;
            self.current_config = new_config;
        }

        colorspace::bgra_to_i420(
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
            &mut self.i420_buf,
        )?;

        let pts_ms = self.start_time.elapsed().as_millis() as i64;
        let force_kf = self.force_keyframe;
        if force_kf {
            tracing::debug!("forcing keyframe");
            self.force_keyframe = false;
        }
        let (data, is_keyframe) = sys::encode_frame_raw(
            &mut self.ctx,
            &self.i420_buf.y,
            &self.i420_buf.u,
            &self.i420_buf.v,
            self.width,
            self.height,
            self.i420_buf.y_stride,
            self.i420_buf.uv_stride,
            pts_ms,
            force_kf,
        )?;

        self.frame_count += 1;

        let elapsed = start.elapsed();

        Ok(EncodedPacket {
            data,
            pts: pts_ms as u64,
            is_keyframe,
            encode_duration_ns: elapsed.as_nanos() as u64,
            capture_timestamp_ns: frame.timestamp_ns,
            width: self.width,
            height: self.height,
        })
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
        let flushed = sys::flush_encoder(&mut self.ctx)?;
        let packets = flushed
            .into_iter()
            .enumerate()
            .map(|(i, (data, is_keyframe))| EncodedPacket {
                data,
                pts: self.frame_count + i as u64,
                is_keyframe,
                encode_duration_ns: 0,
                capture_timestamp_ns: 0,
                width: self.width,
                height: self.height,
            })
            .collect();
        Ok(packets)
    }

    fn update_params(&mut self, params: &EncoderParams) -> Result<(), EncodeError> {
        self.current_config.target_bitrate_kbps = params.target_bitrate_kbps;
        self.current_config.max_quantizer = params.max_quantizer;
        self.current_config.min_quantizer = params.min_quantizer;
        self.current_config.target_fps = params.target_fps;
        sys::update_config(&mut self.ctx, &mut self.cfg, params)
    }
}
