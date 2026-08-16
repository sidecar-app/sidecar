//! Safe wrappers around libvpx FFI calls for VP9 encoding.
//!
//! All `unsafe` FFI interactions are contained here, providing a safe
//! API surface for the VpxEncoder in `mod.rs`.
//!
//! Bindings are generated at build time by `bindgen` from system headers,
//! ensuring struct layouts match the installed libvpx version.

use crate::{EncodeError, EncoderConfig, EncoderParams};
use std::ptr;

// Generated bindings from system libvpx headers (via build.rs)
#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(dead_code)]
#[allow(clippy::all)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/vpx_bindings.rs"));
}

use ffi::*;

/// RAII wrapper around `vpx_codec_ctx_t`. Calls `vpx_codec_destroy` on drop.
pub struct VpxCodecCtx {
    ctx: vpx_codec_ctx_t,
}

// Safety: VpxCodecCtx is used only from a single encode thread.
// libvpx encoder contexts are not thread-safe, but we never share them.
unsafe impl Send for VpxCodecCtx {}

impl Drop for VpxCodecCtx {
    fn drop(&mut self) {
        unsafe {
            vpx_codec_destroy(&mut self.ctx);
        }
    }
}

/// Raw encoder config. Wraps the system `vpx_codec_enc_cfg_t`.
pub struct EncCfg {
    cfg: vpx_codec_enc_cfg_t,
}

/// Initialize a VP9 encoder with the given configuration.
///
/// Returns the codec context and the encoder config (needed for later updates).
/// Width and height MUST be even -- the caller is responsible for rounding.
pub fn init_encoder(
    width: u32,
    height: u32,
    config: &EncoderConfig,
) -> Result<(VpxCodecCtx, EncCfg), EncodeError> {
    unsafe {
        let mut cfg: vpx_codec_enc_cfg_t = std::mem::zeroed();

        let ret = vpx_codec_enc_config_default(vpx_codec_vp9_cx(), &mut cfg, 0);
        if ret != vpx_codec_err_t_VPX_CODEC_OK {
            return Err(EncodeError::InitFailed(format!(
                "vpx_codec_enc_config_default failed: {}",
                ret
            )));
        }

        cfg.g_w = width;
        cfg.g_h = height;
        cfg.g_timebase.num = 1;
        cfg.g_timebase.den = 1000;
        cfg.rc_target_bitrate = config.target_bitrate_kbps;
        cfg.g_lag_in_frames = 0; // no lookahead for realtime
        cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
        cfg.rc_end_usage = vpx_rc_mode_VPX_CBR;
        cfg.kf_mode = vpx_kf_mode_VPX_KF_AUTO;
        cfg.kf_max_dist = if width >= 2560 { 90 } else { 60 };
        cfg.kf_min_dist = 0;
        cfg.rc_undershoot_pct = 95;
        cfg.rc_overshoot_pct = 15;
        cfg.rc_buf_sz = 1000;
        cfg.rc_buf_initial_sz = 600;
        cfg.rc_buf_optimal_sz = 800;
        cfg.rc_min_quantizer = config.min_quantizer;
        cfg.rc_max_quantizer = config.max_quantizer;

        // Scale the thread count with resolution. 4K needs 8+ threads to hit the
        // realtime deadline with row MT and 8 tile columns.
        let threads = if width >= 3840 {
            config.threads.max(8)
        } else if width >= 2560 {
            config.threads.max(6)
        } else {
            config.threads.max(4)
        };
        cfg.g_threads = threads;

        let mut ctx: vpx_codec_ctx_t = std::mem::zeroed();
        let ret = vpx_codec_enc_init_ver(
            &mut ctx,
            vpx_codec_vp9_cx(),
            &cfg,
            0, // flags
            VPX_ENCODER_ABI_VERSION as i32,
        );
        if ret != vpx_codec_err_t_VPX_CODEC_OK {
            return Err(EncodeError::InitFailed(format!(
                "vpx_codec_enc_init_ver failed: {}",
                ret
            )));
        }

        // Screen-content controls set once at init. These survive runtime
        // update_config() and resize_encoder() calls, which only modify the
        // enc_cfg struct via vpx_codec_enc_config_set, they never reset
        // vpx_codec_control_ values. Full re-init via configure() re-applies
        // all controls by calling this function again.

        vpx_codec_control_(
            &mut ctx,
            vp8e_enc_control_id_VP9E_SET_TUNE_CONTENT as i32,
            vp9e_tune_content_VP9E_CONTENT_SCREEN as i32,
        );
        vpx_codec_control_(&mut ctx, vp8e_enc_control_id_VP8E_SET_CPUUSED as i32, 7i32);
        vpx_codec_control_(&mut ctx, vp8e_enc_control_id_VP9E_SET_ROW_MT as i32, 1i32);
        // Disable cyclic refresh AQ at 4K, where overhead scales with pixel count
        // and PLI keyframes handle error recovery. Keep for <= 1440p.
        let aq_mode: i32 = if width >= 2560 { 0 } else { 3 };
        vpx_codec_control_(
            &mut ctx,
            vp8e_enc_control_id_VP9E_SET_AQ_MODE as i32,
            aq_mode,
        );
        vpx_codec_control_(
            &mut ctx,
            vp8e_enc_control_id_VP9E_SET_NOISE_SENSITIVITY as i32,
            0i32,
        );

        // Higher intra cap at 4K so keyframes after PLI/scene change stay sharp
        let max_intra_pct: i32 = if width >= 2560 { 450 } else { 300 };
        vpx_codec_control_(
            &mut ctx,
            vp8e_enc_control_id_VP8E_SET_MAX_INTRA_BITRATE_PCT as i32,
            max_intra_pct,
        );

        // Tile columns for parallelism (log2 value)
        let tile_cols: i32 = if width >= 2560 {
            3
        } else if width >= 1280 {
            2
        } else {
            1
        };
        vpx_codec_control_(
            &mut ctx,
            vp8e_enc_control_id_VP9E_SET_TILE_COLUMNS as i32,
            tile_cols,
        );

        eprintln!(
            "encode-vpx: config {}x{} @ {}kbps, rc=cbr buf=1000ms threads={} tune=screen cpuused=7 row_mt=1 aq={} tiles={} kf_max={} overshoot={}% intra_cap={}%",
            width,
            height,
            config.target_bitrate_kbps,
            threads,
            aq_mode,
            tile_cols,
            cfg.kf_max_dist,
            cfg.rc_overshoot_pct,
            max_intra_pct
        );

        Ok((VpxCodecCtx { ctx }, EncCfg { cfg }))
    }
}

/// Encode a single I420 frame, returning the encoded data and whether it's a keyframe.
///
/// Data is copied immediately from libvpx's internal buffers before returning.
/// Pass `force_keyframe = true` to force this frame as a keyframe (VPX_EFLAG_FORCE_KF).
#[allow(clippy::too_many_arguments)]
pub fn encode_frame_raw(
    codec: &mut VpxCodecCtx,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
    pts: i64,
    force_keyframe: bool,
) -> Result<(Vec<u8>, bool), EncodeError> {
    unsafe {
        let mut raw: vpx_image_t = std::mem::zeroed();
        vpx_img_wrap(
            &mut raw,
            vpx_img_fmt_VPX_IMG_FMT_I420,
            width,
            height,
            1, // alignment
            y.as_ptr() as *mut u8,
        );
        // Set U and V plane pointers and strides (may be > width due to alignment)
        raw.planes[1] = u.as_ptr() as *mut u8;
        raw.planes[2] = v.as_ptr() as *mut u8;
        raw.stride[0] = y_stride as i32;
        raw.stride[1] = uv_stride as i32;
        raw.stride[2] = uv_stride as i32;

        let flags = if force_keyframe {
            VPX_EFLAG_FORCE_KF as vpx_enc_frame_flags_t
        } else {
            0
        };

        let ret = vpx_codec_encode(
            &mut codec.ctx,
            &raw,
            pts as vpx_codec_pts_t,
            1,                    // duration
            flags,                // flags (VPX_EFLAG_FORCE_KF or 0)
            VPX_DL_REALTIME as _, // deadline
        );
        if ret != vpx_codec_err_t_VPX_CODEC_OK {
            return Err(EncodeError::EncodeFailed(format!(
                "vpx_codec_encode failed: {}",
                ret
            )));
        }

        // Collect encoded data -- MUST copy before next encode call
        collect_cx_data(&mut codec.ctx)
    }
}

/// Flush the encoder: signal end of stream and collect remaining packets.
pub fn flush_encoder(codec: &mut VpxCodecCtx) -> Result<Vec<(Vec<u8>, bool)>, EncodeError> {
    unsafe {
        let ret = vpx_codec_encode(&mut codec.ctx, ptr::null(), 0, 1, 0, VPX_DL_REALTIME as _);
        if ret != vpx_codec_err_t_VPX_CODEC_OK {
            return Err(EncodeError::EncodeFailed(format!(
                "vpx_codec_encode (flush) failed: {}",
                ret
            )));
        }

        let mut packets = Vec::new();
        loop {
            match collect_cx_data(&mut codec.ctx) {
                Ok((data, is_kf)) => {
                    if data.is_empty() {
                        break;
                    }
                    packets.push((data, is_kf));
                }
                Err(e) => return Err(e),
            }
            let ret = vpx_codec_encode(&mut codec.ctx, ptr::null(), 0, 1, 0, VPX_DL_REALTIME as _);
            if ret != vpx_codec_err_t_VPX_CODEC_OK {
                break;
            }
        }
        Ok(packets)
    }
}

/// Update encoder configuration at runtime (bitrate, quantizer, timebase) without reinitializing.
pub fn update_config(
    codec: &mut VpxCodecCtx,
    enc_cfg: &mut EncCfg,
    params: &EncoderParams,
) -> Result<(), EncodeError> {
    enc_cfg.cfg.rc_target_bitrate = params.target_bitrate_kbps;
    enc_cfg.cfg.rc_min_quantizer = params.min_quantizer;
    enc_cfg.cfg.rc_max_quantizer = params.max_quantizer;
    // NOTE: g_timebase is intentionally NOT changed here. It is fixed at
    // {1, 1000} (millisecond resolution). FPS changes are handled by the
    // encode thread's frame-skipping logic and by passing real elapsed
    // milliseconds as PTS to vpx_codec_encode. Changing g_timebase at
    // runtime would invalidate the meaning of accumulated PTS values,
    // causing the CBR rate controller to miscalculate elapsed time.

    unsafe {
        let ret = vpx_codec_enc_config_set(&mut codec.ctx, &enc_cfg.cfg);
        if ret != vpx_codec_err_t_VPX_CODEC_OK {
            return Err(EncodeError::EncodeFailed(format!(
                "vpx_codec_enc_config_set failed: {}",
                ret
            )));
        }
    }
    Ok(())
}

/// Collect encoded frame data from the codec's output iterator.
/// Returns (encoded_data, is_keyframe). Empty data means no more packets.
unsafe fn collect_cx_data(ctx: &mut vpx_codec_ctx_t) -> Result<(Vec<u8>, bool), EncodeError> {
    unsafe {
        let mut encoded = Vec::new();
        let mut is_keyframe = false;
        let mut iter: vpx_codec_iter_t = ptr::null();

        loop {
            let pkt = vpx_codec_get_cx_data(ctx, &mut iter);
            if pkt.is_null() {
                break;
            }
            let pkt = &*pkt;
            if pkt.kind == vpx_codec_cx_pkt_kind_VPX_CODEC_CX_FRAME_PKT {
                let frame = &pkt.data.frame;
                let slice = std::slice::from_raw_parts(frame.buf as *const u8, frame.sz);
                encoded.extend_from_slice(slice);
                if (frame.flags & VPX_FRAME_IS_KEY) != 0 {
                    is_keyframe = true;
                }
            }
        }

        Ok((encoded, is_keyframe))
    }
}
