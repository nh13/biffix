import type { Page } from '@playwright/test';
import {
  API,
  CORS,
  test,
  expect,
  expectNoSeriousA11yViolations,
  onApi,
  THEMES,
  useTheme,
} from '../fixtures';

// Monitoring in the Lab view: progress, steps and time left of a running analysis without a
// samplesheet, results across an experiment's analyses beyond a wall of cards, each try of a
// failed step in plain words, what a pipeline gives and takes on its card, the Lab's Settings, and
// where the Expert view is for a colleague.

// A running Nextflow analysis with no samplesheet: 2 of 5 known steps done, its pipeline usually
// takes 30 min.
const NF_LIVE = '/runs/run_01HGW8nflive';
// A failed analysis whose alignment was lost to a spot reclaim, then ran out of memory.
const OOM = '/runs/run_01HGWEoom';
const LIVER = '/experiments/exp_liver_tumor';

test.use({ view: 'lab' });

/** The analysis's pipeline has no past runs: no typical run. */
async function noTypicalRun(page: Page) {
  await onApi(page, 'GET', /\/workflows\/.+\/estimate$/, (route) =>
    route.fulfill({ json: {}, headers: CORS }),
  );
}

test.describe('a running analysis without a samplesheet', () => {
  test('shows a bar against the known steps, each step, and the time left', async ({ page }) => {
    await page.goto(NF_LIVE);
    const banner = page.getByTestId('analysis-status');
    await expect(banner).toContainText('Running · 2 of 5+ steps done');
    // Against the steps seen so far plus one: never a full bar while it still finds work.
    const bar = banner.getByRole('progressbar', { name: 'Progress' });
    await expect(bar).toHaveAttribute('aria-valuenow', '33');
    await expect(bar).toHaveAttribute('aria-valuetext', /more may come/);
    await expect(page.getByTestId('analysis-time')).toHaveText(
      /^Running for \d+ min · usually ~30 min · about \d+ min left$/,
    );
    // Steps in pipeline order (SAY feeds REVERSE), each with its state.
    const steps = page.getByRole('list', { name: 'Steps' }).getByRole('listitem');
    await expect(steps).toHaveCount(2);
    await expect(steps.nth(0)).toContainText('Say');
    await expect(steps.nth(0)).toContainText('done');
    await expect(steps.nth(1)).toContainText('Reverse');
    await expect(steps.nth(1)).toContainText('running · 0/3');
  });

  test('without past runs to go by, says only how long it has run', async ({ page }) => {
    await noTypicalRun(page);
    await page.goto(NF_LIVE);
    await expect(page.getByTestId('analysis-time')).toHaveText(/^Running for \d+ min$/);
  });

  test('a finished analysis shows no progress block', async ({ page }) => {
    await page.goto('/runs/run_01HGWDdone');
    await expect(page.getByTestId('analysis-status')).toContainText('Finished');
    await expect(page.getByTestId('analysis-time')).toHaveCount(0);
    await expect(page.getByTestId('steps-strip')).toHaveCount(0);
  });
});

test.describe('results across an experiment', () => {
  test('a table of analyses × results, grouped by kind, opening a result', async ({ page }) => {
    await page.goto(LIVER);
    const results = page.getByRole('region', { name: 'Results across analyses' });
    const table = results.getByRole('table', { name: 'Key results of each analysis' });
    await expect(table).toBeVisible();
    await expect(results.getByRole('button', { name: 'Table' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    // Kinds over the results: reports first, then tables, images, QC and alignments.
    await expect(table.getByRole('columnheader', { name: 'Report', exact: true })).toBeVisible();
    await expect(table.getByRole('columnheader', { name: /^Gene counts/ })).toBeVisible();
    await expect(table.getByRole('columnheader', { name: /^Per-sample QC/ })).toContainText(
      'one per sample',
    );
    const batch10 = table.getByRole('row', { name: /rnaseq · batch-10/ });
    await batch10.getByRole('button', { name: 'Open Gene counts of rnaseq · batch-10' }).click();
    await expect(page.getByRole('dialog')).toBeVisible();
    await page.keyboard.press('Escape');
    // batch-12 declares none: a dash in each result column, and nothing to open.
    const batch12 = table.getByRole('row', { name: /rnaseq · batch-12/ });
    await expect(batch12.locator('td .none')).toHaveCount(await batch12.locator('td').count());
    await expect(batch12.getByRole('button')).toHaveCount(0);
  });

  test('one sample across analyses, compared result by result', async ({ page }) => {
    await page.goto(LIVER);
    const results = page.getByRole('region', { name: 'Results across analyses' });
    await results.getByLabel('Sample', { exact: true }).selectOption('Tumor_1');
    await expect(results.getByRole('status')).toContainText("Showing Tumor_1's results");
    const table = results.getByRole('table', { name: 'Key results of each analysis' });
    const batch10 = table.getByRole('row', { name: /rnaseq · batch-10/ });
    // A per-sample cell opens that sample's file only (no sample picker).
    await expect(
      batch10.getByRole('button', { name: 'Open Per-sample QC · Tumor_1 of rnaseq · batch-10' }),
    ).toBeVisible();

    await results.getByRole('button', { name: 'By result' }).click();
    const qc = results.getByRole('region', { name: /^Per-sample QC/ });
    await expect(qc.getByRole('article', { name: 'rnaseq · batch-10' })).toBeVisible();
    await expect(qc.getByRole('article', { name: 'rnaseq · batch-10' })).toContainText('Tumor_1');

    // The view is remembered in this browser.
    await page.reload();
    await expect(
      page
        .getByRole('region', { name: 'Results across analyses' })
        .getByRole('button', { name: 'By result' }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  test('by analysis is the card grid, filtered to the sample', async ({ page }) => {
    await page.goto(LIVER);
    const results = page.getByRole('region', { name: 'Results across analyses' });
    await results.getByRole('button', { name: 'By analysis' }).click();
    const batch10 = results.getByRole('region', { name: 'rnaseq · batch-10' });
    await expect(batch10.getByRole('article', { name: 'Gene counts' })).toBeVisible();
    await results.getByLabel('Sample', { exact: true }).selectOption('Normal_2');
    // Its per-sample cards hold that sample's files alone.
    const picker = batch10.getByRole('article', { name: 'Per-sample QC' }).getByRole('combobox');
    await expect(picker.getByRole('option')).toHaveText(['Normal_2']);
    await expect(batch10.getByRole('article', { name: 'Gene counts' })).toBeVisible();
  });
});

test.describe('each try of a failed step', () => {
  test('Why? tells each try without a log line', async ({ page }) => {
    await page.goto(OOM);
    const story = page.getByTestId('attempt-story').getByRole('listitem');
    await expect(story).toHaveCount(2);
    await expect(story.nth(0)).toContainText('Try 1');
    await expect(story.nth(0)).toContainText('on a low-cost (spot) machine');
    await expect(story.nth(0)).toContainText('AWS took the machine back after 40 min');
    await expect(story.nth(0)).toContainText('tried again automatically');
    await expect(story.nth(1)).toContainText('Try 2');
    await expect(story.nth(1)).toContainText('ran out of memory after 1 h 10 min');
    await expect(story.nth(1)).not.toContainText('tried again');
    // No log line, nor the raw reason: the failure's log tail and reason stay out of the story.
    const told = page.getByTestId('attempt-story');
    for (const raw of ['EXITING because of fatal ERROR', 'Killed', 'OutOfMemoryError'])
      await expect(told).not.toContainText(raw);
    // Logs stay one click away, in the Expert view.
    await expect(
      page.getByRole('link', { name: 'Open in Expert view for full logs →' }),
    ).toBeVisible();
  });

  test('says the count of tries when the records cannot be read', async ({ page }) => {
    await onApi(page, 'POST', /\/attempts\/query$/, (route) =>
      route.fulfill({ status: 500, body: 'down', headers: CORS }),
    );
    await page.goto(OOM);
    await expect(page.getByText('Likely cause')).toBeVisible();
    await expect(page.locator('.why dd').filter({ hasText: /^\s*2 times\s*$/ })).toBeVisible();
  });
});

test.describe('a pipeline card says what you get', () => {
  test('its results, its inputs, and its usual time and cost', async ({ page }) => {
    // The typical run comes with the list: no card asks for its own.
    const asked: string[] = [];
    page.on('request', (r) => {
      if (/\/workflows\/.+\/estimate/.test(r.url())) asked.push(r.url());
    });
    await page.goto('/workflows');
    const card = page.getByRole('article', { name: 'Bulk RNA-seq' });
    const get = card.getByTestId('what-you-get');
    await expect(get).toContainText('You get');
    await expect(get).toContainText('Gene counts · PCA · Per-sample QC · Alignments');
    await expect(get).toContainText('Samplesheet (one row per sample) · Reference genome');
    await expect(get).toContainText('Usually ~2.5 h · about $14 · from 6 analyses');
    // A pipeline with nothing declared and no history says none of it.
    await expect(
      page.getByRole('article', { name: 'umi-consensus' }).getByTestId('what-you-get'),
    ).toHaveCount(0);
    expect(asked).toEqual([]);
  });

  test("the pipeline's page says it too", async ({ page }) => {
    await page.goto('/workflows/aabbccddeeff%3Av1/about');
    await expect(page.getByTestId('what-you-get')).toContainText('Gene counts · PCA');
    await expect(page.getByTestId('what-you-get')).toContainText('Usually ~2.5 h');
  });
});

test.describe('Lab Settings', () => {
  test('theme, notifications, pins and who to ask, in plain words; no log order', async ({
    page,
  }) => {
    await page.goto('/settings');
    await expect(page.getByRole('heading', { name: /^Settings/ })).toBeVisible();
    await expect(page.getByText('Log order')).toHaveCount(0);
    await page.getByRole('radio', { name: /^Dark/ }).check();
    await expect(page.locator('html')).toHaveClass(/dark/);
    await page.getByRole('radio', { name: /^Light/ }).check();
    await expect(page.locator('html')).not.toHaveClass(/dark/);
    await expect(page.getByRole('heading', { name: /^Notifications/ })).toBeVisible();
    await expect(page.getByText("email notifications aren't available yet")).toBeVisible();
    await expect(page.getByTestId('no-pins')).toBeVisible();
    await expect(page.getByTestId('settings-support')).toContainText('No one is named yet');
  });

  test('a pinned pipeline can be unpinned here', async ({ page }) => {
    await page.goto('/workflows');
    await page.getByRole('button', { name: 'Pin Bulk RNA-seq' }).click();
    await page.getByRole('link', { name: 'Settings' }).first().click();
    const pins = page.getByRole('list', { name: 'Pinned pipelines' });
    await expect(pins).toContainText('Bulk RNA-seq');
    await pins.getByRole('button', { name: 'Unpin Bulk RNA-seq' }).click();
    await expect(page.getByTestId('no-pins')).toBeVisible();
  });

  test("a pin is not called gone when the pipelines can't be read", async ({ page }) => {
    await page.goto('/workflows');
    await page.getByRole('button', { name: 'Pin Bulk RNA-seq' }).click();
    await onApi(page, 'GET', '/workflows', (route) =>
      route.fulfill({ status: 503, headers: CORS, body: 'down for maintenance' }),
    );
    await page.getByRole('link', { name: 'Settings' }).first().click();
    const pins = page.getByRole('list', { name: 'Pinned pipelines' });
    await expect(page.getByText("Couldn't load the pipelines")).toBeVisible();
    await expect(pins).toContainText('nf-core/rnaseq');
    await expect(pins).not.toContainText('no longer in the catalog');
    await pins.getByRole('button', { name: /^Unpin/ }).click();
    await expect(page.getByTestId('no-pins')).toBeVisible();
  });

  test("says it couldn't read who to ask, not that no one is named", async ({ page }) => {
    await onApi(page, 'GET', '/settings', (route) =>
      route.fulfill({ status: 500, headers: CORS, body: 'the settings store is down' }),
    );
    await page.goto('/settings');
    await expect(page.getByRole('alert')).toContainText("Couldn't load who to ask");
    await expect(page.getByText('No one is named yet')).toHaveCount(0);
  });

  test('My work says when its analyses could not be refreshed', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('.work')).toBeVisible();
    await onApi(page, 'POST', '/runs/query', (route) =>
      route.fulfill({ status: 503, headers: CORS, body: 'busy' }),
    );
    // Shown again: My work refreshes at once rather than at the next tick. A show while a read is
    // still pending is (rightly) skipped, so show it again until a refresh goes out.
    await expect(async () => {
      await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
      await expect(page.getByText(/Couldn't refresh your analyses/)).toBeVisible({ timeout: 1000 });
    }).toPass();
  });

  test('says who Ask goes to, when the site names someone', async ({ page }) => {
    await onApi(page, 'GET', '/settings', async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.support = { name: 'Sam Okafor', email: 'help@lab.example.org' };
      await route.fulfill({ response: res, json: body });
    });
    await page.goto('/settings');
    await expect(page.getByTestId('settings-support')).toContainText('Sam Okafor');
    await expect(page.getByRole('link', { name: 'help@lab.example.org' })).toBeVisible();
  });
});

test.describe('finding the Expert view', () => {
  test('a line on the analysis page switches to Expert on the same page', async ({ page }) => {
    await page.goto(OOM);
    const hint = page.getByTestId('expert-hint');
    await expect(hint).toContainText('Looking for logs and task details?');
    await hint.getByRole('button', { name: 'Switch to Expert view' }).click();
    await expect(page.getByRole('button', { name: 'Expert', exact: true })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await expect(page).toHaveURL(/\/runs\/run_01HGWEoom$/);
    await expect(page.getByTestId('expert-hint')).toHaveCount(0);
  });

  test('dismissed once, it stays away, and Settings brings it back', async ({ page }) => {
    await page.goto(OOM);
    await page.getByRole('button', { name: "Don't show this again" }).click();
    await expect(page.getByTestId('expert-hint')).toHaveCount(0);
    await page.goto('/workflows/aabbccddeeff%3Av1/about');
    await expect(page.getByTestId('what-you-get')).toBeVisible();
    await expect(page.getByTestId('expert-hint')).toHaveCount(0);
    await page.goto('/settings');
    await page.getByRole('button', { name: 'Show the "Switch to Expert" hint again' }).click();
    await page.goto(OOM);
    await expect(page.getByTestId('expert-hint')).toBeVisible();
  });

  test('the toggle is labelled, and Share tells a colleague where Expert is', async ({ page }) => {
    await page.goto(OOM);
    await expect(page.locator('.view-bar').getByText('View', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Share' }).click();
    await expect(page.getByTestId('share-expert')).toContainText(
      'Sending it to a bioinformatician?',
    );
  });
});

for (const theme of THEMES) {
  test.describe(`touched pages have no serious a11y violations (${theme})`, () => {
    test('running analysis, failed analysis, experiment, catalog, settings', async ({ page }) => {
      await useTheme(page, theme);
      await page.goto(NF_LIVE);
      await expect(page.getByTestId('steps-strip')).toBeVisible();
      await expectNoSeriousA11yViolations(page);

      await page.goto(OOM);
      await expect(page.getByTestId('attempt-story')).toBeVisible();
      await expect(page.getByTestId('expert-hint')).toBeVisible();
      await expectNoSeriousA11yViolations(page);

      await page.goto(LIVER);
      const results = page.getByRole('region', { name: 'Results across analyses' });
      await expect(results.getByRole('table')).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await results.getByRole('button', { name: 'By result' }).click();
      await expect(results.getByRole('region', { name: /^Gene counts/ })).toBeVisible();
      await expectNoSeriousA11yViolations(page);

      await page.goto('/workflows');
      await expect(page.getByTestId('what-you-get').first()).toContainText('Usually');
      await expectNoSeriousA11yViolations(page);

      await page.goto('/settings');
      await expect(page.getByTestId('no-pins')).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  });
}
