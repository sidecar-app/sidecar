use smithay_client_toolkit::shell::{
    WaylandSurface,
    wlr_layer::{Anchor, KeyboardInteractivity, Layer},
};
use wayland_client::QueueHandle;

use super::OverlayState;
use super::passthrough;

/// Create a layer surface at the Overlay layer covering the full output.
/// The surface starts in passthrough mode (empty input region).
pub fn create_layer_surface(state: &mut OverlayState, qh: &QueueHandle<OverlayState>) {
    let wl_surface = state.compositor_state.create_surface(qh);

    let layer_surface = state.layer_shell.create_layer_surface(
        qh,
        wl_surface.clone(),
        Layer::Overlay,
        Some("sidecar-overlay"),
        None, // default output
    );

    layer_surface.set_anchor(Anchor::all());

    // Start with no keyboard focus so the terminal TUI keeps it.
    // Switched to OnDemand when draw mode is activated.
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);

    // Let compositor decide size (fullscreen on output)
    layer_surface.set_size(0, 0);

    // Don't reserve space for the overlay
    layer_surface.set_exclusive_zone(-1);

    // Initial commit with no buffer to trigger compositor configure
    layer_surface.commit();

    state.wl_surface = Some(wl_surface);
    state.layer_surface = Some(layer_surface);

    // Start in passthrough mode (clicks pass through to desktop)
    passthrough::set_clickthrough(state, true);

    tracing::info!("Layer surface created at Overlay layer");
}
