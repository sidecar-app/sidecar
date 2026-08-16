use crate::EncodeError;
use dcp::{ColorSpace, ImageFormat, PixelFormat};
use dcv_color_primitives as dcp;

/// Pre-allocated I420 buffer with Y, U, V planes.
///
/// Dimensions are always even (rounded up from source if needed),
/// since VP9 and most video codecs require even dimensions.
/// Strides are padded to 32-byte alignment for SIMD safety.
pub struct I420Buffer {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// Even width (rounded up from source if needed)
    pub width: u32,
    /// Even height (rounded up from source if needed)
    pub height: u32,
    /// Y plane stride (>= width, 32-byte aligned)
    pub y_stride: u32,
    /// UV plane stride (>= width/2, 32-byte aligned)
    pub uv_stride: u32,
}

/// Round up to next multiple of 32 for SIMD alignment safety.
fn align32(v: u32) -> u32 {
    (v + 31) & !31
}

impl I420Buffer {
    /// Create a new I420 buffer with 32-byte aligned strides.
    pub fn new(width: u32, height: u32) -> Self {
        let w = (width + 1) & !1;
        let h = (height + 1) & !1;
        let y_stride = align32(w);
        let uv_stride = align32(w / 2);
        Self {
            y: vec![0u8; (y_stride * h) as usize],
            u: vec![128u8; (uv_stride * (h / 2)) as usize],
            v: vec![128u8; (uv_stride * (h / 2)) as usize],
            width: w,
            height: h,
            y_stride,
            uv_stride,
        }
    }

    /// Resize buffer if the new dimensions (after rounding to even) differ.
    pub fn ensure_size(&mut self, width: u32, height: u32) {
        let w = (width + 1) & !1;
        let h = (height + 1) & !1;
        if w != self.width || h != self.height {
            self.width = w;
            self.height = h;
            self.y_stride = align32(w);
            self.uv_stride = align32(w / 2);
            self.y.resize((self.y_stride * h) as usize, 0);
            self.u.resize((self.uv_stride * (h / 2)) as usize, 0);
            self.v.resize((self.uv_stride * (h / 2)) as usize, 0);
            self.y.fill(0);
            self.u.fill(128);
            self.v.fill(128);
        }
    }
}

/// Convert BGRA directly into pre-allocated NV12 Y and UV planes.
///
/// Used by the VideoToolbox H264 encoder: the hardware frames context
/// expects NV12 software frames, so we convert in software before upload.
/// This avoids relying on `av_hwframe_transfer_data` for colorspace
/// conversion, which is unsupported on some hardware generations.
#[allow(clippy::too_many_arguments)]
pub fn bgra_to_nv12_planes(
    bgra: &[u8],
    width: u32,
    height: u32,
    bgra_stride: u32,
    y_plane: &mut [u8],
    uv_plane: &mut [u8],
    y_stride: usize,
    uv_stride: usize,
) -> Result<(), EncodeError> {
    let conv_width = width & !1;
    let conv_height = height & !1;
    if conv_width < 2 || conv_height < 2 {
        return Err(EncodeError::ColorspaceError(
            "image dimensions too small for NV12 conversion".to_string(),
        ));
    }

    let needed = (bgra_stride as usize) * (conv_height as usize);
    if bgra.len() < needed {
        return Err(EncodeError::ColorspaceError(format!(
            "source buffer too small: {} bytes, need {}",
            bgra.len(),
            needed,
        )));
    }

    let src_format = ImageFormat {
        pixel_format: PixelFormat::Bgra,
        color_space: ColorSpace::Rgb,
        num_planes: 1,
    };
    let dst_format = ImageFormat {
        pixel_format: PixelFormat::Nv12,
        color_space: ColorSpace::Bt601,
        num_planes: 2,
    };

    let src_strides = [bgra_stride as usize];
    let dst_strides = [y_stride, uv_stride];
    let src_buffers = [bgra];
    let mut dst_buffers: [&mut [u8]; 2] = [y_plane, uv_plane];

    dcp::convert_image(
        conv_width,
        conv_height,
        &src_format,
        Some(&src_strides),
        &src_buffers,
        &dst_format,
        Some(&dst_strides),
        &mut dst_buffers,
    )
    .map_err(|e| EncodeError::ColorspaceError(format!("BGRA→NV12 conversion failed: {:?}", e)))
}

/// Convert a BGRA frame to I420 (YUV 4:2:0) using dcv-color-primitives SIMD.
///
/// Buffer strides are 32-byte aligned to prevent SSE2/AVX overruns.
/// Conversion dimensions are rounded down to even for I420 compatibility.
pub fn bgra_to_i420(
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    buf: &mut I420Buffer,
) -> Result<(), EncodeError> {
    buf.ensure_size(width, height);

    // Round down to even for I420 (must be at least 2)
    let conv_width = width & !1;
    let conv_height = height & !1;

    if conv_width < 2 || conv_height < 2 {
        return Err(EncodeError::ColorspaceError(
            "image dimensions too small for conversion".to_string(),
        ));
    }

    let needed = (stride as usize) * (conv_height as usize);
    if bgra.len() < needed {
        return Err(EncodeError::ColorspaceError(format!(
            "source buffer too small: {} bytes, need {} ({}x{} stride={})",
            bgra.len(),
            needed,
            conv_width,
            conv_height,
            stride
        )));
    }

    let src_format = ImageFormat {
        pixel_format: PixelFormat::Bgra,
        color_space: ColorSpace::Rgb,
        num_planes: 1,
    };

    let dst_format = ImageFormat {
        pixel_format: PixelFormat::I420,
        color_space: ColorSpace::Bt601,
        num_planes: 3,
    };

    // Source stride from the frame (may include PipeWire padding)
    let src_strides = [stride as usize];

    // Destination strides are 32-byte aligned, giving SIMD routines
    // safe headroom to write in 16/32-byte blocks without overrunning
    let dst_strides = [
        buf.y_stride as usize,
        buf.uv_stride as usize,
        buf.uv_stride as usize,
    ];

    let src_buffers = [bgra];
    let (y_buf, u_buf, v_buf) = (&mut buf.y[..], &mut buf.u[..], &mut buf.v[..]);
    let mut dst_buffers: [&mut [u8]; 3] = [y_buf, u_buf, v_buf];

    dcp::convert_image(
        conv_width,
        conv_height,
        &src_format,
        Some(&src_strides),
        &src_buffers,
        &dst_format,
        Some(&dst_strides),
        &mut dst_buffers,
    )
    .map_err(|e| EncodeError::ColorspaceError(format!("dcv conversion failed: {:?}", e)))
}
