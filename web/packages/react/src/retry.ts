import { isPisaError, type PisaClient } from "@aqora/pisa-client";

/** Whether a failed query runs again, the default for every query here: once
 *  after a 503, twice after a network error, and never otherwise. A 429
 *  already means the server waited its max wait for a worker, and other
 *  errors would only recur. */
export function retryQuery(failures: number, error: unknown): boolean {
  if (isPisaError(error, "unavailable")) return failures < 1;
  if (isPisaError(error, "network")) return failures < 2;
  return false;
}

/** How a query of `client` runs again: not at all when the client retries
 *  its requests itself, as it does by default; else as `retryQuery` says. */
export function queryRetry(client: PisaClient): typeof retryQuery | false {
  return client.retry === undefined ? retryQuery : false;
}
