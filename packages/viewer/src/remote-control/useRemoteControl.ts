import { useCallback, useEffect, useRef } from "react";
import type { WebRTCManager } from "../connection/WebRTCManager";

interface UseRemoteControlOptions {
  remoteControlMode: boolean;
  myColor: string | null;
  dimensions: { width: number; height: number };
  managerRef: React.MutableRefObject<WebRTCManager | null>;
  myId: React.MutableRefObject<string>;
  /** Called when the user presses Escape to stop controlling. */
  onExit: () => void;
  /**
   * When true, the on-screen mobile keyboard owns keyboard input (via a hidden
   * input's `beforeinput`), so the window-level key capture is suspended to
   * avoid double events.
   */
  keyboardSuspended?: boolean;
}

/**
 * Captures the viewer's mouse/scroll/keyboard while in remote-control mode and
 * forwards them to the host. Pointer position rides on the existing `c` cursor
 * packet (also drives the visible cursor); buttons/scroll/keys use dedicated
 * packets the relay forwards to the host only.
 *
 * Mirrors `useDrawing`: the returned handlers are wired to the Konva Stage,
 * while keyboard/scroll/context-menu are captured at the window level so they
 * reach the host instead of triggering browser or viewer shortcuts.
 */
export function useRemoteControl({
  remoteControlMode,
  myColor,
  dimensions,
  managerRef,
  myId,
  onExit,
  keyboardSuspended = false,
}: UseRemoteControlOptions) {
  // deno-lint-ignore no-explicit-any
  const getNormalizedPos = (e: any) => {
    const stage = e.target?.getStage?.();
    const pos = stage?.getPointerPosition();
    if (!pos || dimensions.width === 0 || dimensions.height === 0) return null;
    return { x: pos.x / dimensions.width, y: pos.y / dimensions.height };
  };

  const sendCursor = (x: number, y: number) => {
    managerRef.current?.send({
      t: "c",
      x,
      y,
      id: myId.current,
      c: myColor || undefined,
      m: "c",
    });
  };

  // Track inputs the host currently believes are pressed so we can release them
  // if the corresponding "up" event never arrives (released off-canvas, or the
  // browser loses focus mid-press), which would otherwise leave a button or
  // modifier stuck "down" on the host.
  const heldButtons = useRef<Set<number>>(new Set());
  const heldKeys = useRef<Set<string>>(new Set());

  const releaseButton = useCallback(
    (button: number) => {
      // Emit at most once per button: the Stage onMouseUp and the window-level
      // listener can both fire for the same release.
      if (!heldButtons.current.delete(button)) return;
      managerRef.current?.send({
        t: "mb",
        b: button,
        p: false,
        id: myId.current,
      });
    },
    [managerRef, myId],
  );

  const releaseAllInput = useCallback(() => {
    for (const button of heldButtons.current) {
      managerRef.current?.send({
        t: "mb",
        b: button,
        p: false,
        id: myId.current,
      });
    }
    heldButtons.current.clear();
    for (const code of heldKeys.current) {
      managerRef.current?.send({ t: "ku", k: code, id: myId.current });
    }
    heldKeys.current.clear();
  }, [managerRef, myId]);

  // deno-lint-ignore no-explicit-any
  const handleMouseMove = (e: any) => {
    if (!remoteControlMode) return;
    const pos = getNormalizedPos(e);
    if (pos) sendCursor(pos.x, pos.y);
  };

  // deno-lint-ignore no-explicit-any
  const handleMouseDown = (e: any) => {
    if (!remoteControlMode) return;
    const pos = getNormalizedPos(e);
    if (pos) sendCursor(pos.x, pos.y); // ensure host pointer is in place first
    const button = e.evt?.button ?? 0;
    heldButtons.current.add(button);
    managerRef.current?.send({
      t: "mb",
      b: button,
      p: true,
      id: myId.current,
    });
  };

  // deno-lint-ignore no-explicit-any
  const handleMouseUp = (e: any) => {
    if (!remoteControlMode) return;
    releaseButton(e.evt?.button ?? 0);
  };

  // Announce entering/leaving remote-control mode.
  useEffect(() => {
    if (!remoteControlMode) return;
    managerRef.current?.send({ t: "rc", on: true, id: myId.current });
    return () => {
      managerRef.current?.send({ t: "rc", on: false, id: myId.current });
    };
  }, [remoteControlMode, managerRef, myId]);

  // While controlling, capture keyboard, scroll, and context-menu at the window
  // level (capture phase + stopImmediatePropagation) so they go to the host and
  // never trigger the browser's or viewer's own shortcuts. Escape exits.
  useEffect(() => {
    if (!remoteControlMode) return;

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      managerRef.current?.send({
        t: "ms",
        dx: e.deltaX,
        dy: e.deltaY,
        id: myId.current,
      });
    };
    const onContext = (e: MouseEvent) => e.preventDefault();
    window.addEventListener("wheel", onWheel, {
      passive: false,
      capture: true,
    });
    window.addEventListener("contextmenu", onContext, true);

    // Catch releases the Stage never sees: a button released off the canvas, and
    // (on blur) every key/button still held when the browser loses focus -- the
    // matching "up"/"keyup" never arrives, so without this the host stays stuck.
    const onMouseUpWindow = (e: MouseEvent) => releaseButton(e.button);
    const onBlur = () => releaseAllInput();
    window.addEventListener("mouseup", onMouseUpWindow);
    window.addEventListener("blur", onBlur);

    // Physical-keyboard capture. Suspended while the mobile on-screen keyboard
    // is active (that path forwards keys via a hidden input's beforeinput).
    let onKey: ((e: KeyboardEvent) => void) | null = null;
    if (!keyboardSuspended) {
      onKey = (e: KeyboardEvent) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopImmediatePropagation();
          onExit();
          return;
        }
        e.preventDefault();
        e.stopImmediatePropagation();
        if (e.type === "keydown") heldKeys.current.add(e.code);
        else heldKeys.current.delete(e.code);
        managerRef.current?.send({
          t: e.type === "keydown" ? "kd" : "ku",
          k: e.code,
          id: myId.current,
        });
      };
      window.addEventListener("keydown", onKey, true);
      window.addEventListener("keyup", onKey, true);
    }

    return () => {
      window.removeEventListener("wheel", onWheel, true);
      window.removeEventListener("contextmenu", onContext, true);
      window.removeEventListener("mouseup", onMouseUpWindow);
      window.removeEventListener("blur", onBlur);
      if (onKey) {
        window.removeEventListener("keydown", onKey, true);
        window.removeEventListener("keyup", onKey, true);
      }
      // Leaving remote control: release anything still held so it doesn't stick
      // on the host. (The host also releases on rc-off as a safety net.)
      releaseAllInput();
    };
  }, [
    remoteControlMode,
    keyboardSuspended,
    managerRef,
    onExit,
    releaseButton,
    releaseAllInput,
  ]);

  return { handleMouseDown, handleMouseMove, handleMouseUp };
}
