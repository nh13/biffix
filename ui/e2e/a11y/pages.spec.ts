import { test as signedOut, type Locator, type Page } from '@playwright/test';
import { deflateSync } from 'node:zlib';
import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  runTabs,
  THEMES,
  useTheme,
} from '../fixtures';

// No serious or critical accessibility violations on any page or run view (axe-core, WCAG A/AA),
// in either theme. Each scan waits for something only that page's loaded data renders, so it
// checks the page as a person sees it, not a half-loaded shell.

// A running run (graph, jobs, coordinator log, cost, params) and a finished one that has outputs
// and reports.
const LIVE = '/runs/run_01HGW3align';
const DONE = '/runs/run_01HGW2wgs30x';
const views: [string, (page: Page) => Locator][] = [
  [LIVE, (p) => p.getByRole('heading', { name: 'Needs attention' })],
  [`${LIVE}/graph`, (p) => p.getByRole('heading', { name: 'Graph' })],
  [`${LIVE}/jobs`, (p) => p.getByRole('button', { name: 'joint_call' })],
  [
    `${LIVE}/jobs#job=align%3Asample%3DHG00096`,
    (p) => p.getByRole('complementary', { name: /Job align:sample=HG00096/ }).getByText('needs'),
  ],
  [`${LIVE}/coordinator`, (p) => p.getByText('[mock] coordinator: log line 1')],
  [`${LIVE}/cost`, (p) => p.getByRole('heading', { name: /^Cost/ })],
  [`${LIVE}/params`, (p) => p.getByRole('heading', { name: 'Parameters' })],
  [`${DONE}/outputs`, (p) => p.getByRole('button', { name: 'summary.tsv' })],
  [`${DONE}/reports`, (p) => p.getByRole('button', { name: 'MultiQC' })],
];

for (const theme of THEMES) {
  for (const [path, loaded] of views) {
    test(`run view ${path} (${theme}) has no serious accessibility violations`, async ({
      page,
    }) => {
      await useTheme(page, theme);
      await page.goto(path);
      await expect(runTabs(page)).toBeVisible();
      await expect(loaded(page).first()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }
}

// The run page of a run still preparing its images (the amber badge and the preparing panel).
for (const theme of THEMES) {
  test(`a preparing run (${theme}) has no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto('/runs/run_01HGW7preparing');
    await expect(page.getByTestId('preparing')).toBeVisible();
    await expect(page.locator('.run-header .badge')).toHaveText('preparing images');
    await expectNoSeriousA11yViolations(page);
  });
}

// A preview link's payload: a minimal form, deflated and base64url-encoded into the fragment.
const preview = `/workflows/preview#${deflateSync(
  Buffer.from(
    JSON.stringify({
      name: 'a11y-preview',
      engine: 'nextflow',
      form_schema: { fields: [], warnings: [] },
    }),
  ),
).toString('base64url')}`;

const WF = '/workflows/aabbccddeeff%3Av1';
const pages: [string, (page: Page) => Locator][] = [
  ['/', (p) => p.getByRole('heading', { level: 1 })],
  ['/runs', (p) => p.locator('tbody tr')],
  ['/runs/new', (p) => p.getByRole('link', { name: /nf-core\/rnaseq/ })],
  ['/workflows', (p) => p.locator('tbody tr')],
  [`${WF}/parameters`, (p) => p.getByRole('heading', { level: 1 })],
  [`${WF}/versions`, (p) => p.getByRole('heading', { level: 1 })],
  [`${WF}/provenance`, (p) => p.getByRole('heading', { name: 'Frozen id' })],
  [preview, (p) => p.getByRole('heading', { name: 'a11y-preview', level: 1 })],
  ['/api-docs', (p) => p.locator('.opblock-summary')],
];
for (const theme of THEMES) {
  for (const [path, loaded] of pages) {
    const name = path.startsWith('/workflows/preview') ? '/workflows/preview' : path;
    test(`${name} (${theme}) has no serious accessibility violations`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto(path);
      await expect(loaded(page).first()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }

  // The pages a signed-out visitor sees (the base test: no session). The IdP is an outside host,
  // stood in for so nothing leaves the machine.
  for (const [path, heading] of [
    ['/signed-out', "You're signed out"],
    ['/auth/callback?error=access_denied', 'Sign-in failed'],
  ] as const) {
    signedOut(`${path} (${theme}) has no serious accessibility violations`, async ({ page }) => {
      await page.route(/^https:\/\/mock\.auth\.us-east-1\.amazoncognito\.com\//, (route) =>
        route.fulfill({ contentType: 'text/html', body: '<h1>Mock IdP</h1>' }),
      );
      await useTheme(page, theme);
      await page.goto(path);
      await expect(page.getByRole('heading', { name: heading })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }
}
