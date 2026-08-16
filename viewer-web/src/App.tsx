import { useState } from "react";
import { extractRoomCode, ViewerScreen } from "@sidecar/viewer";
import lockupDark from "./assets/sidecar-lockup-dark.svg";

export default function App() {
  const [initialRoom] = useState(() => {
    const room = new URLSearchParams(window.location.search).get("room") || "";
    if (room) window.history.replaceState({}, "", window.location.pathname);
    return room;
  });
  const [roomCode, setRoomCode] = useState(initialRoom);
  const [joined, setJoined] = useState(!!initialRoom);
  const isValidCode = /^[a-zA-Z]+-[a-zA-Z]+-\d+$/.test(roomCode.trim());

  if (joined) {
    return (
      <ViewerScreen
        roomCode={roomCode}
        onLeave={() => setJoined(false)}
      />
    );
  }

  return (
    <div className="join-screen">
      <a href="https://sidecar.rs" target="_blank" rel="noopener noreferrer">
        <img src={lockupDark} alt="Sidecar" className="lockup" />
      </a>
      <p className="subtitle">Enter a room code to join a session</p>
      <div className="join-code-wrapper">
        <input
          type="text"
          placeholder="Room code"
          value={roomCode}
          onChange={(e) => setRoomCode(extractRoomCode(e.target.value))}
          onKeyDown={(e) => {
            if (e.key === "Enter" && isValidCode) setJoined(true);
          }}
          autoFocus
        />
        <button
          type="button"
          className="join-code-submit"
          onClick={() => setJoined(true)}
          disabled={!isValidCode}
          aria-label="Join session"
          title="Join session"
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
  );
}
