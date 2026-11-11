import { type GoalsMode, type LinePos, type ProbeAt, posMapper, proofText } from "@aqora/pisa-client";
import { useDebouncedValue, useDocument, useDocumentGoals } from "@aqora/pisa-react";
import { useMemo, useState } from "react";

import { Probes } from "../components/probes.tsx";
import { Badge, Button, CopyButton, ErrorNote, Field, Goals, Messages, Panel, SubtleButton, inputClass } from "../components/ui.tsx";
import { toDiagnostics } from "../editor/diagnostics.ts";
import { LeanEditor } from "../editor/LeanEditor.tsx";
import { parseList } from "../labels.ts";
import { useGo } from "../nav.tsx";

export const DOCUMENT_EXAMPLE = `theorem double (n : Nat) : n + n = 2 * n := by
  sorry

example : 2 + 2 = 4 := by
  sorry`;

const HINTS = {
  conflict: "The document is at another revision than this page knows: open it again from your code.",
  not_found: "The document was closed, or has expired: open it again.",
};

/** Where probes run: a sorry, by its index, or the cursor. */
type Target = number | "cursor";

export function DocumentsTab({ code, setCode, version }: { code: string; setCode: (code: string) => void; version: string }) {
  const go = useGo();
  const [limit, setLimit] = useState("");
  const seconds = Number(limit);
  const timeout = limit.trim() !== "" && seconds > 0 ? seconds : undefined;
  const doc = useDocument({ ...(version !== "" && { version }), ...(timeout !== undefined && { timeout }) });
  const [cursor, setCursor] = useState(0);
  const [mode, setMode] = useState<GoalsMode>("before");
  const [lines, setLines] = useState("");
  const [target, setTarget] = useState<Target>(0);

  const open = doc.document;
  // Positions are about the code of the document's revision: only while the
  // editor has that code do they line up with it.
  const fresh = open !== undefined && doc.code === code;
  // Asked about once the cursor rests.
  const settled = useDebouncedValue(cursor, 300);
  const here = fresh ? posMapper(code).toPos(settled) : undefined;
  const at: LinePos[] = [
    ...(here !== undefined ? [here] : []),
    ...parseList(lines)
      .map(Number)
      .filter((n) => Number.isInteger(n) && n > 0)
      .map((line) => ({ line })),
  ].slice(0, 64);
  const goals = useDocumentGoals(fresh ? { id: open.id, revision: open.revision } : null, at, {
    mode,
    ...(timeout !== undefined && { timeout }),
  });
  const diagnostics = useMemo(() => (fresh ? toDiagnostics(code, open.messages) : undefined), [fresh, code, open]);
  const probeAt: ProbeAt | undefined = target === "cursor" ? here : open?.sorries[target] !== undefined ? { sorry: target } : undefined;
  const sorry = typeof target === "number" ? open?.sorries[target] : undefined;

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_28rem]">
      <div className="space-y-3">
        <Panel
          title="Document"
          actions={
            <>
              <SubtleButton onClick={() => go({ to: "check", code })}>Open in Check</SubtleButton>
              {doc.status !== "idle" && <SubtleButton onClick={doc.close}>Close</SubtleButton>}
              {doc.status === "open" ? (
                <>
                  <SubtleButton onClick={() => doc.open(code)}>Open again</SubtleButton>
                  <Button onClick={() => doc.edit(code)} disabled={fresh || doc.editing}>
                    {doc.editing ? "Saving…" : `Save as revision ${(open?.revision ?? 0) + 1}`}
                  </Button>
                </>
              ) : (
                <Button onClick={() => doc.open(code)} disabled={doc.status === "opening"}>
                  {doc.status === "opening" ? "Opening…" : "Open"}
                </Button>
              )}
            </>
          }
        >
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-3">
              <Field label="Timeout (s)">
                <input className={`${inputClass} w-20`} value={limit} onChange={(e) => setLimit(e.target.value)} placeholder="60" inputMode="numeric" />
              </Field>
              {open !== undefined && (
                <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs">
                  <Badge tone="indigo">revision {open.revision}</Badge>
                  {!fresh && !doc.editing && <Badge tone="amber">edited since</Badge>}
                  <span className="truncate font-mono text-slate-600">{open.id}</span>
                  <CopyButton text={open.id} />
                </div>
              )}
            </div>
            <LeanEditor
              value={code}
              onChange={setCode}
              diagnostics={diagnostics}
              onCursor={setCursor}
              onRun={() => (open === undefined ? doc.open(code) : doc.edit(code))}
              className="h-[55vh]"
              label="Document code"
            />
            <p className="text-xs text-slate-500">Ctrl/⌘-Enter opens the code, or saves it as the next revision.</p>
          </div>
        </Panel>
        <ErrorNote error={doc.error} hints={HINTS} />
        {open !== undefined && open.messages.length > 0 && (
          <Panel title="Messages">
            <Messages messages={open.messages} />
          </Panel>
        )}
      </div>
      <div className="space-y-4">
        {open === undefined ? (
          <Panel title="Goals and probes">
            <p className="text-sm text-slate-500">Open the code to ask about its goals and try tactics.</p>
          </Panel>
        ) : (
          <>
            <Panel title="Sorries">
              {open.sorries.length === 0 ? (
                <p className="text-sm text-slate-500">The code has no sorry.</p>
              ) : (
                <ul className="space-y-2">
                  {open.sorries.map((s, i) => (
                    <li key={i} className="space-y-1">
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium text-slate-700">sorry {i + 1}</span>
                        {s.pos !== undefined && (
                          <Badge tone="amber">
                            {s.pos.line}:{s.pos.column}
                          </Badge>
                        )}
                        <SubtleButton className="ml-auto" onClick={() => setTarget(i)}>
                          Probe here
                        </SubtleButton>
                      </div>
                      <Goals goals={[s.goal]} />
                    </li>
                  ))}
                </ul>
              )}
            </Panel>
            <Panel
              title="Goals"
              actions={
                <select className={inputClass} aria-label="Goals" value={mode} onChange={(e) => setMode(e.target.value as GoalsMode)}>
                  <option value="before">before the tactic</option>
                  <option value="after">after it</option>
                </select>
              }
            >
              <div className="space-y-2">
                <input className={`${inputClass} w-full`} value={lines} onChange={(e) => setLines(e.target.value)} placeholder="more lines: 2, 5" />
                {!fresh && <p className="text-sm text-slate-500">Save the code to ask about it as it stands.</p>}
                <ErrorNote error={goals.error} hints={HINTS} />
                {goals.data?.at.map((g, i) => (
                  <div key={i} className="space-y-1">
                    <Badge tone={i === 0 && here !== undefined ? "indigo" : "slate"}>
                      {i === 0 && here !== undefined ? "cursor " : ""}
                      {g.pos.line}:{g.pos.column}
                    </Badge>
                    {g.goals.length === 0 ? <p className="text-xs text-slate-500">No goals here.</p> : <Goals goals={g.goals} />}
                  </div>
                ))}
              </div>
            </Panel>
            <Panel
              title="Probes"
              actions={
                <select
                  className={inputClass}
                  aria-label="Probe at"
                  value={String(target)}
                  onChange={(e) => setTarget(e.target.value === "cursor" ? "cursor" : Number(e.target.value))}
                >
                  {open.sorries.map((_, i) => (
                    <option key={i} value={i}>
                      sorry {i + 1}
                    </option>
                  ))}
                  <option value="cursor">the cursor</option>
                </select>
              }
            >
              {probeAt === undefined ? (
                <p className="text-sm text-slate-500">{target === "cursor" ? "Save the code to probe at the cursor." : "Pick where to probe."}</p>
              ) : (
                <Probes
                  key={`${open.id}@${open.revision}:${String(target)}`}
                  id={open.id}
                  at={probeAt}
                  revision={open.revision}
                  {...(timeout !== undefined && { timeout })}
                  closing
                  {...(fresh &&
                    sorry !== undefined && {
                      onUse: (candidate: string) => setCode(proofText(code, [{ sorry, tactics: [candidate] }])),
                    })}
                />
              )}
            </Panel>
          </>
        )}
      </div>
    </div>
  );
}
