//! Tests for configurable signaling URL (INTG-03).
//!
//! Validates that SessionConfig reads SIDECAR_SIGNALING_URL from the
//! environment and falls back to the production default when unset.

use net::{DEFAULT_SIGNALING_URL, SessionConfig};

#[test]
fn signaling_url_env_default_fallback() {
    // INTG-03: SessionConfig reads SIDECAR_SIGNALING_URL env var,
    // falls back to production URL.
    //
    // This test verifies the DEFAULT behavior (no env var set).
    let config = SessionConfig::default();
    assert_eq!(config.signaling_url, DEFAULT_SIGNALING_URL);
}

#[test]
fn signaling_url_env_reads_env_var() {
    // INTG-03: When SIDECAR_SIGNALING_URL is set, SessionConfig
    // reads it instead of using the hardcoded default.

    // SAFETY: set_var is unsafe since Rust 1.66+ due to thread safety concerns.
    // This test uses a unique value unlikely to collide with other tests.
    unsafe {
        std::env::set_var("SIDECAR_SIGNALING_URL", "ws://custom-server:9090");
    }

    let config = SessionConfig::default();
    assert_eq!(config.signaling_url, "ws://custom-server:9090");

    // Clean up to avoid affecting other tests
    unsafe {
        std::env::remove_var("SIDECAR_SIGNALING_URL");
    }
}
