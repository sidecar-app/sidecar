import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { LinkIcon, PencilIcon, PointerIcon, StopIcon } from "@sidecar/viewer";
import { ToastContainer, useToast } from "./Toast";
import { VIEWER_BASE_URL } from "./types";
import type { LatencyEvent, ViewerCountEvent } from "./types";

const modKey = navigator.platform.includes("Mac") ? "⌘" : "Ctrl+";

interface HostPanelProps {
  roomCode: string;
  onStop: (message?: string) => void;
  overlayActive: boolean;
}

export function HostPanel({ roomCode, onStop, overlayActive }: HostPanelProps) {
  const [viewerCount, setViewerCount] = useState(0);
  const [latency, setLatency] = useState<number | null>(null);
  const [connectionStatus, setConnectionStatus] = useState<
    "connected" | "degraded" | "disconnected"
  >("connected");
  const { toasts, showToast } = useToast();

  // Event listeners (viewer count, latency, session ended)
  useEffect(() => {
    const unlistenViewerCount = listen<ViewerCountEvent>(
      "viewer-count",
      (e) => {
        setViewerCount(e.payload.count);
        if (e.payload.action === "joined") {
          showToast("Viewer joined \u2014 " + e.payload.count + " connected");
        } else if (e.payload.action === "left") {
          showToast("Viewer left \u2014 " + e.payload.count + " connected");
        }
      },
    );

    const unlistenLatency = listen<LatencyEvent>("latency-update", (e) => {
      setLatency(e.payload.has_measured_rtt ? e.payload.rtt_ms : null);
      setConnectionStatus(e.payload.status);
    });

    const unlistenSessionEnded = listen("session-ended", () => {
      onStop();
    });

    const unlistenRoomExpired = listen("room-expired", () => {
      onStop(
        "Your room expired due to inactivity. Start a new session to share again.",
      );
    });

    const unlistenWarning = listen<{ message: string }>(
      "pipeline-warning",
      (e) => {
        showToast(e.payload.message, "warning");
      },
    );

    return () => {
      unlistenViewerCount.then((f) => f());
      unlistenLatency.then((f) => f());
      unlistenSessionEnded.then((f) => f());
      unlistenRoomExpired.then((f) => f());
      unlistenWarning.then((f) => f());
    };
  }, [onStop, showToast]);

  const copyRoomCode = useCallback(() => {
    navigator.clipboard.writeText(roomCode).then(
      () => showToast("Room code copied!"),
      () => showToast("Failed to copy", "warning"),
    );
  }, [roomCode, showToast]);

  const copyRoomLink = useCallback(() => {
    navigator.clipboard.writeText(
      `${VIEWER_BASE_URL}?room=${encodeURIComponent(roomCode)}`,
    ).then(
      () => showToast("Link copied!"),
      () => showToast("Failed to copy", "warning"),
    );
  }, [roomCode, showToast]);

  // Overlay draw mode state (synced from sidecar via events)
  const [overlayDrawMode, setOverlayDrawMode] = useState(false);

  useEffect(() => {
    if (!overlayActive) return;
    const unlisten = listen<boolean>("overlay-draw-mode", (e) => {
      setOverlayDrawMode(e.payload);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [overlayActive]);

  const toggleOverlayDrawMode = useCallback(() => {
    const newMode = !overlayDrawMode;
    invoke("cmd_set_overlay_draw_mode", { drawMode: newMode });
    setOverlayDrawMode(newMode);
  }, [overlayDrawMode]);

  // Session-wide control capability, off by default and synced from the pipeline.
  const [remoteControlEnabled, setRemoteControlEnabled] = useState(false);
  const [remoteControllers, setRemoteControllers] = useState(0);
  const [rcPrompt, setRcPrompt] = useState(false);

  useEffect(() => {
    const unlistenActive = listen<{ enabled: boolean; controllers: number }>(
      "remote-control-active",
      (e) => {
        setRemoteControlEnabled(e.payload.enabled);
        setRemoteControllers(e.payload.controllers);
        if (e.payload.enabled) setRcPrompt(false);
      },
    );
    const unlistenRequested = listen<{ viewer_id: string }>(
      "remote-control-requested",
      () => setRcPrompt(true),
    );
    return () => {
      unlistenActive.then((f) => f());
      unlistenRequested.then((f) => f());
    };
  }, []);

  const toggleRemoteControl = useCallback(() => {
    const next = !remoteControlEnabled;
    invoke("cmd_set_remote_control", { enabled: next });
    setRemoteControlEnabled(next);
  }, [remoteControlEnabled]);

  // Approving a request flips the capability on for everyone in the session.
  const allowRemoteControl = useCallback(() => {
    invoke("cmd_set_remote_control", { enabled: true });
    setRemoteControlEnabled(true);
    setRcPrompt(false);
  }, []);

  const denyRemoteControl = useCallback(() => setRcPrompt(false), []);

  // Draw and control also have a global hotkey (lib.rs), but Wayland delivers no
  // global hotkeys, so handle them here too while the window is focused. On the
  // other platforms the global hotkey grabs the combo first, so no double toggle.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onStop();
        return;
      }
      if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey) {
        if (e.code === "KeyD") {
          e.preventDefault();
          if (overlayActive) toggleOverlayDrawMode();
          return;
        }
        if (e.code === "KeyR") {
          e.preventDefault();
          toggleRemoteControl();
          return;
        }
      }
      const mod = e.metaKey || e.ctrlKey;
      if (!mod || e.shiftKey || e.altKey) return;
      if (e.key === "c" && window.getSelection()?.toString() === "") {
        e.preventDefault();
        copyRoomLink();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [
    copyRoomLink,
    onStop,
    overlayActive,
    toggleOverlayDrawMode,
    toggleRemoteControl,
  ]);

  // A pending request outranks idle/enabled so the decision surfaces first.
  const controlState = rcPrompt
    ? "request"
    : remoteControlEnabled
    ? "enabled"
    : "idle";

  // "connecting" only means something once a viewer has joined and we're waiting
  // on the first RTT; with nobody connected the session is just waiting.
  const latencyLabel = viewerCount === 0
    ? "waiting"
    : connectionStatus === "disconnected"
    ? "disconnected"
    : latency != null
    ? `${Math.round(latency)} ms`
    : "connecting";

  const drawButton = (
    <button
      type="button"
      className={`hp-btn ${overlayDrawMode ? "hp-btn-green" : ""}`}
      onClick={toggleOverlayDrawMode}
      disabled={!overlayActive}
      title="Toggle draw mode (Ctrl+Shift+D)"
    >
      <PencilIcon />
      {overlayDrawMode ? "Drawing" : "Draw"}
    </button>
  );

  return (
    <div className="host-compact-panel">
      <ToastContainer toasts={toasts} />

      <div className="hp-status">
        <span className="hp-live-dot" />
        <span className="hp-live-label">
          {overlayActive ? "Overlay active" : "Sharing"}
        </span>
        <button
          type="button"
          className="hp-status-right"
          onClick={() =>
            invoke("cmd_open_stats_window", { roomCode, viewerCount })}
          title="Connection details"
        >
          <span data-testid="viewer-count">
            {viewerCount} viewer{viewerCount !== 1 ? "s" : ""}
          </span>
          <span className="hp-sep">·</span>
          <span className={`hp-latency ${connectionStatus}`}>
            {latencyLabel}
          </span>
        </button>
      </div>

      <div className="hp-code-field">
        <button
          type="button"
          className="hp-code"
          onClick={copyRoomCode}
          title="Copy room code"
        >
          {roomCode}
        </button>
        <button
          type="button"
          className="hp-code-link"
          onClick={copyRoomLink}
          title={`Copy room link (${modKey}C)`}
        >
          <LinkIcon />
        </button>
      </div>

      {
        /* The control slot morphs in place across idle/request/enabled; Stop
          stays pinned below. */
      }
      <div className="hp-actions">
        {controlState === "request"
          ? (
            /* Draw drops out so the decision card gets the full width. */
            <div className="hp-request-card">
              <div className="hp-request-head">
                <span className="hp-live-dot amber" />
                <span className="hp-request-label">Control request</span>
              </div>
              <p className="hp-request-text">
                A viewer wants to control your screen.
              </p>
              <div className="hp-decision">
                <button
                  type="button"
                  className="hp-btn hp-btn-sm hp-btn-ghost"
                  onClick={denyRemoteControl}
                >
                  Deny
                </button>
                <button
                  type="button"
                  className="hp-btn hp-btn-sm hp-btn-amber"
                  onClick={allowRemoteControl}
                >
                  Allow
                </button>
              </div>
            </div>
          )
          : (
            <div className="hp-action-row">
              {drawButton}
              <button
                type="button"
                className={`hp-btn ${
                  remoteControlEnabled ? "hp-btn-green" : ""
                }`}
                onClick={toggleRemoteControl}
                title={remoteControlEnabled
                  ? "Everyone can control your screen. Click to disable. (Ctrl+Shift+R)"
                  : "Let everyone in the session control your mouse and keyboard (Ctrl+Shift+R)"}
              >
                <PointerIcon />
                {remoteControlEnabled
                  ? remoteControllers > 0
                    ? `Control (${remoteControllers} active)`
                    : "Control enabled"
                  : "Allow control"}
              </button>
            </div>
          )}

        <button
          type="button"
          className="hp-btn hp-btn-danger"
          onClick={() => onStop()}
          title="Stop session (Esc)"
        >
          <StopIcon />
          Stop session
        </button>
      </div>
    </div>
  );
}
