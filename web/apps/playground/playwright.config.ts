import { defineConfig } from "@playwright/test";

// The server the walk drives, through the dev server's proxy: an isolated
// compose stack, as the README says, not one whose data matters. What it
// creates is labelled e2e=<run>, and deleted at the end.
const PISA_URL = process.env["PISA_URL"] ?? "http://localhost:9100";
process.env["PISA_URL"] = PISA_URL;
process.env["PISA_E2E_RUN"] ??= String(Date.now());
const chromium = process.env["PLAYWRIGHT_CHROMIUM"];

export default defineConfig({
  testDir: "e2e",
  outputDir: "e2e/out/results",
  timeout: 300_000,
  expect: { timeout: 60_000 },
  workers: 1,
  reporter: [["list"]],
  globalTeardown: "./e2e/teardown.ts",
  use: {
    baseURL: "http://localhost:5179",
    viewport: { width: 1400, height: 1000 },
    ...(chromium !== undefined && { launchOptions: { executablePath: chromium } }),
  },
  webServer: {
    command: "vite --port 5179 --strictPort",
    url: "http://localhost:5179",
    env: { PISA_URL },
  },
});
