import type { Message, PisaError, PisaErrorKind, ProofStatus } from "@aqora/pisa-client";
import { type ButtonHTMLAttributes, type ReactNode, useId, useState } from "react";

export function Button({ className, ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      type="button"
      className={`rounded-md bg-indigo-600 px-3 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-indigo-500 disabled:cursor-not-allowed disabled:opacity-50 ${className ?? ""}`}
      {...props}
    />
  );
}

export function SubtleButton({ className, ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      type="button"
      className={`rounded-md border border-slate-300 bg-white px-2.5 py-1 text-sm text-slate-700 hover:bg-slate-50 disabled:cursor-not-allowed disabled:opacity-50 ${className ?? ""}`}
      {...props}
    />
  );
}

/** A small spinning ring, in the text's colour. */
export function Spinner() {
  return <span aria-hidden className="inline-block size-3.5 animate-spin rounded-full border-2 border-current border-r-transparent" />;
}

const TONES = {
  green: "bg-emerald-50 text-emerald-700 ring-emerald-600/20",
  red: "bg-rose-50 text-rose-700 ring-rose-600/20",
  amber: "bg-amber-50 text-amber-800 ring-amber-600/20",
  slate: "bg-slate-100 text-slate-700 ring-slate-500/20",
  indigo: "bg-indigo-50 text-indigo-700 ring-indigo-600/20",
};

export function Badge({ tone = "slate", children }: { tone?: keyof typeof TONES; children: ReactNode }) {
  return (
    <span className={`inline-flex items-center rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset ${TONES[tone]}`}>
      {children}
    </span>
  );
}

/** A titled region of a tab; its title names it, for screen readers and
 *  tests. */
export function Panel({ title, actions, children }: { title?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  const id = useId();
  return (
    <section className="rounded-lg border border-slate-200 bg-white p-4 shadow-sm" {...(title !== undefined && { "aria-labelledby": id })}>
      {(title !== undefined || actions !== undefined) && (
        <div className="mb-3 flex items-center justify-between gap-2">
          <h2 id={id} className="text-sm font-semibold text-slate-900">
            {title}
          </h2>
          <div className="flex flex-wrap items-center justify-end gap-2">{actions}</div>
        </div>
      )}
      {children}
    </section>
  );
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-xs font-medium text-slate-600">
      {label}
      {children}
    </label>
  );
}

export const inputClass =
  "rounded-md border border-slate-300 bg-white px-2 py-1 text-sm text-slate-900 shadow-sm focus:border-indigo-500 focus:outline-none";

/** What to do about each kind of error, unless a tab says better. */
const HINTS: Partial<Record<PisaErrorKind, string>> = {
  capacity: "No worker took the job in time: they are all busy, or none serves this version.",
  network: "The server didn't answer.",
  unavailable:
    "No worker can take it now. Sessions and documents need a worker with a Lean Beam slot (PISA_BEAM_SLOTS); otherwise, try again shortly.",
  timeout: "Lean ran out of time: give it a longer timeout, or less to do.",
  out_of_memory: "Lean ran out of memory: give it less to do.",
  conflict: "It changed meanwhile: look again, then try again.",
  not_found: "It isn't there: it was deleted, closed, or has expired.",
};

/** What went wrong, in words a person can act on: a hint by kind, which
 *  `hints` can replace, then what the server said. An aborted request shows
 *  nothing. */
export function ErrorNote({
  error,
  onRetry,
  hints,
}: {
  error: PisaError | null | undefined;
  onRetry?: () => void;
  hints?: Partial<Record<PisaErrorKind, string>>;
}) {
  if (error === null || error === undefined || error.kind === "aborted") return null;
  const hint = hints?.[error.kind] ?? HINTS[error.kind];
  return (
    <div role="alert" className="flex items-start justify-between gap-3 rounded-md bg-rose-50 p-3 text-sm text-rose-800">
      <div className="min-w-0 space-y-1">
        <p>
          <span className="font-semibold">{error.kind}</span>
          {error.status !== undefined && <span className="text-rose-600"> ({error.status})</span>}: {hint ?? error.message}
        </p>
        {hint !== undefined && error.message !== "" && (
          <pre className="whitespace-pre-wrap font-mono text-xs text-rose-700">{error.message}</pre>
        )}
      </div>
      {onRetry !== undefined && <SubtleButton onClick={onRetry}>Retry</SubtleButton>}
    </div>
  );
}

/** A button that acts on a second click, after saying what it will do. */
export function ConfirmButton({
  onConfirm,
  confirm,
  disabled,
  children,
}: {
  onConfirm: () => void;
  confirm: string;
  disabled?: boolean;
  children: ReactNode;
}) {
  const [armed, setArmed] = useState(false);
  return (
    <SubtleButton
      disabled={disabled}
      onBlur={() => setArmed(false)}
      onClick={() => {
        setArmed(!armed);
        if (armed) onConfirm();
      }}
    >
      {armed ? confirm : children}
    </SubtleButton>
  );
}

/** Content shown once its summary is clicked. */
export function Disclosure({ summary, children }: { summary: ReactNode; children: ReactNode }) {
  return (
    <details>
      <summary className="cursor-pointer text-xs font-medium text-slate-600 hover:text-slate-900">{summary}</summary>
      <div className="mt-2">{children}</div>
    </details>
  );
}

/** Something to act on that isn't an error. */
export function Notice({ action, children }: { action?: ReactNode; children: ReactNode }) {
  return (
    <div role="status" className="flex items-start justify-between gap-3 rounded-md bg-amber-50 p-3 text-sm text-amber-900">
      <div className="min-w-0">{children}</div>
      {action}
    </div>
  );
}

export function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <SubtleButton
      onClick={() => {
        void navigator.clipboard?.writeText(text).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        });
      }}
    >
      {copied ? "Copied" : "Copy"}
    </SubtleButton>
  );
}

const SEVERITY_TONE = { error: "red", warning: "amber", info: "indigo", trace: "slate" } as const;

export function Messages({ messages }: { messages: Message[] }) {
  if (messages.length === 0) return null;
  return (
    <ul className="space-y-1.5">
      {messages.map((m, i) => (
        <li key={i} className="flex gap-2 text-sm">
          <Badge tone={SEVERITY_TONE[m.severity]}>
            {m.pos.line}:{m.pos.column}
          </Badge>
          <pre className="whitespace-pre-wrap font-mono text-xs text-slate-800">{m.text}</pre>
        </li>
      ))}
    </ul>
  );
}

export function Goals({ goals }: { goals: string[] }) {
  if (goals.length === 0) return <p className="text-sm text-emerald-700">No goals.</p>;
  return (
    <div className="space-y-2">
      {goals.map((g, i) => (
        <pre key={i} className="whitespace-pre-wrap rounded bg-slate-50 p-2 font-mono text-xs text-slate-800">
          {g}
        </pre>
      ))}
    </div>
  );
}

const STATUS_TONE = { proved: "green", conditional: "indigo", sorry: "amber", rejected: "red" } as const;

/** A proof status: proved, conditional on assumptions, sorry, or rejected. */
export function StatusBadge({ status }: { status: ProofStatus }) {
  return <Badge tone={STATUS_TONE[status]}>{status}</Badge>;
}

export function Checkbox({
  checked,
  onChange,
  disabled,
  children,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <label className="flex items-center gap-1.5 text-sm text-slate-700">
      <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      {children}
    </label>
  );
}
