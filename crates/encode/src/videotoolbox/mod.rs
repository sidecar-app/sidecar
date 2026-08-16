//! VideoToolbox hardware encoder backend using FFmpeg.
//!
//! Supports AV1 (M4+ chips, macOS 15+) and H264 (all Apple Silicon)
//! hardware encoding via VideoToolbox on macOS.

use crate::{EncodeError, EncodedPacket, EncoderBackend, EncoderConfig, EncoderParams, VideoCodec};

use ffmpeg::Dictionary;
use ffmpeg::codec;
use ffmpeg::format::Pixel;
use ffmpeg::frame::Video as VideoFrame;
use ffmpeg_next as ffmpeg;

use std::ptr;

/// VideoToolbox hardware encoder (AV1 on M4+, H264 on all Apple Silicon).
pub struct VideotoolboxEncoder {
    encoder: codec::encoder::video::Encoder,
    _hw_device_ctx: HwDeviceCtx,
    /// Software frame for uploading BGRA data
    sw_frame: VideoFrame,
    /// Hardware frame (VideoToolbox surface)
    hw_frame: VideoFrame,
    start_time: std::time::Instant,
    width: u32,
    height: u32,
    force_keyframe: bool,
    current_config: EncoderConfig,
    /// SPS/PPS in Annex B format, prepended to H264 keyframes.
    /// Empty for non-H264 codecs.
    sps_pps_annexb: Vec<u8>,
    /// NALU length field size from AVCC extradata (H264 only, typically 4).
    nalu_length_size: usize,
}

// Safety: VideotoolboxEncoder is used exclusively from a single dedicated encode thread.
unsafe impl Send for VideotoolboxEncoder {}

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

impl VideotoolboxEncoder {
    /// Try to create a VideoToolbox encoder for the codec specified in config.
    pub fn new(config: &EncoderConfig) -> Result<Self, EncodeError> {
        ffmpeg::init().map_err(|e| EncodeError::InitFailed(format!("ffmpeg init: {e}")))?;

        let hw_device_ctx = open_videotoolbox_device()?;

        let encoder_name = config.codec.videotoolbox_encoder_name();
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
            (*ctx).pix_fmt = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
            (*ctx).bit_rate = (config.target_bitrate_kbps as i64) * 1000;
            (*ctx).rc_max_rate = (config.max_bitrate_kbps as i64) * 1000;
            (*ctx).qmin = config.min_quantizer as i32;
            (*ctx).qmax = config.max_quantizer as i32;
            (*ctx).gop_size = if even_w >= 2560 { 90 } else { 60 };
            (*ctx).max_b_frames = 0;
            (*ctx).thread_count = 0;

            if matches!(config.codec, VideoCodec::H264) {
                (*ctx).profile = 100; // FF_PROFILE_H264_HIGH
            }

            let hw_frames_ref = ffmpeg::sys::av_hwframe_ctx_alloc(hw_device_ctx.ptr);
            if hw_frames_ref.is_null() {
                return Err(EncodeError::InitFailed(
                    "Failed to allocate hw frames context".into(),
                ));
            }
            let frames_ctx = (*hw_frames_ref).data as *mut ffmpeg::sys::AVHWFramesContext;
            (*frames_ctx).format = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
            (*frames_ctx).sw_format = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_NV12;
            (*frames_ctx).width = even_w as i32;
            (*frames_ctx).height = even_h as i32;
            (*frames_ctx).initial_pool_size = 4;

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
        opts.set("realtime", "1");
        opts.set("allow_sw", "0"); // Hardware only

        let video = context
            .encoder()
            .video()
            .map_err(|e| EncodeError::InitFailed(format!("encoder context: {e}")))?;

        let encoder = video
            .open_with(opts)
            .map_err(|e| EncodeError::InitFailed(format!("avcodec_open2: {e}")))?;

        // H264: extract SPS/PPS from AVCC extradata for Annex B conversion.
        // WebRTC expects Annex B (start-code NALUs) but VideoToolbox outputs AVCC
        // (length-prefixed NALUs) with SPS/PPS in extradata.
        let (sps_pps_annexb, nalu_length_size) = if matches!(config.codec, VideoCodec::H264) {
            unsafe {
                let ctx = encoder.as_ptr();
                let ed = (*ctx).extradata;
                let ed_size = (*ctx).extradata_size as usize;
                if !ed.is_null() && ed_size > 0 {
                    let data = std::slice::from_raw_parts(ed, ed_size);
                    let (sps_pps, nalu_len) = crate::h264::parse_avcc_extradata(data);
                    tracing::info!(
                        extradata_size = ed_size,
                        sps_pps_bytes = sps_pps.len(),
                        nalu_length_size = nalu_len,
                        "H264 SPS/PPS extracted from extradata"
                    );
                    (sps_pps, nalu_len)
                } else {
                    tracing::warn!("H264 encoder has no extradata, SPS/PPS may be in-band");
                    (Vec::new(), 4)
                }
            }
        } else {
            (Vec::new(), 4)
        };

        // H264: create NV12 sw_frame and convert BGRA→NV12 ourselves.
        // av_hwframe_transfer_data can't do BGRA→NV12 on all hardware generations.
        // AV1: keep the BGRA sw_frame, since the M4 GPU does the conversion during upload.
        let sw_frame = if matches!(config.codec, VideoCodec::H264) {
            VideoFrame::new(Pixel::NV12, even_w, even_h)
        } else {
            VideoFrame::new(Pixel::BGRA, even_w, even_h)
        };
        let mut hw_frame = VideoFrame::empty();
        unsafe {
            let ret = ffmpeg::sys::av_hwframe_get_buffer(
                (*encoder.as_ptr()).hw_frames_ctx,
                hw_frame.as_mut_ptr(),
                0,
            );
            if ret < 0 {
                return Err(EncodeError::InitFailed(format!(
                    "Failed to allocate VideoToolbox surface: {ret}"
                )));
            }
        }

        tracing::info!(
            width = even_w, height = even_h, codec = ?config.codec,
            bitrate_kbps = config.target_bitrate_kbps,
            min_qp = config.min_quantizer, max_qp = config.max_quantizer,
            "VideoToolbox encoder initialized"
        );

        Ok(Self {
            encoder,
            _hw_device_ctx: hw_device_ctx,
            sw_frame,
            hw_frame,
            start_time: std::time::Instant::now(),
            width: even_w,
            height: even_h,
            force_keyframe: false,
            current_config: EncoderConfig {
                width: even_w,
                height: even_h,
                ..*config
            },
            sps_pps_annexb,
            nalu_length_size,
        })
    }

    /// Encode a BGRA slice via VideoToolbox.
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
                "VideoToolbox resolution changed, reinitializing"
            );
            let new_config = EncoderConfig {
                width: even_w,
                height: even_h,
                ..self.current_config
            };
            *self = Self::new(&new_config)?;
        }

        if matches!(self.current_config.codec, VideoCodec::H264) {
            // H264: convert BGRA→NV12 in software, then upload format-matched frame.
            // av_hwframe_transfer_data can't do BGRA→NV12 on all hardware generations.
            unsafe {
                let y_ptr = (*self.sw_frame.as_mut_ptr()).data[0];
                let uv_ptr = (*self.sw_frame.as_mut_ptr()).data[1];
                let y_stride = (*self.sw_frame.as_mut_ptr()).linesize[0] as usize;
                let uv_stride = (*self.sw_frame.as_mut_ptr()).linesize[1] as usize;

                let y_plane =
                    std::slice::from_raw_parts_mut(y_ptr, y_stride * self.height as usize);
                let uv_plane =
                    std::slice::from_raw_parts_mut(uv_ptr, uv_stride * (self.height / 2) as usize);

                crate::colorspace::bgra_to_nv12_planes(
                    bgra_data, width, height, stride, y_plane, uv_plane, y_stride, uv_stride,
                )?;

                (*self.sw_frame.as_mut_ptr()).width = self.width as i32;
                (*self.sw_frame.as_mut_ptr()).height = self.height as i32;
                (*self.sw_frame.as_mut_ptr()).format =
                    ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            }
        } else {
            // AV1: copy BGRA directly, since the GPU handles colorspace conversion during upload.
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

        // GPU upload + colorspace conversion to VideoToolbox surface
        unsafe {
            let ret = ffmpeg::sys::av_hwframe_transfer_data(
                self.hw_frame.as_mut_ptr(),
                self.sw_frame.as_ptr(),
                0,
            );
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "VideoToolbox surface upload failed: {ret}"
                )));
            }
        }

        let pts_ms = self.start_time.elapsed().as_millis() as i64;
        unsafe {
            (*self.hw_frame.as_mut_ptr()).pts = pts_ms;
            if self.force_keyframe {
                (*self.hw_frame.as_mut_ptr()).pict_type =
                    ffmpeg::sys::AVPictureType::AV_PICTURE_TYPE_I;
                (*self.hw_frame.as_mut_ptr()).flags |= ffmpeg::sys::AV_FRAME_FLAG_KEY;
                tracing::debug!("VideoToolbox forcing keyframe");
                self.force_keyframe = false;
            } else {
                (*self.hw_frame.as_mut_ptr()).pict_type =
                    ffmpeg::sys::AVPictureType::AV_PICTURE_TYPE_NONE;
                (*self.hw_frame.as_mut_ptr()).flags &= !ffmpeg::sys::AV_FRAME_FLAG_KEY;
            }
        }

        unsafe {
            let ret = ffmpeg::sys::avcodec_send_frame(
                self.encoder.as_ptr() as *mut _,
                self.hw_frame.as_ptr(),
            );
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "avcodec_send_frame failed: {ret}"
                )));
            }
        }

        let mut pkt = ffmpeg::Packet::empty();
        unsafe {
            let pkt_ptr = &mut pkt as *mut ffmpeg::Packet as *mut ffmpeg::sys::AVPacket;
            let ret = ffmpeg::sys::avcodec_receive_packet(self.encoder.as_ptr() as *mut _, pkt_ptr);
            // EAGAIN (-35 on macOS, -11 on Linux): encoder is buffering frames
            // and needs more input before producing output. Normal for the first
            // frame or two with h264_videotoolbox.
            if ret == -35 || ret == -11 {
                return Err(EncodeError::EncodeFailed("encoder buffering".into()));
            }
            if ret < 0 {
                return Err(EncodeError::EncodeFailed(format!(
                    "avcodec_receive_packet failed: {ret}"
                )));
            }
        }
        let is_keyframe = pkt.is_key();
        let data = self.convert_packet(&pkt);

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

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Convert an H264 packet from AVCC to Annex B format if needed.
    /// Prepends SPS/PPS from extradata on keyframes.
    /// Returns data unchanged for non-H264 codecs.
    fn convert_packet(&self, pkt: &ffmpeg::Packet) -> Vec<u8> {
        if !matches!(self.current_config.codec, VideoCodec::H264) {
            return pkt.data().map(|d| d.to_vec()).unwrap_or_default();
        }
        let raw = pkt.data().unwrap_or(&[]);
        if crate::h264::is_annexb(raw) {
            return raw.to_vec();
        }
        let header = if pkt.is_key() && !self.sps_pps_annexb.is_empty() {
            Some(self.sps_pps_annexb.as_slice())
        } else {
            None
        };
        crate::h264::avcc_to_annexb(raw, self.nalu_length_size, header)
    }
}

impl EncoderBackend for VideotoolboxEncoder {
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
        VideotoolboxEncoder::encode_bgra_slice(
            self,
            bgra_data,
            width,
            height,
            stride,
            capture_timestamp_ns,
        )
    }

    fn request_keyframe(&mut self) {
        VideotoolboxEncoder::request_keyframe(self);
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
            let data = self.convert_packet(&pkt);
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
        // Params saved to current_config; take effect on next reinit (resolution change).
        Ok(())
    }
}

/// Check if VideoToolbox AV1 encoding is available on this hardware.
pub fn is_av1_available() -> bool {
    if ffmpeg::init().is_err() {
        return false;
    }
    if ffmpeg::encoder::find_by_name("av1_videotoolbox").is_none() {
        return false;
    }
    // Probe: actually try to open the encoder to verify hardware support.
    // On M1/M2/M3, avcodec_open2 will fail because the hardware doesn't
    // support AV1 encoding.
    probe_videotoolbox_encoder("av1_videotoolbox")
}

/// Check if VideoToolbox H264 encoding is available on this hardware.
pub fn is_h264_available() -> bool {
    if ffmpeg::init().is_err() {
        return false;
    }
    if ffmpeg::encoder::find_by_name("h264_videotoolbox").is_none() {
        return false;
    }
    probe_videotoolbox_encoder("h264_videotoolbox")
}

/// Probe whether a VideoToolbox encoder can actually be opened on this hardware.
/// Allocates a minimal 64x64 context and attempts avcodec_open2.
fn probe_videotoolbox_encoder(encoder_name: &str) -> bool {
    let hw_device = match open_videotoolbox_device() {
        Ok(ctx) => ctx,
        Err(_) => return false,
    };

    let codec = match ffmpeg::encoder::find_by_name(encoder_name) {
        Some(c) => c,
        None => return false,
    };

    let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);

    unsafe {
        let ctx = context.as_mut_ptr();
        (*ctx).width = 64;
        (*ctx).height = 64;
        (*ctx).time_base = ffmpeg::sys::AVRational { num: 1, den: 30 };
        (*ctx).pix_fmt = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
        (*ctx).bit_rate = 500_000;
        (*ctx).gop_size = 30;
        (*ctx).max_b_frames = 0;

        let hw_frames_ref = ffmpeg::sys::av_hwframe_ctx_alloc(hw_device.ptr);
        if hw_frames_ref.is_null() {
            return false;
        }
        let frames_ctx = (*hw_frames_ref).data as *mut ffmpeg::sys::AVHWFramesContext;
        (*frames_ctx).format = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
        (*frames_ctx).sw_format = ffmpeg::sys::AVPixelFormat::AV_PIX_FMT_NV12;
        (*frames_ctx).width = 64;
        (*frames_ctx).height = 64;
        (*frames_ctx).initial_pool_size = 1;

        if ffmpeg::sys::av_hwframe_ctx_init(hw_frames_ref) < 0 {
            ffmpeg::sys::av_buffer_unref(&mut (hw_frames_ref as *mut _));
            return false;
        }
        (*ctx).hw_frames_ctx = ffmpeg::sys::av_buffer_ref(hw_frames_ref);
        ffmpeg::sys::av_buffer_unref(&mut (hw_frames_ref as *mut _));
    }

    let mut opts = Dictionary::new();
    opts.set("realtime", "1");
    opts.set("allow_sw", "0");

    let video = match context.encoder().video() {
        Ok(v) => v,
        Err(_) => return false,
    };

    video.open_with(opts).is_ok()
}

fn open_videotoolbox_device() -> Result<HwDeviceCtx, EncodeError> {
    let mut hw_device_ctx: *mut ffmpeg::sys::AVBufferRef = ptr::null_mut();
    let ret = unsafe {
        ffmpeg::sys::av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            ffmpeg::sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    if ret < 0 || hw_device_ctx.is_null() {
        return Err(EncodeError::InitFailed(format!(
            "VideoToolbox device not available: error {ret}"
        )));
    }
    tracing::info!("opened VideoToolbox device");
    Ok(HwDeviceCtx { ptr: hw_device_ctx })
}
