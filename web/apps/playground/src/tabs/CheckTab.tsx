import { type Detail, type TacticInfo, posMapper } from "@aqora/pisa-client";
import { useCheck, useLiveCheck } from "@aqora/pisa-react";
import { useMemo, useState } from "react";

import { Badge, Button, CheckResultView, ErrorNote, Goals, Panel, SubtleButton } from "../components/ui.tsx";
import { toDiagnostics } from "../editor/diagnostics.ts";
import { LeanEditor } from "../editor/LeanEditor.tsx";

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
  const [live, setLive] = useState(true);
  const [submitted, setSubmitted] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const options = { ...(version !== "" && { version }), detail: ["tactics"] as Detail[] };
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
          <label className="flex items-center gap-1.5 text-sm text-slate-700">
            <input type="checkbox" checked={live} onChange={(e) => setLive(e.target.checked)} />
            Check as I type
          </label>
          {!live && <Button onClick={run}>Check</Button>}
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
          value={code}
          onChange={setCode}
          diagnostics={diagnostics}
          onCursor={setCursor}
          onRun={live ? () => void liveCheck.refetch() : run}
          className="h-[65vh]"
        />
      </div>
      <div className="space-y-4">
        <ErrorNote error={check.error} onRetry={() => void check.refetch()} />
        <Panel title="Result">
          {data === undefined ? (
            <p className="text-sm text-slate-500">{check.isFetching ? "Checking…" : "Nothing checked yet."}</p>
          ) : (
            <CheckResultView result={data.result} />
          )}
        </Panel>
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
    </div>
  );
}
