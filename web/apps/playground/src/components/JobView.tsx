import type { JobStatus } from "@aqora/pisa-client";
import type { LongJob } from "@aqora/pisa-react";
import { useState } from "react";

import { useElapsed } from "../elapsed.ts";
import { Badge, CopyButton, SubtleButton } from "./ui.tsx";

const TONE = { submitting: "slate", queued: "amber", running: "indigo", done: "green", cancelled: "slate" } as const;

/** A request run as a job: where it stands, for how long, its id, and a
 *  way to withdraw it. */
export function JobView<T, V>({ long }: { long: LongJob<T, V> }) {
  const elapsed = useElapsed(long.isPending);
  // What the last cancel found, for the job it was about.
  const [answer, setAnswer] = useState<{ id: string | undefined; status: JobStatus | undefined }>();
  if (long.isIdle) return null;
  const status = long.job?.status ?? "submitting";
  const id = long.job?.id;
  const cancel = () => void long.cancel().then((status) => setAnswer({ id, status }));
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <Badge tone={TONE[status]}>{status}</Badge>
      {long.isPending && <span className="text-xs text-slate-500">{elapsed} s</span>}
      {long.job !== undefined && (
        <>
          <span className="font-mono text-xs text-slate-500" title="Anyone with its id can read its answer or cancel it.">
            job {long.job.id.slice(0, 8)}…
          </span>
          <CopyButton text={long.job.id} />
        </>
      )}
      {long.isPending && <SubtleButton onClick={cancel}>Cancel</SubtleButton>}
      {answer?.status === "running" && answer.id === id && long.isPending && <span className="text-xs text-slate-500">A worker has started it: it runs on.</span>}
    </div>
  );
}
