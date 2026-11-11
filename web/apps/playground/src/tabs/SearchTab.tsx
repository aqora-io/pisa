import type { Flags, Hit, Query, SnapshotScope } from "@aqora/pisa-client";
import { type VersionInput, useDebouncedValue, useDeclaration, useResolve, useSearch } from "@aqora/pisa-react";
import { type FormEvent, useState } from "react";

import { SnapshotRef } from "../components/snapshot-ref.tsx";
import { useElapsed } from "../elapsed.ts";
import { Badge, Button, Disclosure, ErrorNote, Field, Goals, Panel, Spinner, StatusBadge, SubtleButton, inputClass } from "../components/ui.tsx";
import { parseLabels, parseList } from "../labels.ts";
import { useGo } from "../nav.tsx";

const MODES = {
  text: "anything",
  description: "a description",
  name: "a name",
  latex: "LaTeX",
  pattern: "a Lean pattern",
  goal: "a goal",
} as const;
type Mode = keyof typeof MODES;

/** A declaration to show: a snapshot's, with its module, or the bundle's. */
interface Opened {
  name: string;
  module?: string;
}

/** An import header that a name needs, with a link to try it. */
function Imports({ imports, name }: { imports: string; name: string }) {
  const go = useGo();
  return (
    <div className="flex items-start gap-2">
      <pre className="min-w-0 flex-1 whitespace-pre-wrap rounded bg-slate-50 p-1.5 font-mono text-xs text-slate-700">{imports}</pre>
      <SubtleButton onClick={() => go({ to: "check", code: `${imports}\n\n#check ${name}\n` })}>Try in Check</SubtleButton>
    </div>
  );
}

/** A declaration's flags, but its imports. */
function FlagBadges({ flags }: { flags: Flags }) {
  return (
    <>
      {flags.uses_sorry === true && <Badge tone="amber">uses sorry</Badge>}
      {flags.informal === true && <Badge tone="amber">informal: stated, not proved</Badge>}
      {flags.deprecated_to !== undefined && <Badge tone="amber">deprecated: {flags.deprecated_to}</Badge>}
      {flags.additive_sibling !== undefined && <Badge>additive: {flags.additive_sibling}</Badge>}
    </>
  );
}

function HitView({ hit, onOpen }: { hit: Hit; onOpen: (open: Opened) => void }) {
  const snapshot = hit.library === "PisaSnap";
  const open = () => onOpen({ name: hit.name, ...(snapshot && { module: hit.module }) });
  const { imports } = hit.flags;
  return (
    <li className="space-y-1 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" className="font-mono text-sm font-medium text-indigo-700 hover:underline" onClick={open}>
          {hit.name}
        </button>
        {hit.title !== undefined && <span className="text-sm text-slate-700">{hit.title}</span>}
        <Badge>{hit.kind}</Badge>
        {snapshot ? <Badge tone="indigo">snapshot</Badge> : <Badge>{hit.library}</Badge>}
        <span className="text-xs text-slate-500">{hit.module}</span>
        {hit.status !== undefined && <StatusBadge status={hit.status} />}
        {hit.verified === true && <Badge tone="indigo">verified</Badge>}
        <FlagBadges flags={hit.flags} />
        <span className="ml-auto text-xs text-slate-400">{hit.score.toFixed(3)}</span>
      </div>
      <pre className="whitespace-pre-wrap font-mono text-xs text-slate-800">{hit.signature}</pre>
      {hit.summary !== undefined && <p className="text-sm text-slate-600">{hit.summary}</p>}
      {imports !== undefined && <Imports imports={imports} name={hit.name} />}
      {hit.snapshot_ids !== undefined && hit.snapshot_ids.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs text-slate-500">heads</span>
          {hit.snapshot_ids.map((id) => (
            <SnapshotRef key={id} id={id} />
          ))}
        </div>
      )}
      <div className="flex flex-wrap items-start gap-3">
        {hit.source_url !== undefined && (
          <a className="text-xs text-indigo-600 hover:underline" href={hit.source_url} target="_blank" rel="noreferrer">
            source
          </a>
        )}
        <Disclosure summary="What a model reads">
          <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{hit.text}</pre>
        </Disclosure>
      </div>
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

function DeclView({
  name,
  module,
  version,
  onOpen,
}: {
  name: string;
  module: string | undefined;
  version: VersionInput;
  onOpen: (open: Opened) => void;
}) {
  const decl = useDeclaration(name, module, version);
  // A snapshot declaration's dependencies may be its module's too; the
  // server falls back to the bundle's.
  const openName = (n: string) => onOpen({ name: n, ...(module !== undefined && { module }) });
  if (decl.isPending) return <Panel title={name}>Loading…</Panel>;
  if (decl.isError) return <ErrorNote error={decl.error} />;
  const d = decl.data;
  return (
    <Panel title={<span className="font-mono">{d.name}</span>}>
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Badge>{d.kind}</Badge>
          <FlagBadges flags={d.flags} />
          <span className="text-xs text-slate-500">
            {d.library} · {d.module} · {version.version ?? "the server's default version"}
          </span>
        </div>
        <pre className="whitespace-pre-wrap font-mono text-xs text-slate-800">{d.signature}</pre>
        <Disclosure summary="With full names and no notation">
          <Goals goals={[d.type_full]} />
        </Disclosure>
        {d.flags.imports !== undefined && <Imports imports={d.flags.imports} name={d.name} />}
        {d.source_url !== undefined && (
          <a className="text-xs text-indigo-600 hover:underline" href={d.source_url} target="_blank" rel="noreferrer">
            source
          </a>
        )}
        {d.docstring !== undefined && <p className="text-sm text-slate-700">{d.docstring}</p>}
        {d.description !== undefined && (
          <div className="rounded bg-indigo-50 p-2 text-sm text-indigo-900">
            <p className="font-medium">{d.description.title}</p>
            <p>{d.description.text}</p>
          </div>
        )}
        {d.dependencies.length > 0 && (
          <Field label="Uses">
            <Names names={d.dependencies} onOpen={openName} />
          </Field>
        )}
        {d.used_by.length > 0 && (
          <Field label="Used by">
            <Names names={d.used_by} onOpen={openName} />
          </Field>
        )}
      </div>
    </Panel>
  );
}

function Resolver({ version, onOpen }: { version: VersionInput; onOpen: (name: string) => void }) {
  const [text, setText] = useState("");
  const names = parseList(useDebouncedValue(text, 300));
  const resolved = useResolve(names, version);
  return (
    <Panel title="Did you mean…">
      <input className={`${inputClass} w-full font-mono`} value={text} onChange={(e) => setText(e.target.value)} placeholder="Real.sqrt_mul_sefl, Nat.add_com" />
      <ErrorNote error={resolved.error} />
      <ul className="mt-2 space-y-3 text-sm">
        {resolved.data?.map((r) => (
          <li key={r.name} className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs">{r.name}</span>
              {r.exists ? <Badge tone="green">exists</Badge> : <Badge tone="amber">unknown</Badge>}
              {r.deprecated_to !== undefined && (
                <button type="button" onClick={() => onOpen(r.deprecated_to ?? r.name)}>
                  <Badge tone="amber">deprecated: use {r.deprecated_to}</Badge>
                </button>
              )}
            </div>
            {r.imports !== undefined && <Imports imports={r.imports} name={r.name} />}
            {r.suggestions !== undefined && r.suggestions.length > 0 && (
              <ul className="space-y-1 border-l-2 border-slate-100 pl-2">
                {r.suggestions.map((g) => (
                  <li key={g.name} className="space-y-0.5">
                    <div className="flex flex-wrap items-center gap-2">
                      <button type="button" className="font-mono text-xs text-indigo-700 hover:underline" onClick={() => onOpen(g.name)}>
                        {g.name}
                      </button>
                      <Badge>{g.kind}</Badge>
                    </div>
                    <pre className="whitespace-pre-wrap font-mono text-xs text-slate-600">{g.signature}</pre>
                    {g.imports !== undefined && <Imports imports={g.imports} name={g.name} />}
                  </li>
                ))}
              </ul>
            )}
          </li>
        ))}
      </ul>
    </Panel>
  );
}

function Searching({ seconds }: { seconds: number }) {
  return (
    <span className="inline-flex items-center gap-2 text-sm font-normal text-slate-500">
      <Spinner />
      Searching…{seconds >= 2 && ` ${seconds} s`}
    </span>
  );
}

export function SearchTab({ version: asked }: { version: string }) {
  const version: VersionInput = asked !== "" ? { version: asked } : {};
  // What to search for: one or more fields, each at most once.
  const [fields, setFields] = useState<{ mode: Mode; text: string }[]>([{ mode: "text", text: "" }]);
  const setField = (i: number, field: { mode: Mode; text: string }) => setFields(fields.map((f, j) => (j === i ? field : f)));
  const unused = (Object.keys(MODES) as Mode[]).filter((m) => !fields.some((f) => f.mode === m));
  const filled = fields.filter((f) => f.text.trim() !== "");
  const [libraries, setLibraries] = useState("");
  const [kinds, setKinds] = useState("");
  const [k, setK] = useState(10);
  // Left out, the server includes them when searching snapshots.
  const [sorried, setSorried] = useState<"default" | "include" | "exclude">("default");
  const [scope, setScope] = useState<"none" | "all" | "labels">("none");
  const [scopeLabels, setScopeLabels] = useState("");
  const [open, setOpen] = useState<Opened | null>(null);
  // What was last asked for, with Enter or the button; nothing runs as you type.
  const [submitted, setSubmitted] = useState<Query | null>(null);
  const search = useSearch(submitted ?? {}, version);
  const elapsed = useElapsed(search.isFetching);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const snapshots: SnapshotScope = scope === "labels" ? { labels: parseLabels(scopeLabels) } : scope;
    const query: Query = {
      ...Object.fromEntries(filled.map((f) => [f.mode, f.text])),
      libraries: parseList(libraries),
      kinds: parseList(kinds),
      limit: k,
      ...(sorried !== "default" && { include_sorried: sorried === "include" }),
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
            <div className="space-y-2">
              {fields.map((f, i) => (
                <div key={i} className="flex gap-2">
                  <select
                    className={inputClass}
                    aria-label="Search for"
                    value={f.mode}
                    onChange={(e) => setField(i, { ...f, mode: e.target.value as Mode })}
                  >
                    {(Object.keys(MODES) as Mode[])
                      .filter((m) => m === f.mode || unused.includes(m))
                      .map((m) => (
                        <option key={m} value={m}>
                          {MODES[m]}
                        </option>
                      ))}
                  </select>
                  <input
                    className={`${inputClass} flex-1`}
                    aria-label={MODES[f.mode]}
                    value={f.text}
                    onChange={(e) => setField(i, { ...f, text: e.target.value })}
                    placeholder={i === 0 ? "trace of a density matrix is one" : ""}
                    autoFocus={i === 0}
                  />
                  {i === 0 ? (
                    <Button type="submit" disabled={filled.length === 0} className="inline-flex items-center gap-2">
                      {search.isFetching ? (
                        <>
                          <Spinner />
                          Searching…
                        </>
                      ) : (
                        "Search"
                      )}
                    </Button>
                  ) : (
                    <SubtleButton aria-label={`Remove ${MODES[f.mode]}`} onClick={() => setFields(fields.filter((_, j) => j !== i))}>
                      ×
                    </SubtleButton>
                  )}
                </div>
              ))}
              {unused[0] !== undefined && (
                <button
                  type="button"
                  className="text-xs text-indigo-600 hover:underline"
                  onClick={() => setFields([...fields, { mode: unused[0] ?? "text", text: "" }])}
                >
                  Add a field
                </button>
              )}
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
              <Field label="Sorried">
                <select className={inputClass} value={sorried} onChange={(e) => setSorried(e.target.value as typeof sorried)}>
                  <option value="default">with snapshots only</option>
                  <option value="include">included</option>
                  <option value="exclude">left out</option>
                </select>
              </Field>
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
        {open !== null && <DeclView name={open.name} module={open.module} version={version} onOpen={setOpen} />}
        <Resolver version={version} onOpen={(name) => setOpen({ name })} />
      </div>
    </div>
  );
}
