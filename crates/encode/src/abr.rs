//! Rule-based adaptive bitrate controller.
//!
//! Replaces static degradation tiers with independent control of FPS, quality,
//! and resolution. Bitrate is computed dynamically from the actual encode
//! resolution, not hardcoded per tier. This means a 720p viewer with great
//! bandwidth gets high bitrate (crisp text), not the low bitrate a static
//! "720p tier" would assign.
//!
//! Degradation priority (user's rule):
//! 1. Reduce FPS: 60 → 30  (biggest bandwidth saving)
//! 2. Reduce quality: QP 33 → 40
//! 3. Reduce FPS: 30 → 15
//! 4. Reduce quality: QP 40 → 48
//! 5. Step down resolution (LAST RESORT)

use crate::profiles::ProfileCatalog;
use crate::{AdaptiveBitrateController, EncoderParams, NetworkStats};

/// Quality level applied to quantizer and bitrate scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quality {
    High,   // min_qp=2, max_qp=33, bitrate factor=1.0
    Medium, // min_qp=4, max_qp=40, bitrate factor=0.7
    Low,    // min_qp=8, max_qp=48, bitrate factor=0.45
}

/// A single degradation step in the fixed priority sequence.
#[derive(Debug, Clone, Copy)]
struct DegradationStep {
    fps_index: usize, // index into config.fps_steps
    quality: Quality,
    resolution_stepdown: bool, // true = use one profile smaller than viewer's
}

/// All degradation steps, ordered best (0) to worst.
const STEPS: &[DegradationStep] = &[
    // Level 0: 60fps, high quality
    DegradationStep {
        fps_index: 0,
        quality: Quality::High,
        resolution_stepdown: false,
    },
    // Level 1: 30fps, high quality (FPS first)
    DegradationStep {
        fps_index: 1,
        quality: Quality::High,
        resolution_stepdown: false,
    },
    // Level 2: 30fps, medium quality (then quality)
    DegradationStep {
        fps_index: 1,
        quality: Quality::Medium,
        resolution_stepdown: false,
    },
    // Level 3: 15fps, medium quality (FPS again)
    DegradationStep {
        fps_index: 2,
        quality: Quality::Medium,
        resolution_stepdown: false,
    },
    // Level 4: 15fps, low quality (quality again)
    DegradationStep {
        fps_index: 2,
        quality: Quality::Low,
        resolution_stepdown: false,
    },
    // Level 5: 15fps, low quality, resolution stepdown (LAST RESORT)
    DegradationStep {
        fps_index: 2,
        quality: Quality::Low,
        resolution_stepdown: true,
    },
];

/// Configuration for the rule-based ABR controller.
pub struct AbrConfig {
    /// Maximum bitrate cap in kbps (from env SIDECAR_MAX_BITRATE_KBPS)
    pub max_bitrate_kbps: u32,
    /// Minimum bitrate floor in kbps
    pub min_bitrate_kbps: u32,
    /// Reference bitrate for 1080p30 at high quality (tunable)
    pub base_rate_kbps: u32,
    /// FPS steps for degradation, ordered high to low
    pub fps_steps: Vec<u32>,
    /// Packet loss threshold to trigger degradation (%)
    pub degrade_loss_pct: f64,
    /// Consecutive good samples needed to recover one step
    pub recovery_count: u32,
}

impl Default for AbrConfig {
    fn default() -> Self {
        Self {
            max_bitrate_kbps: 16_000,
            min_bitrate_kbps: 500,
            base_rate_kbps: 4000,
            fps_steps: vec![60, 30, 15],
            degrade_loss_pct: 2.0,
            recovery_count: 3,
        }
    }
}

/// Rule-based ABR controller with dynamic bitrate computation.
pub struct RuleBasedAbrController {
    config: AbrConfig,
    catalog: Option<ProfileCatalog>,
    current: EncoderParams,

    /// Current degradation level (index into STEPS)
    current_level: usize,

    /// Recovery hysteresis counter
    consecutive_good: u32,

    /// External resolution signals
    viewer_viewport_height: Option<u32>,
    turn_relay_active: bool,
    manual_override_height: Option<u32>,
}

impl RuleBasedAbrController {
    pub fn new(config: AbrConfig) -> Self {
        let fps = config.fps_steps.first().copied().unwrap_or(60);
        let current = EncoderParams {
            target_bitrate_kbps: config.max_bitrate_kbps,
            max_quantizer: 33,
            min_quantizer: 10,
            target_fps: fps,
            target_height: 0, // 0 = use source height (no catalog yet)
        };
        Self {
            config,
            catalog: None,
            current,
            current_level: 0,
            consecutive_good: 0,
            viewer_viewport_height: None,
            turn_relay_active: false,
            manual_override_height: None,
        }
    }

    /// Set the host's capture dimensions. Builds the encode profile catalog.
    /// Call this once the first capture frame reveals the actual resolution.
    pub fn set_capture_dimensions(&mut self, width: u32, height: u32) {
        self.catalog = Some(ProfileCatalog::from_capture(width, height));
        self.current = self.compute_params();
    }

    /// Signal TURN relay. Caps resolution to 1080p and FPS to 30 when active.
    pub fn set_turn_relay(&mut self, active: bool) {
        self.turn_relay_active = active;
    }

    /// Manual resolution override (highest priority). None to clear.
    pub fn set_manual_override(&mut self, height: Option<u32>) {
        self.manual_override_height = height;
    }

    /// Set viewer's viewport height for resolution targeting.
    pub fn set_viewer_max_height(&mut self, height: Option<u32>) {
        self.viewer_viewport_height = height;
    }

    /// Compute the target encode resolution based on viewer, catalog, and degradation.
    fn resolve_height(&self) -> u32 {
        let step = &STEPS[self.current_level];

        // Priority 1: Manual override
        if let Some(h) = self.manual_override_height {
            return h.max(720);
        }

        // Priority 2: TURN relay cap
        if self.turn_relay_active {
            return 1080;
        }

        let base_height = match (&self.catalog, self.viewer_viewport_height) {
            (Some(catalog), Some(viewport_h)) => {
                let profile = catalog.round_up_to_profile(viewport_h);
                if step.resolution_stepdown {
                    // Last resort: step down one profile
                    catalog
                        .next_smaller_profile(profile)
                        .map(|p| p.height)
                        .unwrap_or(profile.height)
                } else {
                    profile.height
                }
            }
            (Some(catalog), None) => {
                // No viewer viewport, so use native (or a stepped-down) resolution.
                let native = &catalog.profiles[0];
                if step.resolution_stepdown {
                    catalog
                        .next_smaller_profile(native)
                        .map(|p| p.height)
                        .unwrap_or(native.height)
                } else {
                    native.height
                }
            }
            (None, Some(viewport_h)) => {
                // No catalog yet, so use the viewport as-is.
                if step.resolution_stepdown {
                    (viewport_h * 2 / 3).max(720) // rough stepdown
                } else {
                    viewport_h
                }
            }
            (None, None) => {
                // No catalog and no viewport, so default to 1080p.
                if step.resolution_stepdown { 720 } else { 1080 }
            }
        };

        base_height.max(720)
    }

    /// Compute EncoderParams from the current degradation level.
    fn compute_params(&self) -> EncoderParams {
        let step = &STEPS[self.current_level];
        let mut fps = self
            .config
            .fps_steps
            .get(step.fps_index)
            .copied()
            .unwrap_or(15);
        let height = self.resolve_height();

        if self.turn_relay_active {
            fps = fps.min(30);
        }

        let (min_qp, max_qp, quality_factor) = match step.quality {
            Quality::High => (2, 33, 1.0_f64),
            Quality::Medium => (4, 40, 0.7),
            Quality::Low => (8, 48, 0.45),
        };

        let bitrate = self.compute_bitrate(height, fps, quality_factor);

        EncoderParams {
            target_bitrate_kbps: bitrate,
            max_quantizer: max_qp,
            min_quantizer: min_qp,
            target_fps: fps,
            target_height: height,
        }
    }

    /// Dynamic bitrate: scales with resolution, FPS, and quality level.
    /// A 720p viewer gets proportionally less bitrate than 1080p, but still
    /// good quality rather than the fixed low bitrate a static tier would assign.
    fn compute_bitrate(&self, height: u32, fps: u32, quality_factor: f64) -> u32 {
        // Estimate width from height assuming 16:9 (close enough for bitrate)
        let width = if let Some(ref catalog) = self.catalog {
            catalog
                .profiles
                .iter()
                .find(|p| p.height == height)
                .map(|p| p.width)
                .unwrap_or(height * 16 / 9)
        } else {
            height * 16 / 9
        };

        let pixels = width as f64 * height as f64;
        let reference_pixels = 1920.0 * 1080.0;
        let pixel_ratio = pixels / reference_pixels;
        let fps_ratio = fps as f64 / 30.0;

        let computed = self.config.base_rate_kbps as f64 * pixel_ratio * fps_ratio * quality_factor;
        (computed as u32)
            .max(self.config.min_bitrate_kbps)
            .min(self.config.max_bitrate_kbps)
    }
}

impl AdaptiveBitrateController for RuleBasedAbrController {
    fn on_network_stats(&mut self, stats: &NetworkStats) -> EncoderParams {
        let loss = stats.packet_loss_pct;
        let max_level = STEPS.len() - 1;

        // Degradation: immediate on loss exceeding threshold
        if loss > self.config.degrade_loss_pct && self.current_level < max_level {
            self.current_level += 1;
            self.consecutive_good = 0;
        }
        // Recovery: gradual, one step at a time
        else if loss <= self.config.degrade_loss_pct && self.current_level > 0 {
            self.consecutive_good += 1;
            if self.consecutive_good >= self.config.recovery_count {
                self.current_level -= 1;
                self.consecutive_good = 0;
            }
        } else {
            // At best level with good conditions, or at current with threshold loss
            // Don't reset consecutive_good, since staying at the same level shouldn't penalize recovery.
        }

        self.current = self.compute_params();
        self.current.clone()
    }

    fn current_params(&self) -> &EncoderParams {
        &self.current
    }
}

/// Return encoder parameters for a manual tier override (1-4).
/// Tier 0 is not handled here -- the caller uses it to clear overrides.
/// Used by the Tauri cmd_set_tier command to keep tier values
/// in sync with ABR's quality model.
pub fn tier_params(tier: u32) -> Option<EncoderParams> {
    match tier {
        1 => Some(EncoderParams {
            target_bitrate_kbps: 2000,
            max_quantizer: 40,
            min_quantizer: 10,
            target_fps: 30,
            target_height: 1080,
        }),
        2 => Some(EncoderParams {
            target_bitrate_kbps: 1000,
            max_quantizer: 44,
            min_quantizer: 10,
            target_fps: 15,
            target_height: 1080,
        }),
        3 => Some(EncoderParams {
            target_bitrate_kbps: 600,
            max_quantizer: 48,
            min_quantizer: 10,
            target_fps: 5,
            target_height: 1080,
        }),
        4 => Some(EncoderParams {
            target_bitrate_kbps: 500,
            max_quantizer: 48,
            min_quantizer: 10,
            target_fps: 5,
            target_height: 720,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_loss(loss_pct: f64) -> NetworkStats {
        NetworkStats {
            rtt_ms: 20.0,
            packet_loss_pct: loss_pct,
            throughput_kbps: 0,
            has_relay: false,
            has_measured_rtt: true,
        }
    }

    fn ctrl_with_4k() -> RuleBasedAbrController {
        let mut ctrl = RuleBasedAbrController::new(AbrConfig::default());
        ctrl.set_capture_dimensions(3840, 2160);
        ctrl
    }

    // --- Bitrate computation tests ---

    #[test]
    fn test_no_loss_full_quality() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_fps, 60);
        assert_eq!(p.max_quantizer, 33);
        assert_eq!(p.target_height, 1080);
        // 1080p60 high: 4000 * 1.0 * 2.0 * 1.0 = 8000
        assert_eq!(p.target_bitrate_kbps, 8000);
    }

    #[test]
    fn test_720p_viewer_gets_good_bitrate() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(720));
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 720);
        assert_eq!(p.target_fps, 60);
        // 720p60 high: 4000 * (1280*720)/(1920*1080) * 2.0 * 1.0 = ~3556
        assert!(
            p.target_bitrate_kbps > 3000,
            "720p should get good bitrate: {}",
            p.target_bitrate_kbps
        );
    }

    #[test]
    fn test_native_4k_capped_at_max() {
        let mut ctrl = ctrl_with_4k();
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 2160);
        // 4K60 would need 32000kbps, capped at 16000
        assert_eq!(p.target_bitrate_kbps, 16_000);
    }

    #[test]
    fn test_bitrate_at_boundary() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        let p = ctrl.on_network_stats(&stats_loss(2.0)); // exactly at threshold
        assert_eq!(p.target_fps, 60, "should stay at level 0");
    }

    // --- Degradation sequence tests ---

    #[test]
    fn test_degradation_fps_first() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        let p0 = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p0.target_fps, 60);
        assert_eq!(p0.target_height, 1080);

        // First degradation: FPS 60→30
        let p1 = ctrl.on_network_stats(&stats_loss(5.0));
        assert_eq!(p1.target_fps, 30);
        assert_eq!(p1.max_quantizer, 33, "quality stays high");
        assert_eq!(p1.target_height, 1080, "resolution unchanged");
    }

    #[test]
    fn test_degradation_then_quality() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        ctrl.on_network_stats(&stats_loss(0.0)); // level 0
        ctrl.on_network_stats(&stats_loss(5.0)); // level 1: 30fps high

        // Next: quality drops to medium
        let p = ctrl.on_network_stats(&stats_loss(5.0));
        assert_eq!(p.target_fps, 30);
        assert_eq!(p.max_quantizer, 40, "quality should degrade to medium");
    }

    #[test]
    fn test_full_degradation_sequence() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));

        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(
            (p.target_fps, p.max_quantizer, p.target_height),
            (60, 33, 1080)
        );

        let p = ctrl.on_network_stats(&stats_loss(5.0)); // level 1
        assert_eq!((p.target_fps, p.max_quantizer), (30, 33));

        let p = ctrl.on_network_stats(&stats_loss(5.0)); // level 2
        assert_eq!((p.target_fps, p.max_quantizer), (30, 40));

        let p = ctrl.on_network_stats(&stats_loss(5.0)); // level 3
        assert_eq!((p.target_fps, p.max_quantizer), (15, 40));

        let p = ctrl.on_network_stats(&stats_loss(5.0)); // level 4
        assert_eq!((p.target_fps, p.max_quantizer), (15, 48));

        // Level 5: resolution stepdown (1080 → 720 on 4K catalog)
        let p = ctrl.on_network_stats(&stats_loss(5.0));
        assert_eq!(p.target_fps, 15);
        assert_eq!(p.target_height, 720, "resolution drops as last resort");
    }

    #[test]
    fn test_no_degradation_past_max_level() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        // Degrade to max
        for _ in 0..10 {
            ctrl.on_network_stats(&stats_loss(5.0));
        }
        assert_eq!(ctrl.current_level, STEPS.len() - 1);
    }

    // --- Recovery tests ---

    #[test]
    fn test_recovery_requires_hysteresis() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        ctrl.on_network_stats(&stats_loss(0.0)); // level 0
        ctrl.on_network_stats(&stats_loss(5.0)); // level 1: 30fps
        ctrl.on_network_stats(&stats_loss(5.0)); // level 2: 30fps medium

        // 2 good samples: should hold
        let p1 = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p1.target_fps, 30, "1st good: hold");
        let p2 = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p2.target_fps, 30, "2nd good: hold");

        // 3rd good: recover one step (level 2 → 1)
        let p3 = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p3.target_fps, 30);
        assert_eq!(
            p3.max_quantizer, 33,
            "recovered from medium to high quality"
        );
    }

    #[test]
    fn test_recovery_one_step_at_a_time() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        ctrl.on_network_stats(&stats_loss(0.0));
        // Degrade to level 3 (15fps medium)
        for _ in 0..3 {
            ctrl.on_network_stats(&stats_loss(5.0));
        }
        assert_eq!(ctrl.current_level, 3);

        // 3 good samples recovers ONE step (3 → 2)
        for _ in 0..3 {
            ctrl.on_network_stats(&stats_loss(0.0));
        }
        assert_eq!(ctrl.current_level, 2);

        // 3 more good samples: 2 → 1
        for _ in 0..3 {
            ctrl.on_network_stats(&stats_loss(0.0));
        }
        assert_eq!(ctrl.current_level, 1);
    }

    #[test]
    fn test_same_level_loss_doesnt_reset_recovery() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        ctrl.on_network_stats(&stats_loss(0.0));
        ctrl.on_network_stats(&stats_loss(5.0)); // level 1

        // 2 good samples
        ctrl.on_network_stats(&stats_loss(0.0));
        ctrl.on_network_stats(&stats_loss(0.0));

        // Loss spike at same level should not reset counter
        ctrl.on_network_stats(&stats_loss(2.0)); // at threshold, not above
        // This doesn't trigger degradation (loss <= threshold)

        // Wait, 2.0 <= 2.0 so it's "good". One more good completes recovery.
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(ctrl.current_level, 0, "should have recovered");
        assert_eq!(p.target_fps, 60);
    }

    #[test]
    fn test_degradation_resets_recovery() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(1080));
        ctrl.on_network_stats(&stats_loss(0.0));
        ctrl.on_network_stats(&stats_loss(5.0)); // level 1

        // 2 good samples
        ctrl.on_network_stats(&stats_loss(0.0));
        ctrl.on_network_stats(&stats_loss(0.0));

        // Further degradation resets counter
        ctrl.on_network_stats(&stats_loss(5.0)); // level 2
        assert_eq!(ctrl.current_level, 2);

        // Need fresh 3 good samples
        ctrl.on_network_stats(&stats_loss(0.0));
        ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(ctrl.current_level, 2, "still holding after 2");
        ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(ctrl.current_level, 1, "recovered after 3");
    }

    // --- Resolution signal tests ---

    #[test]
    fn test_turn_relay_caps_1080p30() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_turn_relay(true);
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 1080);
        assert_eq!(p.target_fps, 30);
    }

    #[test]
    fn test_turn_relay_removed() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_turn_relay(true);
        ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(ctrl.current_params().target_height, 1080);

        ctrl.set_turn_relay(false);
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 2160, "should return to native");
        assert_eq!(p.target_fps, 60, "should return to 60fps");
    }

    #[test]
    fn test_manual_override() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_manual_override(Some(1080));
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 1080);
    }

    #[test]
    fn test_manual_override_below_min() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_manual_override(Some(480));
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 720, "clamped to 720 min");
    }

    #[test]
    fn test_manual_override_over_turn() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_turn_relay(true);
        ctrl.set_manual_override(Some(1080));
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 1080, "manual beats TURN");
    }

    #[test]
    fn test_viewer_viewport_rounds_to_profile() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(900));
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        // 4K catalog: [2160, 1080, 720]. 900 rounds up to 1080.
        assert_eq!(p.target_height, 1080);
    }

    #[test]
    fn test_no_catalog_uses_defaults() {
        let mut ctrl = RuleBasedAbrController::new(AbrConfig::default());
        // No set_capture_dimensions called
        let p = ctrl.on_network_stats(&stats_loss(0.0));
        assert_eq!(p.target_height, 1080, "default without catalog");
        assert_eq!(p.target_fps, 60);
    }

    // --- Bitrate independence test ---

    #[test]
    fn test_720p_bitrate_independent_of_resolution_tier() {
        let mut ctrl = ctrl_with_4k();
        ctrl.set_viewer_max_height(Some(720));
        let p720 = ctrl.on_network_stats(&stats_loss(0.0));

        let mut ctrl2 = ctrl_with_4k();
        ctrl2.set_viewer_max_height(Some(1080));
        let p1080 = ctrl2.on_network_stats(&stats_loss(0.0));

        // 720p should get proportionally less bitrate, not a fixed low value
        assert!(
            p720.target_bitrate_kbps > 3000,
            "720p60 should get >3Mbps: {}",
            p720.target_bitrate_kbps
        );
        assert!(
            p720.target_bitrate_kbps < p1080.target_bitrate_kbps,
            "720p bitrate ({}) should be less than 1080p ({})",
            p720.target_bitrate_kbps,
            p1080.target_bitrate_kbps
        );
    }

    #[test]
    fn test_tier_params_valid_tiers() {
        for tier in 1..=4 {
            let params = tier_params(tier);
            assert!(params.is_some(), "tier {} should return Some", tier);
            let p = params.unwrap();
            assert!(p.target_bitrate_kbps > 0);
            assert!(p.target_fps > 0);
            assert!(p.target_height > 0);
        }
    }

    #[test]
    fn test_tier_params_invalid_tier() {
        assert!(tier_params(0).is_none());
        assert!(tier_params(5).is_none());
        assert!(tier_params(99).is_none());
    }

    #[test]
    fn test_tier_params_ordering() {
        // Higher tiers should have lower bitrate
        let t1 = tier_params(1).unwrap();
        let t4 = tier_params(4).unwrap();
        assert!(t1.target_bitrate_kbps > t4.target_bitrate_kbps);
        assert!(t1.target_fps > t4.target_fps);
    }
}
