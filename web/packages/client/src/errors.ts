import type { ErrorKind } from "./types.ts";

/** What went wrong: an error kind the server sent, or `network` (no answer),
 *  `aborted` (the request's signal fired), or `bad_response` (an answer that
 *  isn't pisa's). A gateway's 502, 503 or 504, such as a proxy's error page,
 *  is `unavailable`. */
export type PisaErrorKind = ErrorKind | "network" | "aborted" | "bad_response";

export class PisaError extends Error {
  override readonly name = "PisaError";
  readonly kind: PisaErrorKind;
  /** The HTTP status, when there was an answer. */
  readonly status: number | undefined;
  /** For a lost job (`worker_lost`), whether sending the request again is
   *  safe, as the server said; `undefined` for any other error. */
  readonly resendSafe: boolean | undefined;

  constructor(
    kind: PisaErrorKind,
    message: string,
    status?: number,
    options?: ErrorOptions & { resendSafe?: boolean | undefined },
  ) {
    super(message, options);
    this.kind = kind;
    this.status = status;
    this.resendSafe = options?.resendSafe;
  }
}

/** Whether `e` is a `PisaError` (of one of `kinds`, if given). It checks the
 *  name rather than the class, so errors from another copy of this package
 *  count too. */
export function isPisaError(e: unknown, ...kinds: PisaErrorKind[]): e is PisaError {
  if (typeof e !== "object" || e === null || (e as { name?: unknown }).name !== "PisaError") {
    return false;
  }
  return kinds.length === 0 || kinds.includes((e as PisaError).kind);
}

/** Lean rejected the code, or the policy did. */
export function isLeanError(e: unknown): e is PisaError {
  return isPisaError(e, "lean", "policy");
}

/** Lean ran out of time or memory, crashed, or reached its recursion or heartbeat limit. */
export function isLimitError(e: unknown): e is PisaError {
  return isPisaError(e, "timeout", "out_of_memory", "crash", "recursion_limit", "heartbeat_limit");
}

/** No worker took the job in time, the server is unavailable, or the job was
 *  lost (`worker_lost`; its error's `resendSafe` says whether to send it
 *  again): try again later. */
export function isCapacityError(e: unknown): e is PisaError {
  return isPisaError(e, "capacity", "unavailable", "worker_lost");
}

/** An unknown or expired job, state or session, or an unknown snapshot or
 *  declaration. */
export function isNotFoundError(e: unknown): e is PisaError {
  return isPisaError(e, "not_found");
}

/** The request doesn't fit what it found: a document at another revision,
 *  a run name another run holds, a spec amended already, or a run to accept
 *  with heads to settle. Read what is there now, and decide again. */
export function isConflictError(e: unknown): e is PisaError {
  return isPisaError(e, "conflict");
}
