import { type CheckBatchInput, useCheckBatch } from "@aqora/pisa-react";
import { useState } from "react";

import { CheckResultView } from "../../components/check-result.tsx";
import { Badge, Button, ErrorNote, Panel, SubtleButton } from "../../components/ui.tsx";
import { LeanEditor } from "../../editor/LeanEditor.tsx";

const SEPARATOR = /^-- ---\s*$/m;
const MAX = 128;

const EXAMPLE = `theorem one : 1 + 1 = 2 := rfl
-- ---
theorem two : 2 + 2 = 5 := by decide
-- ---
theorem three (n : Nat) : n ≤ n + 1 := by omega`;

/** The items of a batch: the code between `-- ---` lines, blanks left out. */
function split(text: string): string[] {
  return text
    .split(SEPARATOR)
    .map((c) => c.trim())
    .filter((c) => c !== "");
}

/** The first line that says what an item is. */
function firstLine(code: string): string {
  return code.split("\n").find((l) => l.trim() !== "" && !l.startsWith("import ")) ?? code;
}

/** Checks several pieces of code in one request. */
export function Batch({ version, timeout }: { version: string; timeout: number | undefined }) {
  const [text, setText] = useState(EXAMPLE);
  const [input, setInput] = useState<CheckBatchInput | null>(null);
  const [open, setOpen] = useState<number | null>(null);
  const batch = useCheckBatch(input);
  const codes = split(text);
  const run = () => {
    setOpen(null);
    const next: CheckBatchInput = {
      codes,
      ...(version !== "" && { version }),
      ...(timeout !== undefined && { timeout }),
    };
    if (JSON.stringify(next) === JSON.stringify(input)) void batch.refetch();
    else setInput(next);
  };
  const results = batch.isPlaceholderData ? undefined : batch.data;
  return (
    <Panel
      title="Check a batch"
      actions={
        <>
          {batch.isFetching && <SubtleButton onClick={batch.cancel}>Stop waiting</SubtleButton>}
          <Button onClick={run} disabled={codes.length === 0 || codes.length > MAX || batch.isFetching}>
            {batch.isFetching ? "Checking…" : `Check ${codes.length}`}
          </Button>
        </>
      }
    >
      <div className="grid gap-4 lg:grid-cols-2">
        <div className="space-y-1">
          <LeanEditor value={text} onChange={setText} onRun={run} className="h-64" label="Batch code" />
          <p className={`text-xs ${codes.length > MAX ? "text-rose-700" : "text-slate-500"}`}>
            Items are separated by <code>-- ---</code> lines; at most {MAX}.
          </p>
        </div>
        <div className="space-y-2">
          <ErrorNote error={batch.error} onRetry={() => void batch.refetch()} />
          {results === undefined ? (
            <p className="text-sm text-slate-500">{batch.isFetching ? "Checking…" : "Nothing checked yet."}</p>
          ) : (
            <table className="w-full text-left text-xs">
              <thead className="text-slate-500">
                <tr>
                  <th className="py-1 font-medium">#</th>
                  <th className="py-1 font-medium">Code</th>
                  <th className="py-1 font-medium">Verdict</th>
                  <th className="py-1 font-medium">Time</th>
                </tr>
              </thead>
              <tbody>
                {results.map(({ code, result }, i) => (
                  <tr key={i} className="border-t border-slate-100 align-top">
                    <td className="py-1 text-slate-500">{i + 1}</td>
                    <td className="max-w-0 py-1">
                      <button
                        type="button"
                        className="block w-full truncate text-left font-mono text-slate-800 hover:underline"
                        aria-expanded={open === i}
                        onClick={() => setOpen(open === i ? null : i)}
                      >
                        {firstLine(code)}
                      </button>
                      {open === i && (
                        <div className="py-2">
                          <CheckResultView result={result} />
                        </div>
                      )}
                    </td>
                    <td className="py-1">
                      {result.ok ? <Badge tone="green">accepted</Badge> : <Badge tone="red">not accepted</Badge>}
                      {result.error !== undefined && <Badge tone="amber">{result.error.kind}</Badge>}
                    </td>
                    <td className="py-1 text-slate-500">{result.time.toFixed(2)} s</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </Panel>
  );
}
