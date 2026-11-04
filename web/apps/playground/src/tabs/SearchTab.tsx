import type { Hit, Labels, Query, SnapshotScope } from "@aqora/pisa-client";
import { useDebouncedValue, useDecl, useResolve, useSearch } from "@aqora/pisa-react";
import { useState } from "react";

import { Badge, ErrorNote, Field, Goals, Panel, inputClass } from "../components/ui.tsx";

const MODES = {
  q: "anything",
  nl: "a description",
  name: "a name",
  latex: "LaTeX",
  pattern: "a Lean pattern",
  goal: "a goal",
} as const;

const split = (text: string) =>
  text
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s !== "");

function parseLabels(text: string): Labels {
  return Object.fromEntries(split(text).map((p) => [p.split("=")[0]?.trim() ?? "", p.split("=").slice(1).join("=").trim()]));
}

function HitView({ hit, onOpen }: { hit: Hit; onOpen: (name: string) => void }) {
  return (
    <li className="space-y-1 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" className="font-mono text-sm font-medium text-indigo-700 hover:underline" onClick={() => onOpen(hit.name)}>
          {hit.name}
        </button>
        <Badge>{hit.kind}</Badge>
        <span className="text-xs text-slate-500">{hit.module}</span>
        {hit.flags.uses_sorry === true && <Badge tone="amber">uses sorry</Badge>}
        {hit.flags.deprecated_to !== undefined && <Badge tone="amber">deprecated: {hit.flags.deprecated_to}</Badge>}
        {hit.snapshot !== undefined && <Badge tone="indigo">snapshot</Badge>}
        <span className="ml-auto text-xs text-slate-400">{hit.score.toFixed(3)}</span>
      </div>
      <pre className="whitespace-pre-wrap font-mono text-xs text-slate-800">{hit.signature}</pre>
      {hit.summary !== undefined && <p className="text-sm text-slate-600">{hit.summary}</p>}
      {hit.source_url !== undefined && (
        <a className="text-xs text-indigo-600 hover:underline" href={hit.source_url} target="_blank" rel="noreferrer">
          source
        </a>
      )}
    </li>
  );
}

function Names({ names, onOpen }: { names: string[]; onOpen: (name: string) => void }) {
  return (
    <div className="flex flex-wrap gap-1">
      {names.map((n) => (
        <button key={n} type="button" className="rounded bg-slate-100 px-1.5 py-0.5 font-mono text-xs hover:bg-indigo-50" onClick={() => onOpen(n)}>
          {n}
        </button>
      ))}
    </div>
  );
}

function DeclView({ name, onOpen }: { name: string; onOpen: (name: string) => void }) {
  const decl = useDecl(name);
  if (decl.isPending) return <Panel title={name}>Loading…</Panel>;
  if (decl.isError) return <ErrorNote error={decl.error} />;
  const d = decl.data;
  return (
    <Panel title={<span className="font-mono">{d.name}</span>}>
      <div className="space-y-3">
        <div className="flex flex-wrap gap-2">
          <Badge>{d.kind}</Badge>
          <span className="text-xs text-slate-500">
            {d.library} · {d.module}
          </span>
        </div>
        <Goals goals={[d.type_full]} />
        {d.docstring !== undefined && <p className="text-sm text-slate-700">{d.docstring}</p>}
        {d.informal !== undefined && (
          <div className="rounded bg-indigo-50 p-2 text-sm text-indigo-900">
            <p className="font-medium">{d.informal.informal_name}</p>
            <p>{d.informal.statement_nl}</p>
          </div>
        )}
        {d.deps.length > 0 && (
          <Field label="Uses">
            <Names names={d.deps} onOpen={onOpen} />
          </Field>
        )}
        {d.used_by.length > 0 && (
          <Field label="Used by">
            <Names names={d.used_by} onOpen={onOpen} />
          </Field>
        )}
      </div>
    </Panel>
  );
}

function Resolver({ onOpen }: { onOpen: (name: string) => void }) {
  const [text, setText] = useState("");
  const names = split(useDebouncedValue(text, 300));
  const resolved = useResolve(names);
  return (
    <Panel title="Did you mean…">
      <input className={`${inputClass} w-full font-mono`} value={text} onChange={(e) => setText(e.target.value)} placeholder="Real.sqrt_mul_sefl, Nat.add_com" />
      <ErrorNote error={resolved.error} />
      <ul className="mt-2 space-y-1 text-sm">
        {resolved.data?.map((r) => (
          <li key={r.name} className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-xs">{r.name}</span>
            {r.exists ? <Badge tone="green">exists</Badge> : <Badge tone="amber">unknown</Badge>}
            {r.suggestions !== undefined && <Names names={r.suggestions.map((s) => s.name)} onOpen={onOpen} />}
          </li>
        ))}
      </ul>
    </Panel>
  );
}

export function SearchTab() {
  const [mode, setMode] = useState<keyof typeof MODES>("q");
  const [text, setText] = useState("");
  const [libraries, setLibraries] = useState("");
  const [kinds, setKinds] = useState("");
  const [k, setK] = useState(10);
  const [sorried, setSorried] = useState(false);
  const [scope, setScope] = useState<"none" | "all" | "labels">("none");
  const [scopeLabels, setScopeLabels] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const snapshots: SnapshotScope = scope === "labels" ? { labels: parseLabels(scopeLabels) } : scope;
  const query: Query = {
    [mode]: useDebouncedValue(text, 300),
    libraries: split(libraries),
    kinds: split(kinds),
    k,
    include_sorried: sorried,
    snapshots,
  };
  const search = useSearch(query);
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_28rem]">
      <div className="space-y-4">
        <Panel>
          <div className="flex gap-2">
            <select className={inputClass} value={mode} onChange={(e) => setMode(e.target.value as keyof typeof MODES)}>
              {Object.entries(MODES).map(([m, label]) => (
                <option key={m} value={m}>
                  {label}
                </option>
              ))}
            </select>
            <input
              className={`${inputClass} flex-1`}
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder="trace of a density matrix is one"
              autoFocus
            />
          </div>
          <div className="mt-3 grid gap-3 sm:grid-cols-5">
            <Field label="Libraries">
              <input className={inputClass} value={libraries} onChange={(e) => setLibraries(e.target.value)} placeholder="Mathlib" />
            </Field>
            <Field label="Kinds">
              <input className={inputClass} value={kinds} onChange={(e) => setKinds(e.target.value)} placeholder="theorem" />
            </Field>
            <Field label="Hits">
              <input className={inputClass} type="number" min={1} max={50} value={k} onChange={(e) => setK(Number(e.target.value))} />
            </Field>
            <Field label="Snapshots">
              <select className={inputClass} value={scope} onChange={(e) => setScope(e.target.value as typeof scope)}>
                <option value="none">none</option>
                <option value="all">all</option>
                <option value="labels">with labels</option>
              </select>
            </Field>
            <label className="flex items-end gap-1.5 pb-1 text-xs font-medium text-slate-600">
              <input type="checkbox" checked={sorried} onChange={(e) => setSorried(e.target.checked)} />
              include sorried
            </label>
          </div>
          {scope === "labels" && (
            <input className={`${inputClass} mt-3 w-full`} value={scopeLabels} onChange={(e) => setScopeLabels(e.target.value)} placeholder="run=7" />
          )}
        </Panel>
        <ErrorNote error={search.error} />
        {search.data?.warnings?.map((w) => (
          <p key={w} className="text-sm text-amber-800">
            {w}
          </p>
        ))}
        {search.data !== undefined && (
          <Panel title={`${search.data.hits.length} hits${search.isFetching ? " (updating…)" : ""}`}>
            <ul className="divide-y divide-slate-100">
              {search.data.hits.map((hit) => (
                <HitView key={`${hit.module}.${hit.name}`} hit={hit} onOpen={setOpen} />
              ))}
            </ul>
          </Panel>
        )}
      </div>
      <div className="space-y-4">
        {open !== null && <DeclView name={open} onOpen={setOpen} />}
        <Resolver onOpen={setOpen} />
      </div>
    </div>
  );
}
