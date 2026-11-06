// The JSON of pisa's HTTP API. The types are generated from the server's
// OpenAPI document into `openapi.ts` (`pnpm -C web generate`, after
// regenerating `openapi.json`); this file names them for the client and adds
// what a schema doesn't say. Field names are snake_case, as on the wire. A
// field marked `?` is absent, never null, when it has no value.

import type { ErrorKind } from "./openapi.ts";

export type {
  Allow,
  AtPosition,
  AuditAssumption,
  AuditSummary,
  AuditUnproved,
  CheckBatchItem,
  CheckBatchRequest,
  CheckBatchResponse,
  CheckRequest,
  CheckResult,
  DeclDetail,
  DeclResponse,
  Declaration,
  Deleted,
  Detail,
  ErrorKind,
  ErrorResponse,
  Flags,
  Health,
  HealthStatus,
  Hit,
  Informal,
  JobKind,
  JobOutcome,
  JobState,
  JobStatus,
  LabelsRequest,
  Message,
  ModuleDecl,
  Pos,
  ProofStatus,
  Query,
  QueryResult,
  RebuildCreated,
  RebuildFailed,
  RebuildItem,
  RebuildRequest,
  RebuildResponse,
  RebuildSkipped,
  Replaced,
  Replacement,
  Replaces,
  Resolution,
  ResolveRequest,
  ResolveResponse,
  SearchHealth,
  SearchRequest,
  SearchResponse,
  SessionCheckRequest,
  Severity,
  SnapshotFailure,
  SnapshotAudit,
  SnapshotDependents,
  SnapshotHit,
  SnapshotInfo,
  SnapshotList,
  SnapshotRequest,
  SnapshotResponse,
  SnapshotScope,
  SnapshotSummary,
  SnapshotVerified,
  Sorry,
  StartRequest,
  StartResponse,
  StateInfo,
  Suggestion,
  TacticInfo,
  TacticRequest,
  TacticResponse,
  TermInfo,
} from "./openapi.ts";

/** An error's `kind` and `message`; the schema's `Error`, renamed so it
 *  doesn't hide JavaScript's. */
export type { Error as ApiError } from "./openapi.ts";

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

// ===================== What a schema doesn't say =====================

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

/** `GET /snapshots`' query. */
export interface ListSnapshotsParams {
  /** Entries with every one of these labels. */
  labels?: Labels;
  /** 1–1000; 100 by default. */
  limit?: number;
  after?: Cursor;
}
