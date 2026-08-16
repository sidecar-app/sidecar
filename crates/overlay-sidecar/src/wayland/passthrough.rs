use smithay_client_toolkit::{compositor::Region, shell::wlr_layer::KeyboardInteractivity};

use super::OverlayState;

/// Toggle click-through and keyboard interactivity on the overlay surface.
///
/// When `passthrough` is true (not drawing): empty input region so clicks
/// pass through, and no keyboard interactivity so the terminal keeps focus.
/// When false (drawing): accept all input and request keyboard on demand.
pub fn set_clickthrough(state: &OverlayState, passthrough: bool) {
    let wl_surface = match &state.wl_surface {
        Some(s) => s,
        None => return,
    };

    if passthrough {
        if let Ok(region) = Region::new(&state.compositor_state) {
            wl_surface.set_input_region(Some(region.wl_region()));
        }
    } else {
        wl_surface.set_input_region(None);
    }

    if let Some(ref layer) = state.layer_surface {
        let kb = if passthrough {
            KeyboardInteractivity::None
        } else {
            KeyboardInteractivity::OnDemand
        };
        layer.set_keyboard_interactivity(kb);
    }

    wl_surface.commit();
}
