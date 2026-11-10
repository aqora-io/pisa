import { PisaError, isPisaError } from "./errors.ts";
import type {
  ApplyTacticResponse,
  AuditSnapshotsResponse,
  CheckBatchResponse,
  CheckResult,
  CreateSnapshotRequest,
  CreateSnapshotResponse,
  DeclarationDetail,
  DeleteSnapshotResponse,
  Detail,
  ErrorKind,
  GetDeclarationResponse,
  Health,
  Job as JobSchema,
  Labels,
  ListDependentsResponse,
  ListSnapshotsParams,
  ListSnapshotsResponse,
  ModuleName,
  Pos,
  Query,
  QueryResult,
  RebuildSnapshotsRequest,
  RebuildSnapshotsResponse,
  ResolveResponse,
  Resolution,
  SearchResponse,
  Session,
  SessionId,
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
}

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

export type CreateSnapshotOptions = RunOptions &
  Pick<CreateSnapshotRequest, "verify" | "labels" | "key" | "replaces" | "fork" | "holes">;

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

/** How long each look of `waitJob` waits: the server's longest. */
const JOB_WAIT = 60;

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
  private readonly fetcher: typeof fetch;
  private readonly headers: Record<string, string>;

  constructor(options: ClientOptions) {
    this.url = options.url.replace(/\/+$/, "");
    this.version = options.version;
    this.timeout = options.timeout;
    this.fetcher = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.headers = options.headers ?? {};
  }

  /** The server's health. A draining server's 503 is returned, not thrown. */
  async health(options: RequestOptions = {}): Promise<Health> {
    const r = await this.send("GET", ["health"], undefined, undefined, options.signal);
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

  /** Checks several pieces of code; every one gets a result, in order. */
  async checkBatch(codes: string[], options: CheckOptions = {}): Promise<CheckResult[]> {
    const body = { items: codes.map((code) => ({ code })), ...this.run(options), ...asked(options) };
    const r = await this.call<CheckBatchResponse>("POST", ["check", "batch"], body, options);
    return r.results;
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

  /** Checks the completed code of a session from scratch. */
  checkSession(session: SessionId, code: string, options: Omit<RunOptions, "version"> = {}): Promise<CheckResult> {
    return this.call(
      "POST",
      ["sessions", session, "check"],
      { code, ...this.timeoutOf(options) },
      options,
    );
  }

  /** `checkSession` as a job. */
  submitCheckSession(
    session: SessionId,
    code: string,
    options: Omit<RunOptions, "version"> = {},
  ): Promise<Job<CheckResult>> {
    return this.submit(["sessions", session, "check"], { code, ...this.timeoutOf(options) }, options);
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

  /** A snapshot, with its module's code, check and exports. */
  getSnapshot(id: SnapshotId, options: RequestOptions = {}): Promise<SnapshotDetail> {
    return this.call("GET", ["snapshots", id], undefined, options);
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
  auditSnapshots(
    params: { labels?: Labels; against?: SnapshotId[]; allow?: string[] } = {},
    options: VersionOptions = {},
  ): Promise<AuditSnapshotsResponse> {
    if (params.against?.length === 0) {
      const message = "against names no snapshot: audit a run against its spec snapshots, or a project by its labels";
      return Promise.reject(new PisaError("bad_request", message));
    }
    const query = params.against ? new URLSearchParams() : this.versionQuery(options);
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    for (const id of params.against ?? []) query.append("against", id);
    for (const name of params.allow ?? []) query.append("allow", name);
    return this.call("GET", ["snapshots", "audit"], undefined, options, query);
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

  /** `rebuildSnapshots` as a job. A dry run isn't one: the server answers it
   *  at once, which throws here. */
  submitRebuildSnapshots(
    from: SnapshotId,
    to: SnapshotId,
    labels: Labels,
    options: RebuildOptions = {},
  ): Promise<Job<RebuildSnapshotsResponse>> {
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

  /** Re-checks the snapshot's module and its imports through the kernel. A
   *  rejection is in `failure`, not thrown. */
  verifySnapshot(id: SnapshotId, options: RequestOptions = {}): Promise<VerifySnapshotResponse> {
    return this.call("POST", ["snapshots", id, "verify"], undefined, options);
  }

  /** `verifySnapshot` as a job. */
  submitVerifySnapshot(id: SnapshotId, options: RequestOptions = {}): Promise<Job<VerifySnapshotResponse>> {
    return this.submit(["snapshots", id, "verify"], undefined, options);
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
    return { done: true, answer: ok<T>(r, body) };
  }

  /** Waits for `job` to finish, however long it takes, and returns what its
   *  request would have, or throws what it would have thrown. A few network
   *  failures in a row are tried again. */
  async waitJob<T>(job: Job<T>, options: RequestOptions = {}): Promise<T> {
    let dropped = 0;
    for (;;) {
      try {
        const polled = await this.pollJob(job, { ...options, wait: JOB_WAIT });
        if (polled.done) return polled.answer;
        dropped = 0;
      } catch (e) {
        if (!isPisaError(e, "network") || dropped >= 3) throw e;
        dropped += 1;
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
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

  private createSnapshotRequest(code: string, options: CreateSnapshotOptions): CreateSnapshotRequest {
    const body: CreateSnapshotRequest = { code, ...this.run(options) };
    if (options.verify !== undefined) body.verify = options.verify;
    if (options.labels !== undefined) body.labels = options.labels;
    if (options.key !== undefined) body.key = options.key;
    if (options.replaces !== undefined) body.replaces = options.replaces;
    if (options.fork !== undefined) body.fork = options.fork;
    if (options.holes !== undefined) body.holes = options.holes;
    return body;
  }

  /** POSTs with `Prefer: respond-async`: the job the server queued. */
  private async submit<T>(path: string[], body: unknown, options: RequestOptions): Promise<Job<T>> {
    const r = await this.send("POST", path, body, undefined, options.signal, { prefer: "respond-async" });
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

  private async send(
    method: string,
    path: string[],
    body: unknown,
    query: URLSearchParams | undefined,
    signal: AbortSignal | undefined,
    extra: Record<string, string> = {},
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
  if (r.ok) {
    if (body === undefined) throw new PisaError("bad_response", "the response isn't JSON", r.status);
    return body as T;
  }
  const error = isObject(body) ? body["error"] : undefined;
  if (isObject(error) && typeof error["kind"] === "string" && ERROR_KINDS.has(error["kind"])) {
    throw new PisaError(error["kind"] as ErrorKind, String(error["message"] ?? ""), r.status);
  }
  throw new PisaError("bad_response", `HTTP ${r.status} ${r.statusText}`.trim(), r.status);
}
