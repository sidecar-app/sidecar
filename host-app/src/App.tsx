import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { openUrl } from "@tauri-apps/plugin-opener";
import { LaunchScreen } from "./LaunchScreen";
import { HostPanel } from "./HostPanel";
import { PermissionSetup } from "./PermissionSetup";
import { VIEWER_BASE_URL } from "./types";
import type { AppMode } from "./types";
import "./App.css";

function App() {
  const [mode, setMode] = useState<AppMode>("launch");
  const [roomCode, setRoomCode] = useState("");
  const [overlayActive, setOverlayActive] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [isStarting, setIsStarting] = useState(false);
  const [permissionGranted, setPermissionGranted] = useState(true);

  useEffect(() => {
    invoke<boolean>("cmd_check_screen_recording").then((granted) => {
      setPermissionGranted(granted);
      if (!granted) setMode("permission-denied");
    });
  }, []);

  // Listen for overlay-state events from Rust backend
  useEffect(() => {
    const unlisten = listen<boolean>("overlay-state", (e) => {
      setOverlayActive(e.payload);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // The host panel wants more room than the launch screen, so size the window
  // per mode rather than locking one size in tauri.conf.
  useEffect(() => {
    const [width, height] = mode === "host" ? [564, 384] : [480, 400];
    getCurrentWindow()
      .setSize(new LogicalSize(width, height))
      .catch(() => {});
  }, [mode]);

  const handleStartHost = async () => {
    try {
      setStartError(null);
      setIsStarting(true);
      const code = await invoke<string>("cmd_start_session");
      setRoomCode(code);
      setMode("host");

      // Create overlay window on the primary display (display_id 0)
      // The overlay spans the captured display automatically
      try {
        await invoke("cmd_create_overlay", { displayId: 0 });
        setOverlayActive(true);
      } catch (e) {
        console.warn("Failed to create overlay:", e);
        setOverlayActive(false);
        // Overlay creation failure is non-fatal -- host can still use in-app drawing
      }
    } catch (e) {
      console.error("Failed to start session:", e);
      // Tauri invoke errors are plain strings, not Error objects
      setIsStarting(false);
      const msg = typeof e === "string"
        ? e
        : e instanceof Error
        ? e.message
        : String(e);
      if (/permission denied/i.test(msg)) {
        setMode("permission-denied");
        return;
      }
      let friendlyMessage: string;
      if (/connection refused|connect/i.test(msg)) {
        friendlyMessage =
          "Could not connect to signaling server. Make sure it is running and try again.";
      } else if (/dns|resolve/i.test(msg)) {
        friendlyMessage =
          "Could not reach the signaling server. Check your network connection.";
      } else if (/timeout/i.test(msg)) {
        friendlyMessage =
          "Connection to signaling server timed out. Try again.";
      } else {
        friendlyMessage = "Failed to start session: " + msg;
      }
      setStartError(friendlyMessage);
    }
  };

  const handleStopHost = async (message?: string) => {
    try {
      await invoke("cmd_stop_session");
    } catch (e) {
      console.error("Failed to stop session:", e);
    }
    setRoomCode("");
    setOverlayActive(false);
    setIsStarting(false);
    if (message) setStartError(message);
    setMode("launch");
  };

  const handleStartViewer = async (code: string) => {
    await openUrl(`${VIEWER_BASE_URL}?room=${encodeURIComponent(code)}`);
  };

  return (
    <div className="app-container">
      {mode === "launch" && (
        <LaunchScreen
          onHost={handleStartHost}
          onViewer={handleStartViewer}
          error={startError}
          clearError={() => setStartError(null)}
          isStarting={isStarting}
        />
      )}
      {mode === "host" && (
        <HostPanel
          roomCode={roomCode}
          onStop={handleStopHost}
          overlayActive={overlayActive}
        />
      )}
      {mode === "permission-denied" && (
        <PermissionSetup
          onBack={permissionGranted ? () => setMode("launch") : undefined}
        />
      )}
    </div>
  );
}

export default App;
