import type { AuditParams, SnapshotId } from "@aqora/pisa-client";
import { createContext, useContext, useState } from "react";

/** Where a link goes, with what it opens there. */
export type Go =
  | { to: "check"; code: string }
  | { to: "prove"; code: string }
  | { to: "documents"; code: string }
  | { to: "snapshots"; open?: SnapshotId; create?: { code: string; replaces?: SnapshotId } }
  | { to: "runs"; audit?: AuditParams; rebuild?: RebuildLink };

/** A rebuild to fill in: the snapshot replaced, and its replacement. */
export interface RebuildLink {
  from?: SnapshotId;
  to?: SnapshotId;
}

/** A value handed to a tab, numbered so that the same value twice is
 *  applied twice. */
export interface Seed<T> {
  n: number;
  value: T;
}

export const GoContext = createContext<(go: Go) => void>(() => undefined);

/** Follows a link to another tab. */
export function useGo(): (go: Go) => void {
  return useContext(GoContext);
}

/** Applies each new seed once, as it arrives: during render, as React's
 *  "adjusting state when a prop changes" does, not in an effect. */
export function useSeed<T>(seed: Seed<T> | undefined, apply: (value: T) => void): void {
  const [applied, setApplied] = useState(seed?.n);
  if (seed !== undefined && seed.n !== applied) {
    setApplied(seed.n);
    apply(seed.value);
  }
}
