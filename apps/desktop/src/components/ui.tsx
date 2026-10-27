import { X } from "lucide-react";
import { useEffect, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from "react";

export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

export function Button({
  variant = "default",
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "ghost" | "danger" }) {
  return (
    <button
      {...props}
      className={cx(
        "inline-flex h-8 items-center justify-center gap-1.5 rounded-md px-3 text-sm font-medium transition-colors disabled:opacity-40",
        variant === "primary" && "bg-accent text-white hover:brightness-110",
        variant === "default" && "border border-line hover:bg-hover",
        variant === "ghost" && "text-ink-2 hover:bg-hover",
        variant === "danger" && "border border-danger/40 text-danger hover:bg-danger/10",
        className,
      )}
    />
  );
}

export function IconButton({ className, ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      {...props}
      className={cx(
        "inline-flex h-6 w-6 shrink-0 items-center justify-center rounded text-ink-3 hover:bg-active hover:text-ink-2",
        className,
      )}
    />
  );
}

export function Input(props: React.InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      {...props}
      className={cx(
        "h-8 w-full rounded-md border border-line bg-hover px-2.5 text-sm outline-none placeholder:text-ink-3 focus:border-accent/60 focus:ring-2 focus:ring-accent/25",
        props.className,
      )}
    />
  );
}

const TEAM_COLORS = ["#d9730d", "#0f7b6c", "#2383e2", "#9065b0", "#c14c8a", "#dfab01", "#e03e3e", "#64473a"];

export function teamColor(id: string, index?: number): string {
  if (index !== undefined) return TEAM_COLORS[index % TEAM_COLORS.length];
  let h = 0;
  for (const ch of id) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return TEAM_COLORS[h % TEAM_COLORS.length];
}

export function TeamIcon({ id, name, size = 20 }: { id: string; name: string; size?: number }) {
  return (
    <span
      className="inline-flex shrink-0 items-center justify-center rounded font-semibold text-white"
      style={{ width: size, height: size, fontSize: size * 0.55, background: teamColor(id) }}
    >
      {name.slice(0, 1).toUpperCase()}
    </span>
  );
}

/** Closes on outside click and Escape. */
function useDismiss(open: boolean, onClose: () => void) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, onClose]);
  return ref;
}

export function Menu({
  trigger,
  children,
  align = "left",
  width = 240,
}: {
  trigger: (props: { open: boolean; toggle: () => void }) => ReactNode;
  children: (close: () => void) => ReactNode;
  align?: "left" | "right";
  width?: number;
}) {
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const close = () => setPos(null);
  const ref = useDismiss(pos !== null, close);
  // Fixed positioning so scrolling containers (the sidebar) do not clip it.
  const toggle = () => {
    if (pos) return close();
    const rect = ref.current?.getBoundingClientRect();
    if (!rect) return;
    const left = align === "right" ? rect.right - width : rect.left;
    setPos({
      top: Math.min(rect.bottom + 4, window.innerHeight - 120),
      left: Math.max(8, Math.min(left, window.innerWidth - width - 8)),
    });
  };
  return (
    <div ref={ref} className="relative">
      {trigger({ open: pos !== null, toggle })}
      {pos && (
        <div className="fixed z-50 rounded-lg bg-popover p-1 shadow-pop" style={{ width, top: pos.top, left: pos.left }}>
          {children(close)}
        </div>
      )}
    </div>
  );
}

export function MenuItem({
  icon,
  children,
  onClick,
  danger,
  right,
}: {
  icon?: ReactNode;
  children: ReactNode;
  onClick?: () => void;
  danger?: boolean;
  right?: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={cx(
        "flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-sm hover:bg-hover",
        danger && "text-danger",
      )}
    >
      {icon && <span className="flex w-5 justify-center text-ink-2">{icon}</span>}
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {right}
    </button>
  );
}

export function MenuSeparator() {
  return <div className="my-1 h-px bg-line" />;
}

export function Dialog({
  title,
  onClose,
  children,
  width = 480,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  width?: number;
}) {
  const ref = useDismiss(true, onClose);
  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 pt-[12vh]">
      <div ref={ref} className="max-h-[76vh] overflow-auto rounded-xl bg-popover shadow-pop" style={{ width }}>
        <div className="flex items-center justify-between px-5 pt-4 pb-2">
          <h2 className="text-base font-semibold">{title}</h2>
          <IconButton onClick={onClose} aria-label="Close">
            <X size={16} />
          </IconButton>
        </div>
        <div className="px-5 pb-5">{children}</div>
      </div>
    </div>
  );
}

export function Spinner({ className }: { className?: string }) {
  return (
    <span
      className={cx("inline-block h-4 w-4 animate-spin rounded-full border-2 border-ink-3 border-t-transparent", className)}
    />
  );
}

export function relativeTime(iso: string): string {
  const seconds = Math.round((Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}
