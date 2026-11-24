import {
  createContext,
  forwardRef,
  useCallback,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from "react";
import { createPortal } from "react-dom";
import { t } from "../strings";
import { XIcon } from "./icons";

export function cx(...c: (string | false | null | undefined)[]): string {
  return c.filter(Boolean).join(" ");
}

// ---------------------------------------------------------------- buttons

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md" | "lg";

const variants: Record<Variant, string> = {
  primary: "bg-accent text-accent-fg hover:brightness-105 active:brightness-95 shadow-sm",
  secondary: "bg-surface text-text border border-border hover:bg-surface-2",
  ghost: "text-text hover:bg-surface-2",
  danger: "bg-danger text-white hover:brightness-105",
};
const sizes: Record<Size, string> = {
  sm: "h-7 px-2.5 text-[13px] gap-1.5 rounded-md",
  md: "h-8 px-3 text-sm gap-2 rounded-lg",
  lg: "h-10 px-4 text-[15px] gap-2 rounded-lg",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", className, type = "button", ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      className={cx(
        "inline-flex items-center justify-center font-medium whitespace-nowrap transition select-none",
        "disabled:opacity-50 disabled:pointer-events-none",
        variants[variant],
        sizes[size],
        className,
      )}
      {...rest}
    />
  );
});

export function IconButton({
  label,
  className,
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cx(
        "inline-flex h-7 w-7 items-center justify-center rounded-md text-muted hover:bg-surface-2 hover:text-text transition",
        "disabled:opacity-40 disabled:pointer-events-none",
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}

// ---------------------------------------------------------------- form controls

/** Full width unless the caller sets its own width (Tailwind can't resolve two w-* classes by order). */
const width = (className?: string) => (/(^|\s)w-/.test(className ?? "") ? "" : "w-full");

const control =
  "rounded-lg border border-border bg-surface px-2.5 text-sm text-text placeholder:text-muted/70 " +
  "focus:outline-none focus:ring-2 focus:ring-accent/40 focus:border-accent disabled:opacity-60";

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input(
  { className, ...rest },
  ref,
) {
  return <input ref={ref} className={cx(control, width(className), "h-8", className)} {...rest} />;
});

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(
  function Textarea({ className, ...rest }, ref) {
    return <textarea ref={ref} className={cx(control, width(className), "py-1.5 min-h-[64px] resize-y", className)} {...rest} />;
  },
);

export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select(
  { className, children, ...rest },
  ref,
) {
  return (
    <select ref={ref} className={cx(control, width(className), "h-8 pr-7", className)} {...rest}>
      {children}
    </select>
  );
});

export function Field({
  label,
  hint,
  error,
  children,
  className,
  htmlFor,
}: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
  className?: string;
  htmlFor?: string;
}) {
  return (
    <div className={cx("flex flex-col gap-1", className)}>
      <label htmlFor={htmlFor} className="text-[13px] font-medium text-text">
        {label}
      </label>
      {children}
      {error ? (
        <p className="text-xs text-danger" role="alert">
          {error}
        </p>
      ) : hint ? (
        <p className="text-xs text-muted">{hint}</p>
      ) : null}
    </div>
  );
}

export function Checkbox({
  checked,
  onChange,
  label,
  disabled,
  className,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <label className={cx("inline-flex items-center gap-2 text-sm", disabled && "opacity-50", className)}>
      <input
        type="checkbox"
        className="h-4 w-4 rounded accent-[var(--accent)]"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      {label}
    </label>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  id?: string;
}) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className={cx(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition",
        checked ? "bg-accent" : "bg-border",
      )}
    >
      <span
        className={cx(
          "inline-block h-4 w-4 rounded-full bg-white shadow transition",
          checked ? "translate-x-[18px]" : "translate-x-0.5",
        )}
      />
    </button>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  label: string;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="inline-flex rounded-lg border border-border bg-surface-2 p-0.5">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={value === o.value}
          onClick={() => onChange(o.value)}
          className={cx(
            "h-7 rounded-md px-3 text-[13px] font-medium transition",
            value === o.value ? "bg-surface text-text shadow-sm" : "text-muted hover:text-text",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------- display

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cx("rounded-xl border border-border bg-surface", className)}>{children}</div>;
}

export function Badge({
  children,
  tone = "neutral",
  title,
}: {
  children: ReactNode;
  tone?: "neutral" | "accent" | "warn" | "danger" | "info";
  title?: string;
}) {
  const tones = {
    neutral: "bg-surface-2 text-muted",
    accent: "bg-mint-500/15 text-mint-700 dark:text-mint-300",
    warn: "bg-amber-500/15 text-amber-700 dark:text-amber-300",
    danger: "bg-red-500/15 text-red-700 dark:text-red-300",
    info: "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  };
  return (
    <span
      title={title}
      className={cx("inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] font-medium", tones[tone])}
    >
      {children}
    </span>
  );
}

export function ColorDot({ color, className }: { color: string | null | undefined; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cx("inline-block h-2.5 w-2.5 shrink-0 rounded-full", className)}
      style={{ background: color || "var(--muted)" }}
    />
  );
}

export function EmptyState({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 px-6 py-14 text-center">
      <p className="text-[15px] font-medium text-text">{title}</p>
      {children && <div className="max-w-sm text-sm text-muted">{children}</div>}
      {action}
    </div>
  );
}

export function Spinner() {
  return (
    <div className="flex items-center justify-center p-10 text-muted" role="status">
      {t.common.loading}
    </div>
  );
}

export function PageHeader({ title, actions, children }: { title: string; actions?: ReactNode; children?: ReactNode }) {
  return (
    <header data-tauri-drag-region className="flex min-h-[60px] flex-wrap items-center gap-3 border-b border-border px-6 py-3">
      <h1 data-tauri-drag-region className="text-lg font-semibold tracking-tight">{title}</h1>
      {children}
      <div className="ml-auto flex items-center gap-2">{actions}</div>
    </header>
  );
}

// ---------------------------------------------------------------- modal

export function Modal({
  open,
  onClose,
  title,
  children,
  footer,
  width = 480,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  // Parents often pass a new inline onClose on every render; keep the latest one in a ref so the
  // focus effect below only re-runs when `open` changes and never steals focus mid-typing (#10).
  const onCloseRef = useRef(onClose);
  useLayoutEffect(() => {
    onCloseRef.current = onClose;
  });
  useEffect(() => {
    if (!open) return;
    const prev = document.activeElement as HTMLElement | null;
    const el = ref.current;
    // A React autoFocus child is already focused during commit; leave focus where it is then.
    if (!el?.contains(document.activeElement)) {
      const first = el?.querySelector<HTMLElement>("[autofocus], input, select, textarea, button:not([data-close])");
      first?.focus();
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onCloseRef.current();
      }
      if (e.key === "Tab" && el) {
        const items = Array.from(
          el.querySelectorAll<HTMLElement>(
            'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
          ),
        ).filter((n) => !n.hasAttribute("disabled"));
        if (items.length === 0) return;
        const firstEl = items[0];
        const lastEl = items[items.length - 1];
        if (e.shiftKey && document.activeElement === firstEl) {
          e.preventDefault();
          lastEl.focus();
        } else if (!e.shiftKey && document.activeElement === lastEl) {
          e.preventDefault();
          firstEl.focus();
        }
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      prev?.focus?.();
    };
  }, [open]);

  if (!open) return null;
  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto bg-black/30 p-6 pt-[10vh] backdrop-blur-[1px]"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="w-full rounded-xl border border-border bg-surface shadow-2xl"
        style={{ maxWidth: width }}
      >
        <div className="flex items-center justify-between border-b border-border px-5 py-3">
          <h2 id={titleId} className="text-[15px] font-semibold">
            {title}
          </h2>
          <IconButton label={t.common.close} onClick={onClose} data-close>
            <XIcon />
          </IconButton>
        </div>
        <div className="px-5 py-4">{children}</div>
        {footer && <div className="flex justify-end gap-2 border-t border-border px-5 py-3">{footer}</div>}
      </div>
    </div>,
    document.body,
  );
}

// ---------------------------------------------------------------- confirm + toast

interface ConfirmOptions {
  title: string;
  body?: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
}

interface Toast {
  id: number;
  text: string;
  tone: "info" | "error" | "warn";
}

interface UiApi {
  confirm: (o: ConfirmOptions) => Promise<boolean>;
  toast: (text: string, tone?: Toast["tone"]) => void;
}

const UiContext = createContext<UiApi | null>(null);

export function UiProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<(ConfirmOptions & { resolve: (v: boolean) => void }) | null>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);

  const confirm = useCallback(
    (o: ConfirmOptions) => new Promise<boolean>((resolve) => setPending({ ...o, resolve })),
    [],
  );
  const toast = useCallback((text: string, tone: Toast["tone"] = "info") => {
    const id = Date.now() + Math.random();
    setToasts((ts) => [...ts, { id, text, tone }]);
    setTimeout(() => setToasts((ts) => ts.filter((x) => x.id !== id)), tone === "error" ? 7000 : 4000);
  }, []);

  const close = (v: boolean) => {
    pending?.resolve(v);
    setPending(null);
  };

  return (
    <UiContext.Provider value={{ confirm, toast }}>
      {children}
      <Modal
        open={!!pending}
        onClose={() => close(false)}
        title={pending?.title ?? ""}
        width={420}
        footer={
          <>
            <Button onClick={() => close(false)}>{t.common.cancel}</Button>
            <Button variant={pending?.danger ? "danger" : "primary"} onClick={() => close(true)} autoFocus>
              {pending?.confirmLabel ?? t.common.confirm}
            </Button>
          </>
        }
      >
        <div className="text-sm text-muted">{pending?.body}</div>
      </Modal>
      <div className="pointer-events-none fixed bottom-4 right-4 z-[60] flex flex-col items-end gap-2" aria-live="polite">
        {toasts.map((x) => (
          <div
            key={x.id}
            role={x.tone === "error" ? "alert" : "status"}
            className={cx(
              "pointer-events-auto max-w-sm rounded-lg border px-3.5 py-2.5 text-sm shadow-lg",
              x.tone === "error"
                ? "border-red-500/40 bg-red-50 text-red-800 dark:bg-red-950 dark:text-red-200"
                : x.tone === "warn"
                  ? "border-amber-500/40 bg-amber-50 text-amber-900 dark:bg-amber-950 dark:text-amber-200"
                  : "border-border bg-surface text-text",
            )}
          >
            {x.text}
          </div>
        ))}
      </div>
    </UiContext.Provider>
  );
}

export function useUi(): UiApi {
  const ctx = useContext(UiContext);
  if (!ctx) throw new Error("UiProvider missing");
  return ctx;
}
