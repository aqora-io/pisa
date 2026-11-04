import tailwind from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The pisa server that `/api` reaches, e.g. compose's Traefik.
const target = process.env["PISA_URL"] ?? "http://localhost:8000";
const proxy = { "/api": { target, changeOrigin: true, rewrite: (path: string) => path.replace(/^\/api/, "") } };

export default defineConfig({
  plugins: [react(), tailwind()],
  // The workspace's packages by their sources, as `source` in their exports.
  resolve: { conditions: ["source", "module", "browser", "development|production"] },
  server: { proxy },
  preview: { proxy },
  // React and CodeMirror come to about 700 kB, in one chunk: fine here.
  build: { chunkSizeWarningLimit: 1000 },
});
