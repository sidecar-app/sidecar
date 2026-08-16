import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";

export interface EdgeIndicatorHandle {
  triggerFlash: (
    edge: "left" | "right" | "top" | "bottom",
    color: string,
  ) => void;
}

// deno-lint-ignore no-empty-interface
interface EdgeIndicatorProps {}

const VISIBLE_MS = 600;
const FADE_MS = 300;

export function getOffScreenEdge(
  nx: number,
  ny: number,
  rect: { left: number; top: number; right: number; bottom: number },
): "left" | "right" | "top" | "bottom" | null {
  if (
    nx >= rect.left &&
    nx <= rect.right &&
    ny >= rect.top &&
    ny <= rect.bottom
  ) {
    return null;
  }
  const dLeft = rect.left - nx;
  const dRight = nx - rect.right;
  const dTop = rect.top - ny;
  const dBottom = ny - rect.bottom;
  const maxD = Math.max(dLeft, dRight, dTop, dBottom);
  if (maxD === dLeft) return "left";
  if (maxD === dRight) return "right";
  if (maxD === dTop) return "top";
  return "bottom";
}

type Edge = "left" | "right" | "top" | "bottom";

const EDGE_STYLE: Record<Edge, React.CSSProperties> = {
  left: { left: 0, top: 0, width: 3, height: "100%" },
  right: { right: 0, top: 0, width: 3, height: "100%" },
  top: { left: 0, top: 0, width: "100%", height: 3 },
  bottom: { left: 0, bottom: 0, width: "100%", height: 3 },
};

export const EdgeIndicator = forwardRef<
  EdgeIndicatorHandle,
  EdgeIndicatorProps
>(function EdgeIndicator(_props, ref) {
  const [edges, setEdges] = useState<
    Partial<Record<Edge, { color: string; visible: boolean }>>
  >({});
  const timers = useRef<Partial<Record<Edge, ReturnType<typeof setTimeout>>>>(
    {},
  );

  useEffect(() => {
    const t = timers.current;
    return () => {
      Object.values(t).forEach((id) => id && clearTimeout(id));
    };
  }, []);

  const triggerFlash = useCallback((edge: Edge, color: string) => {
    if (timers.current[edge]) clearTimeout(timers.current[edge]);

    setEdges((prev) => ({ ...prev, [edge]: { color, visible: true } }));

    timers.current[edge] = setTimeout(() => {
      setEdges((prev) =>
        prev[edge]
          ? { ...prev, [edge]: { ...prev[edge], visible: false } }
          : prev
      );
    }, VISIBLE_MS);
  }, []);

  useImperativeHandle(ref, () => ({ triggerFlash }), [triggerFlash]);

  return (
    <div className="edge-indicator-container">
      {(["left", "right", "top", "bottom"] as const).map((edge) => {
        const state = edges[edge];
        return (
          <div
            key={edge}
            style={{
              position: "absolute",
              ...EDGE_STYLE[edge],
              background: state?.color ?? "transparent",
              opacity: state?.visible ? 0.6 : 0,
              transition: `opacity ${FADE_MS}ms ease`,
              borderRadius: 2,
            }}
          />
        );
      })}
    </div>
  );
});
