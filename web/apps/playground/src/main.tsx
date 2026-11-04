import "./styles.css";

import { PisaClient } from "@aqora/pisa-client";
import { PisaProvider } from "@aqora/pisa-react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App.tsx";

// `/api` is proxied to the server by `vite dev` and `vite preview`; set
// VITE_PISA_URL for a server that allows this page's origin (CORS).
const client = new PisaClient({ url: import.meta.env["VITE_PISA_URL"] ?? "/api" });
const queryClient = new QueryClient();

const root = document.getElementById("root");
if (root === null) throw new Error("no #root");
createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <PisaProvider client={client}>
        <App />
      </PisaProvider>
    </QueryClientProvider>
  </StrictMode>,
);
