import { PisaError, isPisaError } from "./errors.ts";
import type {
  AcceptRequest,
  AcceptResponse,
  ApplyTacticResponse,
  AuditSnapshotsResponse,
  CheckBatchResponse,
  CheckResult,
  CheckSessionRequest,
  CreateSnapshotRequest,
  CreateSnapshotResponse,
  DeclarationDetail,
  DeleteSnapshotResponse,
  Detail,
  Document,
  DocumentGoals,
  DocumentProbes,
  ErrorKind,
  GetDeclarationResponse,
  GetJobResponse,
  GoalsMode,
  Health,
  Job as JobSchema,
  Labels,
  LeanDocument,
  LinePos,
  ListAcceptancesResponse,
  ListDependentsResponse,
  ListSnapshotsParams,
  ListSnapshotsResponse,
  ModuleName,
  NamedRun,
  Pos,
  ProbeAt,
  Query,
  QueryResult,
  RebuildSnapshotsRequest,
  RebuildSnapshotsResponse,
  Resolution,
  ResolveResponse,
  ResubmitSnapshotsRequest,
  ResubmitSnapshotsResponse,
  SearchResponse,
  Session,
  SessionId,
  SettleRunRequest,
  Snapshot,
  SnapshotDetail,
  SnapshotId,
  StateId,
  UpdateSnapshotRequest,
  VerifySnapshotResponse,
} from "./types.ts";

export interface ClientOptions {
  /** The server, e.g. `https://pisa.example.com` or `/api`; it may have a
   *  path. */
  url: string;
  /** The pool version requests name when they name none. */
  version?: string;
  /** Seconds Lean may run for a request, when it says nothing (the server's
   *  default is 60). */
  timeout?: number;
  fetch?: typeof fetch;
  headers?: Record<string, string>;
  /** Labels every request is scoped to, sent as the `Pisa-Scope` header:
   *  what a request creates carries them, and what it names or reaches must,
   *  else it is `not_found`. Run names and idempotency keys are unique
   *  within a scope. */
  scope?: Record<string, string>;
  /** How requests are sent again (see `RetryPolicy`): `false` sends each
   *  once; by default, or with `true`, the default policy; fields given
   *  change it. */
  retry?: boolean | Partial<RetryPolicy>;
}

/** When and how often the client sends a request again: after a 429, which
 *  says nothing ran, and, for a request that is safe to send twice (a read,
 *  a check, a tactic, goals, probes, a verify, a create, which carries a key,
 *  or a dry run), after a 502, 503 or 504 or a failed connection. Each wait
 *  is random, up to twice the last one's bound, from `baseDelayMs` to
 *  `maxDelayMs`, and at least the server's `Retry-After`. No attempt starts
 *  later than `deadlineMs` after the first. */
export interface RetryPolicy {
  /** Attempts in all, the first included. */
  attempts: number;
  baseDelayMs: number;
  maxDelayMs: number;
  deadlineMs: number;
}

const DEFAULT_RETRY: RetryPolicy = { attempts: 4, baseDelayMs: 1000, maxDelayMs: 30_000, deadlineMs: 300_000 };

/** The `Retry-After` the server sends with a 429 or a 503, in seconds: the
 *  least wait before a batch's items that no worker took are sent again. */
const RETRY_AFTER = 2;

/** Whether a request may be sent again: `safe`, after a 429, a 502, 503 or
 *  504, or a failed connection; `unsafe`, after a 429 only, as it may have
 *  run; `never`. */
type Repeat = "safe" | "unsafe" | "never";

/** What any request takes. */
export interface RequestOptions {
  signal?: AbortSignal;
}

/** What a request that runs Lean takes. */
export interface RunOptions extends RequestOptions {
  version?: string;
  timeout?: number;
}

export interface CheckOptions extends RunOptions {
  detail?: Detail[];
  /** Up to 64 positions in the code to report on, in the result's `at`: the
   *  tactic at each, and with `detail: ["terms"]` the term there and the type
   *  Lean expected it to have. Ask about them all in one check: each check
   *  elaborates the code again. */
  at?: Pos[];
}

/** What a listing, an audit or a search takes. Requests that take an id
 *  don't: it carries its version. Search answers for the version of its
 *  index and of its snapshots, and any other is `not_found`. */
export interface VersionOptions extends RequestOptions {
  version?: string;
}

/** A rebuild runs on the version of its `from` and `to`. */
export interface RebuildOptions extends RequestOptions {
  /** Seconds Lean may run for each create. */
  timeout?: number;
  /** Only plan: create each item's `code` with its `labels`, `key` and
   *  `replaces`, a level at a time. */
  dryRun?: boolean;
  /** Plan or rebuild even though `to`, created without `replaces`, changes
   *  what `from` states. */
  allowChanges?: boolean;
  /** For a run: once every item is created, delete the old snapshots whose
   *  copies replace them, from the top down. `from` stays. */
  retire?: boolean;
  /** For a run: the kernel re-check for each create. */
  verify?: boolean;
}

export interface DeleteSnapshotOptions extends RequestOptions {
  /** Delete the snapshots that replace this one too, directly or through
   *  others, and splice nothing: a whole branch of a run goes. What the
   *  snapshot replaced is a head again if nothing else replaces it. */
  withReplacers?: boolean;
}

export interface GetDeclarationOptions extends VersionOptions {
  /** A snapshot module to look in first, as a snapshot hit's `module`. */
  module?: ModuleName;
}

/** What `auditSnapshots` audits: a project by its `labels`, or a run
 *  `against` its spec snapshots. */
export interface AuditParams {
  labels?: Labels;
  against?: SnapshotId[];
  /** With `against`: definitions the run may change. */
  allow?: string[];
  /** With `against`: also list each head's declarations, with its holes'
   *  values, for a reader to compare with the spec. */
  review?: boolean;
  /** With `review`: the most declarations the list holds over all its
   *  entries, 1 to 10000 (the server's 1000 unless given);
   *  `review_left_out` counts the rest. */
  reviewLimit?: number;
}

/** An acceptance's limits: `timeout` for each pair (600 by default) and
 *  `buildTimeout` more for the first, which builds the run (1800). The
 *  server's unless given, not the client's timeout. */
export type AcceptOptions = Omit<RunOptions, "version"> & { buildTimeout?: number };

/** A session's check takes no version: the session's id carries it. Of
 *  `detail`, only `declarations` applies. */
export type CheckSessionOptions = Omit<RunOptions, "version"> & { detail?: Detail[] };

/** With `replaces` and no `version`, the snapshot is built on the replaced
 *  snapshot's version, not the client's: a different one is refused. */
export type CreateSnapshotOptions = RunOptions &
  Pick<
    CreateSnapshotRequest,
    "verify" | "labels" | "key" | "replaces" | "fork" | "holes" | "amends" | "refutes" | "detail" | "name"
  >;

export interface VerifyOptions extends RequestOptions {
  /** Seconds Lean may run for each module. */
  timeout?: number;
  /** Re-check the modules that are verified already too, which describes
   *  their records again. */
  again?: boolean;
}

/** A settle runs on the version of its spec snapshots. */
export interface SettleOptions extends RequestOptions {
  /** Seconds Lean may run for each create. */
  timeout?: number;
  /** As the audit's `allow`. */
  allow?: string[];
  /** Only plan. */
  dryRun?: boolean;
  /** For a run: the kernel re-check for each create. */
  verify?: boolean;
}

/** A resubmit runs on the version of its `from` and `to`. */
export interface ResubmitOptions extends RequestOptions {
  /** Seconds Lean may run for each create. */
  timeout?: number;
  /** Only plan: compare each head with the amending spec. */
  dryRun?: boolean;
  /** The new snapshots' labels; by default each head's. */
  labels?: Labels;
  /** For a run: the kernel re-check for each create. */
  verify?: boolean;
}

/** A request sent with `Prefer: respond-async`: its job, whose answer is a
 *  `T`. Anyone with its id can read the answer or cancel it, so keep it
 *  secret; the server keeps the answer for an hour after the job is done. */
export interface Job<T> extends JobSchema {
  /** The answer's type; never set. */
  readonly answer?: T;
}

/** Where a job stands: done with its request's answer, or not yet. */
export type JobPoll<T> = { done: true; answer: T } | { done: false; job: JobSchema };

export interface JobOptions extends RequestOptions {
  /** Seconds to wait for the job to finish, at most 60; by default none. */
  wait?: number;
}

export interface WaitJobOptions extends RequestOptions {
  /** Seconds each look waits, 1–60; 25 by default. A job that finishes is
   *  answered at once, whatever this is, so it only sets how soon a job's
   *  progress is seen. */
  wait?: number;
  /** Called with the job each time a look finds it `queued` or
   *  `running`. */
  onPending?: (job: JobSchema) => void;
}

/** How long each look of `waitJob` waits: well short of the 60 s at which
 *  proxies cut requests off, which async requests are there to avoid. */
const JOB_WAIT = 25;

const ERROR_KINDS: ReadonlySet<string> = new Set<ErrorKind>([
  "timeout",
  "out_of_memory",
  "crash",
  "policy",
  "lean",
  "capacity",
  "unavailable",
  "bad_request",
  "not_found",
  "conflict",
  "internal",
]);

/** A client of one pisa server. Every method returns the response's JSON, or
 *  throws a `PisaError`. */
export class PisaClient {
  readonly url: string;
  readonly version: string | undefined;
  readonly timeout: number | undefined;
  /** How requests are sent again, or `undefined` if each is sent once. */
  readonly retry: RetryPolicy | undefined;
  private readonly fetcher: typeof fetch;
  private readonly headers: Record<string, string>;

  constructor(options: ClientOptions) {
    this.url = options.url.replace(/\/+$/, "");
    this.version = options.version;
    this.timeout = options.timeout;
    const retry = options.retry ?? true;
    this.retry = retry === false ? undefined : { ...DEFAULT_RETRY, ...(retry === true ? {} : retry) };
    this.fetcher = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.headers = { ...options.headers };
    const scope = Object.entries(options.scope ?? {});
    if (scope.length > 0) this.headers["pisa-scope"] = scope.map(([k, v]) => `${k}=${v}`).join(",");
  }

  /** The server's health. A draining server's 503 is returned, not thrown. */
  async health(options: RequestOptions = {}): Promise<Health> {
    const r = await this.send("GET", ["health"], undefined, undefined, options.signal, {}, "never");
    const body = await readJson(r);
    if (r.status === 503 && isObject(body) && typeof body["status"] === "string") {
      return body as unknown as Health;
    }
    return ok<Health>(r, body);
  }

  /** Checks one piece of code. `ok` is the verdict; a check that couldn't
   *  complete has `error` set rather than throwing. */
  check(code: string, options: CheckOptions = {}): Promise<CheckResult> {
    return this.call("POST", ["check"], { code, ...this.run(options), ...asked(options) }, options);
  }

  /** `check` as a job, which outlives the request and its proxies: wait for
   *  it with `waitJob`. */
  submitCheck(code: string, options: CheckOptions = {}): Promise<Job<CheckResult>> {
    return this.submit(["check"], { code, ...this.run(options), ...asked(options) }, options);
  }

  /** Checks several pieces of code, up to 128; every one gets a result, in
   *  order. */
  async checkBatch(codes: string[], options: CheckOptions = {}): Promise<CheckResult[]> {
    const rest = { ...this.run(options), ...asked(options) };
    const batch = async (items: string[]) => {
      const body = { items: items.map((code) => ({ code })), ...rest };
      return (await this.call<CheckBatchResponse>("POST", ["check", "batch"], body, options)).results;
    };
    const results = await batch(codes);
    // An item no worker took in time, or whose worker was lost, is sent again
    // alone, as a 429 or a 503 of `check` would be.
    const started = Date.now();
    for (let made = 1; ; made += 1) {
      const failed = results.flatMap((r, i) =>
        r.error?.kind === "capacity" || r.error?.kind === "unavailable" ? [i] : [],
      );
      const wait = this.retry && delay(this.retry, made, Date.now() - started, RETRY_AFTER * 1000);
      if (failed.length === 0 || wait === undefined) return results;
      await sleep(wait, options.signal);
      const again = await batch(failed.map((i) => codes[i] ?? ""));
      failed.forEach((i, j) => {
        const r = again[j];
        if (r !== undefined) results[i] = r;
      });
    }
  }

  /** Starts a session on code with `sorry`s, each of which becomes a state. */
  startSession(code: string, options: RunOptions = {}): Promise<Session> {
    return this.call("POST", ["sessions"], { code, ...this.run(options) }, options);
  }

  /** `startSession` as a job. */
  submitStartSession(code: string, options: RunOptions = {}): Promise<Job<Session>> {
    return this.submit(["sessions"], { code, ...this.run(options) }, options);
  }

  /** Applies a tactic to a state; the state stays valid, so this can branch.
   *  A tactic Lean rejects throws `lean`. */
  applyTactic(state: StateId, tactic: string, options: Omit<RunOptions, "version"> = {}): Promise<ApplyTacticResponse> {
    return this.call(
      "POST",
      ["states", state, "tactic"],
      { tactic, ...this.timeoutOf(options) },
      options,
    );
  }

  /** `applyTactic` as a job. */
  submitApplyTactic(
    state: StateId,
    tactic: string,
    options: Omit<RunOptions, "version"> = {},
  ): Promise<Job<ApplyTacticResponse>> {
    return this.submit(["states", state, "tactic"], { tactic, ...this.timeoutOf(options) }, options);
  }

  /** Checks the completed code of a session from scratch. `detail:
   *  ["declarations"]` gives every field of its declarations. */
  checkSession(session: SessionId, code: string, options: CheckSessionOptions = {}): Promise<CheckResult> {
    return this.call("POST", ["sessions", session, "check"], this.checkSessionRequest(code, options), options);
  }

  /** `checkSession` as a job. */
  submitCheckSession(
    session: SessionId,
    code: string,
    options: CheckSessionOptions = {},
  ): Promise<Job<CheckResult>> {
    return this.submit(["sessions", session, "check"], this.checkSessionRequest(code, options), options);
  }

  /** Checks code and keeps it as a snapshot that later code can import
   *  through its `header`. Failures are in `check.error`, with no
   *  `snapshot`. */
  createSnapshot(code: string, options: CreateSnapshotOptions = {}): Promise<CreateSnapshotResponse> {
    return this.call("POST", ["snapshots"], this.createSnapshotRequest(code, options), options);
  }

  /** `createSnapshot` as a job. */
  submitCreateSnapshot(code: string, options: CreateSnapshotOptions = {}): Promise<Job<CreateSnapshotResponse>> {
    return this.submit(["snapshots"], this.createSnapshotRequest(code, options), options);
  }

  /** A snapshot, with its module's code, check and exports. `detail:
   *  ["declarations"]` gives every field of the check's declarations. */
  getSnapshot(id: SnapshotId, options: RequestOptions & { detail?: Detail[] } = {}): Promise<SnapshotDetail> {
    const query = new URLSearchParams();
    if (options.detail?.includes("declarations")) query.append("detail", "declarations");
    return this.call("GET", ["snapshots", id], undefined, options, query.size ? query : undefined);
  }

  /** A page of snapshots, newest first; pass `next` back as `after`. With
   *  `heads`, only those that no other snapshot replaces. */
  listSnapshots(params: ListSnapshotsParams = {}, options: VersionOptions = {}): Promise<ListSnapshotsResponse> {
    const query = this.versionQuery(options);
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    if (params.limit !== undefined) query.set("limit", String(params.limit));
    if (params.after !== undefined) query.set("after", params.after);
    if (params.heads) query.set("heads", "true");
    return this.call("GET", ["snapshots"], undefined, options, query);
  }

  /** What the snapshots with every label rest on: their declarations by
   *  `status`, each assumption (an axiom declared in snapshot code) with what
   *  uses it, and what isn't proved. With `against`, a run's spec snapshots,
   *  all of one version, instead: the `heads` the run reaches through the
   *  snapshots that replace them, the `forks` on the way, each spec
   *  declaration's status on each head (`targets`), the `links` that allowed
   *  changes, and whether the run is `clean`, with no definition changed
   *  but the spec's holes and those `allow` names (the links' own allows
   *  don't count). The ids
   *  carry the version, and `labels` doesn't combine with them. An empty
   *  `against` is refused (`bad_request`) without sending it, as it would be
   *  a label audit. */
  auditSnapshots(params: AuditParams = {}, options: VersionOptions = {}): Promise<AuditSnapshotsResponse> {
    if (params.against?.length === 0) {
      const message = "against names no snapshot: audit a run against its spec snapshots, or a project by its labels";
      return Promise.reject(new PisaError("bad_request", message));
    }
    if (params.review && !params.against) {
      const message = "review is for an audit against a spec: name its spec snapshots in against";
      return Promise.reject(new PisaError("bad_request", message));
    }
    const query = params.against ? new URLSearchParams() : this.versionQuery(options);
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    for (const id of params.against ?? []) query.append("against", id);
    for (const name of params.allow ?? []) query.append("allow", name);
    if (params.review) query.set("review", "true");
    if (params.reviewLimit !== undefined) query.set("review_limit", String(params.reviewLimit));
    return this.call("GET", ["snapshots", "audit"], undefined, options, query);
  }

  /** The run that spec snapshots `against` start, as a Lake project for
   *  `lake comparator` in `pisa-run/` of a tar.gz: its bytes. */
  async exportRun(
    against: SnapshotId[],
    params: { allow?: string[] } = {},
    options: RequestOptions = {},
  ): Promise<ArrayBuffer> {
    const query = new URLSearchParams();
    for (const id of against) query.append("against", id);
    for (const name of params.allow ?? []) query.append("allow", name);
    const r = await this.send("GET", ["snapshots", "export"], undefined, query, options.signal);
    if (!r.ok) ok(r, await readJson(r));
    return r.arrayBuffer();
  }

  /** Judges the run that spec snapshots `against` start: audits it and runs
   *  `lake comparator` on the pairs the audit compared, with `timeout`
   *  seconds for each pair, and stores the result. It is `accepted` when the
   *  audit is `clean` and comparator accepted every pair. */
  acceptRun(
    against: SnapshotId[],
    params: { allow?: string[]; paranoid?: boolean } = {},
    options: AcceptOptions = {},
  ): Promise<AcceptResponse> {
    return this.call("POST", ["snapshots", "accept"], this.acceptRequest(against, params, options), options);
  }

  /** `acceptRun` as a job. */
  submitAcceptRun(
    against: SnapshotId[],
    params: { allow?: string[]; paranoid?: boolean } = {},
    options: AcceptOptions = {},
  ): Promise<Job<AcceptResponse>> {
    return this.submit(["snapshots", "accept"], this.acceptRequest(against, params, options), options);
  }

  /** An acceptance `acceptRun` stored, by its id. */
  getAcceptance(id: string, options: RequestOptions = {}): Promise<AcceptResponse> {
    return this.call("GET", ["snapshots", "accept", id], undefined, options);
  }

  /** The run named `name`: its root, its heads, and, when they are on one
   *  module, what `import PisaRun.<name>` resolves to now. */
  getRun(name: string, options: VersionOptions = {}): Promise<NamedRun> {
    return this.call("GET", ["runs", name], undefined, options, this.versionQuery(options));
  }

  /** Names `name` the run snapshot `snapshotId` is in, as a create's `name`
   *  names the run it starts: a name the run had goes, and one another run
   *  with snapshots has is a conflict. */
  nameRun(name: string, snapshotId: SnapshotId, options: RequestOptions = {}): Promise<NamedRun> {
    return this.call("PUT", ["runs", name], { snapshot_id: snapshotId }, options);
  }

  /** The acceptances that judged spec snapshot `spec`, newest first, at most
   *  100; each says `stale` once the run's heads have moved on. */
  listAcceptances(spec: SnapshotId, options: RequestOptions = {}): Promise<ListAcceptancesResponse> {
    const query = new URLSearchParams({ spec });
    return this.call("GET", ["snapshots", "accept"], undefined, options, query);
  }

  private acceptRequest(
    against: SnapshotId[],
    params: { allow?: string[]; paranoid?: boolean },
    options: AcceptOptions,
  ): AcceptRequest {
    // An acceptance has limits of its own, not the client's.
    const body: AcceptRequest = { against };
    if (options.timeout !== undefined) body.timeout = options.timeout;
    if (options.buildTimeout !== undefined) body.build_timeout = options.buildTimeout;
    if (params.allow !== undefined && params.allow.length > 0) body.allow = params.allow;
    if (params.paranoid !== undefined) body.paranoid = params.paranoid;
    return body;
  }

  /** Opens `code` on Lean Beam, which keeps it elaborated for goals and
   *  probes until `closeDocument` or until it expires. */
  openDocument(code: string, options: RunOptions = {}): Promise<Document> {
    return this.call("POST", ["documents"], { code, ...this.run(options) }, options);
  }

  /** Replaces a document's code, which is at `revision`. */
  editDocument(
    id: string,
    revision: number,
    code: string,
    options: Omit<RunOptions, "version"> = {},
  ): Promise<Document> {
    return this.call("PATCH", ["documents", id], { code, revision, ...this.timeoutOf(options) }, options);
  }

  /** Closes a document and deletes its code. */
  async closeDocument(id: string, options: RequestOptions = {}): Promise<void> {
    const r = await this.send("DELETE", ["documents", id], undefined, undefined, options.signal);
    if (!r.ok) ok(r, await readJson(r));
  }

  /** The goals at each of `at`, lines from 1; a position with only a line
   *  is the line's first tactic. With `revision`, a document at another
   *  revision is a `conflict`. */
  documentGoals(
    id: string,
    at: LinePos[],
    params: { revision?: number; mode?: GoalsMode } = {},
    options: Omit<RunOptions, "version"> = {},
  ): Promise<DocumentGoals> {
    const body = { at, ...params, ...this.timeoutOf(options) };
    return this.call("POST", ["documents", id, "goals"], body, options);
  }

  /** Runs each candidate tactic at `at`, a `sorry`'s index or a position, on
   *  the document as it is: nothing a candidate does stays. */
  probeDocument(
    id: string,
    at: ProbeAt,
    candidates: string[],
    params: { revision?: number } = {},
    options: Omit<RunOptions, "version"> = {},
  ): Promise<DocumentProbes> {
    const body = { at, candidates, ...params, ...this.timeoutOf(options) };
    return this.call("POST", ["documents", id, "probes"], body, options);
  }

  /** The snapshots of every module that imports `id`'s, directly or through
   *  others, parents first: by level, then oldest first. */
  listDependents(
    id: SnapshotId,
    params: { labels?: Labels } = {},
    options: RequestOptions = {},
  ): Promise<ListDependentsResponse> {
    const query = new URLSearchParams();
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    return this.call("GET", ["snapshots", id, "dependents"], undefined, options, query);
  }

  /** Rebuilds `from`'s dependents with every label in `labels` on `to`: the
   *  server plans the creates that copy their heads, by level, and unless
   *  `dryRun` runs them, a level at a time, then with `retire` deletes the
   *  old snapshots whose copies replace them, though not `from`. Sending it
   *  again replays the creates that are done. A run can take minutes:
   *  `submitRebuildSnapshots` runs it as a job. */
  rebuildSnapshots(
    from: SnapshotId,
    to: SnapshotId,
    labels: Labels,
    options: RebuildOptions = {},
  ): Promise<RebuildSnapshotsResponse> {
    return this.call("POST", ["snapshots", "rebuild"], this.rebuildSnapshotsRequest(from, to, labels, options), options);
  }

  /** `rebuildSnapshots` as a job, for a run. A dry run is never one: the
   *  server answers it at once, so it is refused here unsent. */
  submitRebuildSnapshots(
    from: SnapshotId,
    to: SnapshotId,
    labels: Labels,
    options: Omit<RebuildOptions, "dryRun"> = {},
  ): Promise<Job<RebuildSnapshotsResponse>> {
    if ((options as RebuildOptions).dryRun) {
      return Promise.reject(new PisaError("bad_request", "a dry run is answered at once: call rebuildSnapshots"));
    }
    return this.submit(["snapshots", "rebuild"], this.rebuildSnapshotsRequest(from, to, labels, options), options);
  }

  private rebuildSnapshotsRequest(
    from: SnapshotId,
    to: SnapshotId,
    labels: Labels,
    options: RebuildOptions,
  ): RebuildSnapshotsRequest {
    const body: RebuildSnapshotsRequest = { from, to, labels, dry_run: options.dryRun ?? false, ...this.timeoutOf(options) };
    if (options.allowChanges) body.allow_changes = true;
    if (options.retire) body.retire = true;
    if (options.verify !== undefined) body.verify = options.verify;
    return body;
  }

  /** Carries the run of spec `from` over to `to`, the spec that amends it:
   *  the server compares each head of the run with `to`, and unless `dryRun`
   *  creates each head that keeps `to`'s statements again, replacing `to`.
   *  Sending it again replays the creates that are done. */
  resubmitSnapshots(from: SnapshotId, to: SnapshotId, options: ResubmitOptions = {}): Promise<ResubmitSnapshotsResponse> {
    return this.call("POST", ["snapshots", "resubmit"], this.resubmitRequest(from, to, options), options);
  }

  /** `resubmitSnapshots` as a job. A dry run isn't one: the server answers
   *  it at once, which throws here. */
  submitResubmitSnapshots(
    from: SnapshotId,
    to: SnapshotId,
    options: ResubmitOptions = {},
  ): Promise<Job<ResubmitSnapshotsResponse>> {
    return this.submit(["snapshots", "resubmit"], this.resubmitRequest(from, to, options), options);
  }

  private resubmitRequest(from: SnapshotId, to: SnapshotId, options: ResubmitOptions): ResubmitSnapshotsRequest {
    const body: ResubmitSnapshotsRequest = { from, to, ...this.timeoutOf(options) };
    if (options.dryRun) body.dry_run = true;
    if (options.labels !== undefined) body.labels = options.labels;
    if (options.verify !== undefined) body.verify = options.verify;
    return body;
  }

  /** Changes a snapshot's labels as a merge patch: a label set to `null` is
   *  removed, one set to a string takes it, and the rest stay. */
  updateSnapshot(id: SnapshotId, update: UpdateSnapshotRequest, options: RequestOptions = {}): Promise<Snapshot> {
    return this.call("PATCH", ["snapshots", id], update, options);
  }

  /** Deletes a snapshot, and the modules nothing else needs. The snapshots
   *  that replace it replace what it replaced instead. With `withReplacers`
   *  it deletes those too, and the ones that replace them; `deleted` lists
   *  every snapshot that went, the asked one first. */
  deleteSnapshot(id: SnapshotId, options: DeleteSnapshotOptions = {}): Promise<DeleteSnapshotResponse> {
    const query = new URLSearchParams();
    if (options.withReplacers) query.set("with_replacers", "true");
    return this.call("DELETE", ["snapshots", id], undefined, options, query);
  }

  /** Re-checks the snapshot's module and its imports through the kernel,
   *  with `timeout` seconds for each module once their imports are loaded. A
   *  rejection is in `failure`, not thrown. */
  verifySnapshot(id: SnapshotId, options: VerifyOptions = {}): Promise<VerifySnapshotResponse> {
    return this.call("POST", ["snapshots", id, "verify"], undefined, options, this.verifyQuery(options));
  }

  /** `verifySnapshot` as a job. */
  submitVerifySnapshot(id: SnapshotId, options: VerifyOptions = {}): Promise<Job<VerifySnapshotResponse>> {
    return this.submit(["snapshots", id, "verify"], undefined, options, this.verifyQuery(options));
  }

  private verifyQuery(options: VerifyOptions): URLSearchParams {
    const query = this.timeoutQuery(options);
    if (options.again) query.set("again", "true");
    return query;
  }

  /** Settles the run that spec snapshots `against` start: rebuilds each of
   *  its heads whose targets hold only through links onto the heads they
   *  are read through, in one plan, as the audit's `unsettled` lists them.
   *  A head with blockers refuses the plan, with them in `changes`. */
  settleRun(against: SnapshotId[], options: SettleOptions = {}): Promise<RebuildSnapshotsResponse> {
    return this.call("POST", ["snapshots", "settle"], this.settleRequest(against, options), options);
  }

  /** `settleRun` as a job, for a run. A dry run is never one: the server
   *  answers it at once, so it is refused here unsent. */
  submitSettleRun(
    against: SnapshotId[],
    options: Omit<SettleOptions, "dryRun"> = {},
  ): Promise<Job<RebuildSnapshotsResponse>> {
    if ((options as SettleOptions).dryRun) {
      return Promise.reject(new PisaError("bad_request", "a dry run is answered at once: call settleRun"));
    }
    return this.submit(["snapshots", "settle"], this.settleRequest(against, options), options);
  }

  private settleRequest(against: SnapshotId[], options: SettleOptions): SettleRunRequest {
    const body: SettleRunRequest = { against, ...this.timeoutOf(options) };
    if (options.allow !== undefined && options.allow.length > 0) body.allow = options.allow;
    if (options.dryRun) body.dry_run = true;
    if (options.verify !== undefined) body.verify = options.verify;
    return body;
  }

  /** Where `job` stands, after waiting up to `wait` seconds for it to
   *  finish. Done, the answer is what its request would have returned, and
   *  an error is thrown as it would have been. */
  async pollJob<T>(job: Job<T>, options: JobOptions = {}): Promise<JobPoll<T>> {
    const query = new URLSearchParams();
    if (options.wait !== undefined) query.set("wait", String(options.wait));
    const r = await this.send("GET", ["jobs", job.id], undefined, query, options.signal);
    const body = await readJson(r);
    if (r.status === 202) return { done: false, job: body as JobSchema };
    // Done: the status and answer its request would have had.
    const done = ok<GetJobResponse>(r, body);
    return { done: true, answer: answerOf<T>(done.http_status, "", done.body) };
  }

  /** Waits for `job` to finish, however long it takes, and returns what its
   *  request would have, or throws what it would have thrown. Each look is
   *  sent again as the client's `retry` says. */
  async waitJob<T>(job: Job<T>, options: WaitJobOptions = {}): Promise<T> {
    const { wait = JOB_WAIT, onPending, ...request } = options;
    for (;;) {
      const polled = await this.pollJob(job, { ...request, wait });
      if (polled.done) return polled.answer;
      onPending?.(polled.job);
    }
  }

  /** Withdraws `job` if no worker has started it (`cancelled`); one that is
   *  `running` or `done` stays as it is. */
  cancelJob(job: JobSchema, options: RequestOptions = {}): Promise<JobSchema> {
    return this.call("DELETE", ["jobs", job.id], undefined, options);
  }

  async search(query: Query, options: VersionOptions = {}): Promise<QueryResult> {
    const [result] = await this.searchBatch([query], options);
    if (result === undefined) throw new PisaError("bad_response", "the search returned no result");
    return result;
  }

  /** Runs up to 64 queries; one result each, in order. */
  async searchBatch(queries: Query[], options: VersionOptions = {}): Promise<QueryResult[]> {
    const r = await this.call<SearchResponse>("POST", ["search"], { queries, ...this.versionOf(options) }, options);
    return r.results;
  }

  /** Whether each name exists, and close names for those that don't. */
  async resolve(names: string[], options: VersionOptions = {}): Promise<Resolution[]> {
    const r = await this.call<ResolveResponse>("POST", ["resolve"], { names, ...this.versionOf(options) }, options);
    return r.results;
  }

  /** A declaration with its description, dependencies and users: the one in
   *  snapshot module `module` if it has one, else the bundle's. */
  async getDeclaration(name: string, options: GetDeclarationOptions = {}): Promise<DeclarationDetail> {
    const query = this.versionQuery(options);
    if (options.module !== undefined) query.set("module", options.module);
    const r = await this.call<GetDeclarationResponse>("GET", ["declarations", name], undefined, options, query);
    return r.declaration;
  }

  private checkSessionRequest(code: string, options: CheckSessionOptions): CheckSessionRequest {
    return { code, ...this.timeoutOf(options), ...(options.detail ? { detail: options.detail } : {}) };
  }

  private createSnapshotRequest(code: string, options: CreateSnapshotOptions): CreateSnapshotRequest {
    // The server takes the version of a create that names a spec, to replace,
    // amend or refute, from that spec.
    const named = options.replaces !== undefined || options.amends !== undefined || options.refutes !== undefined;
    const run = named && options.version === undefined ? this.timeoutOf(options) : this.run(options);
    const body: CreateSnapshotRequest = { code, ...run };
    if (options.verify !== undefined) body.verify = options.verify;
    if (options.labels !== undefined) body.labels = options.labels;
    // A key of the client's, if the call has none, so that sending the
    // create again can't keep a second snapshot.
    body.key = options.key ?? clientKey();
    if (options.replaces !== undefined) body.replaces = options.replaces;
    if (options.fork !== undefined) body.fork = options.fork;
    if (options.holes !== undefined) body.holes = options.holes;
    if (options.amends !== undefined) body.amends = options.amends;
    if (options.refutes !== undefined) body.refutes = options.refutes;
    if (options.detail !== undefined) body.detail = options.detail;
    if (options.name !== undefined) body.name = options.name;
    return body;
  }

  /** POSTs with `Prefer: respond-async`: the job the server queued. */
  private async submit<T>(
    path: string[],
    body: unknown,
    options: RequestOptions,
    query?: URLSearchParams,
  ): Promise<Job<T>> {
    const r = await this.send("POST", path, body, query, options.signal, { prefer: "respond-async" });
    const answer = ok<Job<T>>(r, await readJson(r));
    if (r.status !== 202) {
      throw new PisaError("bad_response", "the server answered at once: it doesn't queue requests", r.status);
    }
    return answer;
  }

  private run(options: RunOptions): { version?: string; timeout?: number } {
    const version = options.version ?? this.version;
    return { ...(version !== undefined && { version }), ...this.timeoutOf(options) };
  }

  private timeoutOf(options: RunOptions): { timeout?: number } {
    const timeout = options.timeout ?? this.timeout;
    return timeout !== undefined ? { timeout } : {};
  }

  private timeoutQuery(options: RunOptions): URLSearchParams {
    const { timeout } = this.timeoutOf(options);
    return new URLSearchParams(timeout !== undefined ? { timeout: String(timeout) } : {});
  }

  private versionQuery(options: VersionOptions): URLSearchParams {
    const query = new URLSearchParams();
    const version = options.version ?? this.version;
    if (version !== undefined) query.set("version", version);
    return query;
  }

  private versionOf(options: VersionOptions): { version?: string } {
    const version = options.version ?? this.version;
    return version !== undefined ? { version } : {};
  }

  private async call<T>(
    method: string,
    path: string[],
    body: unknown,
    options: RequestOptions,
    query?: URLSearchParams,
  ): Promise<T> {
    const r = await this.send(method, path, body, query, options.signal);
    return ok<T>(r, await readJson(r));
  }

  /** Sends a request, again as `retry` allows for one classed `repeat`. */
  private async send(
    method: string,
    path: string[],
    body: unknown,
    query: URLSearchParams | undefined,
    signal: AbortSignal | undefined,
    extra: Record<string, string> = {},
    repeat: Repeat = repeatOf(method, path, body),
  ): Promise<Response> {
    const started = Date.now();
    for (let made = 1; ; made += 1) {
      let answer: Response | undefined;
      let failure: unknown;
      try {
        answer = await this.sendOnce(method, path, body, query, signal, extra);
      } catch (e) {
        if (!isPisaError(e, "network")) throw e;
        failure = e;
      }
      const again =
        answer === undefined
          ? repeat === "safe"
          : (answer.status === 429 && repeat !== "never") ||
            (repeat === "safe" && answer.status >= 502 && answer.status <= 504);
      const retryAfter = Number(answer?.headers.get("retry-after") ?? NaN);
      const floor = Number.isFinite(retryAfter) ? retryAfter * 1000 : 0;
      const wait = again && this.retry ? delay(this.retry, made, Date.now() - started, floor) : undefined;
      if (wait === undefined) {
        if (answer === undefined) throw failure;
        return answer;
      }
      await answer?.body?.cancel();
      await sleep(wait, signal);
    }
  }

  private async sendOnce(
    method: string,
    path: string[],
    body: unknown,
    query: URLSearchParams | undefined,
    signal: AbortSignal | undefined,
    extra: Record<string, string>,
  ): Promise<Response> {
    const search = query?.toString();
    const url = `${this.url}/${path.map(encodeURIComponent).join("/")}${search ? `?${search}` : ""}`;
    const headers: Record<string, string> = { ...this.headers, ...extra };
    if (body !== undefined) headers["content-type"] = "application/json";
    try {
      return await this.fetcher(url, {
        method,
        headers,
        ...(body !== undefined && { body: JSON.stringify(body) }),
        ...(signal !== undefined && { signal }),
      });
    } catch (e) {
      if (signal?.aborted) throw new PisaError("aborted", "the request was aborted", undefined, { cause: e });
      throw new PisaError("network", `no answer from ${url}: ${String(e)}`, undefined, { cause: e });
    }
  }
}

/** Whether a request may be sent again, by its method and path: reads,
 *  checks, tactics, goals, probes, verifies, creates, searches and dry runs
 *  are `safe`; other writes `unsafe`. */
function repeatOf(method: string, path: string[], body: unknown): Repeat {
  if (method === "GET") return "safe";
  if (method !== "POST") return "unsafe";
  const [first, , third] = path;
  const route = path.length === 3 ? `${first}/*/${third}` : path.join("/");
  if (["check", "check/batch", "snapshots", "search", "resolve"].includes(route)) return "safe";
  if (["states/*/tactic", "sessions/*/check", "snapshots/*/verify"].includes(route)) return "safe";
  if (["documents/*/goals", "documents/*/probes"].includes(route)) return "safe";
  const dryRun = isObject(body) && body["dry_run"] === true;
  return dryRun ? "safe" : "unsafe";
}

/** How long to wait before the next attempt, after `made` attempts and
 *  `elapsedMs` since the first, at least `floorMs`; `undefined` when no
 *  attempt is left. */
function delay(policy: RetryPolicy, made: number, elapsedMs: number, floorMs: number): number | undefined {
  if (made >= policy.attempts) return undefined;
  const bound = Math.min(policy.maxDelayMs, policy.baseDelayMs * 2 ** (made - 1));
  const wait = Math.max(bound * Math.random(), floorMs);
  return elapsedMs + wait <= policy.deadlineMs ? wait : undefined;
}

/** Waits `ms`, or throws `aborted` when `signal` fires first. */
function sleep(ms: number, signal: AbortSignal | undefined): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(new PisaError("aborted", "the request was aborted"));
    const stop = () => {
      clearTimeout(timer);
      reject(new PisaError("aborted", "the request was aborted"));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", stop);
      resolve();
    }, ms);
    signal?.addEventListener("abort", stop, { once: true });
  });
}

/** A key for a create that has none: `client:` and 32 hex digits. */
function clientKey(): string {
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
  return `client:${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`;
}

/** What a check asks for beyond its verdict. */
function asked(options: CheckOptions): { detail?: Detail[]; at?: Pos[] } {
  return {
    ...(options.detail !== undefined && options.detail.length > 0 ? { detail: options.detail } : {}),
    ...(options.at !== undefined && options.at.length > 0 ? { at: options.at } : {}),
  };
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** The body as JSON, or `undefined` if it isn't JSON. */
async function readJson(r: Response): Promise<unknown> {
  const text = await r.text();
  if (text === "") return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

/** The body of a successful response, or the error it carries. */
function ok<T>(r: Response, body: unknown): T {
  return answerOf<T>(r.status, r.statusText, body);
}

/** The answer `body` is with `status`, or the error it is thrown as. */
function answerOf<T>(status: number, statusText: string, body: unknown): T {
  if (status >= 200 && status < 300) {
    if (body === undefined) throw new PisaError("bad_response", "the response isn't JSON", status);
    return body as T;
  }
  const error = isObject(body) ? body["error"] : undefined;
  if (isObject(error) && typeof error["kind"] === "string" && ERROR_KINDS.has(error["kind"])) {
    throw new PisaError(error["kind"] as ErrorKind, String(error["message"] ?? ""), status);
  }
  // A gateway's error: the server behind it may answer later.
  const kind = status >= 502 && status <= 504 ? "unavailable" : "bad_response";
  throw new PisaError(kind, `HTTP ${status} ${statusText}`.trim(), status);
}
