interface StallOverlayProps {
  stallState: "none" | "stalled" | "lost";
  onReconnect: () => void;
}

export function StallOverlay({ stallState, onReconnect }: StallOverlayProps) {
  if (stallState === "none") return null;

  return (
    <div
      style={{
        position: "absolute",
        top: 8,
        left: 8,
        zIndex: 100,
        color: "#a1a1aa",
        fontSize: "13px",
        background: "rgba(0, 0, 0, 0.5)",
        padding: "4px 8px",
        borderRadius: "8px",
        pointerEvents: stallState === "lost" ? "auto" : "none",
      }}
    >
      {stallState === "stalled"
        ? (
          "Video stalled..."
        )
        : (
          <a
            onClick={onReconnect}
            style={{
              color: "#60a5fa",
              cursor: "pointer",
              textDecoration: "underline",
            }}
          >
            Stream lost — click to reconnect
          </a>
        )}
    </div>
  );
}
