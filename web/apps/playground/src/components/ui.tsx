import { type CheckResult, type Message, type PisaError, isPisaError } from "@aqora/pisa-client";
import { type ButtonHTMLAttributes, type ReactNode, useState } from "react";

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

export function Panel({ title, actions, children }: { title?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="rounded-lg border border-slate-200 bg-white p-4 shadow-sm">
      {(title !== undefined || actions !== undefined) && (
        <div className="mb-3 flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-slate-900">{title}</h2>
          <div className="flex items-center gap-2">{actions}</div>
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

/** What went wrong, in words a person can act on. */
export function ErrorNote({ error, onRetry }: { error: PisaError | null | undefined; onRetry?: () => void }) {
  if (error === null || error === undefined) return null;
  let text = error.message;
  if (isPisaError(error, "capacity")) {
    text = "No worker took the job in time: they are all busy, or none serves this version.";
  } else if (isPisaError(error, "network")) {
    text = "The server didn't answer.";
  }
  return (
    <div className="flex items-start justify-between gap-3 rounded-md bg-rose-50 p-3 text-sm text-rose-800">
      <div>
        <span className="font-semibold">{error.kind}</span>
        {error.status !== undefined && <span className="text-rose-600"> ({error.status})</span>}: {text}
      </div>
      {onRetry !== undefined && <SubtleButton onClick={onRetry}>Retry</SubtleButton>}
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
          <pre className="whitespace-pre-wrap font-mono text-xs text-slate-800">{m.data}</pre>
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

/** A check's verdict, and why. */
export function CheckResultView({ result }: { result: CheckResult }) {
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        {result.ok ? <Badge tone="green">✓ accepted</Badge> : <Badge tone="red">✗ not accepted</Badge>}
        {result.error !== undefined && <Badge tone="amber">{result.error.kind}</Badge>}
        <span className="text-xs text-slate-500">{result.time.toFixed(2)} s</span>
      </div>
      {result.error !== undefined && <p className="text-sm text-amber-800">{result.error.message}</p>}
      {result.reasons.length > 0 && (
        <ul className="list-inside list-disc text-sm text-rose-800">
          {result.reasons.map((r) => (
            <li key={r}>{r}</li>
          ))}
        </ul>
      )}
      {result.declarations.length > 0 && (
        <table className="w-full text-left text-xs">
          <thead className="text-slate-500">
            <tr>
              <th className="py-1 font-medium">Declaration</th>
              <th className="py-1 font-medium">Kind</th>
              <th className="py-1 font-medium">Axioms</th>
            </tr>
          </thead>
          <tbody className="font-mono text-slate-800">
            {result.declarations.map((d) => (
              <tr key={d.name} className="border-t border-slate-100">
                <td className="py-1">{d.name}</td>
                <td className="py-1">{d.kind}</td>
                <td className="py-1">{d.axioms.join(", ") || "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {result.sorries.length > 0 && (
        <div className="space-y-1">
          <p className="text-xs font-medium text-slate-600">Sorries</p>
          <Goals goals={result.sorries.map((s) => s.goal)} />
        </div>
      )}
      <Messages messages={result.messages} />
    </div>
  );
}
