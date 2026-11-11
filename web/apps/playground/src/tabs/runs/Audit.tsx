import type { AuditParams, AuditReview, AuditSnapshotsResponse } from "@aqora/pisa-client";
import { useAudit, useDeleteSnapshot } from "@aqora/pisa-react";
import { type ReactNode, useState } from "react";

import { ChangesView, ReplacementView } from "../../components/check-result.tsx";
import { SnapshotRef } from "../../components/snapshot-ref.tsx";
import { Badge, Button, Checkbox, ConfirmButton, Disclosure, ErrorNote, Field, Notice, Panel, StatusBadge, inputClass } from "../../components/ui.tsx";
import { formatLabels, parseLabels, parseList } from "../../labels.ts";

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="space-y-1.5">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-slate-500">{title}</h3>
      {children}
    </div>
  );
}

function Verdict({ audit, against }: { audit: AuditSnapshotsResponse; against: boolean }) {
  const s = audit.summary;
  if (against) {
    return audit.clean === true ? (
      <p className="rounded-md bg-emerald-50 p-3 text-sm font-medium text-emerald-800">Clean: the run is done.</p>
    ) : (
      <Notice>Not clean yet: see the forks, comparisons and targets below.</Notice>
    );
  }
  const trusted = s.rejected === 0 && s.sorry === 0 && s.unverified_modules === 0;
  return trusted ? (
    <p className="rounded-md bg-emerald-50 p-3 text-sm font-medium text-emerald-800">Nothing rejected, no sorry, every module verified.</p>
  ) : (
    <Notice>Not trusted yet: something is rejected or sorry, or a module isn't verified.</Notice>
  );
}

/** Deletes a branch of a run: a snapshot and what replaces it. */
function DropBranch({ id }: { id: string }) {
  const remove = useDeleteSnapshot();
  return (
    <>
      <ConfirmButton
        onConfirm={() => remove.mutate({ id, withReplacers: true })}
        confirm="Delete it and what replaces it?"
        disabled={remove.isPending}
      >
        Drop this branch
      </ConfirmButton>
      <ErrorNote error={remove.error} />
    </>
  );
}

function Review({ review }: { review: AuditReview }) {
  return (
    <div className="space-y-2">
      <p className="flex items-center gap-2 text-xs text-slate-600">
        spec <SnapshotRef id={review.spec} /> on head <SnapshotRef id={review.head} />
      </p>
      <ul className="divide-y divide-slate-100">
        {review.declarations.map((d) => (
          <li key={`${d.origin}.${d.name}`} className="space-y-1 py-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs font-medium text-slate-900">{d.name}</span>
              <Badge>{d.kind}</Badge>
              <Badge tone={d.origin === "head" ? "indigo" : "slate"}>{d.origin}</Badge>
              {d.hole === true && <Badge tone="amber">hole</Badge>}
              {d.snapshot_id !== undefined && <SnapshotRef id={d.snapshot_id} />}
            </div>
            {(d.signature ?? d.type) !== undefined && (
              <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{d.signature ?? d.type}</pre>
            )}
            {d.value !== undefined && (
              <pre className="whitespace-pre-wrap rounded bg-amber-50 p-1.5 font-mono text-xs text-amber-900">{d.value}</pre>
            )}
          </li>
        ))}
      </ul>
      {review.library.length > 0 && (
        <Disclosure summary={`${review.library.length} library constants`}>
          <p className="font-mono text-xs text-slate-600">{review.library.join(", ")}</p>
        </Disclosure>
      )}
    </div>
  );
}

function Report({ audit, against }: { audit: AuditSnapshotsResponse; against: boolean }) {
  const s = audit.summary;
  return (
    <div className="space-y-4">
      <Verdict audit={audit} against={against} />
      <div className="flex flex-wrap gap-2">
        <Badge tone="green">{s.proved} proved</Badge>
        <Badge tone="indigo">{s.conditional} conditional</Badge>
        <Badge tone="amber">{s.sorry} sorry</Badge>
        <Badge tone="red">{s.rejected} rejected</Badge>
        <Badge tone={s.unverified_modules > 0 ? "amber" : "slate"}>{s.unverified_modules} unverified modules</Badge>
      </div>
      {audit.heads !== undefined && (
        <Section title="Heads">
          <div className="flex flex-wrap gap-2">
            {audit.heads.map((h) => (
              <SnapshotRef key={h} id={h} />
            ))}
          </div>
        </Section>
      )}
      {audit.forks !== undefined && audit.forks.length > 0 && (
        <Section title="Forks">
          {audit.forks.map((f) => (
            <div key={f.snapshot_id} className="space-y-1">
              <p className="flex items-center gap-2 text-xs text-slate-600">
                <SnapshotRef id={f.snapshot_id} /> is replaced by
              </p>
              <ul className="space-y-1 pl-4">
                {f.replaced_by.map((r) => (
                  <li key={r} className="flex flex-wrap items-center gap-2">
                    <SnapshotRef id={r} />
                    <DropBranch id={r} />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </Section>
      )}
      {audit.targets !== undefined && audit.targets.length > 0 && (
        <Section title="Targets">
          <table className="w-full text-left text-xs">
            <thead className="text-slate-500">
              <tr>
                <th className="py-1 font-medium">Declaration</th>
                <th className="py-1 font-medium">Spec</th>
                <th className="py-1 font-medium">Head</th>
                <th className="py-1 font-medium">Status</th>
                <th className="py-1 font-medium">Accepted</th>
              </tr>
            </thead>
            <tbody>
              {audit.targets.map((t) => (
                <tr key={`${t.spec}.${t.head}.${t.name}`} className="border-t border-slate-100">
                  <td className="py-1 font-mono">{t.name}</td>
                  <td className="py-1">
                    <SnapshotRef id={t.spec} />
                  </td>
                  <td className="py-1">
                    <SnapshotRef id={t.head} />
                  </td>
                  <td className="py-1">
                    <StatusBadge status={t.status} /> {!t.verified && <Badge tone="amber">unverified</Badge>}
                  </td>
                  <td className="py-1">{t.accepted ? <Badge tone="green">yes</Badge> : <Badge tone="red">no</Badge>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Section>
      )}
      {audit.comparisons !== undefined && audit.comparisons.length > 0 && (
        <Section title="Comparisons">
          {audit.comparisons.map((c) => (
            <div key={`${c.spec}.${c.head}`} className="space-y-1.5 rounded border border-slate-100 p-2">
              <p className="flex items-center gap-2 text-xs text-slate-600">
                head <SnapshotRef id={c.head} /> against spec <SnapshotRef id={c.spec} />
              </p>
              {c.reasons !== undefined && c.reasons.length > 0 && (
                <ul className="list-inside list-disc text-sm text-rose-800">
                  {c.reasons.map((r) => (
                    <li key={r}>{r}</li>
                  ))}
                </ul>
              )}
              <ChangesView changes={c.changes ?? []} />
              {c.replacement !== undefined && <ReplacementView replacement={c.replacement} />}
            </div>
          ))}
        </Section>
      )}
      {audit.links !== undefined && audit.links.length > 0 && (
        <Section title="Links that changed definitions">
          {audit.links.map((l) => (
            <div key={l.snapshot_id} className="space-y-1">
              <p className="flex flex-wrap items-center gap-2 text-xs text-slate-600">
                <SnapshotRef id={l.snapshot_id} /> replaces <SnapshotRef id={l.replaces.snapshot_id} />
                {l.replaces.allow !== undefined && (
                  <span className="font-mono text-amber-800">may change {l.replaces.allow.definitions.join(", ")}</span>
                )}
              </p>
              <ReplacementView replacement={l.replacement} />
            </div>
          ))}
        </Section>
      )}
      {audit.assumptions.length > 0 && (
        <Section title="Assumptions">
          <ul className="divide-y divide-slate-100">
            {audit.assumptions.map((a) => (
              <li key={a.name} className="space-y-1 py-1.5">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-mono text-xs font-medium">{a.name}</span>
                  {a.in_spec === true && <Badge tone="green">in the spec</Badge>}
                  {a.in_spec === false && <Badge tone="red">not in the spec</Badge>}
                  {!a.verified && <Badge tone="amber">unverified</Badge>}
                  {a.snapshot_id !== undefined && <SnapshotRef id={a.snapshot_id} />}
                </div>
                {a.signature !== undefined && <pre className="whitespace-pre-wrap font-mono text-xs text-slate-700">{a.signature}</pre>}
                <p className="font-mono text-xs text-slate-500">used by {a.used_by.join(", ")}</p>
              </li>
            ))}
          </ul>
        </Section>
      )}
      {audit.not_proved.length > 0 && (
        <Section title="Not proved">
          <ul className="space-y-1">
            {audit.not_proved.map((u) => (
              <li key={`${u.snapshot_id}.${u.name}`} className="flex flex-wrap items-center gap-2 text-xs">
                <span className="font-mono">{u.name}</span>
                <StatusBadge status={u.status} />
                <SnapshotRef id={u.snapshot_id} />
                {u.axioms.length > 0 && <span className="font-mono text-slate-500">{u.axioms.join(", ")}</span>}
              </li>
            ))}
          </ul>
        </Section>
      )}
      {audit.review !== undefined && audit.review.length > 0 && (
        <Section title="Review">
          {audit.review.map((r) => (
            <Review key={`${r.spec}.${r.head}`} review={r} />
          ))}
        </Section>
      )}
    </div>
  );
}

/** Audits a project by its labels, or a run against its spec snapshots:
 *  `initial`'s at once, if given. */
export function Audit({ version, initial }: { version: string; initial: AuditParams | undefined }) {
  const [by, setBy] = useState<"labels" | "against">(initial?.against !== undefined ? "against" : "labels");
  const [labels, setLabels] = useState(initial?.labels !== undefined ? formatLabels(initial.labels) : "playground=1");
  const [specs, setSpecs] = useState(initial?.against?.join(", ") ?? "");
  const [allow, setAllow] = useState(initial?.allow?.join(", ") ?? "");
  const [review, setReview] = useState(initial?.review ?? false);
  const [asked, setAsked] = useState<AuditParams | null>(initial ?? null);
  const audit = useAudit(asked, version !== "" ? { version } : {});
  const run = () => {
    const allowed = parseList(allow);
    setAsked(
      by === "labels"
        ? { labels: parseLabels(labels) }
        : { against: parseList(specs), ...(allowed.length > 0 && { allow: allowed }), ...(review && { review }) },
    );
  };
  return (
    <Panel
      title="Audit"
      actions={
        <Button onClick={run} disabled={by === "against" && parseList(specs).length === 0}>
          Audit
        </Button>
      }
    >
      <div className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-3">
          <Field label="Audit">
            <select className={inputClass} value={by} onChange={(e) => setBy(e.target.value as typeof by)}>
              <option value="labels">a project, by its labels</option>
              <option value="against">a run, against its spec</option>
            </select>
          </Field>
          {by === "labels" ? (
            <Field label="Labels (a=1, b=2)">
              <input className={inputClass} value={labels} onChange={(e) => setLabels(e.target.value)} />
            </Field>
          ) : (
            <>
              <Field label="Spec snapshots (ids)">
                <input className={inputClass} value={specs} onChange={(e) => setSpecs(e.target.value)} />
              </Field>
              <Field label="Also allow (definitions)">
                <input className={inputClass} value={allow} onChange={(e) => setAllow(e.target.value)} placeholder="none" />
              </Field>
            </>
          )}
        </div>
        {by === "against" && (
          <Checkbox checked={review} onChange={setReview}>
            With the review: what the spec's statements depend on, on each head
          </Checkbox>
        )}
        <ErrorNote error={audit.error} hints={{ not_found: "A spec snapshot doesn't exist." }} />
        {audit.isFetching && audit.data === undefined && <p className="text-sm text-slate-500">Auditing…</p>}
        {audit.data !== undefined && <Report audit={audit.data} against={asked?.against !== undefined} />}
      </div>
    </Panel>
  );
}
