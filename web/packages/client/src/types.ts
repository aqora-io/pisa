// The JSON of pisa's HTTP API. The types are generated from the server's
// OpenAPI document into `openapi.ts` (`pnpm -C web generate`, after
// regenerating `openapi.json`); this file names them for the client and adds
// what a schema doesn't say. Field names are snake_case, as on the wire. A
// field marked `?` is absent, never null, when it has no value.

import type { ErrorKind } from "./openapi.ts";

export type {
  AcceptPair,
  AcceptRequest,
  AcceptResponse,
  AuditReview,
  Document,
  DocumentGoals,
  DocumentGoalsRequest,
  DocumentProbes,
  DocumentProbesRequest,
  EditDocumentRequest,
  GoalsAt,
  GoalsMode,
  LeftOut,
  LeftOutReason,
  LinePos,
  OpenDocumentRequest,
  PairError,
  PairErrorKind,
  ProbeAt,
  ProbeResult,
  ReviewDeclaration,
  ReviewOrigin,
  Allow,
  ApplyTacticRequest,
  ApplyTacticResponse,
  AtPosition,
  AuditAssumption,
  AuditComparison,
  AuditFork,
  AuditLink,
  AuditSnapshotsResponse,
  AuditSummary,
  AuditTarget,
  AuditUnproved,
  CheckBatchItem,
  CheckBatchRequest,
  CheckBatchResponse,
  CheckRequest,
  Change,
  CheckResult,
  CheckSessionRequest,
  CreateSnapshotRequest,
  CreateSnapshotResponse,
  Declaration,
  DeclarationChange,
  DeclarationDetail,
  DeleteSnapshotResponse,
  Description,
  Detail,
  ErrorKind,
  ErrorResponse,
  Export,
  Flags,
  GetDeclarationResponse,
  Health,
  HealthStatus,
  Hit,
  Job,
  JobKind,
  JobOutcome,
  JobStatus,
  ListDependentsResponse,
  ListSnapshotsResponse,
  Message,
  Pos,
  ProofStatus,
  Query,
  QueryResult,
  RebuildCreated,
  RebuildFailed,
  RebuildItem,
  RebuildNotDeleted,
  RebuildReplaced,
  RebuildSkipped,
  RebuildSnapshotsRequest,
  RebuildSnapshotsResponse,
  Replacement,
  Replaces,
  Resolution,
  ResolveRequest,
  ResolveResponse,
  SearchHealth,
  SearchRequest,
  SearchResponse,
  Session,
  Severity,
  Snapshot,
  SnapshotDetail,
  SnapshotScope,
  Sorry,
  StartSessionRequest,
  State,
  Suggestion,
  TacticInfo,
  TermInfo,
  UpdateSnapshotRequest,
  VerifyFailure,
  VerifySnapshotResponse,
} from "./openapi.ts";

/** An error's `kind` and `message`; the schema's `Error`, renamed so it
 *  doesn't hide JavaScript's. */
export type { Error as ApiError } from "./openapi.ts";

// ===================== Identifiers =====================

/** A proof state: `<version>~<session>~<32 hex>`. Opaque to clients. The same
 *  tactic on the same state gives the same state, and the same id. A state
 *  stays valid after tactics are applied to it, so a session is a tree. */
export type StateId = string;
/** A tactic session, `<version>~<32 hex>`: also its open document's id, for
 *  the `/documents` endpoints. */
export type SessionId = string;
/** A snapshot's id, `<version>~<32 lowercase hex>`. */
export type SnapshotId = string;
/** A snapshot module, `PisaSnap.s<32 hex>`, shared by every snapshot of the
 *  same code. */
export type ModuleName = string;
/** A position in a listing of snapshots: opaque. */
export type Cursor = string;
/** Label keys are 1–128 bytes of `[A-Za-z0-9._/-]`; values at most 512 bytes,
 *  without NUL. A snapshot has at most 64. */
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
  conflict: 409,
  internal: 500,
};

/** `GET /snapshots`' query. */
export interface ListSnapshotsParams {
  /** Snapshots with every one of these labels. */
  labels?: Labels;
  /** 1–1000; 100 by default. */
  limit?: number;
  after?: Cursor;
  /** Only the snapshots that no other snapshot replaces; every snapshot by
   *  default. */
  heads?: boolean;
}
