//! VAAPI hardware encoder backend using FFmpeg.
//!
//! Supports VP9 and AV1 hardware encoding via VAAPI on Linux. Falls back
//! gracefully if VAAPI is not available (no GPU, missing drivers, etc.).

use crate::{EncodeError, EncodedPacket, EncoderBackend, EncoderConfig, EncoderParams, VideoCodec};

use ffmpeg::Dictionary;
use ffmpeg::codec;
use ffmpeg::format::Pixel;
use ffmpeg::frame::Video as VideoFrame;
use ffmpeg_next as ffmpeg;

use std::ptr;

/// VAAPI hardware encoder (VP9 or AV1, depending on config).
pub struct VaapiEncoder {
    encoder: codec::encoder::video::Encoder,
    /// Hardware device context (kept alive for the encoder's lifetime)
    _hw_device_ctx: HwDeviceCtx,
    /// Software frame for uploading BGRA data
    sw_frame: VideoFrame,
    /// Hardware frame (VAAPI surface)
    hw_frame: VideoFrame,
    start_time: std::time::Instant,
    #[allow(dead_code)]
    codec: VideoCodec,
    width: u32,
    height: u32,
    force_keyframe: bool,
    current_config: EncoderConfig,
}

// Safety: VaapiEncoder is used exclusively from a single dedicated encode thread.
unsafe impl Send for VaapiEncoder {}

/// Wrapper around AVBufferRef for hardware device context.
struct HwDeviceCtx {
    ptr: *mut ffmpeg::sys::AVBufferRef,
}

impl Drop for HwDeviceCtx {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffmpeg::sys::av_buffer_unref(&mut self.ptr) };
        }
    }
}

// Safety: HwDeviceCtx is only used from the encode thread.
unsafe impl Send for HwDeviceCtx {}

impl VaapiEncoder {
    /// Try to create a VAAPI encoder for the codec specified in config.
    /// Returns Err if VAAPI is not available or the codec is unsupported.
    pub fn new(config: &EncoderConfig) -> Result<Self, EncodeError> {
        ffmpeg::init().map_err(|e| EncodeError::InitFailed(format!("ffmpeg init: {e}")))?;

        // Open VAAPI device
        let hw_device_ctx = open_vaapi_device()?;

        let encoder_name = config.codec.vaapi_encoder_name();
        let codec = ffmpeg::encoder::find_by_name(encoder_name)
            .ok_or_else(|| EncodeError::InitFailed(format!("{encoder_name} encoder not found")))?;

        let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);
        let even_w = (config.width + 1) & !1;
        let even_h = (config.height + 1) & !1;

        unsafe {
            let ctx = context.as_mut_ptr();
            (*ctx).width = even_w as i32;
            (*ctx).height = even_h as i32;
            (*ctx).time_base = ffmpeg::sys::AVRational { num: 1, den: 1000 };
            (*ctx).pix_fmt = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*ctx).bit_rate = (config.target_bitrate_kbps as i64) * 1000;
            (*ctx).rc_max_rate = (config.max_bitrate_kbps as i64) * 1000;
            (*ctx).qmin = config.min_quantizer as i32;
            (*ctx).qmax = config.max_quantizer as i32;
            (*ctx).gop_size = 90;
            (*ctx).max_b_frames = 0; // Low latency
            (*ctx).thread_count = 0; // Let driver decide

            let hw_frames_ref = ffmpeg::sys::av_hwframe_ctx_alloc(hw_device_ctx.ptr);
            if hw_frames_ref.is_null() {
                return Err(EncodeError::InitFailed(
                    "Failed to allocate hw frames context".into(),
                ));
            }
            let frames_ctx = (*hw_frames_ref).data as *mut ffmpeg::sys::AVHWFramesContext;
            (*frames_ctx).format = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*frames_ctx).sw_format = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_NV12;
            (*frames_ctx).width = even_w as i32;
            (*frames_ctx).height = even_h as i32;
            // Pool must hold the in-flight pipeline frames (async_depth) plus the
            // ones still queued for reclaim, so the pipelined path can always
            // check out a fresh surface.
            (*frames_ctx).initial_pool_size = 8;

            let ret = ffmpeg::sys::av_hwframe_ctx_init(hw_frames_ref);
            if ret < 0 {
                ffmpeg::sys::av_buffer_unref(&mut (hw_frames_ref as *mut _));
                return Err(EncodeError::InitFailed(format!(
                    "Failed to init hw frames context: {}",
                    ret
                )));
            }

            (*ctx).hw_frames_ctx = ffmpeg::sys::av_buffer_ref(hw_frames_ref);
            ffmpeg::sys::av_buffer_unref(&mut (hw_frames_ref as *mut _));
        }

        let mut opts = Dictionary::new();
        // async_depth=2 lets the driver keep one frame in flight, so the encode
        // loop's next upload (av_hwframe_transfer_data, copy engine) overlaps the
        // hardware encode of the previous frame (video engine). 1-frame latency.
        opts.set("async_depth", "2");

        let video = context
            .encoder()
            .video()
            .map_err(|e| EncodeError::InitFailed(format!("encoder context: {e}")))?;

        let encoder = video
            .open_with(opts)
            .map_err(|e| EncodeError::InitFailed(format!("avcodec_open2: {e}")))?;

        let sw_frame = VideoFrame::new(Pixel::BGRA, even_w, even_h);
        let mut hw_frame = VideoFrame::empty();
        unsafe {
            let ret = ffmpeg::sys::av_hwframe_get_buffer(
                (*encoder.as_ptr()).hw_frames_ctx,
                hw_frame.as_mut_ptr(),
                0,
            );
            if ret < 0 {
                return Err(EncodeError::InitFailed(format!(
                    "Failed to allocate VAAPI surface: {ret}"
                )));
            }
        }

        tracing::info!(
            width = even_w, height = even_h, codec = ?config.codec,
            bitrate_kbps = config.target_bitrate_kbps,
            min_qp = config.min_quantizer, max_qp = config.max_quantizer,
            "VAAPI encoder initialized"
        );

        Ok(Self {
            encoder,
            _hw_device_ctx: hw_device_ctx,
            sw_frame,
            hw_frame,
            start_time: std::time::Instant::now(),
            codec: config.codec,
            width: even_w,
            height: even_h,
            force_keyframe: false,
            current_config: EncoderConfig {
                width: even_w,
                height: even_h,
                ..*config
            },
        })
    }

    /// Round dimensions up to even and reinitialize the encoder if the capture
    /// resolution changed (VAAPI cannot resize an open encoder).
    fn reinit_if_resolution_changed(&mut self, width: u32, height: u32) -> Result<(), EncodeError> {
        let even_w = (width + 1) & !1;
        let even_h = (height + 1) & !1;
        if even_w != self.width || even_h != self.height {
            tracing::info!(
                old_width = self.width,
                old_height = self.height,
                new_width = even_w,
                new_height = even_h,
                "VAAPI resolution changed, reinitializing"
            );
            let new_config = EncoderConfig {
                width: even_w,
                height: even_h,
                ..self.current_config
            };
            *self = Self::new(&new_config)?;
        }
        Ok(())
    }

    /// Copy a BGRA slice into the reusable software frame, handling row padding.
    fn fill_sw_frame_from_bgra(&mut self, bgra_data: &[u8], width: u32, height: u32, stride: u32) {
        unsafe {
            let dst = (*self.sw_frame.as_mut_ptr()).data[0];
            let dst_stride = (*self.sw_frame.as_mut_ptr()).linesize[0] as u32;
            if dst_stride == stride {
                ptr::copy_nonoverlapping(bgra_data.as_ptr(), dst, (stride * height) as usize);
            } else {
                let copy_width = std::cmp::min(stride, dst_stride) as usize;
                for row in 0..height as usize {
                    ptr::copy_nonoverlapping(
                        bgra_data.as_ptr().add(row * stride as usize),
                        dst.add(row * dst_stride as usize),
                        copy_width,
                    );
                }
            }
            (*self.sw_frame.as_mut_ptr()).width = width as i32;
            (*self.sw_frame.as_mut_ptr()).height = height as i32;
            (*self.sw_frame.as_mut_ptr()).format =
                ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_BGRA as i32;
        }
    }

    /// Stamp the PTS and keyframe/picture-type flags on a hardware frame,
    /// consuming any pending force-keyframe request.
    ///
    /// # Safety
    /// `frame_ptr` must point to a valid `AVFrame` owned by this encoder.
    unsafe fn set_frame_timing(&mut self, frame_ptr: *mut ffmpeg::sys::AVFrame, pts_ms: i64) {
        unsafe {
            (*frame_ptr).pts = pts_ms;
            if self.force_keyframe {
                (*frame_ptr).pict_type = ffmpeg::sys::AVPictureType::AV_PICTURE_TYPE_I;
                (*frame_ptr).flags |= ffmpeg::sys::AV_FRAME_FLAG_KEY;
                tracing::debug!("VAAPI forcing keyframe");
                self.force_keyframe = false;
            } else {
                (*frame_ptr).pict_type = ffmpeg::sys::AVPictureType::AV_PICTURE_TYPE_NONE;
                (*frame_ptr).flags &= !ffmpeg::sys::AV_FRAME_FLAG_KEY;
            }
        }
    }

    /// Copy a received packet's bitstream into an `EncodedPacket`.
    fn build_packet(
        &self,
        pkt: &ffmpeg::Packet,
        pts: u64,
        capture_timestamp_ns: u64,
        encode_duration_ns: u64,
    ) -> EncodedPacket {
        EncodedPacket {
            data: pkt.data().map(|d| d.to_vec()).unwrap_or_default(),
            pts,
            is_keyframe: pkt.is_key(),
            encode_duration_ns,
            capture_timestamp_ns,
            width: self.width,
            height: self.height,
        }
    }

    /// Encode a BGRA slice via VAAPI.
    pub fn encode_bgra_slice(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        let start = std::time::Instant::now();
        self.reinit_if_resolution_changed(width, height)?;
        self.fill_sw_frame_from_bgra(bgra_data, width, height, stride);

        let pts_ms = self.start_time.elapsed().as_millis() as i64;
        let hw_ptr = unsafe { self.hw_frame.as_mut_ptr() };
        unsafe {
            let ret = ffmpeg::sys::av_hwframe_transfer_data(hw_ptr, self.sw_frame.as_ptr(), 0);
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "VAAPI surface upload failed: {ret}"
                )));
            }
            self.set_frame_timing(hw_ptr, pts_ms);
            let ret = ffmpeg::sys::avcodec_send_frame(self.encoder.as_ptr() as *mut _, hw_ptr);
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "avcodec_send_frame failed: {ret}"
                )));
            }
        }

        let mut pkt = ffmpeg::Packet::empty();
        let ret = unsafe {
            let pkt_ptr = &mut pkt as *mut ffmpeg::Packet as *mut ffmpeg::sys::AVPacket;
            ffmpeg::sys::avcodec_receive_packet(self.encoder.as_ptr() as *mut _, pkt_ptr)
        };
        if ret < 0 {
            return Err(EncodeError::EncodeFailed(format!(
                "avcodec_receive_packet failed: {ret}"
            )));
        }
        Ok(self.build_packet(
            &pkt,
            pts_ms as u64,
            capture_timestamp_ns,
            start.elapsed().as_nanos() as u64,
        ))
    }

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Pipelined encode: upload + submit this frame on a fresh pool surface, then
    /// return whatever packet is ready (an earlier in-flight frame) or `None` if
    /// the hardware pipeline is still filling. Overlapping this frame's GPU upload
    /// with the previous frame's encode is what lifts the throughput ceiling that
    /// the fully-synchronous `encode_bgra_slice` imposes.
    pub fn encode_bgra_slice_pipelined(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        // -EAGAIN: the pipeline has not produced a packet for this submission yet.
        // VAAPI is Linux-only, where EAGAIN == 11, so AVERROR(EAGAIN) == -11.
        const AVERROR_EAGAIN: i32 = -11;

        let start = std::time::Instant::now();
        self.reinit_if_resolution_changed(width, height)?;
        self.fill_sw_frame_from_bgra(bgra_data, width, height, stride);

        // Check out a FRESH hardware surface from the pool. In-flight frames must
        // not share a surface, or the next upload corrupts a frame still encoding.
        let mut hw = VideoFrame::empty();
        let hw_ptr = unsafe { hw.as_mut_ptr() };
        let pts_ms = self.start_time.elapsed().as_millis() as i64;
        unsafe {
            let ret = ffmpeg::sys::av_hwframe_get_buffer(
                (*self.encoder.as_ptr()).hw_frames_ctx,
                hw_ptr,
                0,
            );
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "av_hwframe_get_buffer failed: {ret}"
                )));
            }
            let ret = ffmpeg::sys::av_hwframe_transfer_data(hw_ptr, self.sw_frame.as_ptr(), 0);
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "VAAPI surface upload failed: {ret}"
                )));
            }
            self.set_frame_timing(hw_ptr, pts_ms);
            let ret = ffmpeg::sys::avcodec_send_frame(self.encoder.as_ptr() as *mut _, hw.as_ptr());
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "avcodec_send_frame failed: {ret}"
                )));
            }
        }

        // Non-blocking drain: returns the oldest ready packet, or EAGAIN while the
        // pipeline fills. We do NOT block on this frame's own packet -- that is
        // what lets the caller's next upload overlap this frame's encode.
        let mut pkt = ffmpeg::Packet::empty();
        let ret = unsafe {
            let pkt_ptr = &mut pkt as *mut ffmpeg::Packet as *mut ffmpeg::sys::AVPacket;
            ffmpeg::sys::avcodec_receive_packet(self.encoder.as_ptr() as *mut _, pkt_ptr)
        };
        if ret == AVERROR_EAGAIN {
            return Ok(None);
        }
        if ret < 0 {
            return Err(EncodeError::EncodeFailed(format!(
                "avcodec_receive_packet failed: {ret}"
            )));
        }
        let pts = pkt.pts().unwrap_or(pts_ms) as u64;
        Ok(Some(self.build_packet(
            &pkt,
            pts,
            capture_timestamp_ns,
            start.elapsed().as_nanos() as u64,
        )))
    }
}

impl EncoderBackend for VaapiEncoder {
    fn configure(&mut self, config: &EncoderConfig) -> Result<(), EncodeError> {
        *self = Self::new(config)?;
        Ok(())
    }

    fn encode_frame(&mut self, frame: &capture::Frame) -> Result<EncodedPacket, EncodeError> {
        self.encode_bgra_slice(
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
            frame.timestamp_ns,
        )
    }

    fn encode_bgra_slice(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        VaapiEncoder::encode_bgra_slice(
            self,
            bgra_data,
            width,
            height,
            stride,
            capture_timestamp_ns,
        )
    }

    fn encode_pipelined(
        &mut self,
        bgra_data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        capture_timestamp_ns: u64,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        VaapiEncoder::encode_bgra_slice_pipelined(
            self,
            bgra_data,
            width,
            height,
            stride,
            capture_timestamp_ns,
        )
    }

    fn request_keyframe(&mut self) {
        VaapiEncoder::request_keyframe(self);
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncodeError> {
        unsafe {
            ffmpeg::sys::avcodec_send_frame(self.encoder.as_ptr() as *mut _, ptr::null());
        }

        let mut packets = Vec::new();
        loop {
            let mut pkt = ffmpeg::Packet::empty();
            let ret = unsafe {
                let pkt_ptr = &mut pkt as *mut ffmpeg::Packet as *mut ffmpeg::sys::AVPacket;
                ffmpeg::sys::avcodec_receive_packet(self.encoder.as_ptr() as *mut _, pkt_ptr)
            };
            if ret < 0 {
                break;
            }
            let is_keyframe = pkt.is_key();
            let data = pkt.data().map(|d| d.to_vec()).unwrap_or_default();
            let pts = pkt.pts().unwrap_or(0);
            packets.push(EncodedPacket {
                data,
                pts: pts as u64,
                is_keyframe,
                encode_duration_ns: 0,
                capture_timestamp_ns: 0,
                width: self.width,
                height: self.height,
            });
        }
        Ok(packets)
    }

    fn update_params(&mut self, params: &EncoderParams) -> Result<(), EncodeError> {
        self.current_config.target_bitrate_kbps = params.target_bitrate_kbps;
        self.current_config.max_quantizer = params.max_quantizer;
        self.current_config.min_quantizer = params.min_quantizer;
        self.current_config.target_fps = params.target_fps;
        // Note: VAAPI drivers generally ignore runtime bitrate/qp changes.
        // Params are saved to current_config and take effect on next reinit
        // (e.g. resolution change).
        Ok(())
    }
}

/// Check if a VAAPI device is available (without keeping it open).
pub fn is_vaapi_available() -> bool {
    open_vaapi_device().is_ok()
}

fn open_vaapi_device() -> Result<HwDeviceCtx, EncodeError> {
    let devices = ["/dev/dri/renderD128", "/dev/dri/renderD129"];
    let mut last_err = String::new();

    for device in &devices {
        let mut hw_device_ctx: *mut ffmpeg::sys::AVBufferRef = ptr::null_mut();
        let device_cstr =
            std::ffi::CString::new(*device).map_err(|e| EncodeError::InitFailed(e.to_string()))?;
        let ret = unsafe {
            ffmpeg::sys::av_hwdevice_ctx_create(
                &mut hw_device_ctx,
                ffmpeg::sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                device_cstr.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        if ret >= 0 && !hw_device_ctx.is_null() {
            tracing::info!(device = device, "opened VAAPI device");
            return Ok(HwDeviceCtx { ptr: hw_device_ctx });
        }
        last_err = format!("{device}: error {ret}");
    }

    Err(EncodeError::InitFailed(format!(
        "No VAAPI device available ({last_err})"
    )))
}
