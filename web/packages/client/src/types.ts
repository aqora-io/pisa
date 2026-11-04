// The JSON of pisa's HTTP API, as the server reads and writes it. Field names
// are snake_case, as on the wire. A field marked `?` in a response is absent,
// never null, when it has no value. In a request, an optional field may be
// left out; `null` is the same as leaving it out only where the type allows
// it.

// ===================== Identifiers =====================

/** A proof state: `<version>~<header_key>~<hash>`. Opaque to clients. The hash
 *  is of the state's contents, so two tactics that reach the same state give
 *  the same key. A state stays valid after tactics are applied to it, so a
 *  session is a tree. */
export type StateKey = string;
/** A tactic session, `<version>~<header_key>~<id>`. */
export type SessionId = string;
/** A snapshot entry: 32 lowercase hex digits. It holds the labels. */
export type EntryId = string;
/** A snapshot module, `PisaSnap.s<32 hex>`, shared by every entry on the
 *  same code. */
export type ModuleName = string;
/** A position in a listing of snapshots: opaque. */
export type Cursor = string;
/** Label keys are 1–128 bytes of `[A-Za-z0-9._/-]`; values at most 512 bytes,
 *  without NUL. An entry has at most 64. */
export type Labels = Record<string, string>;

// ===================== Errors =====================

export type ErrorKind =
  | "timeout"
  | "out_of_memory"
  | "crash"
  | "policy"
  | "lean"
  | "capacity"
  | "unavailable"
  | "bad_request"
  | "not_found"
  | "internal";

/** The HTTP status that goes with each kind. */
export const ERROR_STATUS: Readonly<Record<ErrorKind, number>> = {
  timeout: 422,
  out_of_memory: 422,
  crash: 422,
  policy: 422,
  lean: 422,
  capacity: 429,
  unavailable: 503,
  bad_request: 400,
  not_found: 404,
  internal: 500,
};

export interface ApiError {
  kind: ErrorKind;
  message: string;
}

/** The body of an error response. */
export interface ErrorResponse {
  error: ApiError;
}

// ===================== Checks =====================

/** Lines count from 1; columns from 0, in Unicode code points (not UTF-16
 *  units), in the submitted code. See `posMapper`. */
export interface Pos {
  line: number;
  column: number;
}

export type Severity = "trace" | "info" | "warning" | "error";

export interface Message {
  severity: Severity;
  pos: Pos;
  end_pos?: Pos;
  data: string;
}

export interface Sorry {
  goal: string;
  pos?: Pos;
  end_pos?: Pos;
}

/** Extra detail a check can return. */
export type Detail = "tactics";

/** A new declaration and the axioms it depends on. */
export interface Declaration {
  name: string;
  kind: string;
  axioms: string[];
  type_hash: string;
  /** Definitions only. */
  value_hash?: string;
}

/** One run of a tactic, with the goals before and after it. */
export interface TacticInfo {
  tactic: string;
  goals_before: string[];
  goals_after: string[];
  pos: Pos;
  end_pos: Pos;
}

export interface CheckResult {
  /** Only in batch results, where it is the item's id. */
  id?: string;
  /** The verdict: it elaborated without errors, passed the policy, and uses
   *  only allowed axioms. */
  ok: boolean;
  /** Seconds Lean ran. */
  time: number;
  /** Why the check couldn't complete (e.g. `timeout`); the rest is then
   *  partial. */
  error?: ApiError;
  messages: Message[];
  sorries: Sorry[];
  /** Why `ok` is false, e.g. `axiom: t uses sorryAx`. */
  reasons: string[];
  declarations: Declaration[];
  /** With `detail: ["tactics"]`, every tactic run, when there were any. */
  tactics?: TacticInfo[];
}

export interface CheckRequest {
  code: string;
  /** A pool version or alias; the server's default when absent. */
  version?: string | null;
  /** Seconds Lean may run (default 60). */
  timeout?: number;
  detail?: Detail[];
}

export interface BatchItem {
  id: string;
  code: string;
}

export interface BatchRequest {
  items: BatchItem[];
  version?: string | null;
  timeout?: number;
  detail?: Detail[];
}

/** Every item gets a result, in order; failures, running out of capacity
 *  included, are in that item's `error`. */
export interface BatchResponse {
  results: CheckResult[];
}

// ===================== Sessions =====================

export interface StartRequest {
  /** Code with `sorry`s, each of which becomes a state. */
  code: string;
  version?: string | null;
  timeout?: number;
}

export interface StateInfo {
  state: StateKey;
  goal: string;
  /** Where the `sorry` is in the code. */
  pos?: Pos;
  end_pos?: Pos;
}

export interface StartResponse {
  session: SessionId;
  /** One per `sorry`, in source order. */
  states: StateInfo[];
  messages: Message[];
}

export interface TacticRequest {
  tactic: string;
  timeout?: number;
}

export interface TacticResponse {
  /** The state the tactic led to. */
  state: StateKey;
  goals: string[];
  /** No goals are left for this `sorry`; it says nothing of the others. */
  complete: boolean;
  messages: Message[];
}

/** The completed code, checked from scratch. It must also keep the session's
 *  statements and imports; any that changed are in `reasons`, as
 *  `statement: …`. */
export interface SessionCheckRequest {
  code: string;
  timeout?: number;
}

// ===================== Snapshots =====================

export interface SnapshotRequest {
  code: string;
  version?: string | null;
  timeout?: number;
  /** Re-check through the kernel: `true` also the imports, `false` never;
   *  absent, only code that uses macros. */
  verify?: boolean | null;
  labels?: Labels;
  /** Idempotency key: creating with it again returns the same entry.
   *  1–256 bytes of `[A-Za-z0-9._:/-]`. */
  key?: string | null;
}

export interface SnapshotResponse {
  /** Absent when nothing was kept, e.g. the code had errors. */
  entry?: EntryId;
  snapshot?: ModuleName;
  /** The imports that load the snapshot: later code starts with it. */
  header?: string;
  /** The pool version the snapshot belongs to. */
  version: string;
  /** Kept although a declaration uses `sorry` or an axiom that isn't
   *  allowed; `result.reasons` says which. Shared by the module's entries. */
  dirty: boolean;
  /** The module and its imports passed the kernel re-check. Shared by the
   *  module's entries. */
  verified: boolean;
  labels?: Labels;
  result: CheckResult;
}

export interface SnapshotInfo {
  entry: EntryId;
  snapshot: ModuleName;
  imports: ModuleName[];
  header: string;
  code: string;
  labels?: Labels;
  dirty: boolean;
  verified: boolean;
  result: CheckResult;
  /** Seconds since the Unix epoch. */
  created: number;
}

/** An entry as a listing has it; `labels` is `{}` when it has none. */
export interface SnapshotSummary {
  entry: EntryId;
  snapshot: ModuleName;
  imports: ModuleName[];
  header: string;
  labels: Labels;
  dirty: boolean;
  verified: boolean;
  created: number;
}

/** Newest first. `next` is there exactly when the page is full; the page
 *  after it may be empty. */
export interface SnapshotList {
  entries: SnapshotSummary[];
  next?: Cursor;
}

/** Removes first, then sets. */
export interface LabelsRequest {
  set?: Labels;
  remove?: string[];
}

export interface SnapshotFailure {
  snapshot: ModuleName;
  reason: string;
}

export interface SnapshotVerified {
  entry: EntryId;
  snapshot: ModuleName;
  verified: boolean;
  dirty: boolean;
  /** The modules that passed in this run, in order. */
  checked: ModuleName[];
  failure?: SnapshotFailure;
}

export interface Deleted {
  entry: EntryId;
  /** The modules that went with the entry: its own, then up its imports. */
  removed: ModuleName[];
}

export interface ListSnapshotsParams {
  /** Entries with every one of these labels. */
  labels?: Labels;
  /** 1–1000; 100 by default. */
  limit?: number;
  after?: Cursor;
}

// ===================== Search =====================

/** Which snapshots a query searches besides the libraries: none, every
 *  snapshot with entries, or those of the entries with every label. */
export type SnapshotScope = "none" | "all" | { labels: Labels };

/** Fields that are blank count as absent; `q` is classified as one of the
 *  others. */
export interface Query {
  q?: string | null;
  nl?: string | null;
  latex?: string | null;
  pattern?: string | null;
  goal?: string | null;
  name?: string | null;
  libraries?: string[];
  kinds?: string[];
  /** Hits wanted: 5 by default, at most 50. */
  k?: number;
  include_sorried?: boolean;
  verify?: boolean;
  snapshots?: SnapshotScope;
}

export interface SearchRequest {
  /** At most 64. */
  queries: Query[];
  /** Fails with `not_found` unless the server has this environment. */
  env_id?: string | null;
}

export interface Flags {
  uses_sorry?: boolean;
  informal?: boolean;
  deprecated_to?: string;
  additive_sibling?: string;
  /** The header that loads the declaration. */
  imports?: string;
}

export interface SnapshotHit {
  /** The scope's entries on the module, newest first, at most 10. */
  entries: EntryId[];
  dirty: boolean;
  verified: boolean;
}

export interface Hit {
  name: string;
  kind: string;
  /** `PisaSnap` for a snapshot's declaration. */
  library: string;
  module: string;
  signature: string;
  informal_name?: string;
  summary?: string;
  flags: Flags;
  /** Comparable only within one query. */
  score: number;
  source_url?: string;
  /** About 150 tokens, ready for a model's context. */
  text: string;
  snapshot?: SnapshotHit;
}

export interface QueryResult {
  hits: Hit[];
  verification?: string;
  warnings?: string[];
}

export interface SearchResponse {
  env_id: string;
  /** One per query, in order. */
  results: QueryResult[];
}

export interface ResolveRequest {
  /** At most 64. */
  names: string[];
  env_id?: string | null;
}

export interface Suggestion {
  name: string;
  kind: string;
  signature: string;
}

export interface Resolution {
  name: string;
  exists: boolean;
  deprecated_to?: string;
  /** Close names, when the name doesn't exist. */
  suggestions?: Suggestion[];
}

export interface ResolveResponse {
  env_id: string;
  results: Resolution[];
}

export interface Informal {
  informal_name: string;
  statement_nl: string;
  statement_latex?: string;
  aliases?: string[];
  source: string;
}

export interface DeclDetail {
  name: string;
  kind: string;
  library: string;
  module: string;
  signature: string;
  type_full: string;
  docstring?: string;
  informal?: Informal;
  flags: Flags;
  source_url?: string;
  deps: string[];
  /** At most 50. */
  used_by: string[];
}

export interface DeclResponse {
  env_id: string;
  decl: DeclDetail;
}

// ===================== Health =====================

export interface SearchHealth {
  /** The index is open and Qdrant answers. */
  ready: boolean;
  env_id: string;
  qdrant: boolean;
  /** This server indexes snapshots. */
  leader: boolean;
  /** Why the index isn't open yet. */
  error?: string;
}

export interface Health {
  /** `draining` (with HTTP 503) once the server is stopping. */
  status: "ok" | "draining";
  /** Workers taking jobs from the queue; `null` if it can't be read. With
   *  none, every job waits the server's max wait, then gets a 429. */
  workers: number | null;
  /** `null` when the server doesn't search. */
  search: SearchHealth | null;
}
