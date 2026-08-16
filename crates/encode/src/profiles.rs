//! Encode profile catalog that computes fast-resize resolutions from the host capture size.
//!
//! Only integer-ratio downscales (÷1, ÷2, ÷3, ÷4) use the fast box filter path.
//! Non-integer ratios fall back to Lanczos3 which is ~10x slower and can prevent
//! hitting 60fps. The profile catalog ensures ABR only targets resolutions that
//! produce fast encodes.

/// A resolution the encoder can target efficiently via integer-ratio box filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeProfile {
    pub width: u32,
    pub height: u32,
    /// Integer downscale ratio from source (1 = native, 2 = half, etc.)
    pub scale_ratio: u32,
}

/// Available encode profiles for a given capture resolution, ordered largest to smallest.
#[derive(Debug, Clone)]
pub struct ProfileCatalog {
    pub source_width: u32,
    pub source_height: u32,
    /// Profiles ordered from largest (native) to smallest, all ≥ min_height.
    pub profiles: Vec<EncodeProfile>,
}

/// Minimum encode height. Below this, text and code become unreadable.
const MIN_ENCODE_HEIGHT: u32 = 720;

/// Maximum integer downscale ratio to consider.
const MAX_RATIO: u32 = 4;

impl ProfileCatalog {
    /// Build a catalog of fast encode profiles from the host's capture dimensions.
    ///
    /// Generates native (ratio=1) plus ÷2, ÷3, ÷4 profiles. Each profile has
    /// even dimensions (VP9/I420 requirement) and height ≥ 720p.
    pub fn from_capture(src_w: u32, src_h: u32) -> Self {
        let mut profiles = Vec::with_capacity(MAX_RATIO as usize);

        for ratio in 1..=MAX_RATIO {
            let h = src_h / ratio;
            if h < MIN_ENCODE_HEIGHT {
                break;
            }
            // Preserve aspect ratio, round to even
            let w = (src_w / ratio) & !1;
            let h = h & !1;
            if w < 2 || h < 2 {
                break;
            }
            profiles.push(EncodeProfile {
                width: w,
                height: h,
                scale_ratio: ratio,
            });
        }

        // Ensure at least native is always included even if below min height
        if profiles.is_empty() {
            profiles.push(EncodeProfile {
                width: (src_w + 1) & !1,
                height: (src_h + 1) & !1,
                scale_ratio: 1,
            });
        }

        Self {
            source_width: src_w,
            source_height: src_h,
            profiles,
        }
    }

    /// Find the smallest profile whose height is ≥ the requested viewport height.
    /// If no profile is large enough, returns the largest (native).
    /// If viewport is smaller than the smallest profile, returns the smallest.
    pub fn round_up_to_profile(&self, viewport_height: u32) -> &EncodeProfile {
        // Profiles are largest-to-smallest. Find the last one that's still ≥ viewport.
        self.profiles
            .iter()
            .rev()
            .find(|p| p.height >= viewport_height)
            .unwrap_or(&self.profiles[0])
    }

    /// Get the next smaller profile after the given one. Returns None if already smallest.
    pub fn next_smaller_profile(&self, current: &EncodeProfile) -> Option<&EncodeProfile> {
        self.profiles
            .iter()
            .position(|p| p.height == current.height && p.width == current.width)
            .and_then(|idx| self.profiles.get(idx + 1))
    }

    /// Get the smallest available profile (for last-resort degradation).
    pub fn smallest_profile(&self) -> &EncodeProfile {
        self.profiles.last().unwrap_or(&self.profiles[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_4k_profiles() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        assert_eq!(catalog.profiles.len(), 3); // native, ÷2, ÷3 (÷4=540 < 720)
        assert_eq!(
            catalog.profiles[0],
            EncodeProfile {
                width: 3840,
                height: 2160,
                scale_ratio: 1
            }
        );
        assert_eq!(
            catalog.profiles[1],
            EncodeProfile {
                width: 1920,
                height: 1080,
                scale_ratio: 2
            }
        );
        assert_eq!(
            catalog.profiles[2],
            EncodeProfile {
                width: 1280,
                height: 720,
                scale_ratio: 3
            }
        );
    }

    #[test]
    fn test_5k_profiles() {
        let catalog = ProfileCatalog::from_capture(5120, 2880);
        assert_eq!(catalog.profiles.len(), 4); // native, 1440, 960, 720
        assert_eq!(catalog.profiles[0].height, 2880);
        assert_eq!(catalog.profiles[1].height, 1440);
        assert_eq!(catalog.profiles[2].height, 960);
        assert_eq!(catalog.profiles[3].height, 720);
    }

    #[test]
    fn test_1440p_profiles() {
        let catalog = ProfileCatalog::from_capture(2560, 1440);
        assert_eq!(catalog.profiles.len(), 2); // native, ÷2=720
        assert_eq!(catalog.profiles[0].height, 1440);
        assert_eq!(catalog.profiles[1].height, 720);
    }

    #[test]
    fn test_1080p_profiles() {
        let catalog = ProfileCatalog::from_capture(1920, 1080);
        // ÷2=540 < 720, so only native
        assert_eq!(catalog.profiles.len(), 1);
        assert_eq!(catalog.profiles[0].height, 1080);
    }

    #[test]
    fn test_720p_profiles() {
        let catalog = ProfileCatalog::from_capture(1280, 720);
        assert_eq!(catalog.profiles.len(), 1); // native only
        assert_eq!(catalog.profiles[0].height, 720);
    }

    #[test]
    fn test_small_display_always_has_native() {
        let catalog = ProfileCatalog::from_capture(640, 480);
        assert_eq!(catalog.profiles.len(), 1);
        assert_eq!(catalog.profiles[0].height, 480);
    }

    #[test]
    fn test_round_up_to_profile_exact() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        let p = catalog.round_up_to_profile(1080);
        assert_eq!(p.height, 1080);
    }

    #[test]
    fn test_round_up_to_profile_between() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        // 900px → rounds up to 1080p (smallest profile ≥ 900)
        let p = catalog.round_up_to_profile(900);
        assert_eq!(p.height, 1080);
    }

    #[test]
    fn test_round_up_to_profile_larger_than_native() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        // 4K viewer → gets native
        let p = catalog.round_up_to_profile(2160);
        assert_eq!(p.height, 2160);
    }

    #[test]
    fn test_round_up_to_profile_very_large() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        // 8K viewer on 4K host → gets native (largest available)
        let p = catalog.round_up_to_profile(4320);
        assert_eq!(p.height, 2160);
    }

    #[test]
    fn test_round_up_to_profile_small() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        // 480p viewer → gets 720p (smallest profile)
        let p = catalog.round_up_to_profile(480);
        assert_eq!(p.height, 720);
    }

    #[test]
    fn test_next_smaller_profile() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        let native = &catalog.profiles[0];
        let next = catalog.next_smaller_profile(native);
        assert_eq!(next.unwrap().height, 1080);
    }

    #[test]
    fn test_next_smaller_at_smallest() {
        let catalog = ProfileCatalog::from_capture(3840, 2160);
        let smallest = catalog.smallest_profile();
        assert!(catalog.next_smaller_profile(smallest).is_none());
    }

    #[test]
    fn test_even_dimensions() {
        let catalog = ProfileCatalog::from_capture(2561, 1441);
        for p in &catalog.profiles {
            assert_eq!(p.width % 2, 0, "width {} not even", p.width);
            assert_eq!(p.height % 2, 0, "height {} not even", p.height);
        }
    }

    #[test]
    fn test_ultrawide_4k() {
        // 3440x1440 ultrawide
        let catalog = ProfileCatalog::from_capture(3440, 1440);
        assert_eq!(catalog.profiles.len(), 2); // native, ÷2=720
        assert_eq!(
            catalog.profiles[0],
            EncodeProfile {
                width: 3440,
                height: 1440,
                scale_ratio: 1
            }
        );
        assert_eq!(
            catalog.profiles[1],
            EncodeProfile {
                width: 1720,
                height: 720,
                scale_ratio: 2
            }
        );
    }
}
