import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DataPacket } from "./connection/protocol";
import { v4 as uuidv4 } from "uuid";
import { useFadeLoop } from "./drawing/useFadeLoop";
import { ConnectionOverlay } from "./overlays/ConnectionOverlay";
import { StatsOverlay } from "./overlays/StatsOverlay";
import { useZoom } from "./hooks/useZoom";
import { useOutsideClick } from "./hooks/useOutsideClick";
import { EdgeIndicator, getOffScreenEdge } from "./overlays/EdgeIndicator";
import type { EdgeIndicatorHandle } from "./overlays/EdgeIndicator";
import { useViewerConnection } from "./connection/useViewerConnection";
import { useDrawing } from "./drawing/useDrawing";
import { useRemoteControl } from "./remote-control/useRemoteControl";
import { useMobileKeyboard } from "./remote-control/useMobileKeyboard";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { CanvasOverlay } from "./drawing/CanvasOverlay";
import { StallOverlay } from "./overlays/StallOverlay";
import { HostLeftOverlay } from "./overlays/HostLeftOverlay";
import { ViewerToolbar } from "./toolbar/ViewerToolbar";
import { DEFAULT_STROKE_WIDTH } from "./toolbar/drawConstants";
import type { CursorData, LineData } from "./viewer-types";
import "./ViewerScreen.css";

export interface ViewerScreenProps {
  roomCode: string;
  onLeave: () => void;
}

export function ViewerScreen({ roomCode, onLeave }: ViewerScreenProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const [dimensions, setDimensions] = useState({
    width: 0,
    height: 0,
    left: 0,
    top: 0,
  });
  const [videoSize, setVideoSize] = useState({ w: 0, h: 0 });

  const [lines, setLines] = useState<LineData[]>([]);
  const [cursors, setCursors] = useState<Map<string, CursorData>>(new Map());
  const [drawMode, setDrawMode] = useState(false);
  const [remoteControlMode, setRemoteControlMode] = useState(false);
  const [myColor, setMyColor] = useState<string | null>(null);
  const [strokeWidth, setStrokeWidth] = useState(DEFAULT_STROKE_WIDTH);
  const [showDrawSettings, setShowDrawSettings] = useState(false);
  const [showStats, setShowStats] = useState(false);
  const [ctaVisible, setCtaVisible] = useState(true);

  const drawSettingsBtnRef = useRef<HTMLButtonElement>(null);
  const drawSettingsPopoverRef = useRef<HTMLDivElement>(null);
  const myId = useRef(uuidv4());
  const cursorTimestamps = useRef<Map<string, number>>(new Map());
  const viewerColors = useRef<Map<string, string>>(new Map());
  // deno-lint-ignore no-explicit-any
  const lineRefs = useRef<Map<string, any>>(new Map());
  const { startFade, fadingRef } = useFadeLoop(setLines, lineRefs);
  const edgeIndicatorRef = useRef<EdgeIndicatorHandle>(null);

  // --- Zoom ---

  const zoom = useZoom({
    videoWidth: videoSize.w,
    videoHeight: videoSize.h,
    dimensions,
    containerRef,
  });

  // Visible viewport in normalized 0-1 coordinates for off-screen edge detection (D-14)
  const visibleRect = useMemo(() => {
    if (!zoom.isZoomed || dimensions.width === 0) return null;
    const containerW = containerRef.current?.clientWidth ?? 0;
    const containerH = containerRef.current?.clientHeight ?? 0;
    if (containerW === 0 || containerH === 0) return null;
    const contentLeft = -zoom.panX / zoom.scale;
    const contentTop = -zoom.panY / zoom.scale;
    const contentRight = contentLeft + containerW / zoom.scale;
    const contentBottom = contentTop + containerH / zoom.scale;
    return {
      left: (contentLeft - dimensions.left) / dimensions.width,
      top: (contentTop - dimensions.top) / dimensions.height,
      right: (contentRight - dimensions.left) / dimensions.width,
      bottom: (contentBottom - dimensions.top) / dimensions.height,
    };
  }, [zoom.scale, zoom.panX, zoom.panY, zoom.isZoomed, dimensions]);

  const visibleRectRef = useRef(visibleRect);
  visibleRectRef.current = visibleRect;
  const isZoomedRef = useRef(zoom.isZoomed);
  isZoomedRef.current = zoom.isZoomed;

  // --- Video layout ---

  const updateVideoLayout = useCallback(() => {
    if (!videoRef.current || !containerRef.current) return;
    const video = videoRef.current;
    const container = containerRef.current;
    if (video.videoWidth === 0) return;

    const videoRatio = video.videoWidth / video.videoHeight;
    const containerRatio = container.clientWidth / container.clientHeight;
    let width, height, left, top;

    if (containerRatio > videoRatio) {
      height = container.clientHeight;
      width = height * videoRatio;
      top = 0;
      left = (container.clientWidth - width) / 2;
    } else {
      width = container.clientWidth;
      height = width / videoRatio;
      left = 0;
      top = (container.clientHeight - height) / 2;
    }
    setDimensions({ width, height, left, top });
  }, []);

  useEffect(() => {
    const handleResize = () => updateVideoLayout();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [updateVideoLayout]);

  // --- Data packet dispatch ---

  const handleDataPacket = useCallback(
    (packet: DataPacket) => {
      switch (packet.t) {
        case "ca":
          setMyColor(packet.c);
          break;
        case "c":
          cursorTimestamps.current.set(packet.id, Date.now());
          if (packet.c) viewerColors.current.set(packet.id, packet.c);
          setCursors((prev) => {
            const newMap = new Map(prev);
            newMap.set(packet.id, {
              id: packet.id,
              x: packet.x,
              y: packet.y,
              color: viewerColors.current.get(packet.id),
              mode: packet.m,
            });
            return newMap;
          });
          break;
        case "ds":
          viewerColors.current.set(packet.id, packet.c);
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
          if (
            isZoomedRef.current &&
            packet.id !== myId.current &&
            visibleRectRef.current
          ) {
            const edge = getOffScreenEdge(
              packet.x,
              packet.y,
              visibleRectRef.current,
            );
            if (edge) edgeIndicatorRef.current?.triggerFlash(edge, packet.c);
          }
          break;
        case "dm":
          setLines((prev) =>
            prev.map((line) =>
              line.id === packet.l
                ? { ...line, points: [...line.points, packet.x, packet.y] }
                : line
            )
          );
          if (
            isZoomedRef.current &&
            packet.id !== myId.current &&
            visibleRectRef.current
          ) {
            const edge = getOffScreenEdge(
              packet.x,
              packet.y,
              visibleRectRef.current,
            );
            if (edge) {
              const color = viewerColors.current.get(packet.id) || "#fff";
              edgeIndicatorRef.current?.triggerFlash(edge, color);
            }
          }
          break;
        case "de":
          setLines((prev) => {
            const line = prev.find((l) => l.id === packet.l);
            if (line && !line.persist) startFade(packet.l);
            return prev.map((l) =>
              l.id === packet.l ? { ...l, finished: true } : l
            );
          });
          break;
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
      }
    },
    [startFade, fadingRef],
  );

  // --- Connection lifecycle ---

  const {
    connected,
    status,
    hostLeft,
    stallState,
    latency,
    connectionStatus,
    managerRef,
    handleReconnect,
  } = useViewerConnection({
    roomCode,
    videoRef,
    updateVideoLayout,
    setVideoSize,
    onData: handleDataPacket,
  });

  // --- Drawing ---

  const { handleMouseDown, handleMouseMove, handleMouseUp } = useDrawing({
    drawMode,
    myColor,
    strokeWidth,
    dimensions,
    managerRef,
    myId,
    setLines,
    startFade,
  });

  // --- Remote control ---

  const exitRemoteControl = useCallback(() => setRemoteControlMode(false), []);

  const {
    isMobile,
    inputRef: mobileKeyboardRef,
    keyboardSuspended,
    showKeyboard,
  } = useMobileKeyboard({ remoteControlMode, managerRef, myId });

  const rc = useRemoteControl({
    remoteControlMode,
    myColor,
    dimensions,
    managerRef,
    myId,
    onExit: exitRemoteControl,
    keyboardSuspended,
  });

  // Route the canvas pointer events to whichever mode is active.
  const stageHandlers = remoteControlMode
    ? {
      onMouseDown: rc.handleMouseDown,
      onMouseMove: rc.handleMouseMove,
      onMouseUp: rc.handleMouseUp,
    }
    : {
      onMouseDown: handleMouseDown,
      onMouseMove: handleMouseMove,
      onMouseUp: handleMouseUp,
    };

  const handleClearMine = useCallback(() => {
    setLines((prev) => {
      prev.forEach((l) => {
        if (l.senderId === myId.current) {
          lineRefs.current.delete(l.id);
          fadingRef.current.delete(l.id);
        }
      });
      return prev.filter((l) => l.senderId !== myId.current);
    });
    managerRef.current?.send({ t: "cl", id: myId.current });
  }, [fadingRef, managerRef]);

  const toggleDrawMode = useCallback(() => {
    if (!myColor) return;
    setDrawMode((prev) => {
      const next = !prev;
      if (next) setRemoteControlMode(false); // modes are mutually exclusive
      return next;
    });
  }, [myColor]);

  const toggleRemoteControlMode = useCallback(() => {
    if (!myColor) return;
    setRemoteControlMode((prev) => {
      const next = !prev;
      if (next) setDrawMode(false); // modes are mutually exclusive
      return next;
    });
  }, [myColor]);

  // --- Keyboard shortcuts ---

  useKeyboardShortcuts({
    toggleDrawMode,
    toggleRemoteControlMode,
    onLeave,
    setShowStats,
  });

  // --- CTA auto-hide ---

  useEffect(() => {
    if (!connected) return;
    const timer = setTimeout(() => setCtaVisible(false), 8_000);
    return () => clearTimeout(timer);
  }, [connected]);

  // --- Cursor cleanup ---

  useEffect(() => {
    const interval = setInterval(() => {
      const now = Date.now();
      setCursors((prev) => {
        const next = new Map(prev);
        let changed = false;
        for (const [id] of next) {
          const ts = cursorTimestamps.current.get(id) || 0;
          if (now - ts > 3000) {
            next.delete(id);
            cursorTimestamps.current.delete(id);
            changed = true;
          }
        }
        return changed ? next : prev;
      });
    }, 1000);
    return () => clearInterval(interval);
  }, []);

  // --- Draw settings popover ---

  const closeDrawSettings = useCallback(() => setShowDrawSettings(false), []);
  useOutsideClick(
    [drawSettingsBtnRef, drawSettingsPopoverRef],
    showDrawSettings,
    closeDrawSettings,
  );

  // --- Render ---

  return (
    <div className="viewer-container" ref={containerRef}>
      {/* Status text while waiting for stream */}
      {!connected && !hostLeft && (
        <p
          style={{
            position: "absolute",
            zIndex: 50,
            color: "#a1a1aa",
            fontSize: "14px",
          }}
        >
          {status}
        </p>
      )}

      {
        /* Zoom wrapper -- contains only video and canvas overlay.
          CSS transform on zoom-wrapper causes browser to automatically apply
          inverse transform for pointer events. Konva getPointerPosition()
          returns coordinates in Stage's local (unzoomed) space, so
          normalization works correctly at any zoom level. */
      }
      <div
        className={`zoom-wrapper${zoom.isZoomed ? " zoomed" : ""}${
          zoom.spacePanning ? " space-pan" : ""
        }`}
        style={{
          transform:
            `translate(${zoom.panX}px, ${zoom.panY}px) scale(${zoom.scale})`,
          transformOrigin: "0 0",
        }}
        onMouseDown={zoom.isZoomed && zoom.spacePanning
          ? zoom.handlePanStart
          : undefined}
        onMouseMove={zoom.isZoomed && zoom.spacePanning
          ? zoom.handlePanMove
          : undefined}
        onMouseUp={zoom.isZoomed && zoom.spacePanning
          ? zoom.handlePanEnd
          : undefined}
        onDoubleClick={zoom.isZoomed ? zoom.resetZoom : undefined}
      >
        <video
          ref={videoRef}
          autoPlay
          playsInline
          style={{ width: "100%", height: "100%", objectFit: "contain" }}
        />
        <CanvasOverlay
          dimensions={dimensions}
          drawMode={drawMode}
          remoteControlMode={remoteControlMode}
          spacePanning={zoom.spacePanning && zoom.isZoomed}
          lines={lines}
          lineRefs={lineRefs}
          cursors={cursors}
          onMouseDown={stageHandlers.onMouseDown}
          onMouseMove={stageHandlers.onMouseMove}
          onMouseUp={stageHandlers.onMouseUp}
        />
      </div>

      {/* Everything below stays OUTSIDE zoom-wrapper */}

      {/* Remote-control active banner */}
      {remoteControlMode && (
        <div
          style={{
            position: "absolute",
            top: 12,
            left: "50%",
            transform: "translateX(-50%)",
            zIndex: 60,
            padding: "6px 14px",
            borderRadius: 8,
            background: "rgba(0, 199, 190, 0.92)",
            color: "#0a0a0a",
            fontSize: 13,
            fontWeight: 600,
            pointerEvents: "none",
          }}
        >
          Controlling host - press Esc to stop
        </div>
      )}

      {/* Edge indicator for off-screen remote strokes (D-14) -- always mounted so ref is stable */}
      <EdgeIndicator ref={edgeIndicatorRef} />

      <StallOverlay stallState={stallState} onReconnect={handleReconnect} />

      {/* Connection status overlay -- top-left, fades on idle */}
      {connected && (
        <ConnectionOverlay
          status={connectionStatus}
          latencyMs={latency ?? undefined}
          stallState={stallState}
        />
      )}
      {/* Stats for Nerds overlay */}
      {connected && (
        <StatsOverlay
          peerConnection={managerRef.current?.peerConnection ?? null}
          visible={showStats}
          onClose={() => setShowStats(false)}
        />
      )}

      <HostLeftOverlay visible={hostLeft} onLeave={onLeave} />

      <ViewerToolbar
        drawMode={drawMode}
        toggleDrawMode={toggleDrawMode}
        remoteControlMode={remoteControlMode}
        toggleRemoteControlMode={toggleRemoteControlMode}
        showKeyboardButton={isMobile && remoteControlMode}
        onShowKeyboard={showKeyboard}
        myColor={myColor}
        setShowDrawSettings={setShowDrawSettings}
        showDrawSettings={showDrawSettings}
        drawSettingsBtnRef={drawSettingsBtnRef}
        drawSettingsPopoverRef={drawSettingsPopoverRef}
        handleClearMine={handleClearMine}
        zoom={zoom}
        onLeave={onLeave}
        strokeWidth={strokeWidth}
        onColorChange={(c) => {
          setMyColor(c);
          // deno-lint-ignore no-explicit-any
          managerRef.current?.send({ t: "cc" as any, c, id: myId.current });
        }}
        onStrokeWidthChange={setStrokeWidth}
      />

      {/* Download Client CTA */}
      {ctaVisible && (
        <a
          href="https://sidecar.rs"
          target="_blank"
          rel="noopener noreferrer"
          className="viewer-host-cta"
        >
          Want to share your screen?{" "}
          <span className="viewer-host-cta-link">Get Sidecar</span>
        </a>
      )}

      {
        /* Hidden input on mobile: focusing it summons the on-screen keyboard;
          its typed text is forwarded to the host by useMobileKeyboard. */
      }
      {isMobile && (
        <input
          ref={mobileKeyboardRef}
          aria-hidden="true"
          inputMode="text"
          autoCapitalize="off"
          autoComplete="off"
          autoCorrect="off"
          spellCheck={false}
          tabIndex={-1}
          style={{
            position: "absolute",
            bottom: 0,
            left: 0,
            width: 1,
            height: 1,
            opacity: 0,
            border: 0,
            padding: 0,
            pointerEvents: "none",
          }}
        />
      )}
    </div>
  );
}
