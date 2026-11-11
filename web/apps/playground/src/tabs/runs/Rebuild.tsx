import type { RebuildItem, RebuildSnapshotsResponse, SnapshotId } from "@aqora/pisa-client";
import { type RebuildPlanInput, useRebuildPlan, useRebuildSnapshots, useSnapshot } from "@aqora/pisa-react";
import { useState } from "react";

import { CheckResultView } from "../../components/check-result.tsx";
import { JobView } from "../../components/JobView.tsx";
import { SnapshotRef } from "../../components/snapshot-ref.tsx";
import {
  Badge,
  Button,
  Checkbox,
  ConfirmButton,
  Disclosure,
  ErrorNote,
  Field,
  Notice,
  Panel,
  StatusBadge,
  SubtleButton,
  inputClass,
} from "../../components/ui.tsx";
import { parseLabels } from "../../labels.ts";
import type { RebuildLink } from "../../nav.tsx";

/** A snapshot named by id, as it stands. */
function Preview({ id }: { id: SnapshotId }) {
  const snapshot = useSnapshot(id.trim() === "" ? null : id.trim());
  if (id.trim() === "") return null;
  if (snapshot.isError) return <ErrorNote error={snapshot.error} hints={{ not_found: "No such snapshot." }} />;
  if (snapshot.data === undefined) return <p className="text-xs text-slate-500">…</p>;
  const s = snapshot.data;
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      <SnapshotRef id={s.id} />
      <StatusBadge status={s.status} />
      <span className="truncate font-mono text-slate-500">{s.module}</span>
      {s.replaces !== undefined && (
        <span className="flex items-center gap-1 text-slate-500">
          replaces <SnapshotRef id={s.replaces.snapshot_id} />
        </span>
      )}
    </div>
  );
}

function Item({ item }: { item: RebuildItem }) {
  return (
    <li className="space-y-1 py-1.5">
      <div className="flex flex-wrap items-center gap-2 text-xs">
        {item.snapshot_id !== undefined ? <SnapshotRef id={item.snapshot_id} /> : <Badge>support</Badge>}
        <span className="font-mono text-slate-600">
          {item.module} → {item.new_module}
        </span>
        {item.snapshot_id !== undefined &&
          (item.replaces !== undefined ? (
            <span className="text-slate-500">
              linked
              {item.replaces.allow !== undefined && (
                <span className="font-mono text-amber-800">, may change {item.replaces.allow.definitions.join(", ")}</span>
              )}
            </span>
          ) : (
            <Badge tone="amber">no link: its copy belongs to no run</Badge>
          ))}
      </div>
      {item.affected !== undefined && item.affected.length > 0 && (
        <p className="font-mono text-xs text-amber-800">affects {item.affected.join(", ")}</p>
      )}
      <Disclosure summary="Its code">
        <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{item.code}</pre>
      </Disclosure>
    </li>
  );
}

function Plan({ plan }: { plan: RebuildSnapshotsResponse }) {
  const levels = [...new Set(plan.plan.map((i) => i.level))].sort((a, b) => a - b);
  return (
    <div className="space-y-3">
      {!plan.ok && <Notice>No plan: To changes what From states. Allow changes to plan anyway.</Notice>}
      {plan.changes.length > 0 && (
        <ul className="list-inside list-disc text-sm text-amber-800">
          {plan.changes.map((c) => (
            <li key={c}>{c}</li>
          ))}
        </ul>
      )}
      {plan.changed_definitions !== undefined && plan.changed_definitions.length > 0 && (
        <p className="font-mono text-xs text-amber-800">changed definitions: {plan.changed_definitions.join(", ")}</p>
      )}
      {plan.replaced !== undefined && plan.replaced.length > 0 && (
        <div className="space-y-1 text-xs text-slate-600">
          <p>Left out, as a snapshot without every label replaces them:</p>
          {plan.replaced.map((r) => (
            <p key={r.snapshot_id} className="flex flex-wrap items-center gap-2">
              <SnapshotRef id={r.snapshot_id} /> by {r.replaced_by.map((b) => <SnapshotRef key={b} id={b} />)}
            </p>
          ))}
        </div>
      )}
      {plan.plan.length === 0 ? (
        <p className="text-sm text-slate-500">Nothing to rebuild.</p>
      ) : (
        levels.map((level) => (
          <div key={level}>
            <p className="text-xs font-semibold text-slate-600">Level {level}</p>
            <ul className="divide-y divide-slate-100">
              {plan.plan
                .filter((i) => i.level === level)
                .map((i) => (
                  <Item key={i.key} item={i} />
                ))}
            </ul>
          </div>
        ))
      )}
    </div>
  );
}

function Outcome({ run }: { run: RebuildSnapshotsResponse }) {
  return (
    <div className="space-y-2">
      {run.ok ? (
        <p className="rounded-md bg-emerald-50 p-3 text-sm font-medium text-emerald-800">Every item was created, and everything to delete deleted.</p>
      ) : (
        <Notice>Not every item was created, or not everything deleted: sending the rebuild again replays what is done.</Notice>
      )}
      {run.created !== undefined && run.created.length > 0 && (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <span className="text-slate-600">created</span>
          {run.created.map((c) =>
            c.new_snapshot_id !== undefined ? (
              <SnapshotRef key={c.new_module} id={c.new_snapshot_id} />
            ) : (
              <Badge key={c.new_module}>support</Badge>
            ),
          )}
        </div>
      )}
      {run.failed?.map((f) => (
        <Disclosure key={f.module} summary={<span className="text-rose-700">failed: {f.module}</span>}>
          <CheckResultView result={f.check} />
        </Disclosure>
      ))}
      {run.skipped !== undefined && run.skipped.length > 0 && (
        <p className="font-mono text-xs text-slate-500">skipped {run.skipped.map((s) => s.module).join(", ")}</p>
      )}
      {run.not_deleted?.map((n) => (
        <p key={n.snapshot_id} className="flex flex-wrap items-center gap-2 text-xs text-rose-700">
          not deleted <SnapshotRef id={n.snapshot_id} />: {n.error.message}
        </p>
      ))}
      {run.retired !== undefined && run.retired.length > 0 && (
        <p className="flex flex-wrap items-center gap-2 text-xs text-slate-600">
          retired {run.retired.map((r) => <SnapshotRef key={r} id={r} />)}
        </p>
      )}
    </div>
  );
}

/** Plans how what builds on a snapshot moves onto its replacement, and runs
 *  it: from and onto `initial`'s snapshots, if given. */
export function Rebuild({ initial }: { initial: RebuildLink | undefined }) {
  const [from, setFrom] = useState(initial?.from ?? "");
  const [to, setTo] = useState(initial?.to ?? "");
  const [labels, setLabels] = useState("playground=1");
  const [allowChanges, setAllowChanges] = useState(false);
  const [retire, setRetire] = useState(false);
  const [verify, setVerify] = useState<"auto" | "yes" | "no">("auto");
  const [limit, setLimit] = useState("");
  const [planned, setPlanned] = useState<RebuildPlanInput | null>(null);
  const plan = useRebuildPlan(planned);
  const rebuild = useRebuildSnapshots();
  const parsed = parseLabels(labels);
  const ready = from.trim() !== "" && to.trim() !== "" && Object.keys(parsed).length > 0;
  const input = { from: from.trim(), to: to.trim(), labels: parsed, ...(allowChanges && { allowChanges }) };
  const seconds = Number(limit);
  const run = () =>
    rebuild.mutate({
      ...input,
      ...(retire && { retire }),
      ...(verify !== "auto" && { verify: verify === "yes" }),
      ...(limit.trim() !== "" && seconds > 0 && { timeout: seconds }),
    });
  return (
    <Panel
      title="Rebuild"
      actions={
        <>
          <SubtleButton onClick={() => setPlanned(input)} disabled={!ready}>
            Plan
          </SubtleButton>
          {retire ? (
            <ConfirmButton onConfirm={run} confirm="Run, and delete the old snapshots?" disabled={!ready || rebuild.isPending}>
              Run
            </ConfirmButton>
          ) : (
            <Button onClick={run} disabled={!ready || rebuild.isPending}>
              Run
            </Button>
          )}
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1">
            <Field label="From: the snapshot replaced">
              <input className={inputClass} value={from} onChange={(e) => setFrom(e.target.value)} />
            </Field>
            <Preview id={from} />
          </div>
          <div className="space-y-1">
            <Field label="To: its replacement">
              <input className={inputClass} value={to} onChange={(e) => setTo(e.target.value)} />
            </Field>
            <Preview id={to} />
          </div>
          <Field label="Labels of the projects to follow (required)">
            <input className={inputClass} value={labels} onChange={(e) => setLabels(e.target.value)} />
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label="Kernel re-check">
              <select className={inputClass} value={verify} onChange={(e) => setVerify(e.target.value as typeof verify)}>
                <option value="auto">each copy (the default)</option>
                <option value="yes">each copy and its imports</option>
                <option value="no">none</option>
              </select>
            </Field>
            <Field label="Timeout per create (s)">
              <input className={inputClass} value={limit} onChange={(e) => setLimit(e.target.value)} placeholder="60" inputMode="numeric" />
            </Field>
          </div>
        </div>
        <div className="flex flex-wrap gap-4">
          <Checkbox checked={allowChanges} onChange={setAllowChanges}>
            <span title="Plan even though To, not created to replace From, changes what it states.">Allow changes</span>
          </Checkbox>
          <Checkbox checked={retire} onChange={setRetire}>
            <span title="Once every item is created, delete the old snapshots whose copies replace them.">Retire the old snapshots</span>
          </Checkbox>
        </div>
        <ErrorNote error={plan.error} />
        {plan.isFetching && plan.data === undefined && <p className="text-sm text-slate-500">Planning…</p>}
        {plan.data !== undefined && <Plan plan={plan.data} />}
        <JobView long={rebuild} />
        <ErrorNote error={rebuild.error} />
        {rebuild.data !== undefined && <Outcome run={rebuild.data} />}
      </div>
    </Panel>
  );
}
