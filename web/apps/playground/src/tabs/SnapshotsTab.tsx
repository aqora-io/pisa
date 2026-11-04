import type { Labels } from "@aqora/pisa-client";
import {
  useCreateSnapshot,
  useDeleteSnapshot,
  useSetSnapshotLabels,
  useSnapshot,
  useSnapshots,
  useVerifySnapshot,
} from "@aqora/pisa-react";
import { useState } from "react";

import { Badge, Button, CheckResultView, CopyButton, ErrorNote, Field, Panel, SubtleButton, inputClass } from "../components/ui.tsx";
import { LeanEditor } from "../editor/LeanEditor.tsx";

const EXAMPLE = `def double (n : Nat) : Nat := 2 * n

theorem double_eq (n : Nat) : double n = n + n := by
  unfold double
  omega`;

/** `a=1, b=2` as labels. */
function parseLabels(text: string): Labels {
  const labels: Labels = {};
  for (const part of text.split(",")) {
    const [key, ...rest] = part.split("=");
    if (key !== undefined && key.trim() !== "") labels[key.trim()] = rest.join("=").trim();
  }
  return labels;
}

function Flags({ dirty, verified }: { dirty: boolean; verified: boolean }) {
  return (
    <>
      {dirty ? <Badge tone="amber">dirty</Badge> : <Badge tone="green">clean</Badge>}
      {verified && <Badge tone="indigo">verified</Badge>}
    </>
  );
}

function Header({ header, onUse }: { header: string; onUse: (header: string) => void }) {
  return (
    <div className="space-y-2">
      <pre className="whitespace-pre-wrap rounded bg-slate-50 p-2 font-mono text-xs text-slate-800">{header}</pre>
      <div className="flex gap-2">
        <CopyButton text={header} />
        <SubtleButton onClick={() => onUse(header)}>Build on it in Check</SubtleButton>
      </div>
    </div>
  );
}

function Create({ version, onUse }: { version: string; onUse: (header: string) => void }) {
  const [code, setCode] = useState(EXAMPLE);
  const [labels, setLabels] = useState("playground=1");
  const [verify, setVerify] = useState<"auto" | "yes" | "no">("auto");
  const [key, setKey] = useState("");
  const create = useCreateSnapshot(version !== "" ? { version } : {});
  const submit = () =>
    create.mutate({
      code,
      labels: parseLabels(labels),
      ...(verify !== "auto" && { verify: verify === "yes" }),
      ...(key.trim() !== "" && { key: key.trim() }),
    });
  const made = create.data;
  return (
    <Panel
      title="Keep code as a snapshot"
      actions={
        <Button onClick={submit} disabled={create.isPending}>
          {create.isPending ? "Building…" : "Create"}
        </Button>
      }
    >
      <LeanEditor value={code} onChange={setCode} onRun={submit} className="h-56" />
      <div className="mt-3 grid gap-3 sm:grid-cols-3">
        <Field label="Labels (a=1, b=2)">
          <input className={inputClass} value={labels} onChange={(e) => setLabels(e.target.value)} />
        </Field>
        <Field label="Kernel re-check">
          <select className={inputClass} value={verify} onChange={(e) => setVerify(e.target.value as typeof verify)}>
            <option value="auto">if it uses macros</option>
            <option value="yes">yes, with imports</option>
            <option value="no">no</option>
          </select>
        </Field>
        <Field label="Idempotency key (optional)">
          <input className={inputClass} value={key} onChange={(e) => setKey(e.target.value)} />
        </Field>
      </div>
      <div className="mt-3 space-y-3">
        <ErrorNote error={create.error} />
        {made !== undefined &&
          (made.entry === undefined ? (
            <div className="space-y-2">
              <p className="text-sm text-rose-800">Nothing was kept.</p>
              <CheckResultView result={made.result} />
            </div>
          ) : (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <span className="font-mono text-xs">{made.snapshot}</span>
                <Flags dirty={made.dirty} verified={made.verified} />
              </div>
              {made.header !== undefined && <Header header={made.header} onUse={onUse} />}
              <CheckResultView result={made.result} />
            </div>
          ))}
      </div>
    </Panel>
  );
}

function Detail({ entry, version, onUse, onGone }: { entry: string; version: string; onUse: (h: string) => void; onGone: () => void }) {
  const input = version !== "" ? { version } : {};
  const info = useSnapshot(entry, input);
  const setLabels = useSetSnapshotLabels(input);
  const verify = useVerifySnapshot(input);
  const remove = useDeleteSnapshot(input);
  const [label, setLabel] = useState("");
  if (info.isPending) return <Panel title="Entry">Loading…</Panel>;
  if (info.isError) return <ErrorNote error={info.error} />;
  const e = info.data;
  return (
    <Panel
      title={<span className="font-mono text-xs">{e.snapshot}</span>}
      actions={
        <>
          <SubtleButton onClick={() => verify.mutate(entry)} disabled={verify.isPending}>
            {verify.isPending ? "Verifying…" : "Verify"}
          </SubtleButton>
          <SubtleButton onClick={() => remove.mutate(entry, { onSuccess: onGone })} disabled={remove.isPending}>
            Delete
          </SubtleButton>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Flags dirty={e.dirty} verified={e.verified} />
          <span className="text-xs text-slate-500">{new Date(e.created * 1000).toLocaleString()}</span>
        </div>
        {verify.data?.failure !== undefined && (
          <p className="text-sm text-rose-800">
            {verify.data.failure.snapshot}: {verify.data.failure.reason}
          </p>
        )}
        <ErrorNote error={verify.error ?? remove.error ?? setLabels.error} />
        <div className="flex flex-wrap items-center gap-1.5">
          {Object.entries(e.labels ?? {}).map(([k, v]) => (
            <span key={k} className="inline-flex items-center gap-1 rounded bg-slate-100 px-2 py-0.5 font-mono text-xs">
              {k}={v}
              <button type="button" className="text-slate-500 hover:text-rose-600" onClick={() => setLabels.mutate({ entry, remove: [k] })}>
                ×
              </button>
            </span>
          ))}
          <form
            className="flex gap-1"
            onSubmit={(ev) => {
              ev.preventDefault();
              setLabels.mutate({ entry, set: parseLabels(label) });
              setLabel("");
            }}
          >
            <input className={`${inputClass} w-32`} value={label} onChange={(ev) => setLabel(ev.target.value)} placeholder="add k=v" />
          </form>
        </div>
        {e.imports.length > 0 && <p className="font-mono text-xs text-slate-600">imports {e.imports.join(", ")}</p>}
        <Header header={e.header} onUse={onUse} />
        <LeanEditor value={e.code} readOnly className="h-40" />
      </div>
    </Panel>
  );
}

export function SnapshotsTab({ version, onUse }: { version: string; onUse: (header: string) => void }) {
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const list = useSnapshots({ labels: parseLabels(filter), limit: 20 }, version !== "" ? { version } : {});
  const entries = list.data?.pages.flatMap((p) => p.entries) ?? [];
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Create version={version} onUse={onUse} />
      <div className="space-y-4">
        <Panel title="Entries">
          <input
            className={`${inputClass} mb-3 w-full`}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="filter by labels: a=1, b=2"
          />
          <ErrorNote error={list.error} onRetry={() => void list.refetch()} />
          {entries.length === 0 && !list.isPending && <p className="text-sm text-slate-500">No entries.</p>}
          <ul className="divide-y divide-slate-100">
            {entries.map((e) => (
              <li key={e.entry}>
                <button
                  type="button"
                  onClick={() => setSelected(e.entry)}
                  className={`flex w-full flex-wrap items-center gap-2 px-1 py-1.5 text-left text-sm ${
                    selected === e.entry ? "bg-indigo-50" : "hover:bg-slate-50"
                  }`}
                >
                  <span className="font-mono text-xs text-slate-800">{e.snapshot.slice(0, 18)}…</span>
                  <Flags dirty={e.dirty} verified={e.verified} />
                  {Object.entries(e.labels).map(([k, v]) => (
                    <span key={k} className="font-mono text-xs text-slate-500">
                      {k}={v}
                    </span>
                  ))}
                </button>
              </li>
            ))}
          </ul>
          {list.hasNextPage && (
            <SubtleButton className="mt-2" onClick={() => void list.fetchNextPage()} disabled={list.isFetchingNextPage}>
              Load more
            </SubtleButton>
          )}
        </Panel>
        {selected !== null && <Detail entry={selected} version={version} onUse={onUse} onGone={() => setSelected(null)} />}
      </div>
    </div>
  );
}
