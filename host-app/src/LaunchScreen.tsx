import { useState } from "react";
import { extractRoomCode } from "@sidecar/viewer";
import lockupDark from "./assets/sidecar-lockup-dark.svg";

interface LaunchScreenProps {
  onHost: () => void;
  onViewer: (roomCode: string) => void;
  error?: string | null;
  clearError?: () => void;
  isStarting?: boolean;
}

export function LaunchScreen(
  { onHost, onViewer, error, clearError, isStarting }: LaunchScreenProps,
) {
  const [roomCode, setRoomCode] = useState("");
  const isValidCode = /^[a-zA-Z]+-[a-zA-Z]+-\d+$/.test(roomCode.trim());

  return (
    <div className="launch-screen">
      <img src={lockupDark} alt="Sidecar" className="launch-lockup" />
      <p className="launch-subtitle">Low-latency peer-to-peer screen sharing</p>
      {error && (
        <div className="launch-error">
          <span className="launch-error-text">{error}</span>
          <button
            type="button"
            className="launch-error-dismiss"
            onClick={clearError}
            aria-label="Dismiss error"
          >
            &times;
          </button>
        </div>
      )}
      <button
        type="button"
        className="launch-btn primary"
        onClick={onHost}
        disabled={isStarting}
      >
        {isStarting ? "Starting..." : "Start session"}
      </button>
      <div className="launch-join-row">
        <div className="launch-code-wrapper">
          <input
            value={roomCode}
            onChange={(e) => setRoomCode(extractRoomCode(e.target.value))}
            placeholder="Join session"
            onKeyDown={(e) => {
              if (e.key === "Enter" && isValidCode) onViewer(roomCode.trim());
            }}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
          />
          <button
            type="button"
            className="launch-code-submit"
            onClick={() => onViewer(roomCode.trim())}
            disabled={!isValidCode}
            aria-label="Join room"
          >
            <svg
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M5 12h14" />
              <path d="m12 5 7 7-7 7" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
