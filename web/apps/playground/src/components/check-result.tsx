import type { AtPosition, CheckResult, Declaration, DeclarationChange, Replacement } from "@aqora/pisa-client";
import { useState } from "react";

import { Badge, Goals, Messages, StatusBadge } from "./ui.tsx";

/** What a change to a declaration is, in words. */
const CHANGE: Record<DeclarationChange["change"], string> = {
  missing: "is missing",
  kind: "is another kind",
  type: "has another type",
  value: "has another value",
  derived: "changed with its type",
  added_assumption: "is a new assumption",
  assumption: "is another assumption",
  ambiguous: "is ambiguous",
};

/** Declarations that don't keep the statements they must. */
export function ChangesView({ changes }: { changes: DeclarationChange[] }) {
  if (changes.length === 0) return null;
  return (
    <ul className="space-y-2">
      {changes.map((c) => (
        <li key={`${c.module ?? ""}.${c.name}.${c.change}`} className="space-y-1 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-xs font-medium text-slate-900">{c.name}</span>
            <Badge tone="red">{CHANGE[c.change]}</Badge>
            {c.module !== undefined && <span className="font-mono text-xs text-slate-500">in {c.module}</span>}
          </div>
          {(c.was !== undefined || c.now !== undefined) && (
            <div className="grid gap-1 sm:grid-cols-2">
              <pre className="whitespace-pre-wrap rounded bg-rose-50 p-2 font-mono text-xs text-rose-900">{c.was ?? "—"}</pre>
              <pre className="whitespace-pre-wrap rounded bg-emerald-50 p-2 font-mono text-xs text-emerald-900">{c.now ?? "—"}</pre>
            </div>
          )}
          {c.includes !== undefined && c.includes.length > 0 && (
            <p className="font-mono text-xs text-slate-600">including {c.includes.join(", ")}</p>
          )}
        </li>
      ))}
    </ul>
  );
}

/** What a replacement changed: the sorries it filled, the definitions it
 *  was allowed to change, and the theorems whose type uses one of them. */
export function ReplacementView({ replacement }: { replacement: Replacement }) {
  const rows: [string, string[], "green" | "amber"][] = [
    ["Filled", replacement.filled, "green"],
    ["Changed definitions", replacement.changed_definitions ?? [], "amber"],
    ["Theorems that use a changed definition", replacement.affected ?? [], "amber"],
  ];
  if (rows.every(([, names]) => names.length === 0)) return <p className="text-sm text-slate-500">The replacement fills no sorry and changes no definition.</p>;
  return (
    <div className="space-y-1">
      {rows.map(
        ([label, names, tone]) =>
          names.length > 0 && (
            <div key={label} className="flex flex-wrap items-center gap-1.5">
              <span className="text-xs font-medium text-slate-600">{label}</span>
              {names.map((n) => (
                <Badge key={n} tone={tone}>
                  <span className="font-mono">{n}</span>
                </Badge>
              ))}
            </div>
          ),
      )}
    </div>
  );
}

/** A declaration's fields that only a check in full has. */
function Full({ d }: { d: Declaration }) {
  const rows: [string, string | undefined][] = [
    ["assumes", d.assumes?.join(", ")],
    ["parent", d.parent],
    ["type hash", d.type_hash],
    ["value hash", d.value_hash],
    ["type uses", d.type_refs?.join(", ")],
    ["value uses", d.value_refs?.join(", ")],
  ];
  return (
    <dl className="grid grid-cols-[8rem_minmax(0,1fr)] gap-x-2 gap-y-0.5 py-1 text-xs">
      {rows.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-slate-500">{k}</dt>
          <dd className="truncate font-mono text-slate-800" title={v}>
            {v === undefined || v === "" ? "—" : v}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function Declarations({ declarations }: { declarations: Declaration[] }) {
  const [generated, setGenerated] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  // Only a declaration in full has a type hash.
  const full = declarations.some((d) => d.type_hash !== undefined);
  const made = declarations.filter((d) => d.generated === true).length;
  const shown = generated ? declarations : declarations.filter((d) => d.generated !== true);
  return (
    <div className="space-y-1">
      <table className="w-full text-left text-xs">
        <thead className="text-slate-500">
          <tr>
            <th className="py-1 font-medium">Declaration</th>
            <th className="py-1 font-medium">Kind</th>
            <th className="py-1 font-medium">Status</th>
            <th className="py-1 font-medium">{full ? "Axioms" : "Axioms beyond the allowed"}</th>
          </tr>
        </thead>
        <tbody className="font-mono text-slate-800">
          {shown.map((d) => (
            <DeclarationRow key={d.name} d={d} full={full} open={open === d.name} onToggle={() => setOpen(open === d.name ? null : d.name)} />
          ))}
        </tbody>
      </table>
      {made > 0 && (
        <button type="button" className="text-xs text-indigo-600 hover:underline" onClick={() => setGenerated(!generated)}>
          {generated ? "Hide" : "Show"} the {made} that Lean generated
        </button>
      )}
    </div>
  );
}

function DeclarationRow({ d, full, open, onToggle }: { d: Declaration; full: boolean; open: boolean; onToggle: () => void }) {
  return (
    <>
      <tr className="border-t border-slate-100">
        <td className="py-1">
          {full ? (
            <button type="button" className="text-left hover:underline" aria-expanded={open} onClick={onToggle}>
              {d.name}
            </button>
          ) : (
            d.name
          )}
        </td>
        <td className="py-1">{d.kind}</td>
        <td className="py-1">
          <StatusBadge status={d.status} />
        </td>
        <td className="py-1">{d.axioms.join(", ") || "—"}</td>
      </tr>
      {open && (
        <tr>
          <td colSpan={4}>
            <Full d={d} />
          </td>
        </tr>
      )}
    </>
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
      {result.error !== undefined && <p className="whitespace-pre-wrap text-sm text-amber-800">{result.error.message}</p>}
      {result.reasons.length > 0 && (
        <ul className="list-inside list-disc text-sm text-rose-800">
          {result.reasons.map((r) => (
            <li key={r}>{r}</li>
          ))}
        </ul>
      )}
      <ChangesView changes={result.changes ?? []} />
      {result.declarations.length > 0 && <Declarations declarations={result.declarations} />}
      {result.sorries.length > 0 && (
        <div className="space-y-1">
          <p className="text-xs font-medium text-slate-600">Sorries</p>
          {result.sorries.map((s, i) => (
            <div key={i} className="flex items-start gap-2">
              {s.pos !== undefined && (
                <Badge tone="amber">
                  {s.pos.line}:{s.pos.column}
                </Badge>
              )}
              <div className="min-w-0 flex-1">
                <Goals goals={[s.goal]} />
              </div>
            </div>
          ))}
        </div>
      )}
      <Messages messages={result.messages} />
    </div>
  );
}

/** What Lean recorded at each position a check was asked about: the tactic
 *  there with its goals, and with `terms` the term and its expected type. */
export function AtView({ at }: { at: AtPosition[] }) {
  return (
    <ul className="space-y-3">
      {at.map((a, i) => (
        <li key={i} className="space-y-1.5">
          <Badge tone="indigo">
            {a.pos.line}:{a.pos.column}
          </Badge>
          {a.tactic === undefined && a.term === undefined && <p className="text-sm text-slate-500">Lean recorded nothing here.</p>}
          {a.tactic !== undefined && (
            <div className="space-y-1">
              <p className="font-mono text-xs text-slate-700">{a.tactic.tactic}</p>
              <p className="text-xs font-medium text-slate-600">Before</p>
              <Goals goals={a.tactic.goals_before} />
              <p className="text-xs font-medium text-slate-600">After</p>
              <Goals goals={a.tactic.goals_after} />
            </div>
          )}
          {a.term !== undefined && (
            <div className="space-y-1">
              <p className="font-mono text-xs text-slate-700">{a.term.term}</p>
              <p className="text-xs font-medium text-slate-600">Expected type</p>
              <Goals goals={[a.term.expected_type]} />
              <p className="text-xs font-medium text-slate-600">As Lean elaborated it</p>
              <pre className="whitespace-pre-wrap font-mono text-xs text-slate-800">{a.term.expr}</pre>
            </div>
          )}
        </li>
      ))}
    </ul>
  );
}
