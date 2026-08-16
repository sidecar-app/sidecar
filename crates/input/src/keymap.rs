//! Maps DOM `KeyboardEvent.code` values to Linux evdev keycodes (`KEY_*` from
//! `<linux/input-event-codes.h>`).
//!
//! DOM `code` identifies a *physical* key (layout-independent), which is
//! exactly what the RemoteDesktop portal's `NotifyKeyboardKeycode` expects: we
//! forward the physical key and the host's own keymap produces the character.

/// Returns the evdev keycode for a DOM `KeyboardEvent.code`, or `None` if the
/// key is unmapped (the caller should ignore it).
pub fn dom_code_to_evdev(code: &str) -> Option<i32> {
    let kc = match code {
        // Letters
        "KeyA" => 30,
        "KeyB" => 48,
        "KeyC" => 46,
        "KeyD" => 32,
        "KeyE" => 18,
        "KeyF" => 33,
        "KeyG" => 34,
        "KeyH" => 35,
        "KeyI" => 23,
        "KeyJ" => 36,
        "KeyK" => 37,
        "KeyL" => 38,
        "KeyM" => 50,
        "KeyN" => 49,
        "KeyO" => 24,
        "KeyP" => 25,
        "KeyQ" => 16,
        "KeyR" => 19,
        "KeyS" => 31,
        "KeyT" => 20,
        "KeyU" => 22,
        "KeyV" => 47,
        "KeyW" => 17,
        "KeyX" => 45,
        "KeyY" => 21,
        "KeyZ" => 44,
        // Digits (top row)
        "Digit1" => 2,
        "Digit2" => 3,
        "Digit3" => 4,
        "Digit4" => 5,
        "Digit5" => 6,
        "Digit6" => 7,
        "Digit7" => 8,
        "Digit8" => 9,
        "Digit9" => 10,
        "Digit0" => 11,
        // Whitespace / editing
        "Enter" => 28,
        "Escape" => 1,
        "Backspace" => 14,
        "Tab" => 15,
        "Space" => 57,
        "CapsLock" => 58,
        // Punctuation
        "Minus" => 12,
        "Equal" => 13,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Backslash" => 43,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        // Modifiers
        "ShiftLeft" => 42,
        "ShiftRight" => 54,
        "ControlLeft" => 29,
        "ControlRight" => 97,
        "AltLeft" => 56,
        "AltRight" => 100,
        "MetaLeft" => 125,
        "MetaRight" => 126,
        "ContextMenu" => 127,
        // Navigation
        "ArrowUp" => 103,
        "ArrowDown" => 108,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "Home" => 102,
        "End" => 107,
        "PageUp" => 104,
        "PageDown" => 109,
        "Insert" => 110,
        "Delete" => 111,
        // Function keys
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        // System
        "PrintScreen" => 99,
        "ScrollLock" => 70,
        "Pause" => 119,
        // Numpad
        "NumLock" => 69,
        "NumpadDivide" => 98,
        "NumpadMultiply" => 55,
        "NumpadSubtract" => 74,
        "NumpadAdd" => 78,
        "NumpadEnter" => 96,
        "NumpadDecimal" => 83,
        "Numpad0" => 82,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        _ => return None,
    };
    Some(kc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_keys() {
        assert_eq!(dom_code_to_evdev("KeyA"), Some(30));
        assert_eq!(dom_code_to_evdev("Enter"), Some(28));
        assert_eq!(dom_code_to_evdev("Space"), Some(57));
        assert_eq!(dom_code_to_evdev("Escape"), Some(1));
        assert_eq!(dom_code_to_evdev("Digit0"), Some(11));
        assert_eq!(dom_code_to_evdev("ArrowLeft"), Some(105));
        assert_eq!(dom_code_to_evdev("ControlLeft"), Some(29));
        assert_eq!(dom_code_to_evdev("F12"), Some(88));
        assert_eq!(dom_code_to_evdev("Numpad7"), Some(71));
    }

    #[test]
    fn unmapped_keys_return_none() {
        assert_eq!(dom_code_to_evdev(""), None);
        assert_eq!(dom_code_to_evdev("Unidentified"), None);
        assert_eq!(dom_code_to_evdev("MediaPlayPause"), None);
    }
}
