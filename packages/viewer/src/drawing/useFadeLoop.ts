import { useCallback, useEffect, useRef } from "react";
import { BASE_OPACITY, FADE_DURATION_MS, HOLD_DELAY_MS } from "./fadeConstants";

/**
 * A single shared requestAnimationFrame loop that drives fade animations
 * for all finished strokes. Replaces per-stroke Konva node.to() tweens.
 *
 * Each stroke gets a finishedAt timestamp (performance.now()) when draw ends.
 * The loop computes elapsed time per stroke, applies hold delay, then
 * cubic ease-out interpolation to set opacity via Konva node.opacity().
 *
 * Returns { startFade, fadingRef } where:
 * - startFade(lineId): registers a stroke for fading
 * - fadingRef: Map<string, number> of lineId -> finishedAt (for clear-all cleanup)
 */
export function useFadeLoop(
  // deno-lint-ignore no-explicit-any
  setLines: React.Dispatch<React.SetStateAction<any[]>>,
  // deno-lint-ignore no-explicit-any
  lineRefs: React.RefObject<Map<string, any>>,
  holdDelayMs: number = HOLD_DELAY_MS,
) {
  const fadingRef = useRef<Map<string, number>>(new Map());
  const rafRef = useRef<number | null>(null);

  const tick = useCallback(() => {
    const now = performance.now();
    let anyActive = false;
    let needsDraw = false;

    fadingRef.current.forEach((finishedAt, lineId) => {
      const elapsed = now - finishedAt;

      if (elapsed < holdDelayMs) {
        // Still in hold period -- full opacity, no visual change
        anyActive = true;
        return;
      }

      const fadeElapsed = elapsed - holdDelayMs;
      const t = Math.min(fadeElapsed / FADE_DURATION_MS, 1.0);
      const easedT = 1 - Math.pow(1 - t, 3); // cubic ease-out
      const opacity = BASE_OPACITY * (1 - easedT);

      const node = lineRefs.current.get(lineId);

      if (t >= 1.0) {
        // Fully faded -- remove
        fadingRef.current.delete(lineId);
        lineRefs.current.delete(lineId);
        setLines((prev) => prev.filter((l) => l.id !== lineId));
        needsDraw = true;
      } else if (node) {
        node.opacity(opacity);
        needsDraw = true;
        anyActive = true;
      } else {
        // Node not found (removed by clear) -- clean up
        fadingRef.current.delete(lineId);
      }
    });

    // Single batchDraw per tick (not per stroke)
    if (needsDraw) {
      // Find any remaining node to access its layer for batchDraw
      for (const [, node] of lineRefs.current) {
        const layer = node?.getLayer?.();
        if (layer) {
          layer.batchDraw();
          break;
        }
      }
    }

    if (anyActive) {
      rafRef.current = requestAnimationFrame(tick);
    } else {
      rafRef.current = null;
    }
  }, [setLines, lineRefs, holdDelayMs]);

  const startFade = useCallback(
    (lineId: string) => {
      fadingRef.current.set(lineId, performance.now());
      if (rafRef.current === null) {
        rafRef.current = requestAnimationFrame(tick);
      }
    },
    [tick],
  );

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
      }
    };
  }, []);

  return { startFade, fadingRef };
}
