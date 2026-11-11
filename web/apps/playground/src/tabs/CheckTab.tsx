import { type Detail, type Pos, type TacticInfo, posMapper } from "@aqora/pisa-client";
import { type CheckInput, useCheck, useLiveCheck, useLongJob } from "@aqora/pisa-react";
import { useMemo, useState } from "react";

import { AtView, CheckResultView } from "../components/check-result.tsx";
import { JobView } from "../components/JobView.tsx";
import { Badge, Button, Checkbox, ErrorNote, Field, Goals, Panel, SubtleButton, inputClass } from "../components/ui.tsx";
import { toDiagnostics } from "../editor/diagnostics.ts";
import { LeanEditor } from "../editor/LeanEditor.tsx";
import { useGo } from "../nav.tsx";
import { Batch } from "./check/Batch.tsx";

/** The innermost tactic around `offset` in `code`. */
function tacticAt(code: string, tactics: TacticInfo[], offset: number): TacticInfo | undefined {
  const map = posMapper(code);
  let best: { tactic: TacticInfo; size: number } | undefined;
  for (const t of tactics) {
    const from = map.toOffset(t.pos);
    const to = map.toOffset(t.end_pos);
    if (from <= offset && offset <= to && (best === undefined || to - from < best.size)) {
      best = { tactic: t, size: to - from };
    }
  }
  return best?.tactic;
}

export function CheckTab({ code, setCode, version }: { code: string; setCode: (code: string) => void; version: string }) {
  const go = useGo();
  const [live, setLive] = useState(true);
  const [full, setFull] = useState(false);
  const [terms, setTerms] = useState(false);
  const [limit, setLimit] = useState("");
  // Positions to ask about, at most 64.
  const [pins, setPins] = useState<Pos[]>([]);
  const [submitted, setSubmitted] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const detail: Detail[] = ["tactics", ...(full ? (["declarations"] as const) : []), ...(terms && pins.length > 0 ? (["terms"] as const) : [])];
  const seconds = Number(limit);
  const options = {
    ...(version !== "" && { version }),
    detail,
    ...(pins.length > 0 && { at: pins }),
    ...(limit.trim() !== "" && seconds > 0 && { timeout: seconds }),
  };
  const pin = () => {
    const at = posMapper(code).toPos(cursor);
    if (!pins.some((p) => p.line === at.line && p.column === at.column)) setPins([...pins, at].slice(0, 64));
  };
  const asJob = useLongJob((client, { code, ...rest }: CheckInput, { signal }) => client.submitCheck(code, { ...rest, signal }));
  const liveCheck = useLiveCheck(live ? code : "", options);
  const manualCheck = useCheck(!live && submitted !== null ? { ...options, code: submitted } : null);
  const check = live ? liveCheck : manualCheck;
  const data = check.data;
  const fresh = data?.code === code;
  // Positions refer to the code that was checked: only then are they drawn.
  const diagnostics = useMemo(
    () => (data !== undefined && fresh ? toDiagnostics(data.code, data.result.messages) : undefined),
    [data, fresh],
  );
  const atCursor = data !== undefined && fresh ? tacticAt(code, data.result.tactics ?? [], cursor) : undefined;

  const run = () => {
    if (submitted === code) void manualCheck.refetch();
    else setSubmitted(code);
  };

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_26rem]">
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-3">
          <Checkbox checked={live} onChange={setLive}>
            Check as I type
          </Checkbox>
          <Checkbox checked={full} onChange={setFull}>
            Declarations in full
          </Checkbox>
          <Checkbox checked={terms} onChange={setTerms} disabled={pins.length === 0}>
            <span title="Terms cost 2–6 times a check: ask for them only where you need them.">Terms at the pins</span>
          </Checkbox>
          <Field label="Timeout (s)">
            <input className={`${inputClass} w-20`} value={limit} onChange={(e) => setLimit(e.target.value)} placeholder="60" inputMode="numeric" />
          </Field>
          {!live && <Button onClick={run}>Check</Button>}
          <SubtleButton onClick={() => asJob.mutate({ ...options, code })} disabled={asJob.isPending}>
            Check as a job
          </SubtleButton>
          {check.isFetching && (
            <>
              <Badge tone="indigo">checking…</Badge>
              <SubtleButton onClick={check.cancel}>Stop waiting</SubtleButton>
            </>
          )}
          {!check.isFetching && data !== undefined && !fresh && <Badge>edited since the last check</Badge>}
          <span className="ml-auto text-xs text-slate-500">Ctrl/⌘-Enter checks</span>
        </div>
        <LeanEditor
          label="Code to check"
          value={code}
          onChange={setCode}
          diagnostics={diagnostics}
          onCursor={setCursor}
          onRun={live ? () => void liveCheck.refetch() : run}
          className="h-[65vh]"
        />
        <div className="flex flex-wrap items-center gap-1.5">
          <SubtleButton onClick={pin} disabled={pins.length >= 64}>
            Pin the cursor
          </SubtleButton>
          {pins.map((p) => (
            <span key={`${p.line}:${p.column}`} className="inline-flex items-center gap-1 rounded bg-indigo-50 px-2 py-0.5 font-mono text-xs text-indigo-800">
              {p.line}:{p.column}
              <button
                type="button"
                aria-label={`Unpin ${p.line}:${p.column}`}
                className="text-indigo-400 hover:text-rose-600"
                onClick={() => setPins(pins.filter((q) => q !== p))}
              >
                ×
              </button>
            </span>
          ))}
          {pins.length > 0 && (
            <button type="button" className="text-xs text-slate-500 hover:underline" onClick={() => setPins([])}>
              Clear
            </button>
          )}
          {pins.length === 0 && <span className="text-xs text-slate-500">Pin positions to see what Lean has there.</span>}
        </div>
      </div>
      <div className="space-y-4">
        <ErrorNote error={check.error} onRetry={() => void check.refetch()} />
        <Panel
          title="Result"
          actions={
            <>
              <SubtleButton onClick={() => go({ to: "snapshots", create: { code } })}>Keep as a snapshot</SubtleButton>
              <SubtleButton onClick={() => go({ to: "documents", code })}>Open as a document</SubtleButton>
              {data !== undefined && data.result.sorries.length > 0 && (
                <SubtleButton onClick={() => go({ to: "prove", code: data.code })}>Prove its sorries</SubtleButton>
              )}
            </>
          }
        >
          {data === undefined ? (
            <p className="text-sm text-slate-500">{check.isFetching ? "Checking…" : "Nothing checked yet."}</p>
          ) : (
            <CheckResultView result={data.result} />
          )}
        </Panel>
        {!asJob.isIdle && (
          <Panel title="As a job" actions={<SubtleButton onClick={asJob.reset}>Clear</SubtleButton>}>
            <div className="space-y-3">
              <JobView long={asJob} />
              <ErrorNote error={asJob.error} />
              {asJob.data !== undefined && <CheckResultView result={asJob.data} />}
            </div>
          </Panel>
        )}
        {pins.length > 0 && data !== undefined && fresh && data.result.at !== undefined && (
          <Panel title="At pinned positions">
            <AtView at={data.result.at} />
          </Panel>
        )}
        <Panel title="Goals at the cursor">
          {atCursor === undefined ? (
            <p className="text-sm text-slate-500">Put the cursor on a tactic.</p>
          ) : (
            <div className="space-y-2">
              <p className="font-mono text-xs text-slate-600">{atCursor.tactic}</p>
              <p className="text-xs font-medium text-slate-600">Before</p>
              <Goals goals={atCursor.goals_before} />
              <p className="text-xs font-medium text-slate-600">After</p>
              <Goals goals={atCursor.goals_after} />
            </div>
          )}
        </Panel>
      </div>
      <div className="lg:col-span-2">
        <Batch version={version} timeout={limit.trim() !== "" && seconds > 0 ? seconds : undefined} />
      </div>
    </div>
  );
}
