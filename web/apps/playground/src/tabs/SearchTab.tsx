import type { Hit, Labels, Query, SnapshotScope } from "@aqora/pisa-client";
import { useDebouncedValue, useDecl, useResolve, useSearch } from "@aqora/pisa-react";
import { type FormEvent, useEffect, useState } from "react";

import { Badge, Button, ErrorNote, Field, Goals, Panel, Spinner, SubtleButton, inputClass } from "../components/ui.tsx";

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

/** Whole seconds since `active` last became true. */
function useElapsed(active: boolean): number {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    if (!active) return;
    setSeconds(0);
    const started = Date.now();
    const timer = setInterval(() => setSeconds(Math.floor((Date.now() - started) / 1000)), 250);
    return () => clearInterval(timer);
  }, [active]);
  return seconds;
}

function Searching({ seconds }: { seconds: number }) {
  return (
    <span className="inline-flex items-center gap-2 text-sm font-normal text-slate-500">
      <Spinner />
      Searching…{seconds >= 2 && ` ${seconds} s`}
    </span>
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
  // What was last asked for, with Enter or the button; nothing runs as you type.
  const [submitted, setSubmitted] = useState<Query | null>(null);
  const search = useSearch(submitted ?? {});
  const elapsed = useElapsed(search.isFetching);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const snapshots: SnapshotScope = scope === "labels" ? { labels: parseLabels(scopeLabels) } : scope;
    const query: Query = {
      [mode]: text,
      libraries: split(libraries),
      kinds: split(kinds),
      k,
      include_sorried: sorried,
      snapshots,
    };
    // The same query again searches again.
    if (JSON.stringify(query) === JSON.stringify(submitted)) void search.refetch();
    else setSubmitted(query);
  };
  // The previous query's hits stay up while the next ones load.
  const result = search.isPlaceholderData ? undefined : search.data;
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_28rem]">
      <div className="space-y-4">
        <Panel>
          <form onSubmit={submit}>
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
              <Button type="submit" disabled={text.trim() === ""} className="inline-flex items-center gap-2">
                {search.isFetching ? (
                  <>
                    <Spinner />
                    Searching…
                  </>
                ) : (
                  "Search"
                )}
              </Button>
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
          </form>
        </Panel>
        <ErrorNote error={search.error} />
        {result?.warnings !== undefined && result.warnings.length > 0 && (
          <div className="flex flex-wrap items-start gap-3 rounded-md bg-amber-50 p-3 text-sm text-amber-900 ring-1 ring-amber-200">
            <div className="min-w-0 flex-1 space-y-1">
              {result.warnings.map((w) => (
                <p key={w}>{w}</p>
              ))}
            </div>
            {result.dense_pending === true && (
              <SubtleButton onClick={() => void search.refetch()} disabled={search.isFetching} className="inline-flex items-center gap-2">
                {search.isFetching ? (
                  <>
                    <Spinner />
                    Trying…
                  </>
                ) : (
                  "Try again"
                )}
              </SubtleButton>
            )}
          </div>
        )}
        {search.data === undefined && search.isFetching && (
          <Panel>
            <Searching seconds={elapsed} />
          </Panel>
        )}
        {search.data !== undefined && (
          <Panel
            title={
              <span className="flex items-center gap-3">
                {search.data.hits.length} hit{search.data.hits.length === 1 ? "" : "s"}
                {search.isFetching && <Searching seconds={elapsed} />}
              </span>
            }
          >
            <ul className={`divide-y divide-slate-100 transition-opacity ${search.isFetching ? "opacity-50" : ""}`}>
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
