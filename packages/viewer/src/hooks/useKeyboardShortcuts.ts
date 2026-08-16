import { useEffect } from "react";

interface UseKeyboardShortcutsOptions {
  toggleDrawMode: () => void;
  toggleRemoteControlMode: () => void;
  onLeave: () => void;
  setShowStats: React.Dispatch<React.SetStateAction<boolean>>;
}

export function useKeyboardShortcuts({
  toggleDrawMode,
  toggleRemoteControlMode,
  onLeave,
  setShowStats,
}: UseKeyboardShortcutsOptions) {
  // D key toggle draw mode
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      if (e.key === "d" || e.key === "D") {
        if (!e.ctrlKey && !e.metaKey && !e.altKey) {
          e.preventDefault();
          toggleDrawMode();
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [toggleDrawMode]);

  // R key toggle remote-control mode.
  // While remote control is active, useRemoteControl intercepts keys in the
  // capture phase (stopImmediatePropagation), so this only fires to turn it ON.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      if (e.key === "r" || e.key === "R") {
        if (!e.ctrlKey && !e.metaKey && !e.altKey) {
          e.preventDefault();
          toggleRemoteControlMode();
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [toggleRemoteControlMode]);

  // Escape key to leave
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onLeave();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onLeave]);

  // S key toggle stats overlay
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      if (e.key === "s" || e.key === "S") {
        if (!e.ctrlKey && !e.metaKey && !e.altKey) {
          e.preventDefault();
          setShowStats((prev) => !prev);
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [setShowStats]);
}
