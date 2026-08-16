import { type RefObject, useEffect } from "react";

/** Close callback when clicking outside one or more ref targets. */
export function useOutsideClick(
  refs: RefObject<Element | null>[],
  active: boolean,
  onClose: () => void,
) {
  useEffect(() => {
    if (!active) return;
    const handler = (e: MouseEvent) => {
      const t = e.target as Node;
      if (refs.every((r) => !r.current?.contains(t))) onClose();
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [active, refs, onClose]);
}
