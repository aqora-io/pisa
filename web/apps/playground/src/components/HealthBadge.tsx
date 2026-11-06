import { useHealth } from "@aqora/pisa-react";

import { Badge } from "./ui.tsx";

/** The server's health: up or not, its live workers, and search. */
export function HealthBadge() {
  const health = useHealth();
  if (health.isPending) return <Badge>connecting…</Badge>;
  if (health.isError) return <Badge tone="red">server unreachable</Badge>;
  const { status, workers, search } = health.data;
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Badge tone={status === "ok" ? "green" : "amber"}>{status}</Badge>
      <Badge tone={workers === 0 || workers === null ? "amber" : "slate"}>
        {workers === null ? "workers unknown" : `${workers} worker${workers === 1 ? "" : "s"}`}
      </Badge>
      {search === null ? (
        <Badge>no search</Badge>
      ) : search.ready ? (
        <Badge tone="indigo">search {search.version}</Badge>
      ) : (
        <span title={search.error}>
          <Badge tone="amber">search not ready</Badge>
        </span>
      )}
    </div>
  );
}
