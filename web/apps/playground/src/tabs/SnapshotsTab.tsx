import type { Export, Labels, ProofStatus, SnapshotId } from "@aqora/pisa-client";
import {
  useCreateSnapshot,
  useDeleteSnapshot,
  useSnapshot,
  useSnapshots,
  useUpdateSnapshot,
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

function Flags({ status, verified }: { status: ProofStatus; verified: boolean }) {
  return (
    <>
      <Badge tone={status === "proved" ? "green" : "amber"}>{status}</Badge>
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

/** A module's exports, described once search has written descriptions. */
function Exports({ exports, onRefresh }: { exports: Export[]; onRefresh: () => void }) {
  const undescribed = exports.some((d) => d.description === undefined);
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <p className="text-xs font-medium text-slate-600">Declarations</p>
        {undescribed && (
          <button type="button" className="text-xs text-indigo-600 hover:underline" onClick={onRefresh}>
            Refresh
          </button>
        )}
      </div>
      <ul className="divide-y divide-slate-100">
        {exports.map((d) => (
          <li key={d.name} className="space-y-1 py-2">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs font-medium text-slate-900">{d.name}</span>
              <Badge>{d.kind}</Badge>
              {d.description !== undefined && <span className="text-sm font-medium text-slate-700">{d.description.title}</span>}
            </div>
            <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{d.signature}</pre>
            {d.description !== undefined ? (
              <p className="text-sm text-slate-600">{d.description.text}</p>
            ) : (
              d.docstring !== undefined && <p className="text-sm text-slate-500">{d.docstring}</p>
            )}
          </li>
        ))}
      </ul>
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
            <option value="auto">this code (the default)</option>
            <option value="yes">this code and its imports</option>
            <option value="no">none: the build's report only</option>
          </select>
        </Field>
        <Field label="Idempotency key (optional)">
          <input className={inputClass} value={key} onChange={(e) => setKey(e.target.value)} />
        </Field>
      </div>
      <div className="mt-3 space-y-3">
        <ErrorNote error={create.error} />
        {made !== undefined &&
          (made.snapshot === undefined ? (
            <div className="space-y-2">
              <p className="text-sm text-rose-800">Nothing was kept.</p>
              <CheckResultView result={made.check} />
            </div>
          ) : (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <span className="font-mono text-xs">{made.snapshot.module}</span>
                <Flags status={made.snapshot.status} verified={made.snapshot.verified} />
              </div>
              <Header header={made.snapshot.header} onUse={onUse} />
              <CheckResultView result={made.check} />
            </div>
          ))}
      </div>
    </Panel>
  );
}

function Detail({ id, onUse, onGone }: { id: SnapshotId; onUse: (h: string) => void; onGone: () => void }) {
  const info = useSnapshot(id);
  const update = useUpdateSnapshot();
  const verify = useVerifySnapshot();
  const remove = useDeleteSnapshot();
  const [label, setLabel] = useState("");
  if (info.isPending) return <Panel title="Snapshot">Loading…</Panel>;
  if (info.isError) return <ErrorNote error={info.error} />;
  const e = info.data;
  return (
    <Panel
      title={<span className="font-mono text-xs">{e.module}</span>}
      actions={
        <>
          <SubtleButton onClick={() => verify.mutate(id)} disabled={verify.isPending}>
            {verify.isPending ? "Verifying…" : "Verify"}
          </SubtleButton>
          <SubtleButton onClick={() => remove.mutate(id, { onSuccess: onGone })} disabled={remove.isPending}>
            Delete
          </SubtleButton>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Flags status={e.status} verified={e.verified} />
          <span className="text-xs text-slate-500">{new Date(e.created * 1000).toLocaleString()}</span>
        </div>
        {verify.data?.failure !== undefined && (
          <p className="text-sm text-rose-800">
            {verify.data.failure.module}: {verify.data.failure.reason}
          </p>
        )}
        <ErrorNote error={verify.error ?? remove.error ?? update.error} />
        <div className="flex flex-wrap items-center gap-1.5">
          {Object.entries(e.labels).map(([k, v]) => (
            <span key={k} className="inline-flex items-center gap-1 rounded bg-slate-100 px-2 py-0.5 font-mono text-xs">
              {k}={v}
              <button type="button" className="text-slate-500 hover:text-rose-600" onClick={() => update.mutate({ id, labels: { [k]: null } })}>
                ×
              </button>
            </span>
          ))}
          <form
            className="flex gap-1"
            onSubmit={(ev) => {
              ev.preventDefault();
              update.mutate({ id, labels: parseLabels(label) });
              setLabel("");
            }}
          >
            <input className={`${inputClass} w-32`} value={label} onChange={(ev) => setLabel(ev.target.value)} placeholder="add k=v" />
          </form>
        </div>
        {e.imports.length > 0 && <p className="font-mono text-xs text-slate-600">imports {e.imports.join(", ")}</p>}
        <Header header={e.header} onUse={onUse} />
        {e.exports !== undefined && e.exports.length > 0 && <Exports exports={e.exports} onRefresh={() => void info.refetch()} />}
        <LeanEditor value={e.code} readOnly className="h-40" />
      </div>
    </Panel>
  );
}

export function SnapshotsTab({ version, onUse }: { version: string; onUse: (header: string) => void }) {
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<SnapshotId | null>(null);
  const list = useSnapshots({ labels: parseLabels(filter), limit: 20 }, version !== "" ? { version } : {});
  const snapshots = list.data?.pages.flatMap((p) => p.snapshots) ?? [];
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Create version={version} onUse={onUse} />
      <div className="space-y-4">
        <Panel title="Snapshots">
          <input
            className={`${inputClass} mb-3 w-full`}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="filter by labels: a=1, b=2"
          />
          <ErrorNote error={list.error} onRetry={() => void list.refetch()} />
          {snapshots.length === 0 && !list.isPending && <p className="text-sm text-slate-500">No snapshots.</p>}
          <ul className="divide-y divide-slate-100">
            {snapshots.map((e) => (
              <li key={e.id}>
                <button
                  type="button"
                  onClick={() => setSelected(e.id)}
                  className={`flex w-full flex-wrap items-center gap-2 px-1 py-1.5 text-left text-sm ${
                    selected === e.id ? "bg-indigo-50" : "hover:bg-slate-50"
                  }`}
                >
                  <span className="font-mono text-xs text-slate-800">{e.module.slice(0, 18)}…</span>
                  <Flags status={e.status} verified={e.verified} />
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
        {selected !== null && <Detail key={selected} id={selected} onUse={onUse} onGone={() => setSelected(null)} />}
      </div>
    </div>
  );
}
