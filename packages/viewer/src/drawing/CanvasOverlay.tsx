import { Layer, Line, Stage } from "react-konva";
import type { CursorData, LineData } from "../viewer-types";

interface CanvasOverlayProps {
  dimensions: { width: number; height: number; left: number; top: number };
  drawMode: boolean;
  remoteControlMode: boolean;
  spacePanning: boolean;
  lines: LineData[];
  // deno-lint-ignore no-explicit-any
  lineRefs: React.MutableRefObject<Map<string, any>>;
  cursors: Map<string, CursorData>;
  // deno-lint-ignore no-explicit-any
  onMouseDown: (e: any) => void;
  // deno-lint-ignore no-explicit-any
  onMouseMove: (e: any) => void;
  // deno-lint-ignore no-explicit-any
  onMouseUp: (e: any) => void;
}

export function CanvasOverlay({
  dimensions,
  drawMode,
  remoteControlMode,
  spacePanning,
  lines,
  lineRefs,
  cursors,
  onMouseDown,
  onMouseMove,
  onMouseUp,
}: CanvasOverlayProps) {
  if (dimensions.width <= 0) return null;

  const interactive = (drawMode || remoteControlMode) && !spacePanning;

  // Scale the cursor with the displayed video (like strokes) so it matches the
  // host's ~18px-on-screen pointer at any display size instead of being a fixed
  // pixel size that looks huge on small/mobile viewports. viewBox is 12x19.
  const cursorH = Math.max(dimensions.height * 0.0167, 6);
  const cursorW = cursorH * (12 / 19);

  return (
    <div
      className={`canvas-overlay${drawMode ? " draw-mode" : ""}${
        remoteControlMode ? " rc-mode" : ""
      }${spacePanning ? " space-pan" : ""}`}
      style={{
        position: "absolute",
        left: dimensions.left,
        top: dimensions.top,
        width: dimensions.width,
        height: dimensions.height,
        pointerEvents: interactive ? "auto" : "none",
      }}
    >
      <Stage
        width={dimensions.width}
        height={dimensions.height}
        onMouseDown={onMouseDown}
        onMouseMove={onMouseMove}
        onMouseUp={onMouseUp}
        onTouchStart={onMouseDown}
        onTouchMove={onMouseMove}
        onTouchEnd={onMouseUp}
        style={{ background: "transparent" }}
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

      {
        /* Remote cursors, color-coded per viewer: a pencil for draw-mode viewers,
          an arrow pointer for control-mode (and as the default). Both glyphs put
          their tip at (0,0) so the SVG's top-left corner lands on the position.
          The arrow matches the host overlay's draw_pointer glyph; the pencil
          matches its draw_pencil glyph. */
      }
      {Array.from(cursors.values()).map((cursor) => {
        const style = {
          position: "absolute" as const,
          left: `${cursor.x * 100}%`,
          top: `${cursor.y * 100}%`,
          transition: "top 0.1s linear, left 0.1s linear",
          pointerEvents: "none" as const,
          overflow: "visible" as const,
        };
        const fill = cursor.color || "red";
        if (cursor.mode === "d") {
          return (
            <svg
              key={cursor.id}
              width={cursorH}
              height={cursorH}
              viewBox="0 0 19 19"
              style={style}
            >
              <path
                d="M0,0 L5.5,2.2 L18,14.7 L14.7,18 L2.2,5.5 Z"
                fill={fill}
                stroke="white"
                strokeWidth="1.5"
                strokeLinejoin="round"
              />
              <path
                d="M5.5,2.2 L2.2,5.5"
                stroke="white"
                strokeWidth="1.2"
                strokeLinecap="round"
              />
            </svg>
          );
        }
        return (
          <svg
            key={cursor.id}
            width={cursorW}
            height={cursorH}
            viewBox="0 0 12 19"
            style={style}
          >
            <path
              d="M0,0 L0,16 L4,12.5 L6.5,18 L8.5,17 L6,11.5 L11,11.5 Z"
              fill={fill}
              stroke="white"
              strokeWidth="1.5"
              strokeLinejoin="round"
            />
          </svg>
        );
      })}
    </div>
  );
}
