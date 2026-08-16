//! Frame downsampling module for adaptive resolution.
//!
//! Provides pure functions for computing target dimensions and selecting
//! resize filters, plus a reusable buffer for BGRA downsampling using
//! fast_image_resize.

use fast_image_resize as fr;

/// Compute downsample target dimensions, or None if no resize needed.
/// Output dimensions are always even (VP9/I420 requirement).
/// Never upscales. Preserves aspect ratio.
pub fn compute_target(src_width: u32, src_height: u32, target_height: u32) -> Option<(u32, u32)> {
    if src_height <= target_height {
        return None;
    }
    let aspect = src_width as f64 / src_height as f64;
    let dst_h = target_height & !1;
    let dst_w = ((dst_h as f64 * aspect).round() as u32) & !1;
    if dst_w < 2 || dst_h < 2 {
        return None;
    }
    Some((dst_w, dst_h))
}

/// Select resize algorithm: Box for any integer downscale ratio, Lanczos3 otherwise.
///
/// Box filter averages NxN pixel blocks and is equally fast at 2:1, 3:1, or 4:1.
/// This enables fast encode profiles at ÷2, ÷3, ÷4 of the host's capture resolution.
pub fn select_filter(src_height: u32, dst_height: u32) -> fr::ResizeAlg {
    if dst_height > 0 && src_height > dst_height && src_height.is_multiple_of(dst_height) {
        fr::ResizeAlg::Convolution(fr::FilterType::Box)
    } else {
        fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3)
    }
}

/// Pre-allocated resize buffer for the encode thread.
/// Reuses destination image allocation across frames when dimensions are unchanged.
pub struct ResizeBuffer {
    dst_image: Option<fr::images::Image<'static>>,
    resizer: fr::Resizer,
    current_dst_w: u32,
    current_dst_h: u32,
}

impl Default for ResizeBuffer {
    fn default() -> Self {
        Self {
            dst_image: None,
            resizer: fr::Resizer::new(),
            current_dst_w: 0,
            current_dst_h: 0,
        }
    }
}

impl ResizeBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resize BGRA data to target dimensions. Returns slice of resized data.
    /// src_data MUST be packed (stride == width * 4, no padding).
    /// The capture pipeline's strip_row_padding() ensures this.
    ///
    /// Takes `&mut [u8]` to avoid an internal copy -- fast_image_resize's
    /// `Image::from_slice_u8` requires a mutable reference. The source data
    /// is NOT modified; mutability is a library API requirement.
    pub fn resize_bgra(
        &mut self,
        src_data: &mut [u8],
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> &[u8] {
        if dst_w != self.current_dst_w || dst_h != self.current_dst_h {
            self.dst_image = Some(fr::images::Image::new(
                dst_w,
                dst_h,
                fr::pixels::PixelType::U8x4,
            ));
            self.current_dst_w = dst_w;
            self.current_dst_h = dst_h;
        }

        let src_image =
            fr::images::Image::from_slice_u8(src_w, src_h, src_data, fr::pixels::PixelType::U8x4)
                .expect("valid source image");

        let filter = select_filter(src_h, dst_h);
        let dst = self.dst_image.as_mut().unwrap();
        self.resizer
            .resize(
                &src_image,
                dst,
                Some(&fr::ResizeOptions::new().resize_alg(filter)),
            )
            .expect("resize succeeded");

        dst.buffer()
    }

    /// Get the current destination dimensions (0,0 if no resize done yet).
    pub fn current_dims(&self) -> (u32, u32) {
        (self.current_dst_w, self.current_dst_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- compute_target tests ---

    #[test]
    fn test_4k_to_1080() {
        let result = compute_target(3840, 2160, 1080);
        assert_eq!(result, Some((1920, 1080)));
    }

    #[test]
    fn test_1440p_to_1080() {
        // 2560/1440 * 1080 = 1920
        let result = compute_target(2560, 1440, 1080);
        assert_eq!(result, Some((1920, 1080)));
    }

    #[test]
    fn test_ultrawide_1440_to_1080() {
        // 3440/1440 * 1080 = 2580
        let result = compute_target(3440, 1440, 1080);
        assert_eq!(result, Some((2580, 1080)));
    }

    #[test]
    fn test_no_downscale_when_equal() {
        let result = compute_target(1920, 1080, 1080);
        assert_eq!(result, None);
    }

    #[test]
    fn test_no_upscale() {
        let result = compute_target(1280, 720, 1080);
        assert_eq!(result, None);
    }

    #[test]
    fn test_1080_to_480() {
        // 1920/1080 * 480 = 853.33 -> rounds to 853, 853 & !1 = 852 (even)
        let result = compute_target(1920, 1080, 480);
        assert_eq!(result, Some((852, 480)));
    }

    #[test]
    fn test_degenerate_input() {
        let result = compute_target(100, 50, 1080);
        assert_eq!(result, None);
    }

    #[test]
    fn test_even_dimensions() {
        // Verify all outputs are even for a range of inputs
        let test_cases = vec![
            (3840, 2160, 1080),
            (2560, 1440, 1080),
            (3440, 1440, 1080),
            (1920, 1080, 720),
            (1920, 1080, 480),
            (2560, 1440, 720),
        ];
        for (sw, sh, th) in test_cases {
            if let Some((dw, dh)) = compute_target(sw, sh, th) {
                assert_eq!(
                    dw & 1,
                    0,
                    "width {} not even for input ({}, {}, {})",
                    dw,
                    sw,
                    sh,
                    th
                );
                assert_eq!(
                    dh & 1,
                    0,
                    "height {} not even for input ({}, {}, {})",
                    dh,
                    sw,
                    sh,
                    th
                );
            }
        }
    }

    // --- select_filter tests ---

    #[test]
    fn test_box_filter_2to1() {
        let alg = select_filter(2160, 1080);
        assert!(matches!(
            alg,
            fr::ResizeAlg::Convolution(fr::FilterType::Box)
        ));
    }

    #[test]
    fn test_lanczos3_non_integer() {
        let alg = select_filter(1440, 1080);
        assert!(matches!(
            alg,
            fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3)
        ));
    }

    #[test]
    fn test_box_filter_3to1() {
        let alg = select_filter(2160, 720);
        assert!(matches!(
            alg,
            fr::ResizeAlg::Convolution(fr::FilterType::Box)
        ));
    }

    #[test]
    fn test_box_filter_4to1() {
        let alg = select_filter(4320, 1080);
        assert!(matches!(
            alg,
            fr::ResizeAlg::Convolution(fr::FilterType::Box)
        ));
    }

    // --- ResizeBuffer tests ---

    #[test]
    fn test_resize_buffer_new() {
        let buf = ResizeBuffer::new();
        assert_eq!(buf.current_dims(), (0, 0));
    }

    #[test]
    fn test_resize_bgra_output_length() {
        let mut buf = ResizeBuffer::new();
        // 4x4 BGRA input (4*4*4 = 64 bytes)
        let mut src = vec![128u8; 4 * 4 * 4];
        let result = buf.resize_bgra(&mut src, 4, 4, 2, 2);
        assert_eq!(result.len(), 2 * 2 * 4); // 16 bytes
    }

    #[test]
    fn test_resize_bgra_reuses_allocation() {
        let mut buf = ResizeBuffer::new();
        let mut src = vec![128u8; 4 * 4 * 4];

        // First resize
        let _result1 = buf.resize_bgra(&mut src, 4, 4, 2, 2);
        assert_eq!(buf.current_dims(), (2, 2));

        // Second resize with same dimensions should reuse
        let _result2 = buf.resize_bgra(&mut src, 4, 4, 2, 2);
        assert_eq!(buf.current_dims(), (2, 2));
    }
}
