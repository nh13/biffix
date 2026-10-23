import { defineConfig, devices } from '@playwright/test';

// End-to-end checks of the UI against the mock API (`mock/server.mjs`), not a real backend: each
// run starts the mock on :8080 and the dev server on :5174 pointed at it.
//
// The mock keeps state (a launch, a cancel, a finished upload), so servers are never reused: every
// `playwright test` starts clean. Within a run the one mock serves every worker, every retry and
// every `--repeat-each` repeat, and has no reset, so no test's outcome may depend on which tests ran
// before it:
// - a test that changes the mock's state keeps the change to itself: a per-test `page.route` store
//   (`e2e/fixtures.ts` keeps every page's `/me/prefs` and notification reads to it), or a run, file
//   or experiment of its own, named for the attempt and repeat (`e2e/data/upload.spec.ts`);
// - a test that reads state other tests change (the caller's newest runs, a pipeline's run count)
//   answers that read for its own page (`withoutLaunchedRuns`, or a route of its own).
// Locally, `PW_EXE` may name an already-installed Chromium to skip `npx playwright install`.
export default defineConfig({
  testDir: 'e2e',
  fullyParallel: true,
  retries: process.env.CI ? 1 : 0,
  // A page can take longer than the default 5 s to settle while every worker shares the one mock
  // and dev server (and on a cold dev server, which compiles each route on first load).
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: 'http://localhost:5174',
    trace: 'retain-on-failure',
    launchOptions: process.env.PW_EXE ? { executablePath: process.env.PW_EXE } : {},
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: [
    {
      command: 'node mock/server.mjs',
      port: 8080,
      reuseExistingServer: false,
    },
    {
      command: 'npx vite dev --port 5174 --strictPort',
      port: 5174,
      env: { PUBLIC_BIFFIX_API: 'http://localhost:8080' },
      reuseExistingServer: false,
    },
  ],
});
