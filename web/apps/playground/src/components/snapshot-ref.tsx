import type { SnapshotId } from "@aqora/pisa-client";

import { useGo } from "../nav.tsx";

/** A snapshot id's first hex digits, enough to tell snapshots apart. */
export function shortId(id: SnapshotId): string {
  return (id.split("~").at(-1) ?? id).slice(0, 8);
}

/** A snapshot, by a short id that opens it in Snapshots; the full id in its
 *  title. */
export function SnapshotRef({ id }: { id: SnapshotId }) {
  const go = useGo();
  return (
    <button type="button" title={id} className="font-mono text-xs text-indigo-700 hover:underline" onClick={() => go({ to: "snapshots", open: id })}>
      {shortId(id)}
    </button>
  );
}
