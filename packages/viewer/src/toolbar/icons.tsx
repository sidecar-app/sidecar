/** Shared SVG icon components for toolbars. */

const ICON_PROPS = {
  width: 20,
  height: 20,
  viewBox: "0 0 20 20",
  fill: "none",
} as const;
const STROKE_PROPS = {
  stroke: "currentColor",
  strokeWidth: 1.5,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

export function PencilIcon() {
  return (
    <svg {...ICON_PROPS}>
      <path
        d="M14.85 2.85a1.5 1.5 0 0 1 2.12 0l.18.18a1.5 1.5 0 0 1 0 2.12L7.4 14.9l-3.18.71.71-3.18L14.85 2.85Z"
        {...STROKE_PROPS}
      />
    </svg>
  );
}

export function TrashIcon() {
  return (
    <svg {...ICON_PROPS}>
      <path
        d="M4 5h12M8 5V3.5a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1V5m2 0v10.5a1.5 1.5 0 0 1-1.5 1.5h-5A1.5 1.5 0 0 1 6 15.5V5"
        {...STROKE_PROPS}
      />
    </svg>
  );
}

export function ExitIcon() {
  return (
    <svg {...ICON_PROPS}>
      <path
        d="M7 17H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h3M13 14l4-4m0 0-4-4m4 4H7"
        {...STROKE_PROPS}
      />
    </svg>
  );
}

export function StopIcon() {
  return (
    <svg {...ICON_PROPS}>
      <rect x="5" y="5" width="10" height="10" rx="2.5" {...STROKE_PROPS} />
    </svg>
  );
}

export function LinkIcon() {
  return (
    <svg {...ICON_PROPS}>
      <path
        d="M8.5 11.5a3 3 0 0 0 4.243 0l2.5-2.5a3 3 0 0 0-4.243-4.243l-1.25 1.25"
        {...STROKE_PROPS}
      />
      <path
        d="M11.5 8.5a3 3 0 0 0-4.243 0l-2.5 2.5a3 3 0 0 0 4.243 4.243l1.25-1.25"
        {...STROKE_PROPS}
      />
    </svg>
  );
}

export function ZoomInIcon() {
  return (
    <svg {...ICON_PROPS}>
      <circle
        cx="8.5"
        cy="8.5"
        r="5.5"
        stroke="currentColor"
        strokeWidth={1.5}
      />
      <path
        d="M13 13l4 4"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
      />
      <path
        d="M6.5 8.5h4M8.5 6.5v4"
        stroke="currentColor"
        strokeWidth={1.4}
        strokeLinecap="round"
      />
    </svg>
  );
}

export function ZoomOutIcon() {
  return (
    <svg {...ICON_PROPS}>
      <circle
        cx="8.5"
        cy="8.5"
        r="5.5"
        stroke="currentColor"
        strokeWidth={1.5}
      />
      <path
        d="M13 13l4 4"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
      />
      <path
        d="M6.5 8.5h4"
        stroke="currentColor"
        strokeWidth={1.4}
        strokeLinecap="round"
      />
    </svg>
  );
}

// Drawn on a 24x24 grid (heavier stroke) rather than the shared 20x20 ICON_PROPS.
const ICON_PROPS_24 = {
  width: 18,
  height: 18,
  viewBox: "0 0 24 24",
  fill: "none",
} as const;
const STROKE_PROPS_24 = {
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

export function PointerIcon() {
  return (
    <svg {...ICON_PROPS_24} {...STROKE_PROPS_24}>
      <path d="m3 3 7.07 16.97 2.51-7.39 7.39-2.51L3 3z" />
      <path d="m13 13 6 6" />
    </svg>
  );
}

export function KeyboardIcon() {
  return (
    <svg {...ICON_PROPS_24} {...STROKE_PROPS_24}>
      <rect x="2" y="6" width="20" height="12" rx="2" />
      <path d="M6 10h0M10 10h0M14 10h0M18 10h0M6 14h0M18 14h0M9 14h6" />
    </svg>
  );
}
