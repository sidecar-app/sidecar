interface HostLeftOverlayProps {
  visible: boolean;
  onLeave: () => void;
}

export function HostLeftOverlay({ visible, onLeave }: HostLeftOverlayProps) {
  if (!visible) return null;

  return (
    <div
      style={{
        position: "absolute",
        inset: 0,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        background: "rgba(0, 0, 0, 0.7)",
        zIndex: 200,
      }}
    >
      <p style={{ fontSize: "20px", fontWeight: 600, marginBottom: "16px" }}>
        Host left the session
      </p>
      <button
        type="button"
        onClick={onLeave}
        style={{
          padding: "10px 24px",
          background: "var(--accent)",
          color: "white",
          border: "none",
          borderRadius: "8px",
          fontSize: "15px",
          fontWeight: 600,
          cursor: "pointer",
        }}
      >
        Back
      </button>
    </div>
  );
}
