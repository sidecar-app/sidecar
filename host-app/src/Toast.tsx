import { useCallback, useState } from "react";

type ToastVariant = "default" | "warning";

interface ToastMessage {
  id: number;
  text: string;
  variant: ToastVariant;
}

let toastId = 0;

export function useToast() {
  const [toasts, setToasts] = useState<ToastMessage[]>([]);

  const showToast = useCallback(
    (text: string, variant: ToastVariant = "default") => {
      const id = ++toastId;
      setToasts((prev) => [...prev, { id, text, variant }]);
      setTimeout(() => {
        setToasts((prev) => prev.filter((t) => t.id !== id));
      }, variant === "warning" ? 8000 : 3000);
    },
    [],
  );

  return { toasts, showToast };
}

export function ToastContainer({ toasts }: { toasts: ToastMessage[] }) {
  return (
    <div
      style={{
        position: "fixed",
        top: "16px",
        right: "16px",
        zIndex: 9999,
        display: "flex",
        flexDirection: "column",
        gap: "8px",
      }}
    >
      {toasts.map((toast) => (
        <div
          key={toast.id}
          style={{
            background: "#27272a",
            color: "#fafafa",
            padding: "10px 16px",
            borderRadius: "8px",
            fontSize: "13px",
            boxShadow: "0 4px 12px rgba(0,0,0,0.3)",
            border: toast.variant === "warning" ? "1px solid #f97316" : "none",
            animation: "toast-in 0.2s ease-out",
          }}
        >
          {toast.text}
        </div>
      ))}
    </div>
  );
}
