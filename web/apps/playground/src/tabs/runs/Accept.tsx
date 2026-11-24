import type { AcceptPair, AuditParams, KernelOutcome, LeftOutReason, PairErrorKind } from "@aqora/pisa-client";
import { useAcceptRun, useExportRun } from "@aqora/pisa-react";
import { useState } from "react";

import { JobView } from "../../components/JobView.tsx";
import { SnapshotRef } from "../../components/snapshot-ref.tsx";
import {
  Badge,
  Button,
  Checkbox,
  CopyButton,
  Disclosure,
  ErrorNote,
  Field,
  Notice,
  Panel,
  SubtleButton,
  inputClass,
} from "../../components/ui.tsx";
import { parseList } from "../../labels.ts";

/** Why comparator didn't accept a pair, in words. */
const ERROR: Record<PairErrorKind, string> = {
  refused: "refused",
  export_failed: "an export failed",
  timeout: "ran out of time",
  out_of_memory: "ran out of memory",
  cannot_run: "couldn't run",
  nothing_to_judge: "nothing to judge",
  unsupported: "con-leche or con-ron exited with 2",
};

/** What a kernel said, in words. */
const KERNEL: Record<KernelOutcome, string> = {
  accepted: "accepts",
  rejected: "rejects",
  unsupported: "not supported",
  cannot_run: "couldn't run",
  failed: "failed, no code",
  out_of_memory: "ran out of memory",
};

/** Why a configuration leaves a name out, in words. */
const LEFT_OUT: Record<LeftOutReason, string> = {
  private: "private",
  not_a_definition_in_spec: "not a definition in the spec",
  not_a_definition_in_head: "not a definition in the head",
  private_in_statement: "its statement uses a private name of the spec",
  axiom_not_in_head: "an axiom the head doesn't build on",
};

/** Saves `blob` as a file the browser downloads. */
function save(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function Pair({ pair }: { pair: AcceptPair }) {
  return (
    <li className="space-y-1 py-2">
      <div className="flex flex-wrap items-center gap-2 text-xs text-slate-600">
        head <SnapshotRef id={pair.head} /> against spec <SnapshotRef id={pair.spec} />
        {pair.accepted ? <Badge tone="green">accepted</Badge> : <Badge tone="red">{pair.error !== undefined ? ERROR[pair.error.kind] : "not accepted"}</Badge>}
        <span>
          {pair.theorems} theorem{pair.theorems === 1 ? "" : "s"}
        </span>
        {pair.exit_code !== undefined && <span className="font-mono">exit {pair.exit_code}</span>}
      </div>
      {pair.error !== undefined && <p className="whitespace-pre-wrap text-xs text-rose-800">{pair.error.message}</p>}
      {pair.kernels !== undefined && pair.kernels.length > 0 && (
        <ul className="flex flex-wrap gap-x-3 gap-y-1 text-xs">
          {pair.kernels.map((k) => (
            <li key={k.name} title={k.reason}>
              <span className="font-mono">{k.name}</span>{" "}
              <Badge tone={k.verdict === "accepted" ? "green" : k.verdict === "unsupported" ? "amber" : "red"}>{KERNEL[k.verdict]}</Badge>
              {k.reason !== undefined && k.verdict !== "unsupported" && <span className="text-xs text-gray-600"> {k.reason}</span>}
            </li>
          ))}
        </ul>
      )}
      {pair.left_out !== undefined && pair.left_out.length > 0 && (
        <p className="text-xs text-amber-800">
          left out:{" "}
          {pair.left_out.map((l, i) => (
            <span key={l.name}>
              {i > 0 && ", "}
              <span className="font-mono">{l.name}</span> ({LEFT_OUT[l.reason]})
            </span>
          ))}
        </p>
      )}
      {pair.accepted ? (
        <Disclosure summary="What it printed">
          <pre className="max-h-64 overflow-auto whitespace-pre-wrap font-mono text-xs text-slate-700">{pair.output}</pre>
        </Disclosure>
      ) : (
        <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-rose-50 p-2 font-mono text-xs text-rose-900">{pair.output}</pre>
      )}
      <Disclosure summary="Its configuration">
        <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{pair.config}</pre>
      </Disclosure>
    </li>
  );
}

/** Exports a run as a Lake project, and accepts it: the audit, and `lake
 *  comparator` on each pair. `initial`'s spec, if given. */
export function Accept({ initial }: { initial: AuditParams | undefined }) {
  const [specs, setSpecs] = useState(initial?.against?.join(", ") ?? "");
  const [allow, setAllow] = useState(initial?.allow?.join(", ") ?? "");
  // The server's default: comparator re-checks with every kernel.
  const [paranoid, setParanoid] = useState(true);
  const [limit, setLimit] = useState("");
  const exporter = useExportRun();
  const accept = useAcceptRun();
  const against = parseList(specs);
  const allowed = parseList(allow);
  const seconds = Number(limit);
  const allowing = allowed.length > 0 ? { allow: allowed } : {};
  const judged = accept.data;
  return (
    <Panel
      title="Export and accept a run"
      actions={
        <>
          <SubtleButton
            onClick={() => exporter.mutate({ against, ...allowing }, { onSuccess: (blob) => save(blob, "pisa-run.tar.gz") })}
            disabled={against.length === 0 || exporter.isPending}
          >
            {exporter.isPending ? "Exporting…" : "Export"}
          </SubtleButton>
          <Button
            onClick={() =>
              accept.mutate({
                against,
                ...allowing,
                paranoid,
                ...(limit.trim() !== "" && seconds > 0 && { timeout: seconds }),
              })
            }
            disabled={against.length === 0 || accept.isPending}
          >
            Accept
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid gap-3 sm:grid-cols-3">
          <Field label="Spec snapshots (ids)">
            <input className={inputClass} value={specs} onChange={(e) => setSpecs(e.target.value)} />
          </Field>
          <Field label="Also allow (definitions)">
            <input className={inputClass} value={allow} onChange={(e) => setAllow(e.target.value)} placeholder="none" />
          </Field>
          <Field label="Timeout per pair (s)">
            <input className={inputClass} value={limit} onChange={(e) => setLimit(e.target.value)} placeholder="600" inputMode="numeric" />
          </Field>
        </div>
        <Checkbox checked={paranoid} onChange={setParanoid}>
          <span title="leanchecker-paranoid, lean4lean, nanoda, con-leche and con-ron">Also run the external kernels Lean bundles (the default)</span>
        </Checkbox>
        <ErrorNote error={exporter.error} />
        {exporter.isSuccess && <p className="text-xs text-slate-500">Saved as pisa-run.tar.gz.</p>}
        <JobView long={accept} />
        <ErrorNote error={accept.error} />
        {judged !== undefined && (
          <div className="space-y-2">
            {judged.accepted ? (
              <p className="rounded-md bg-emerald-50 p-3 text-sm font-medium text-emerald-800">
                Accepted: the audit is clean, and comparator accepted every pair.
              </p>
            ) : (
              <Notice>
                Not accepted:{" "}
                {judged.clean ? "comparator didn't accept every pair." : "the audit isn't clean; audit the run to see why."}
              </Notice>
            )}
            <div className="flex flex-wrap items-center gap-2 text-xs text-slate-600">
              <span>acceptance</span>
              <span className="font-mono">{judged.id}</span>
              <CopyButton text={judged.id} />
              <span>heads</span>
              {judged.heads.map((h) => (
                <SnapshotRef key={h} id={h} />
              ))}
            </div>
            <ul className="divide-y divide-slate-100">
              {judged.pairs.map((p) => (
                <Pair key={`${p.spec}.${p.head}`} pair={p} />
              ))}
            </ul>
          </div>
        )}
      </div>
    </Panel>
  );
}
