import { defineConfig } from "vitest/config";

export default defineConfig({
  // The workspace's packages by their sources, as `source` in their exports.
  resolve: { conditions: ["source", "module", "browser", "development|production"] },
  ssr: { resolve: { conditions: ["source", "node", "development|production"] } },
  test: { include: ["test/*.test.{ts,tsx}"], environment: "jsdom" },
});
