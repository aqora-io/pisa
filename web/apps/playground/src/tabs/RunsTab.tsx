import type { AuditParams } from "@aqora/pisa-client";

import type { RebuildLink, Seed } from "../nav.tsx";
import { Audit } from "./runs/Audit.tsx";
import { Accept } from "./runs/Accept.tsx";
import { Rebuild } from "./runs/Rebuild.tsx";

export function RunsTab({
  version,
  audit,
  rebuild,
}: {
  version: string;
  audit: Seed<AuditParams> | undefined;
  rebuild: Seed<RebuildLink> | undefined;
}) {
  // A link starts the panels it fills afresh, so that no plan, run or
  // verdict of another run stays next to its ids.
  return (
    <div className="space-y-4">
      <Audit key={`audit-${audit?.n ?? 0}`} version={version} initial={audit?.value} />
      <Accept key={`accept-${audit?.n ?? 0}`} initial={audit?.value} />
      <Rebuild key={`rebuild-${rebuild?.n ?? 0}`} initial={rebuild?.value} />
    </div>
  );
}
