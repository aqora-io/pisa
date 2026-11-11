import { type ProofNode, type ProofSession, useProofSession } from "@aqora/pisa-react";
import { useState } from "react";

import { CheckResultView } from "../components/check-result.tsx";
import { Probes } from "../components/probes.tsx";
import { Badge, Button, Checkbox, CopyButton, ErrorNote, Goals, Messages, Panel, SubtleButton, inputClass } from "../components/ui.tsx";
import { LeanEditor } from "../editor/LeanEditor.tsx";
import { useGo } from "../nav.tsx";

export const PROVE_EXAMPLE = `theorem add_zero_both (n : Nat) : n + 0 = n ∧ 0 + n = n := by
  constructor
  · sorry
  · sorry`;

/** What a `not_found` from a session means. */
const GONE = "The session was closed, or has expired (a day after it started, in Compose and the chart): start again.";

const STATUS = {
  pending: <Badge tone="indigo">…</Badge>,
  failed: <Badge tone="red">✗</Badge>,
  ok: null,
};

function rootOf(session: ProofSession, id: number): ProofNode | undefined {
  let node = session.node(id);
  while (node?.parent !== null && node?.parent !== undefined) node = session.node(node.parent);
  return node;
}

/** The first node under `id` with no goals left. */
function completeUnder(session: ProofSession, id: number): number | undefined {
  for (const child of session.children(id)) {
    if (child.status === "ok" && child.complete) return child.id;
    const deeper = completeUnder(session, child.id);
    if (deeper !== undefined) return deeper;
  }
  return undefined;
}

function Tree({
  session,
  node,
  label,
  selected,
  chosen,
  onSelect,
}: {
  session: ProofSession;
  node: ProofNode;
  label: string;
  selected: number | undefined;
  chosen: Set<number>;
  onSelect: (id: number) => void;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect(node.id)}
        className={`flex w-full items-center gap-2 rounded px-2 py-1 text-left text-sm ${
          node.id === selected ? "bg-indigo-50 ring-1 ring-indigo-300" : "hover:bg-slate-50"
        }`}
      >
        <span className="font-mono text-xs text-slate-800">{label}</span>
        {STATUS[node.status]}
        {node.complete && <Badge tone="green">✓ done</Badge>}
        {chosen.has(node.id) && <Badge tone="indigo">in proof</Badge>}
        {node.status === "ok" && !node.complete && (
          <span className="truncate text-xs text-slate-500">
            {node.goals.length} goal{node.goals.length === 1 ? "" : "s"}
          </span>
        )}
      </button>
      {session.children(node.id).length > 0 && (
        <ul className="ml-4 border-l border-slate-200 pl-2">
          {session.children(node.id).map((child) => (
            <Tree
              key={child.id}
              session={session}
              node={child}
              label={child.tactic ?? ""}
              selected={selected}
              chosen={chosen}
              onSelect={onSelect}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

export function ProveTab({ code, setCode, version }: { code: string; setCode: (code: string) => void; version: string }) {
  const go = useGo();
  const session = useProofSession(version !== "" ? { version } : {});
  const [selectedId, setSelected] = useState<number | null>(null);
  const [tactic, setTactic] = useState("");
  // The node whose path proves each root, by root.
  const [picked, setPicked] = useState<Record<number, number>>({});
  const [full, setFull] = useState(false);

  const selected = selectedId ?? session.roots[0]?.id;
  const node = selected === undefined ? undefined : session.node(selected);
  // A root is a sorry of the session's code, which is its document's sorry
  // at the same index.
  const sorry = node === undefined ? -1 : session.roots.findIndex((r) => r.id === node.id);
  const leaves = session.roots.flatMap((root) => {
    const leaf = picked[root.id] ?? completeUnder(session, root.id);
    return leaf === undefined ? [] : [leaf];
  });
  const chosen = new Set(leaves);
  const proof = session.proofText(leaves);

  const start = () => {
    setSelected(null);
    setPicked({});
    session.start(code);
  };
  const apply = () => {
    if (selected === undefined || tactic.trim() === "") return;
    const id = session.apply(selected, tactic.trim());
    if (id !== undefined) {
      setSelected(id);
      setTactic("");
    }
  };

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <div className="space-y-3">
        <Panel
          title="Statement, with a sorry for each goal"
          actions={
            <>
              <Button onClick={start} disabled={session.status === "starting"}>
                {session.status === "idle" ? "Start" : "Start again"}
              </Button>
              {session.status !== "idle" && <SubtleButton onClick={session.reset}>Close</SubtleButton>}
            </>
          }
        >
          <LeanEditor value={code} onChange={setCode} onRun={start} className="h-48" label="Statement" />
          {session.session !== undefined && (
            <div className="mt-2 flex min-w-0 flex-wrap items-center gap-2 text-xs">
              <span className="text-slate-500">session</span>
              <span className="truncate font-mono text-slate-700">{session.session}</span>
              <CopyButton text={session.session} />
              {session.code !== undefined && code !== session.code && <Badge tone="amber">edited since the start</Badge>}
            </div>
          )}
        </Panel>
        <ErrorNote error={session.error} onRetry={start} />
        {session.status === "starting" && <p className="text-sm text-slate-500">Starting the session…</p>}
        {session.messages.length > 0 && (
          <Panel title="Messages">
            <Messages messages={session.messages} />
          </Panel>
        )}
        {session.status === "ready" && (
          <Panel title="States">
            {session.roots.length === 0 ? (
              <p className="text-sm text-slate-500">The code has no sorry with a goal.</p>
            ) : (
              <ul className="space-y-1">
                {session.roots.map((root, i) => (
                  <Tree
                    key={root.id}
                    session={session}
                    node={root}
                    label={`sorry ${i + 1}`}
                    selected={selected}
                    chosen={chosen}
                    onSelect={setSelected}
                  />
                ))}
              </ul>
            )}
          </Panel>
        )}
      </div>
      <div className="space-y-3">
        {node !== undefined && (
          <Panel
            title={node.tactic === null ? "Goal" : `After ${node.tactic}`}
            actions={
              node.parent !== null &&
              node.status === "ok" && (
                <SubtleButton
                  onClick={() => {
                    const root = rootOf(session, node.id);
                    if (root !== undefined) setPicked((p) => ({ ...p, [root.id]: node.id }));
                  }}
                >
                  Use in the proof
                </SubtleButton>
              )
            }
          >
            {node.status === "pending" && <p className="text-sm text-slate-500">Running…</p>}
            {node.status === "failed" && <ErrorNote error={node.error} hints={{ not_found: GONE }} />}
            {node.status === "ok" && <Goals goals={node.goals} />}
            <Messages messages={node.messages} />
            {node.status === "ok" && !node.complete && (
              <form
                className="mt-3 flex gap-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  apply();
                }}
              >
                <input
                  className={`${inputClass} flex-1 font-mono`}
                  value={tactic}
                  onChange={(e) => setTactic(e.target.value)}
                  placeholder="a tactic, e.g. simp"
                />
                <Button type="submit" disabled={tactic.trim() === ""}>
                  Apply
                </Button>
              </form>
            )}
          </Panel>
        )}
        {session.session !== undefined && node !== undefined && sorry >= 0 && node.status === "ok" && (
          <Panel title={`Try candidates at sorry ${sorry + 1}`}>
            <Probes
              key={`${session.session}:${node.id}`}
              id={session.session}
              at={{ sorry }}
              revision={1}
              use="Add to the tree"
              onUse={(candidate) => {
                const id = session.apply(node.id, candidate);
                if (id !== undefined) setSelected(id);
              }}
            />
          </Panel>
        )}
        {session.status === "ready" && session.roots.length > 0 && (
          <Panel
            title="The proof"
            actions={
              <>
                <Checkbox checked={full} onChange={setFull}>
                  Declarations in full
                </Checkbox>
                <SubtleButton onClick={() => go({ to: "check", code: proof })}>Open in Check</SubtleButton>
                <Button
                  onClick={() => session.check(proof, full ? { detail: ["declarations"] } : {})}
                  disabled={leaves.length === 0 || session.verdict.status === "checking"}
                >
                  Check it
                </Button>
              </>
            }
          >
            <p className="mb-2 text-xs text-slate-500">
              Each sorry with a finished branch, or the one you picked, gets that branch's tactics.
            </p>
            <LeanEditor value={proof} readOnly className="h-40" label="The proof" />
            <div className="mt-3">
              {session.verdict.status === "checking" && <p className="text-sm text-slate-500">Checking…</p>}
              <ErrorNote error={session.verdict.error} hints={{ not_found: GONE }} />
              {session.verdict.result !== undefined && <CheckResultView result={session.verdict.result} />}
            </div>
          </Panel>
        )}
      </div>
    </div>
  );
}
