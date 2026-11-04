import type { PisaClient } from "@aqora/pisa-client";
import { type ReactElement, type ReactNode, createContext, use } from "react";

const ClientContext = createContext<PisaClient | null>(null);

/** Gives the hooks below it `client`. They also need a TanStack
 *  `QueryClientProvider` above them, which the app supplies. */
export function PisaProvider({ client, children }: { client: PisaClient; children?: ReactNode }): ReactElement {
  return <ClientContext value={client}>{children}</ClientContext>;
}

export function usePisaClient(): PisaClient {
  const client = use(ClientContext);
  if (client === null) throw new Error("pisa's hooks need a <PisaProvider> above them");
  return client;
}
