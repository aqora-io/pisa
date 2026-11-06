import { PisaError, isPisaError } from "./errors.ts";
import type {
  CheckBatchResponse,
  CheckResult,
  DeclDetail,
  DeclResponse,
  Deleted,
  Detail,
  ErrorKind,
  Health,
  JobStatus,
  Labels,
  ListSnapshotsParams,
  Pos,
  Query,
  QueryResult,
  RebuildResponse,
  RebuildRequest,
  ResolveResponse,
  Resolution,
  SearchResponse,
  SessionId,
  SnapshotAudit,
  SnapshotDependents,
  SnapshotInfo,
  SnapshotList,
  SnapshotRequest,
  SnapshotResponse,
  SnapshotSummary,
  SnapshotVerified,
  StartResponse,
  StateKey,
  TacticResponse,
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
  /** The search environment to pin, so that search fails rather than answer
   *  from another one. */
  envId?: string;
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

export interface VersionOptions extends RequestOptions {
  version?: string;
}

export interface RebuildOptions extends VersionOptions {
  /** Plan or rebuild even though `to`, created without `replaces`, changes
   *  what `from` states. */
  allowChanges?: boolean;
  /** For `rebuild`: once every item is created, delete the old entries,
   *  from the top down, then `from`. */
  retire?: boolean;
  /** For `rebuild`: the kernel re-check for each create. */
  verify?: boolean;
}

export interface SearchOptions extends RequestOptions {
  envId?: string;
}

export interface DeclOptions extends RequestOptions {
  /** A snapshot module to look in first, as a snapshot hit's `module`. */
  module?: string;
}

export type CreateSnapshotOptions = RunOptions &
  Pick<SnapshotRequest, "verify" | "labels" | "key" | "replaces">;

/** A request sent with `Prefer: respond-async`: its job, whose answer is a
 *  `T`. Anyone with its id can read the answer or cancel it, so keep it
 *  secret; the server keeps the answer for an hour after the job is done. */
export interface Job<T> extends JobStatus {
  /** The answer's type; never set. */
  readonly answer?: T;
}

/** Where a job stands: done with its request's answer, or not yet. */
export type JobPoll<T> = { done: true; answer: T } | { done: false; status: JobStatus };

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
  "internal",
]);

/** A client of one pisa server. Every method returns the response's JSON, or
 *  throws a `PisaError`. */
export class PisaClient {
  readonly url: string;
  readonly version: string | undefined;
  readonly timeout: number | undefined;
  readonly envId: string | undefined;
  private readonly fetcher: typeof fetch;
  private readonly headers: Record<string, string>;

  constructor(options: ClientOptions) {
    this.url = options.url.replace(/\/+$/, "");
    this.version = options.version;
    this.timeout = options.timeout;
    this.envId = options.envId;
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
  startSession(code: string, options: RunOptions = {}): Promise<StartResponse> {
    return this.call("POST", ["sessions"], { code, ...this.run(options) }, options);
  }

  /** `startSession` as a job. */
  submitSession(code: string, options: RunOptions = {}): Promise<Job<StartResponse>> {
    return this.submit(["sessions"], { code, ...this.run(options) }, options);
  }

  /** Applies a tactic to a state; the state stays valid, so this can branch.
   *  A tactic Lean rejects throws `lean`. */
  tactic(state: StateKey, tactic: string, options: RunOptions = {}): Promise<TacticResponse> {
    return this.call(
      "POST",
      ["states", state, "tactic"],
      { tactic, ...this.timeoutOf(options) },
      options,
    );
  }

  /** `tactic` as a job. */
  submitTactic(state: StateKey, tactic: string, options: RunOptions = {}): Promise<Job<TacticResponse>> {
    return this.submit(["states", state, "tactic"], { tactic, ...this.timeoutOf(options) }, options);
  }

  /** Checks the completed code of a session from scratch. */
  checkSession(session: SessionId, code: string, options: RunOptions = {}): Promise<CheckResult> {
    return this.call(
      "POST",
      ["sessions", session, "check"],
      { code, ...this.timeoutOf(options) },
      options,
    );
  }

  /** `checkSession` as a job. */
  submitSessionCheck(session: SessionId, code: string, options: RunOptions = {}): Promise<Job<CheckResult>> {
    return this.submit(["sessions", session, "check"], { code, ...this.timeoutOf(options) }, options);
  }

  /** Checks code and keeps it as a snapshot that later code can import
   *  through `header`. Failures are in `result.error`. */
  createSnapshot(code: string, options: CreateSnapshotOptions = {}): Promise<SnapshotResponse> {
    return this.call("POST", ["snapshots"], this.snapshotRequest(code, options), options);
  }

  /** `createSnapshot` as a job. */
  submitSnapshot(code: string, options: CreateSnapshotOptions = {}): Promise<Job<SnapshotResponse>> {
    return this.submit(["snapshots"], this.snapshotRequest(code, options), options);
  }

  getSnapshot(entry: string, options: VersionOptions = {}): Promise<SnapshotInfo> {
    return this.call("GET", ["snapshots", entry], undefined, options, this.versionQuery(options));
  }

  /** A page of entries, newest first; pass `next` back as `after`. */
  listSnapshots(params: ListSnapshotsParams = {}, options: VersionOptions = {}): Promise<SnapshotList> {
    const query = this.versionQuery(options);
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    if (params.limit !== undefined) query.set("limit", String(params.limit));
    if (params.after !== undefined) query.set("after", params.after);
    return this.call("GET", ["snapshots"], undefined, options, query);
  }

  /** Removes the labels in `remove`, then sets those in `set`. */
  /** What the entries with every label rest on: their declarations by
   *  `status`, each assumption (an axiom declared in snapshot code) with what
   *  uses it, and what isn't proved. */
  audit(params: { labels?: Labels } = {}, options: VersionOptions = {}): Promise<SnapshotAudit> {
    const query = this.versionQuery(options);
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    return this.call("GET", ["snapshots", "audit"], undefined, options, query);
  }

  /** The entries on every module that imports `entry`'s, directly or through
   *  others, parents first: by level, then oldest first. */
  dependents(entry: string, params: { labels?: Labels } = {}, options: VersionOptions = {}): Promise<SnapshotDependents> {
    const query = this.versionQuery(options);
    for (const [k, v] of Object.entries(params.labels ?? {})) query.append("label", `${k}:${v}`);
    return this.call("GET", ["snapshots", entry, "dependents"], undefined, options, query);
  }

  /** Plans the creates that rebuild `from`'s dependents with every label in
   *  `labels` on `to`. It only plans: create each item's `code` with its
   *  `labels` and `key`, a level at a time. */
  rebuildPlan(from: string, to: string, labels: Labels, options: RebuildOptions = {}): Promise<RebuildResponse> {
    return this.call("POST", ["snapshots", "rebuild"], this.rebuildRequest(from, to, labels, options, true), options);
  }

  /** Rebuilds `from`'s dependents with every label in `labels` on `to`: the
   *  server runs the plan `rebuildPlan` gives, a level at a time, and with
   *  `retire` then deletes the old entries and `from`. Sending it again
   *  replays the creates that are done. It can take minutes: `submitRebuild`
   *  runs it as a job. */
  rebuild(from: string, to: string, labels: Labels, options: RebuildOptions = {}): Promise<RebuildResponse> {
    return this.call("POST", ["snapshots", "rebuild"], this.rebuildRequest(from, to, labels, options, false), options);
  }

  /** `rebuild` as a job. */
  submitRebuild(from: string, to: string, labels: Labels, options: RebuildOptions = {}): Promise<Job<RebuildResponse>> {
    return this.submit(["snapshots", "rebuild"], this.rebuildRequest(from, to, labels, options, false), options);
  }

  private rebuildRequest(from: string, to: string, labels: Labels, options: RebuildOptions, dryRun: boolean): RebuildRequest {
    const version = options.version ?? this.version;
    const body: RebuildRequest = { from, to, labels, dry_run: dryRun, ...(version !== undefined && { version }) };
    if (options.allowChanges) body.allow_changes = true;
    if (options.retire) body.retire = true;
    if (options.verify !== undefined) body.verify = options.verify;
    return body;
  }

  setSnapshotLabels(
    entry: string,
    change: { set?: Labels; remove?: string[] },
    options: VersionOptions = {},
  ): Promise<SnapshotSummary> {
    return this.call("PATCH", ["snapshots", entry], change, options, this.versionQuery(options));
  }

  /** Deletes an entry, and the modules nothing else needs. */
  deleteSnapshot(entry: string, options: VersionOptions = {}): Promise<Deleted> {
    return this.call("DELETE", ["snapshots", entry], undefined, options, this.versionQuery(options));
  }

  /** Re-checks the entry's module and its imports through the kernel. A
   *  rejection is in `failure`, not thrown. */
  verifySnapshot(entry: string, options: VersionOptions = {}): Promise<SnapshotVerified> {
    return this.call(
      "POST",
      ["snapshots", entry, "verify"],
      undefined,
      options,
      this.versionQuery(options),
    );
  }

  /** `verifySnapshot` as a job. */
  submitVerify(entry: string, options: VersionOptions = {}): Promise<Job<SnapshotVerified>> {
    return this.submit(["snapshots", entry, "verify"], undefined, options, this.versionQuery(options));
  }

  /** Where `job` stands, after waiting up to `wait` seconds for it to
   *  finish. Done, the answer is what its request would have returned, and
   *  an error is thrown as it would have been. */
  async pollJob<T>(job: Job<T>, options: JobOptions = {}): Promise<JobPoll<T>> {
    const query = new URLSearchParams();
    if (options.wait !== undefined) query.set("wait", String(options.wait));
    const r = await this.send("GET", ["jobs", job.job], undefined, query, options.signal);
    const body = await readJson(r);
    if (r.status === 202) return { done: false, status: body as JobStatus };
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
  cancelJob(job: JobStatus, options: RequestOptions = {}): Promise<JobStatus> {
    return this.call("DELETE", ["jobs", job.job], undefined, options);
  }

  async search(query: Query, options: SearchOptions = {}): Promise<QueryResult> {
    const [result] = await this.searchBatch([query], options);
    if (result === undefined) throw new PisaError("bad_response", "the search returned no result");
    return result;
  }

  /** Runs up to 64 queries; one result each, in order. */
  async searchBatch(queries: Query[], options: SearchOptions = {}): Promise<QueryResult[]> {
    const r = await this.call<SearchResponse>(
      "POST",
      ["v1", "search"],
      { queries, ...this.env(options) },
      options,
    );
    return r.results;
  }

  /** Whether each name exists, and close names for those that don't. */
  async resolve(names: string[], options: SearchOptions = {}): Promise<Resolution[]> {
    const r = await this.call<ResolveResponse>(
      "POST",
      ["v1", "resolve"],
      { names, ...this.env(options) },
      options,
    );
    return r.results;
  }

  async decl(name: string, options: DeclOptions = {}): Promise<DeclDetail> {
    const query = options.module === undefined ? undefined : new URLSearchParams({ module: options.module });
    const r = await this.call<DeclResponse>("GET", ["v1", "decl", name], undefined, options, query);
    return r.decl;
  }

  private snapshotRequest(code: string, options: CreateSnapshotOptions): SnapshotRequest {
    const body: SnapshotRequest = { code, ...this.run(options) };
    if (options.verify !== undefined) body.verify = options.verify;
    if (options.labels !== undefined) body.labels = options.labels;
    if (options.key !== undefined) body.key = options.key;
    if (options.replaces !== undefined) body.replaces = options.replaces;
    return body;
  }

  /** POSTs with `Prefer: respond-async`: the job the server queued. */
  private async submit<T>(path: string[], body: unknown, options: RequestOptions, query?: URLSearchParams): Promise<Job<T>> {
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

  private versionQuery(options: VersionOptions): URLSearchParams {
    const query = new URLSearchParams();
    const version = options.version ?? this.version;
    if (version !== undefined) query.set("version", version);
    return query;
  }

  private env(options: SearchOptions): { env_id?: string } {
    const envId = options.envId ?? this.envId;
    return envId !== undefined ? { env_id: envId } : {};
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
