export type CursorPacket = {
  t: "c"; // type: cursor
  x: number; // normalized 0-1
  y: number; // normalized 0-1
  id: string; // user id
  c?: string; // color hex (optional)
  m?: "d" | "c"; // mode: "d" = draw, "c" = control (drives the cursor glyph)
};

export type DrawStartPacket = {
  t: "ds"; // type: draw start
  x: number; // normalized 0-1
  y: number; // normalized 0-1
  c: string; // color hex
  w: number; // stroke width, normalized as fraction of canvas height
  l: string; // line id
  id: string; // user id
  p?: boolean; // persist (true = no fade, false/undefined = default fade)
};

export type DrawMovePacket = {
  t: "dm"; // type: draw move
  x: number; // normalized 0-1
  y: number; // normalized 0-1
  l: string; // line id
  id: string; // user id
};

export type DrawEndPacket = {
  t: "de"; // type: draw end
  l: string; // line id
  id: string; // user id
};

export type ColorAssignPacket = {
  t: "ca"; // type: color-assign
  id: string; // target viewer id
  c: string; // assigned color hex
};

export type ClearAllPacket = {
  t: "cl"; // type: clear — clears lines from this sender, or all if all=true
  id: string; // user who triggered clear
  all?: boolean; // if true, clear all lines (host privilege)
};

export type ColorChangePacket = {
  t: "cc"; // type: color-change
  c: string; // new color hex
  id: string; // viewer id
};

export type ResolutionPacket = {
  t: "res"; // type: resolution update (host -> viewers)
  w: number; // encoded stream width
  h: number; // encoded stream height
};

export type ViewportPacket = {
  t: "vp"; // type: viewport report (viewer -> host)
  w: number; // viewport width in device pixels (CSS * devicePixelRatio)
  h: number; // viewport height in device pixels (CSS * devicePixelRatio)
  id: string; // viewer id
};

// --- Remote control (viewer -> host only; never relayed to other viewers) ---

export type RemoteControlPacket = {
  t: "rc"; // type: remote-control mode toggle (control intent)
  on: boolean; // entering (true) or leaving (false) remote-control mode
  id: string; // viewer id (host tracks the active controller by this)
};

export type MouseButtonPacket = {
  t: "mb"; // type: mouse button
  b: number; // 0 = left, 1 = right, 2 = middle
  p: boolean; // pressed (true) or released (false)
  id: string; // viewer id (a press claims the active-controller slot)
};

export type ScrollPacket = {
  t: "ms"; // type: mouse scroll
  dx: number; // horizontal delta
  dy: number; // vertical delta
  id: string; // viewer id
};

export type KeyPacket = {
  t: "kd" | "ku"; // type: key down / key up
  k: string; // DOM KeyboardEvent.code (e.g. "KeyA", "Enter")
  id: string; // viewer id
};

export type DataPacket =
  | CursorPacket
  | DrawStartPacket
  | DrawMovePacket
  | DrawEndPacket
  | ColorAssignPacket
  | ClearAllPacket
  | ColorChangePacket
  | ResolutionPacket
  | ViewportPacket
  | RemoteControlPacket
  | MouseButtonPacket
  | ScrollPacket
  | KeyPacket;
