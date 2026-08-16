import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { WebRTCManager } from "../connection/WebRTCManager";

/** Best-effort detection of a touch device without a mouse (phone/tablet). */
export function detectMobile(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent || "";
  if (/Android|iPhone|iPad|iPod|Mobile/i.test(ua)) return true;
  // iPadOS reports a Mac UA; fall back to touch + coarse pointer.
  const coarse = typeof matchMedia !== "undefined" &&
    matchMedia("(pointer: coarse)").matches;
  return coarse && (navigator.maxTouchPoints ?? 0) > 1;
}

// US-layout symbols that need a specific key (+ Shift). Letters/digits handled below.
const SYMBOLS: Record<string, [string, boolean]> = {
  " ": ["Space", false],
  "!": ["Digit1", true],
  "@": ["Digit2", true],
  "#": ["Digit3", true],
  "$": ["Digit4", true],
  "%": ["Digit5", true],
  "^": ["Digit6", true],
  "&": ["Digit7", true],
  "*": ["Digit8", true],
  "(": ["Digit9", true],
  ")": ["Digit0", true],
  "-": ["Minus", false],
  "_": ["Minus", true],
  "=": ["Equal", false],
  "+": ["Equal", true],
  "[": ["BracketLeft", false],
  "{": ["BracketLeft", true],
  "]": ["BracketRight", false],
  "}": ["BracketRight", true],
  "\\": ["Backslash", false],
  "|": ["Backslash", true],
  ";": ["Semicolon", false],
  ":": ["Semicolon", true],
  "'": ["Quote", false],
  '"': ["Quote", true],
  "`": ["Backquote", false],
  "~": ["Backquote", true],
  ",": ["Comma", false],
  "<": ["Comma", true],
  ".": ["Period", false],
  ">": ["Period", true],
  "/": ["Slash", false],
  "?": ["Slash", true],
};

/** Map a printable character to a DOM `code` + Shift, US layout. */
function charToKey(ch: string): { code: string; shift: boolean } | null {
  if (ch >= "a" && ch <= "z") {
    return { code: "Key" + ch.toUpperCase(), shift: false };
  }
  if (ch >= "A" && ch <= "Z") return { code: "Key" + ch, shift: true };
  if (ch >= "0" && ch <= "9") return { code: "Digit" + ch, shift: false };
  const s = SYMBOLS[ch];
  return s ? { code: s[0], shift: s[1] } : null;
}

interface UseMobileKeyboardOptions {
  remoteControlMode: boolean;
  managerRef: React.MutableRefObject<WebRTCManager | null>;
  myId: React.MutableRefObject<string>;
}

/**
 * On touch devices, summons the on-screen keyboard by focusing a hidden input
 * and forwards typed text to the host as synthesized key events. Soft keyboards
 * don't emit usable `keydown`/`code`, so we translate `beforeinput` instead.
 *
 * Returns `keyboardSuspended` (driven by the hidden input's focus, not a button
 * toggle, since soft-keyboard visibility can't be observed reliably) so the
 * caller can suspend the window-level key capture while this owns keyboard input.
 */
export function useMobileKeyboard({
  remoteControlMode,
  managerRef,
  myId,
}: UseMobileKeyboardOptions) {
  const isMobile = useMemo(detectMobile, []);
  const inputRef = useRef<HTMLInputElement>(null);
  const [keyboardSuspended, setKeyboardSuspended] = useState(false);

  // Momentary: each tap (re)focuses the hidden input, which summons the soft
  // keyboard. No toggle/active state -- dismissal is handled by the OS keyboard.
  const showKeyboard = useCallback(() => {
    inputRef.current?.focus();
  }, []);

  // Translate the hidden input's text events into key events for the host, and
  // suspend the window key capture only while the input is actually focused.
  useEffect(() => {
    const el = inputRef.current;
    if (!el || !isMobile) return;

    const sendKey = (code: string, pressed: boolean) =>
      managerRef.current?.send({
        t: pressed ? "kd" : "ku",
        k: code,
        id: myId.current,
      });
    const typeChar = (ch: string) => {
      const m = charToKey(ch);
      if (!m) return;
      if (m.shift) sendKey("ShiftLeft", true);
      sendKey(m.code, true);
      sendKey(m.code, false);
      if (m.shift) sendKey("ShiftLeft", false);
    };

    const onBeforeInput = (e: InputEvent) => {
      if (!remoteControlMode) return;
      e.preventDefault(); // keep the field empty; we synthesize keys ourselves
      const it = e.inputType;
      if ((it === "insertText" || it === "insertCompositionText") && e.data) {
        for (const ch of e.data) typeChar(ch);
      } else if (it === "insertLineBreak" || it === "insertParagraph") {
        sendKey("Enter", true);
        sendKey("Enter", false);
      } else if (it.startsWith("delete")) {
        sendKey("Backspace", true);
        sendKey("Backspace", false);
      }
      el.value = "";
    };
    const onFocus = () => setKeyboardSuspended(true);
    const onBlur = () => setKeyboardSuspended(false);

    el.addEventListener("beforeinput", onBeforeInput);
    el.addEventListener("focus", onFocus);
    el.addEventListener("blur", onBlur);
    return () => {
      el.removeEventListener("beforeinput", onBeforeInput);
      el.removeEventListener("focus", onFocus);
      el.removeEventListener("blur", onBlur);
    };
  }, [isMobile, remoteControlMode, managerRef, myId]);

  // Drop focus (dismissing the keyboard) when leaving remote-control mode.
  useEffect(() => {
    if (!remoteControlMode) inputRef.current?.blur();
  }, [remoteControlMode]);

  return { isMobile, inputRef, keyboardSuspended, showKeyboard };
}
