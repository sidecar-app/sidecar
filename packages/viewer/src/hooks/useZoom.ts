import { useCallback, useEffect, useRef, useState } from "react";

const SCALE_FACTOR = 1.1; // ~10% per scroll tick (D-02)

interface UseZoomArgs {
  videoWidth: number; // video.videoWidth (stream source pixels)
  videoHeight: number; // video.videoHeight (stream source pixels)
  dimensions: { width: number; height: number; left: number; top: number };
  containerRef: React.RefObject<HTMLDivElement | null>;
}

interface ZoomState {
  scale: number;
  panX: number;
  panY: number;
}

function clampPan(
  panX: number,
  panY: number,
  scale: number,
  containerW: number,
  containerH: number,
  videoRect?: { left: number; top: number; width: number; height: number },
): { panX: number; panY: number } {
  if (!videoRect || videoRect.width === 0) {
    // Fallback: treat entire container as content
    const scaledW = containerW * scale;
    const scaledH = containerH * scale;
    return {
      panX: Math.min(0, Math.max(containerW - scaledW, panX)),
      panY: Math.min(0, Math.max(containerH - scaledH, panY)),
    };
  }

  // The video sits at (left, top) in the unscaled wrapper.
  // After transform: translate(panX, panY) scale(scale) with origin 0 0,
  // the video's screen-space edges are:
  //   left edge:  panX + videoRect.left * scale
  //   right edge: panX + (videoRect.left + videoRect.width) * scale
  //   top edge:   panY + videoRect.top * scale
  //   bottom edge: panY + (videoRect.top + videoRect.height) * scale
  //
  // Constraint: video left edge <= 0 and video right edge >= containerW
  //   panX + videoRect.left * scale <= 0   →  panX <= -videoRect.left * scale
  //   panX + (videoRect.left + videoRect.width) * scale >= containerW
  //     → panX >= containerW - (videoRect.left + videoRect.width) * scale
  const scaledVideoW = videoRect.width * scale;
  const scaledVideoH = videoRect.height * scale;

  let clampedX: number;
  if (scaledVideoW <= containerW) {
    // Scaled video fits within viewport — center it horizontally
    clampedX = (containerW - scaledVideoW) / 2 - videoRect.left * scale;
  } else {
    // Scaled video overflows — clamp so edges don't reveal letterbox
    const maxX = -videoRect.left * scale;
    const minX = containerW - (videoRect.left + videoRect.width) * scale;
    clampedX = Math.min(maxX, Math.max(minX, panX));
  }

  let clampedY: number;
  if (scaledVideoH <= containerH) {
    // Scaled video fits within viewport — center it vertically
    clampedY = (containerH - scaledVideoH) / 2 - videoRect.top * scale;
  } else {
    // Scaled video overflows — clamp so edges don't reveal letterbox
    const maxY = -videoRect.top * scale;
    const minY = containerH - (videoRect.top + videoRect.height) * scale;
    clampedY = Math.min(maxY, Math.max(minY, panY));
  }

  return { panX: clampedX, panY: clampedY };
}

export function useZoom({ videoWidth, dimensions, containerRef }: UseZoomArgs) {
  // Video rect within container (for clamping pan to video area, not black bars)
  const videoRect = dimensions;
  const [zoom, setZoom] = useState<ZoomState>({ scale: 1, panX: 0, panY: 0 });
  const isPanning = useRef(false);
  const panStart = useRef({ startX: 0, startY: 0, startPanX: 0, startPanY: 0 });
  const [spacePanning, setSpacePanning] = useState(false);
  const scaleRef = useRef(zoom.scale);
  scaleRef.current = zoom.scale;
  // True while the spacebar is held -- the modifier that arms scroll-to-zoom
  // and click-drag panning. Read synchronously by the wheel handler so it can
  // pass wheel events through (e.g. to remote-control scroll) when not held.
  const spaceHeldRef = useRef(false);

  // Max scale: at scale=1, dimensions.width CSS pixels display videoWidth source pixels.
  // For 1:1 physical pixel mapping: maxScale = videoWidth / (dimensions.width * dpr)
  const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
  const maxScale = dimensions.width > 0
    ? videoWidth / (dimensions.width * dpr)
    : 1;
  const canZoom = maxScale > 1;
  const canZoomRef = useRef(canZoom);
  canZoomRef.current = canZoom;

  // Recalculate on resolution/dimensions change
  useEffect(() => {
    if (!canZoom) {
      // Can't zoom -- reset entirely
      setZoom({ scale: 1, panX: 0, panY: 0 });
      return;
    }
    setZoom((prev) => {
      if (prev.scale > maxScale) {
        // Clamp scale down to new max
        const container = containerRef.current;
        const containerW = container?.clientWidth ?? 0;
        const containerH = container?.clientHeight ?? 0;
        const clamped = clampPan(
          prev.panX,
          prev.panY,
          maxScale,
          containerW,
          containerH,
          videoRect,
        );
        return { scale: maxScale, ...clamped };
      }
      return prev;
    });
  }, [
    videoWidth,
    dimensions.width,
    dimensions.height,
    canZoom,
    maxScale,
    containerRef,
  ]);

  // Wheel handler -- registered with { passive: false } for preventDefault()
  const handleWheel = useCallback(
    (e: WheelEvent) => {
      if (!canZoom) return;
      // Scroll-to-zoom is gated on the spacebar, the same modifier as click-drag
      // panning. Without it the wheel event passes through untouched -- e.g. in
      // remote-control mode it scrolls the host instead of zooming the local view.
      if (!spaceHeldRef.current) return;
      e.preventDefault();

      // Filter trackpad micro-scrolls
      if (Math.abs(e.deltaY) < 1) return;

      const container = containerRef.current;
      if (!container) return;
      const rect = container.getBoundingClientRect();

      // Screen-space cursor position relative to container
      const screenX = e.clientX - rect.left;
      const screenY = e.clientY - rect.top;

      setZoom((prev) => {
        let newScale: number;

        if (e.ctrlKey) {
          // Pinch-to-zoom (trackpad sets ctrlKey, uses smaller deltaY values)
          newScale = Math.max(
            1,
            Math.min(maxScale, prev.scale * (1 - e.deltaY * 0.01)),
          );
        } else {
          // Regular scroll wheel
          const direction = e.deltaY < 0 ? 1 : -1;
          newScale = direction > 0
            ? Math.min(prev.scale * SCALE_FACTOR, maxScale)
            : Math.max(prev.scale / SCALE_FACTOR, 1);
        }

        if (Math.abs(newScale - prev.scale) < 0.001) return prev;
        if (newScale <= 1) return { scale: 1, panX: 0, panY: 0 };

        // Cursor-relative zoom: content point under cursor stays fixed (D-05)
        // Clamp anchor to video rect so zooming over letterbox targets the nearest video edge
        let contentX = (screenX - prev.panX) / prev.scale;
        let contentY = (screenY - prev.panY) / prev.scale;
        contentX = Math.max(
          videoRect.left,
          Math.min(videoRect.left + videoRect.width, contentX),
        );
        contentY = Math.max(
          videoRect.top,
          Math.min(videoRect.top + videoRect.height, contentY),
        );
        const newPanX = screenX - contentX * newScale;
        const newPanY = screenY - contentY * newScale;

        const containerW = container.clientWidth;
        const containerH = container.clientHeight;
        const clamped = clampPan(
          newPanX,
          newPanY,
          newScale,
          containerW,
          containerH,
          videoRect,
        );

        return { scale: newScale, ...clamped };
      });
    },
    [canZoom, maxScale, containerRef, videoRect],
  );

  // Register wheel event with passive: false
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    container.addEventListener("wheel", handleWheel, { passive: false });
    return () => container.removeEventListener("wheel", handleWheel);
  }, [containerRef, handleWheel]);

  // Step zoom in 0.2 increments: direction=1 zooms in, direction=-1 zooms out
  const stepZoom = useCallback(
    (direction: 1 | -1) => {
      if (!canZoom) return;

      setZoom((prev) => {
        const raw = prev.scale + direction * 0.2;
        let newScale = Math.round(Math.min(maxScale, Math.max(1, raw)) * 10) /
          10;
        // Snap to maxScale if we're within one step of it and zooming in
        if (
          direction === 1 && newScale < maxScale && maxScale - newScale < 0.2
        ) {
          newScale = maxScale;
        }

        if (Math.abs(newScale - prev.scale) < 0.01) return prev;
        if (newScale <= 1) return { scale: 1, panX: 0, panY: 0 };

        // Zoom around the viewport center — keep whatever is there fixed
        const container = containerRef.current;
        const containerW = container?.clientWidth ?? 0;
        const containerH = container?.clientHeight ?? 0;
        const centerX = containerW / 2;
        const centerY = containerH / 2;
        // Content point currently at viewport center
        const contentX = (centerX - prev.panX) / prev.scale;
        const contentY = (centerY - prev.panY) / prev.scale;
        const panX = centerX - contentX * newScale;
        const panY = centerY - contentY * newScale;
        const clamped = clampPan(
          panX,
          panY,
          newScale,
          containerW,
          containerH,
          videoRect,
        );
        return { scale: newScale, ...clamped };
      });
    },
    [canZoom, maxScale, containerRef, videoRect],
  );

  // Keyboard zoom: +/= to zoom in, - to zoom out (D-03)
  // Uses keypress to reliably match the produced character across browsers
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      if (!canZoom) return;

      const ch = e.key;
      const isZoomIn = ch === "+" || ch === "=";
      const isZoomOut = ch === "-";
      if (!isZoomIn && !isZoomOut) return;

      e.preventDefault();
      stepZoom(isZoomIn ? 1 : -1);
    };

    window.addEventListener("keypress", handler, true);
    return () => window.removeEventListener("keypress", handler, true);
  }, [canZoom, stepZoom]);

  // Keyboard panning: arrow keys and hjkl (vim-style)
  useEffect(() => {
    const PAN_STEP = 50;
    const handler = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      if (scaleRef.current <= 1) return;

      let dx = 0;
      let dy = 0;
      switch (e.code) {
        case "ArrowLeft":
        case "KeyH":
          dx = PAN_STEP;
          break;
        case "ArrowRight":
        case "KeyL":
          dx = -PAN_STEP;
          break;
        case "ArrowUp":
        case "KeyK":
          dy = PAN_STEP;
          break;
        case "ArrowDown":
        case "KeyJ":
          dy = -PAN_STEP;
          break;
        default:
          return;
      }

      e.preventDefault();
      const container = containerRef.current;
      if (!container) return;
      const containerW = container.clientWidth;
      const containerH = container.clientHeight;

      setZoom((prev) => {
        const clamped = clampPan(
          prev.panX + dx,
          prev.panY + dy,
          prev.scale,
          containerW,
          containerH,
          videoRect,
        );
        return { ...prev, ...clamped };
      });
    };

    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [containerRef, videoRect]);

  // Space key arms scroll-to-zoom and click-drag panning (D-07).
  // Uses capture phase so it fires BEFORE the browser's default button-click
  // behavior (Space activates focused buttons, which could toggle draw mode off).
  // Armed whenever the stream is zoomable so you can hold Space and scroll to
  // zoom in from 1x; pan handlers and cursor still require an actual zoom.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      if (e.code !== "Space" || e.repeat || !canZoomRef.current) return;
      e.preventDefault();
      e.stopPropagation();
      if (document.activeElement instanceof HTMLButtonElement) {
        document.activeElement.blur();
      }
      spaceHeldRef.current = true;
      setSpacePanning(true);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.code !== "Space" || !spaceHeldRef.current) return;
      e.preventDefault();
      spaceHeldRef.current = false;
      setSpacePanning(false);
      isPanning.current = false;
    };
    // A lost keyup (alt-tab, focus change) would otherwise leave Space "held"
    // and keep scroll-to-zoom armed -- reset on blur to be safe.
    const onBlur = () => {
      if (!spaceHeldRef.current) return;
      spaceHeldRef.current = false;
      setSpacePanning(false);
      isPanning.current = false;
    };
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  // Pan via click-drag (D-04) -- accepts React.MouseEvent or native MouseEvent
  const handlePanStart = useCallback(
    (e: { clientX: number; clientY: number }) => {
      if (zoom.scale <= 1) return;
      isPanning.current = true;
      panStart.current = {
        startX: e.clientX,
        startY: e.clientY,
        startPanX: zoom.panX,
        startPanY: zoom.panY,
      };
    },
    [zoom.scale, zoom.panX, zoom.panY],
  );

  const handlePanMove = useCallback(
    (e: { clientX: number; clientY: number }) => {
      if (!isPanning.current) return;

      const dx = e.clientX - panStart.current.startX;
      const dy = e.clientY - panStart.current.startY;
      const newPanX = panStart.current.startPanX + dx;
      const newPanY = panStart.current.startPanY + dy;

      const container = containerRef.current;
      if (!container) return;

      const containerW = container.clientWidth;
      const containerH = container.clientHeight;

      setZoom((prev) => {
        const clamped = clampPan(
          newPanX,
          newPanY,
          prev.scale,
          containerW,
          containerH,
          videoRect,
        );
        return { ...prev, ...clamped };
      });
    },
    [containerRef, videoRect],
  );

  const handlePanEnd = useCallback(() => {
    isPanning.current = false;
  }, []);

  // Double-click reset (D-06)
  const resetZoom = useCallback(() => {
    setZoom({ scale: 1, panX: 0, panY: 0 });
  }, []);

  const isZoomed = zoom.scale > 1;

  // Zoom label: "Native" at max, else "1.4x" format (D-09, D-10)
  const zoomLabel = zoom.scale >= maxScale - 0.01
    ? "Native"
    : zoom.scale.toFixed(1) + "x";

  return {
    scale: zoom.scale,
    panX: zoom.panX,
    panY: zoom.panY,
    canZoom,
    maxScale,
    isZoomed,
    zoomLabel,
    handleWheel,
    handlePanStart,
    handlePanMove,
    handlePanEnd,
    resetZoom,
    stepZoom,
    spacePanning,
  };
}
