import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { Layer, Line, Stage } from "react-konva";
import { v4 as uuidv4 } from "uuid";
import type { DataPacket } from "@sidecar/viewer";
import {
  DEFAULT_STROKE_WIDTH,
  DrawSettingsPopover,
  HOST_DEFAULT_COLOR,
  PencilIcon,
  TrashIcon,
  useFadeLoop,
  useOutsideClick,
} from "@sidecar/viewer";

interface LineData {
  id: string;
  senderId: string;
  points: number[];
  color: string;
  width: number;
  persist?: boolean;
  finished?: boolean;
}

export function OverlayWindow() {
  const [drawMode, setDrawMode] = useState(false);
  const [lines, setLines] = useState<LineData[]>([]);
  const [strokeWidth, setStrokeWidth] = useState(DEFAULT_STROKE_WIDTH);
  const [myColor, setMyColor] = useState(HOST_DEFAULT_COLOR);
  const [showDrawSettings, setShowDrawSettings] = useState(false);
  const drawSettingsRef = useRef<HTMLDivElement>(null);
  const [dimensions, setDimensions] = useState({
    width: window.innerWidth,
    height: window.innerHeight,
  });

  const isDrawing = useRef(false);
  const currentLineId = useRef<string | null>(null);
  const hostId = useRef(uuidv4());
  // deno-lint-ignore no-explicit-any
  const lineRefs = useRef<Map<string, any>>(new Map());
  const { startFade, fadingRef } = useFadeLoop(setLines, lineRefs, 1800);
  const currentPersist = useRef(false);

  // Ensure transparent background for overlay window
  useEffect(() => {
    document.body.style.background = "transparent";
    document.documentElement.style.background = "transparent";
    return () => {
      document.body.style.background = "";
      document.documentElement.style.background = "";
    };
  }, []);

  // Update dimensions on resize
  useEffect(() => {
    const handleResize = () => {
      setDimensions({ width: window.innerWidth, height: window.innerHeight });
    };
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  // Listen for draw mode toggle from global shortcut (Ctrl+Shift+D)
  useEffect(() => {
    const unlisten = listen<boolean>("draw-mode-changed", (e) => {
      setDrawMode(e.payload);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const closeDrawSettings = useCallback(() => setShowDrawSettings(false), []);
  useOutsideClick([drawSettingsRef], showDrawSettings, closeDrawSettings);

  // Toggle draw mode from overlay toolbar button
  const handleToggleDrawMode = useCallback(() => {
    setDrawMode((prev) => {
      const newMode = !prev;
      invoke("cmd_set_overlay_draw_mode", { drawMode: newMode });
      return newMode;
    });
  }, []);

  // Normalized position from Konva pointer event
  // deno-lint-ignore no-explicit-any
  const getNormalizedPos = (e: any) => {
    const stage = e.target.getStage();
    const pos = stage.getPointerPosition();
    if (!pos) return null;
    return { x: pos.x / dimensions.width, y: pos.y / dimensions.height };
  };

  // --- Drawing handlers (identical pattern to HostPanel) ---

  // deno-lint-ignore no-explicit-any
  const handleMouseDown = (e: any) => {
    if (!drawMode) return;
    const pos = getNormalizedPos(e);
    if (!pos) return;
    const nativeEvent = e.evt;
    const persist = nativeEvent?.shiftKey ?? false;
    isDrawing.current = true;
    currentPersist.current = persist;
    const lineId = uuidv4();
    currentLineId.current = lineId;
    setLines((prev) => [
      ...prev,
      {
        id: lineId,
        senderId: hostId.current,
        points: [pos.x, pos.y],
        color: myColor,
        width: strokeWidth,
        persist,
      },
    ]);
    const packet = JSON.stringify({
      t: "ds",
      x: pos.x,
      y: pos.y,
      c: myColor,
      w: strokeWidth,
      l: lineId,
      id: hostId.current,
      ...(persist ? { p: true } : {}),
    });
    invoke("cmd_send_annotation", { data: packet });
  };

  // deno-lint-ignore no-explicit-any
  const handleMouseMove = (e: any) => {
    if (!isDrawing.current || !currentLineId.current) return;
    const pos = getNormalizedPos(e);
    if (!pos) return;
    setLines((prev) =>
      prev.map((line) =>
        line.id === currentLineId.current
          ? { ...line, points: [...line.points, pos.x, pos.y] }
          : line
      )
    );
    const packet = JSON.stringify({
      t: "dm",
      x: pos.x,
      y: pos.y,
      l: currentLineId.current,
      id: hostId.current,
    });
    invoke("cmd_send_annotation", { data: packet });
  };

  const handleMouseUp = () => {
    if (!isDrawing.current || !currentLineId.current) return;
    isDrawing.current = false;
    const lineId = currentLineId.current;
    const persist = currentPersist.current;
    const packet = JSON.stringify({ t: "de", l: lineId, id: hostId.current });
    invoke("cmd_send_annotation", { data: packet });
    currentLineId.current = null;
    currentPersist.current = false;
    setLines((prev) =>
      prev.map((line) =>
        line.id === lineId ? { ...line, finished: true } : line
      )
    );
    if (!persist) {
      startFade(lineId);
    }
  };

  // Finalize the in-progress stroke on any release, even off the Stage. Releasing
  // outside the canvas (or losing window focus mid-stroke) otherwise skips the "de"
  // packet, so the stroke never fades and the next move continues the old line. The
  // call is idempotent (handleMouseUp guards on isDrawing.current). See useDrawing.
  const endStrokeRef = useRef(handleMouseUp);
  endStrokeRef.current = handleMouseUp;
  useEffect(() => {
    const end = () => endStrokeRef.current();
    window.addEventListener("mouseup", end);
    window.addEventListener("touchend", end);
    window.addEventListener("touchcancel", end);
    window.addEventListener("blur", end);
    return () => {
      window.removeEventListener("mouseup", end);
      window.removeEventListener("touchend", end);
      window.removeEventListener("touchcancel", end);
      window.removeEventListener("blur", end);
    };
  }, []);

  // Clear all annotations
  const handleClearAll = () => {
    setLines([]);
    lineRefs.current.clear();
    fadingRef.current.clear();
    const packet = JSON.stringify({ t: "cl", id: hostId.current, all: true });
    invoke("cmd_send_annotation", { data: packet });
  };

  // --- Viewer annotation listener (receive viewer strokes via Tauri event) ---
  // Identical pattern to HostPanel
  useEffect(() => {
    const unlisten = listen<{ data: string }>("viewer-annotation", (e) => {
      try {
        const packet = JSON.parse(e.payload.data) as DataPacket;
        switch (packet.t) {
          case "ds":
            setLines((prev) => [
              ...prev,
              {
                id: packet.l,
                senderId: packet.id,
                points: [packet.x, packet.y],
                color: packet.c,
                width: packet.w,
                persist: packet.p ?? false,
              },
            ]);
            break;
          case "dm":
            setLines((prev) =>
              prev.map((line) =>
                line.id === packet.l
                  ? { ...line, points: [...line.points, packet.x, packet.y] }
                  : line
              )
            );
            break;
          case "de": {
            setLines((prev) => {
              const line = prev.find((l) => l.id === packet.l);
              if (line && !line.persist) {
                startFade(packet.l);
              }
              return prev.map((l) =>
                l.id === packet.l ? { ...l, finished: true } : l
              );
            });
            break;
          }
          case "cl":
            if (packet.all) {
              setLines([]);
              lineRefs.current.clear();
              fadingRef.current.clear();
            } else {
              setLines((prev) =>
                prev.filter((l) => {
                  if (l.senderId === packet.id) {
                    lineRefs.current.delete(l.id);
                    fadingRef.current.delete(l.id);
                    return false;
                  }
                  return true;
                })
              );
            }
            break;
          case "ca":
            // Color assign -- no action needed, ds packets carry color
            break;
        }
      } catch {
        /* ignore malformed packets */
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  return (
    <div
      className="overlay-root"
      style={{
        width: "100vw",
        height: "100vh",
        background: "transparent",
        position: "relative",
        overflow: "hidden",
      }}
    >
      {/* Drawing canvas -- covers full overlay */}
      <Stage
        width={dimensions.width}
        height={dimensions.height}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onTouchStart={handleMouseDown}
        onTouchMove={handleMouseMove}
        onTouchEnd={handleMouseUp}
        style={{
          background: "transparent",
          cursor: drawMode ? "crosshair" : "default",
          position: "absolute",
          inset: "0",
        }}
      >
        <Layer>
          {lines.map((line) => (
            <Line
              key={line.id}
              // deno-lint-ignore no-explicit-any
              ref={(node: any) => {
                if (node) lineRefs.current.set(line.id, node);
              }}
              points={line.points.map((p, idx) =>
                idx % 2 === 0 ? p * dimensions.width : p * dimensions.height
              )}
              stroke={line.color}
              strokeWidth={line.width * dimensions.height}
              opacity={0.8}
              tension={0.5}
              lineCap="round"
              lineJoin="round"
            />
          ))}
        </Layer>
      </Stage>

      {drawMode && (
        <div className="overlay-toolbar" ref={drawSettingsRef}>
          <button
            type="button"
            className="toolbar-icon-btn active"
            onClick={handleToggleDrawMode}
            title="Exit Draw Mode (Ctrl+Shift+D)"
          >
            <PencilIcon />
          </button>
          <button
            type="button"
            className="toolbar-icon-btn draw-settings-btn"
            onClick={() => setShowDrawSettings((prev) => !prev)}
            title="Draw Settings"
          >
            <span
              className="settings-color-indicator"
              style={{ borderColor: myColor }}
            />
          </button>
          {showDrawSettings && (
            <DrawSettingsPopover
              myColor={myColor}
              strokeWidth={strokeWidth}
              onColorChange={(c) => setMyColor(c as typeof myColor)}
              onStrokeWidthChange={setStrokeWidth}
            />
          )}
          <button
            type="button"
            className="toolbar-icon-btn"
            onClick={handleClearAll}
            title="Clear All Annotations"
          >
            <TrashIcon />
          </button>
        </div>
      )}
    </div>
  );
}
