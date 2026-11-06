import type { ErrorKind } from "./types.ts";

/** What went wrong: an error kind the server sent, or `network` (no answer),
 *  `aborted` (the request's signal fired), or `bad_response` (an answer that
 *  isn't pisa's, e.g. a proxy's error page). */
export type PisaErrorKind = ErrorKind | "network" | "aborted" | "bad_response";

export class PisaError extends Error {
  override readonly name = "PisaError";
  readonly kind: PisaErrorKind;
  /** The HTTP status, when there was an answer. */
  readonly status: number | undefined;

  constructor(kind: PisaErrorKind, message: string, status?: number, options?: ErrorOptions) {
    super(message, options);
    this.kind = kind;
    this.status = status;
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

/** Lean ran out of time or memory, or crashed. */
export function isLimitError(e: unknown): e is PisaError {
  return isPisaError(e, "timeout", "out_of_memory", "crash");
}

/** No worker took the job in time, or the server is unavailable: try again
 *  later. */
export function isCapacityError(e: unknown): e is PisaError {
  return isPisaError(e, "capacity", "unavailable");
}

/** An unknown or expired job, state or session, or an unknown snapshot or
 *  declaration. */
export function isNotFoundError(e: unknown): e is PisaError {
  return isPisaError(e, "not_found");
}
