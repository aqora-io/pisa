import type { CheckResult, DeleteSnapshotResponse, Export, ProofStatus, SnapshotId } from "@aqora/pisa-client";
import {
  useCreateSnapshot,
  useDeleteSnapshot,
  useDependents,
  useSnapshot,
  useSnapshots,
  useUpdateSnapshot,
  useVerifySnapshot,
} from "@aqora/pisa-react";
import { useState } from "react";

import { CheckResultView, ReplacementView } from "../components/check-result.tsx";
import { SnapshotRef, shortId } from "../components/snapshot-ref.tsx";
import {
  Badge,
  Button,
  Checkbox,
  ConfirmButton,
  CopyButton,
  Disclosure,
  ErrorNote,
  Field,
  Notice,
  Panel,
  StatusBadge,
  SubtleButton,
  inputClass,
} from "../components/ui.tsx";
import { LeanEditor } from "../editor/LeanEditor.tsx";
import { parseLabels, parseList } from "../labels.ts";
import { type Go, type Seed, useGo, useSeed } from "../nav.tsx";

const EXAMPLE = `def double (n : Nat) : Nat := 2 * n

theorem double_eq (n : Nat) : double n = n + n := by
  unfold double
  omega`;

function Flags({ status, verified }: { status: ProofStatus; verified: boolean }) {
  return (
    <>
      <StatusBadge status={status} />
      {verified && <Badge tone="indigo">verified</Badge>}
    </>
  );
}

/** A snapshot's import header, with links to build on it. */
function Header({ header }: { header: string }) {
  const go = useGo();
  return (
    <div className="space-y-2">
      <pre className="whitespace-pre-wrap rounded bg-slate-50 p-2 font-mono text-xs text-slate-800">{header}</pre>
      <div className="flex gap-2">
        <CopyButton text={header} />
        <SubtleButton onClick={() => go({ to: "check", code: `${header}\n\n` })}>Build on it in Check</SubtleButton>
        <SubtleButton onClick={() => go({ to: "prove", code: `${header}\n\n` })}>Prove on it</SubtleButton>
        <SubtleButton onClick={() => go({ to: "documents", code: `${header}\n\n` })}>Open a document on it</SubtleButton>
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
            {d.signature !== undefined ? (
              <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{d.signature}</pre>
            ) : (
              <p className="text-xs text-amber-700">Didn't print: {d.error}</p>
            )}
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

/** Code to keep, and the snapshot it replaces. */
type CreateSeed = Extract<Go, { to: "snapshots" }>["create"] & {};

function Create({ version, initial }: { version: string; initial: CreateSeed }) {
  const [code, setCode] = useState(initial.code);
  const [labels, setLabels] = useState("playground=1");
  const [verify, setVerify] = useState<"auto" | "yes" | "no">("auto");
  const [key, setKey] = useState("");
  const [limit, setLimit] = useState("");
  const [full, setFull] = useState(false);
  const [replaces, setReplaces] = useState(initial.replaces ?? "");
  const [allow, setAllow] = useState("");
  const [fork, setFork] = useState(false);
  const [holes, setHoles] = useState("");
  const replacing = replaces.trim() !== "";
  // A replacement is built on the version of the snapshot it replaces.
  const create = useCreateSnapshot(version !== "" && !replacing ? { version } : {});
  const recheck = useVerifySnapshot();
  const submit = () => {
    const seconds = Number(limit);
    const allowed = parseList(allow);
    const left = parseList(holes);
    recheck.reset();
    create.mutate({
      code,
      labels: parseLabels(labels),
      ...(verify !== "auto" && { verify: verify === "yes" }),
      ...(key.trim() !== "" && { key: key.trim() }),
      ...(limit.trim() !== "" && seconds > 0 && { timeout: seconds }),
      ...(full && { detail: ["declarations"] }),
      ...(replacing
        ? {
            replaces: { snapshot_id: replaces.trim(), ...(allowed.length > 0 && { allow: { definitions: allowed } }) },
            ...(fork && { fork }),
          }
        : left.length > 0 && { holes: left }),
    });
  };
  const made = create.data;
  const kept = made?.snapshot;
  return (
    <Panel
      title="Keep code as a snapshot"
      actions={
        <Button onClick={submit} disabled={create.isPending}>
          {create.isPending ? "Building…" : "Create"}
        </Button>
      }
    >
      <LeanEditor value={code} onChange={setCode} onRun={submit} className="h-56" label="Snapshot code" />
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
        <Field label="Replaces (a snapshot id)">
          <input className={inputClass} value={replaces} onChange={(e) => setReplaces(e.target.value)} placeholder="nothing" />
        </Field>
        <Field label="May change (definitions)">
          <input
            className={inputClass}
            value={allow}
            onChange={(e) => setAllow(e.target.value)}
            disabled={!replacing}
            placeholder={replacing ? "none: f, g" : "with replaces"}
          />
        </Field>
        <Field label="Holes (definitions)">
          <input
            className={inputClass}
            value={holes}
            onChange={(e) => setHoles(e.target.value)}
            disabled={replacing}
            placeholder={replacing ? "the replaced snapshot's" : "none: f, g"}
          />
        </Field>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-4">
        <Checkbox checked={fork} onChange={setFork} disabled={!replacing}>
          <span title="Replace a snapshot that another replaces already.">Fork the run</span>
        </Checkbox>
        <Checkbox checked={full} onChange={setFull}>
          Declarations in full
        </Checkbox>
        <label className="flex items-center gap-1.5 text-sm text-slate-700">
          Timeout (s)
          <input className={`${inputClass} w-20`} value={limit} onChange={(e) => setLimit(e.target.value)} placeholder="60" inputMode="numeric" />
        </label>
        {replacing && version !== "" && <span className="text-xs text-slate-500">Built on the replaced snapshot's version, not {version}.</span>}
      </div>
      <div className="mt-3 space-y-3">
        <ErrorNote error={create.error} />
        {made !== undefined &&
          (kept === undefined ? (
            <div className="space-y-2">
              <p className="text-sm text-rose-800">Nothing was kept.</p>
              {made.heads !== undefined && made.heads.length > 0 && (
                <div className="space-y-1">
                  <p className="text-sm text-slate-700">Another snapshot replaces it already. Replace a head instead, or fork the run.</p>
                  <ul className="space-y-1">
                    {made.heads.map((head) => (
                      <li key={head} className="flex items-center gap-2">
                        <SnapshotRef id={head} />
                        <SubtleButton onClick={() => setReplaces(head)}>Replace this head</SubtleButton>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              <CheckResultView result={made.check} />
            </div>
          ) : (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <SnapshotRef id={kept.id} />
                <span className="font-mono text-xs">{kept.module}</span>
                <Flags status={recheck.data?.snapshot.status ?? kept.status} verified={recheck.data?.snapshot.verified ?? kept.verified} />
              </div>
              {made.unverified !== undefined && recheck.data === undefined && (
                <Notice
                  action={
                    <SubtleButton onClick={() => recheck.mutate(kept.id)} disabled={recheck.isPending}>
                      {recheck.isPending ? "Verifying…" : "Verify"}
                    </SubtleButton>
                  }
                >
                  Kept, but the kernel re-check didn't finish ({made.unverified.module}: {made.unverified.reason}): its statuses are
                  the build's own report.
                </Notice>
              )}
              {recheck.data?.failure !== undefined && (
                <p className="text-sm text-rose-800">
                  {recheck.data.failure.module}: {recheck.data.failure.reason}
                </p>
              )}
              <ErrorNote error={recheck.error} />
              {made.replacement !== undefined && <ReplacementView replacement={made.replacement} />}
              {kept.holes !== undefined && kept.holes.length > 0 && (
                <p className="font-mono text-xs text-amber-800">holes {kept.holes.join(", ")}</p>
              )}
              <Header header={kept.header} />
              <CheckResultView result={made.check} />
            </div>
          ))}
      </div>
    </Panel>
  );
}

/** The snapshots of the modules that import this one's, parents first. */
function Dependents({ id }: { id: SnapshotId }) {
  const dependents = useDependents(id);
  if (dependents.isError) return <ErrorNote error={dependents.error} />;
  const snapshots = dependents.data?.snapshots;
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-xs font-medium text-slate-600">What builds on it</span>
      {snapshots === undefined ? (
        <span className="text-xs text-slate-500">…</span>
      ) : snapshots.length === 0 ? (
        <span className="text-xs text-slate-500">nothing</span>
      ) : (
        snapshots.map((d) => <SnapshotRef key={d.id} id={d.id} />)
      )}
    </div>
  );
}

/** The check the snapshot was kept with, its declarations summarised or in
 *  full. */
function StoredCheck({ id, check }: { id: SnapshotId; check: CheckResult }) {
  const [full, setFull] = useState(false);
  const inFull = useSnapshot(full ? id : null, { detail: ["declarations"] });
  return (
    <Disclosure summary="The stored check">
      <div className="space-y-2">
        <Checkbox checked={full} onChange={setFull}>
          Declarations in full
        </Checkbox>
        <ErrorNote error={inFull.error} />
        {full && inFull.isPending ? (
          <p className="text-sm text-slate-500">Loading…</p>
        ) : (
          <CheckResultView result={full && inFull.data !== undefined ? inFull.data.check : check} />
        )}
      </div>
    </Disclosure>
  );
}

function Detail({ id, onGone }: { id: SnapshotId; onGone: (deleted: DeleteSnapshotResponse) => void }) {
  const info = useSnapshot(id);
  const update = useUpdateSnapshot();
  const verify = useVerifySnapshot();
  const remove = useDeleteSnapshot();
  const go = useGo();
  const [label, setLabel] = useState("");
  const [limit, setLimit] = useState("");
  const [withReplacers, setWithReplacers] = useState(false);
  const seconds = Number(limit);
  if (info.isPending) return <Panel title="Snapshot">Loading…</Panel>;
  if (info.isError) return <ErrorNote error={info.error} hints={{ not_found: "This snapshot was deleted." }} />;
  const e = info.data;
  return (
    <Panel
      title={<span className="font-mono text-xs">{e.module}</span>}
      actions={
        <>
          <SubtleButton onClick={() => go({ to: "snapshots", create: { code: e.code, replaces: id } })}>Replace it</SubtleButton>
          <SubtleButton onClick={() => go({ to: "runs", audit: { against: [id] } })}>Audit against it</SubtleButton>
          <SubtleButton onClick={() => go({ to: "runs", audit: { labels: e.labels } })} disabled={Object.keys(e.labels).length === 0}>
            Audit its labels
          </SubtleButton>
          <SubtleButton onClick={() => go({ to: "runs", rebuild: { from: id } })}>Rebuild from it</SubtleButton>
          {e.replaces !== undefined && (
            <SubtleButton onClick={() => go({ to: "runs", rebuild: { from: e.replaces?.snapshot_id ?? "", to: id } })}>
              Rebuild onto it
            </SubtleButton>
          )}
          <SubtleButton
            onClick={() => verify.mutate({ id, ...(limit.trim() !== "" && seconds > 0 && { timeout: seconds }) })}
            disabled={verify.isPending}
          >
            {verify.isPending ? "Verifying…" : "Verify"}
          </SubtleButton>
          <ConfirmButton
            onConfirm={() => remove.mutate({ id, ...(withReplacers && { withReplacers }) }, { onSuccess: onGone })}
            confirm={withReplacers ? "Delete it and what replaces it?" : "Delete it?"}
            disabled={remove.isPending}
          >
            Delete
          </ConfirmButton>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Flags status={e.status} verified={e.verified} />
          <span className="text-xs text-slate-500">{new Date(e.created * 1000).toLocaleString()}</span>
        </div>
        <div className="flex flex-wrap items-center gap-4">
          <label className="flex items-center gap-1.5 text-sm text-slate-700">
            Verify within (s)
            <input className={`${inputClass} w-20`} value={limit} onChange={(ev) => setLimit(ev.target.value)} placeholder="60" inputMode="numeric" />
          </label>
          <Checkbox checked={withReplacers} onChange={setWithReplacers}>
            Delete what replaces it too
          </Checkbox>
        </div>
        <dl className="grid grid-cols-[5rem_minmax(0,1fr)] items-center gap-x-2 gap-y-1 text-xs">
          <dt className="text-slate-500">id</dt>
          <dd className="flex min-w-0 items-center gap-2">
            <span className="truncate font-mono text-slate-800">{e.id}</span>
            <CopyButton text={e.id} />
          </dd>
          <dt className="text-slate-500">version</dt>
          <dd className="font-mono text-slate-800">{e.version}</dd>
          {e.replaces !== undefined && (
            <>
              <dt className="text-slate-500">replaces</dt>
              <dd className="flex flex-wrap items-center gap-2">
                <SnapshotRef id={e.replaces.snapshot_id} />
                {e.replaces.allow !== undefined && (
                  <span className="font-mono text-amber-800">may change {e.replaces.allow.definitions.join(", ")}</span>
                )}
              </dd>
            </>
          )}
          {e.holes !== undefined && e.holes.length > 0 && (
            <>
              <dt className="text-slate-500">holes</dt>
              <dd className="font-mono text-amber-800">{e.holes.join(", ")}</dd>
            </>
          )}
        </dl>
        {verify.data !== undefined && verify.data.checked.length > 0 && (
          <p className="text-xs text-slate-600">
            Re-checked <span className="font-mono">{verify.data.checked.join(", ")}</span>
          </p>
        )}
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
        <Header header={e.header} />
        {e.exports !== undefined && e.exports.length > 0 && <Exports exports={e.exports} onRefresh={() => void info.refetch()} />}
        <LeanEditor value={e.code} readOnly className="h-40" label="The snapshot's code" />
        <Dependents id={id} />
        <StoredCheck id={id} check={e.check} />
      </div>
    </Panel>
  );
}

export function SnapshotsTab({ version, seed }: { version: string; seed: Seed<Extract<Go, { to: "snapshots" }>> | undefined }) {
  const [filter, setFilter] = useState("");
  const [heads, setHeads] = useState(false);
  const [selected, setSelected] = useState<SnapshotId | null>(null);
  const [deleted, setDeleted] = useState<DeleteSnapshotResponse | null>(null);
  const select = (id: SnapshotId) => {
    setSelected(id);
    setDeleted(null);
  };
  // Code handed over to keep, numbered so that each hand-over starts afresh.
  const [create, setCreate] = useState<{ n: number; seed: CreateSeed }>({ n: 0, seed: { code: EXAMPLE } });
  useSeed(seed, ({ open, create: given }) => {
    if (open !== undefined) select(open);
    if (given !== undefined) setCreate((c) => ({ n: c.n + 1, seed: given }));
  });
  const list = useSnapshots({ labels: parseLabels(filter), limit: 20, ...(heads && { heads }) }, version !== "" ? { version } : {});
  const snapshots = list.data?.pages.flatMap((p) => p.snapshots) ?? [];
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Create key={create.n} version={version} initial={create.seed} />
      <div className="space-y-4">
        <Panel title="Snapshots">
          <div className="mb-3 flex items-center gap-3">
            <input
              className={`${inputClass} min-w-0 flex-1`}
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="filter by labels: a=1, b=2"
            />
            <Checkbox checked={heads} onChange={setHeads}>
              <span title="The snapshots that nothing replaces: each run's current version.">Heads only</span>
            </Checkbox>
          </div>
          <ErrorNote error={list.error} onRetry={() => void list.refetch()} />
          {snapshots.length === 0 && !list.isPending && <p className="text-sm text-slate-500">No snapshots.</p>}
          <ul className="divide-y divide-slate-100">
            {snapshots.map((e) => (
              <li key={e.id}>
                <button
                  type="button"
                  onClick={() => select(e.id)}
                  className={`flex w-full flex-wrap items-center gap-2 px-1 py-1.5 text-left text-sm ${
                    selected === e.id ? "bg-indigo-50" : "hover:bg-slate-50"
                  }`}
                >
                  <span className="font-mono text-xs text-slate-800" title={e.id}>
                    {shortId(e.id)}
                  </span>
                  <Flags status={e.status} verified={e.verified} />
                  {e.replaces !== undefined && <Badge>replaces {shortId(e.replaces.snapshot_id)}</Badge>}
                  {e.holes !== undefined && e.holes.length > 0 && <Badge tone="amber">holes</Badge>}
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
        {selected !== null && (
          <Detail
            key={selected}
            id={selected}
            onGone={(gone) => {
              setSelected(null);
              setDeleted(gone);
            }}
          />
        )}
        {deleted !== null && (
          <Notice action={<SubtleButton onClick={() => setDeleted(null)}>OK</SubtleButton>}>
            <p>
              Deleted <span className="font-mono">{deleted.deleted.map(shortId).join(", ")}</span>
            </p>
            {deleted.removed.length > 0 && (
              <p>
                and removed the modules <span className="font-mono">{deleted.removed.join(", ")}</span>
              </p>
            )}
          </Notice>
        )}
      </div>
    </div>
  );
}
