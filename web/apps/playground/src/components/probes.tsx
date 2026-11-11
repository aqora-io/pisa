import type { DocumentId, ProbeAt, ProbeResult } from "@aqora/pisa-client";
import { useProbeDocument } from "@aqora/pisa-react";
import { useState } from "react";

import { Badge, Button, ErrorNote, Goals, Messages, SubtleButton, inputClass } from "./ui.tsx";

const MAX = 64;

function Verdict({ result }: { result: ProbeResult }) {
  if (result.closed) return <Badge tone="green">closes it</Badge>;
  if (result.ok) return <Badge tone="indigo">leaves {result.goals.length}</Badge>;
  return <Badge tone="red">{result.error?.kind ?? "fails"}</Badge>;
}

/** Candidate tactics tried at a sorry or a position of a document, or of a
 *  session's code, without changing it. `onUse` takes a candidate that
 *  ran: with `closing`, only one that closes the goal. */
export function Probes({
  id,
  at,
  revision,
  timeout,
  onUse,
  use = "Use it",
  closing = false,
}: {
  id: DocumentId;
  at: ProbeAt;
  revision?: number;
  timeout?: number;
  onUse?: (candidate: string) => void;
  use?: string;
  closing?: boolean;
}) {
  const [text, setText] = useState("rfl\nsimp\nomega\ndecide");
  const probe = useProbeDocument();
  const candidates = text
    .split("\n")
    .map((c) => c.trim())
    .filter((c) => c !== "");
  const run = () =>
    probe.mutate({
      id,
      at,
      candidates,
      ...(revision !== undefined && { revision }),
      ...(timeout !== undefined && { timeout }),
    });
  const tried = probe.variables?.candidates ?? [];
  return (
    <div className="space-y-2">
      <textarea
        className={`${inputClass} h-24 w-full font-mono`}
        aria-label="Candidates, one a line"
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <div className="flex items-center gap-2">
        <Button onClick={run} disabled={candidates.length === 0 || candidates.length > MAX || probe.isPending}>
          {probe.isPending ? "Trying…" : `Try ${candidates.length}`}
        </Button>
        <span className={`text-xs ${candidates.length > MAX ? "text-rose-700" : "text-slate-500"}`}>One a line, at most {MAX}.</span>
      </div>
      <ErrorNote error={probe.error} />
      {probe.data !== undefined && (
        <ul className="divide-y divide-slate-100">
          {probe.data.results.map((r, i) => {
            const candidate = tried[i] ?? "";
            return (
              <li key={i} aria-label={candidate} className="space-y-1 py-1.5">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-mono text-xs text-slate-800">{candidate}</span>
                  <Verdict result={r} />
                  {onUse !== undefined && (closing ? r.closed : r.ok) && (
                    <SubtleButton className="ml-auto" onClick={() => onUse(candidate)}>
                      {use}
                    </SubtleButton>
                  )}
                </div>
                {r.error !== undefined && <p className="text-xs text-rose-700">{r.error.message}</p>}
                {r.ok && !r.closed && <Goals goals={r.goals} />}
                <Messages messages={r.messages} />
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
