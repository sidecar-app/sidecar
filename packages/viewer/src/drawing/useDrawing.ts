import { useEffect, useRef } from "react";
import { v4 as uuidv4 } from "uuid";
import type { WebRTCManager } from "../connection/WebRTCManager";
import type { LineData } from "../viewer-types";

interface UseDrawingOptions {
  drawMode: boolean;
  myColor: string | null;
  strokeWidth: number;
  dimensions: { width: number; height: number };
  managerRef: React.MutableRefObject<WebRTCManager | null>;
  myId: React.MutableRefObject<string>;
  setLines: React.Dispatch<React.SetStateAction<LineData[]>>;
  startFade: (lineId: string) => void;
}

export function useDrawing({
  drawMode,
  myColor,
  strokeWidth,
  dimensions,
  managerRef,
  myId,
  setLines,
  startFade,
}: UseDrawingOptions) {
  const isDrawing = useRef(false);
  const currentLineId = useRef<string | null>(null);
  const currentPersist = useRef(false);

  // deno-lint-ignore no-explicit-any
  const getNormalizedPos = (e: any) => {
    const stage = e.target.getStage();
    const pos = stage.getPointerPosition();
    if (!pos) return null;
    return { x: pos.x / dimensions.width, y: pos.y / dimensions.height };
  };

  // deno-lint-ignore no-explicit-any
  const handleMouseDown = (e: any) => {
    if (!drawMode || !myColor) return;
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
        senderId: myId.current,
        points: [pos.x, pos.y],
        color: myColor,
        width: strokeWidth,
        persist,
      },
    ]);

    managerRef.current?.send({
      t: "ds",
      x: pos.x,
      y: pos.y,
      c: myColor,
      w: strokeWidth,
      l: lineId,
      id: myId.current,
      ...(persist ? { p: true } : {}),
    });
  };

  // deno-lint-ignore no-explicit-any
  const handleMouseMove = (e: any) => {
    const pos = getNormalizedPos(e);
    if (!pos) return;

    if (drawMode) {
      managerRef.current?.send({
        t: "c",
        x: pos.x,
        y: pos.y,
        id: myId.current,
        c: myColor || undefined,
        m: "d",
      });
    }

    if (!isDrawing.current || !currentLineId.current) return;

    setLines((prev) =>
      prev.map((line) => {
        if (line.id === currentLineId.current) {
          return { ...line, points: [...line.points, pos.x, pos.y] };
        }
        return line;
      })
    );

    managerRef.current?.send({
      t: "dm",
      x: pos.x,
      y: pos.y,
      l: currentLineId.current,
      id: myId.current,
    });
  };

  const handleMouseUp = () => {
    if (!isDrawing.current || !currentLineId.current) return;
    isDrawing.current = false;
    const lineId = currentLineId.current;
    const persist = currentPersist.current;

    managerRef.current?.send({
      t: "de",
      l: lineId,
      id: myId.current,
    });
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

  // Finalize the in-progress stroke on any release, even if it happens off the
  // Konva Stage. The Stage's onMouseUp only fires over the canvas, so dragging
  // off the video and releasing there (or losing window focus mid-stroke) would
  // otherwise leave the stroke un-finished: no "de" packet, never fades, and the
  // next pointer move continues the old line. A window-level listener catches the
  // release wherever it lands; blur covers leaving the browser entirely. The call
  // is idempotent (handleMouseUp guards on isDrawing.current), so an in-canvas
  // release that already fired the Stage handler is a harmless no-op here.
  const endRef = useRef(handleMouseUp);
  endRef.current = handleMouseUp;
  useEffect(() => {
    const end = () => endRef.current();
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

  return { handleMouseDown, handleMouseMove, handleMouseUp };
}
