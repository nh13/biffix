import type { Page } from '@playwright/test';
import {
  API,
  test,
  expect,
  expectNoSeriousA11yViolations,
  onApi,
  THEMES,
  useTheme,
} from '../fixtures';

// Lab analyses: readable analysis names and inline rename, a resumed analysis says so, the
// analyses list at phone width, friendly step names, previous / next analysis, the pipeline's
// friendly title, friendly error pages and dark-mode previews.

const DONE = '/runs/run_01HGWDdone';
const OOM = '/runs/run_01HGWEoom';
// A Nextflow resume of a cancelled run, both started without a name (from the CLI).
const RESUMED = '/runs/run_01HGW4resume';
// "nf-core/sarek · Oct 1 14:05": a derived name, the pipeline and when it started.
const DERIVED = /^nf-core\/sarek · [A-Z][a-z]{2} \d{1,2}(, \d{4})? \d{2}:\d{2}$/;

/**
 * A per-test store for `runId`'s name: a rename is answered (and remembered) here, and every read
 * of the run carries it, so the change outlives a reload without touching the shared mock.
 */
async function renameStore(page: Page, runId: string): Promise<{ posts: unknown[] }> {
  const state: { name: string | null | undefined; posts: unknown[] } = {
    name: undefined,
    posts: [],
  };
  await onApi(page, 'GET', `/runs/${runId}`, async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    if (state.name !== undefined) {
      if (state.name) body.run_name = state.name;
      else delete body.run_name;
    }
    await route.fulfill({ response, json: body });
  });
  await onApi(page, 'POST', `/runs/${runId}/name`, async (route) => {
    const body = route.request().postDataJSON();
    state.posts.push(body);
    state.name = typeof body.run_name === 'string' ? body.run_name.trim() || null : null;
    const run = await (await route.fetch({ method: 'GET', url: `${API}/runs/${runId}` })).json();
    if (state.name) run.run_name = state.name;
    else delete run.run_name;
    await route.fulfill({
      status: 200,
      json: run,
      headers: { 'access-control-allow-origin': '*' },
    });
  });
  return state;
}

test.describe('Lab polish', () => {
  test.use({ view: 'lab' });

  test('an analysis without a name is named by its pipeline and start, everywhere', async ({
    page,
  }) => {
    await page.goto('/runs');
    const link = page.locator(`a[href="${RESUMED}"]`);
    await expect(link).toHaveText(DERIVED);
    await expect(page.getByRole('link', { name: 'run_01HGW4resume' })).toHaveCount(0);

    await page.goto(RESUMED);
    await expect(page.getByTestId('analysis-name')).toHaveText(DERIVED);
    // The tab's title names it too.
    await expect.poll(() => page.title()).toMatch(/^nf-core\/sarek · .+ · biffix \(mock\)$/);

    // The bell names it the same way (already says the pipeline: not repeated).
    await page
      .locator('.bar-toggle')
      .getByRole('button', { name: /^Notifications/ })
      .click();
    const panel = page.getByRole('region', { name: 'Notifications' });
    await expect(panel.locator(`a[href="${RESUMED}"]`)).toContainText(
      /nf-core\/sarek · [A-Z][a-z]{2} \d{1,2}(, \d{4})? \d{2}:\d{2} finished/,
    );
  });

  test('renaming an analysis is saved, and lasts a reload', async ({ page }) => {
    const store = await renameStore(page, 'run_01HGW4resume');
    await page.goto(RESUMED);
    const rename = page.getByRole('button', { name: /^Rename / });
    await rename.click();
    const field = page.getByRole('textbox', { name: 'Analysis name' });
    await expect(field).toBeFocused();
    // Escape leaves it as it was.
    await field.fill('Not this');
    await field.press('Escape');
    await expect(page.getByTestId('analysis-name')).toHaveText(DERIVED);
    await expect(rename).toBeFocused();
    expect(store.posts).toEqual([]);

    await rename.click();
    await field.fill('  Sarek tumour/normal, cohort 3 ');
    await field.press('Enter');
    await expect(page.getByTestId('analysis-name')).toHaveText('Sarek tumour/normal, cohort 3');
    expect(store.posts).toEqual([{ run_name: 'Sarek tumour/normal, cohort 3' }]);
    await page.reload();
    await expect(page.getByTestId('analysis-name')).toHaveText('Sarek tumour/normal, cohort 3');
    await expect.poll(() => page.title()).toContain('Sarek tumour/normal, cohort 3');

    // Cleared: back to the derived name.
    await page.getByRole('button', { name: /^Rename / }).click();
    await page.getByRole('textbox', { name: 'Analysis name' }).fill('');
    await page.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByTestId('analysis-name')).toHaveText(DERIVED);
    expect(store.posts.at(-1)).toEqual({ run_name: null });
  });

  test('a rename the server refuses says why and keeps the field', async ({ page }) => {
    await onApi(page, 'POST', '/runs/run_01HGW4resume/name', (route) =>
      route.fulfill({
        status: 403,
        body: 'forbidden',
        headers: { 'access-control-allow-origin': '*' },
      }),
    );
    await page.goto(RESUMED);
    await page.getByRole('button', { name: /^Rename / }).click();
    await page.getByRole('textbox', { name: 'Analysis name' }).fill('Mine');
    await page.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByRole('alert')).toContainText(
      'You need permission to start analyses to rename one.',
    );
    await expect(page.getByRole('textbox', { name: 'Analysis name' })).toHaveValue('Mine');
  });

  test('a resumed analysis says which one it continues, and that they share a folder', async ({
    page,
  }) => {
    await page.goto(RESUMED);
    const note = page.getByTestId('resumed-note');
    await expect(note).toContainText(/Continues nf-core\/sarek · [A-Z][a-z]{2} \d{1,2}/);
    await expect(note).toContainText(/\(cancelled [A-Z][a-z]{2} \d{1,2}\)/);
    await expect(note).toContainText('1 step reused');
    await expect(note).toContainText('results written to the same folder');
    await expect(note.getByRole('link')).toHaveAttribute('href', '/runs/run_01HGW0cancel');
    const inputs = page.getByRole('region', { name: 'Inputs used' });
    await expect(inputs.getByTestId('shared-folder')).toContainText(
      'The results folder of the analysis it continues',
    );
    // The list marks it.
    await page.goto('/runs');
    await expect(
      page.getByRole('row').filter({ has: page.locator(`a[href="${RESUMED}"]`) }),
    ).toContainText('continues an earlier analysis');
  });

  test("never dates the original's end with the day it was submitted", async ({ page }) => {
    await onApi(page, 'GET', '/runs/run_01HGW0cancel', async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      await route.fulfill({ response, json: { ...run, completed_at: null } });
    });
    await page.goto(RESUMED);
    const note = page.getByTestId('resumed-note');
    await expect(note).toContainText('(cancelled)');
  });

  test('the analyses list fits a phone: rows, not a table', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/runs');
    const rows = page.getByTestId('analyses-rows');
    await expect(rows.getByRole('listitem').first()).toBeVisible();
    await expect(page.getByRole('table')).toHaveCount(0);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
    // Each row opens its analysis.
    await expect(rows.locator(`a[href="${RESUMED}"]`).first()).toBeVisible();
  });

  test('steps read as a person names them, the engine name kept as a tooltip', async ({ page }) => {
    await page.goto(DONE);
    const grid = page.getByRole('region', { name: /Samples/ });
    for (const [label, raw] of [
      ['FastQC', /FASTQC/],
      ['STAR align', /STAR_ALIGN/],
    ] as const) {
      const header = grid.getByRole('columnheader', { name: label, exact: true });
      await expect(header).toBeVisible();
      await expect(header).toHaveAttribute('title', raw);
    }
    await page.goto(OOM);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText('Tumor_4 · STAR align · it ran out of memory');
  });

  test('previous / next walk the list the user came from, by keyboard too', async ({ page }) => {
    await page.goto('/runs?filter=succeeded');
    const links = page.locator('tbody tr td:first-child a');
    await expect(links.first()).toBeVisible();
    const hrefs = await links.evaluateAll((as) => as.map((a) => a.getAttribute('href')));
    expect(hrefs.length).toBeGreaterThan(2);
    await links.nth(1).click();
    await expect(page).toHaveURL(new RegExp(`${hrefs[1]}$`));
    const nav = page.getByRole('navigation', { name: 'Previous and next analysis' });
    await expect(nav).toContainText(`2 of ${hrefs.length}`);
    await expect(nav.getByRole('link', { name: /^Previous analysis: / })).toHaveAttribute(
      'href',
      hrefs[0]!,
    );
    // "← Analyses" goes back to the list as it was filtered.
    await expect(page.getByRole('link', { name: '← Analyses' })).toHaveAttribute(
      'href',
      '/runs?filter=succeeded',
    );
    await page.keyboard.press(']');
    await expect(page).toHaveURL(new RegExp(`${hrefs[2]}$`));
    // The URL changes before the new page's links do: wait for them, or `[` walks from the old one.
    await expect(nav).toContainText(`3 of ${hrefs.length}`);
    await page.keyboard.press('[');
    await expect(page).toHaveURL(new RegExp(`${hrefs[1]}$`));
    // The links are reachable by Tab, and open with Enter.
    await nav.getByRole('link', { name: /^Next analysis: / }).focus();
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(new RegExp(`${hrefs[2]}$`));
  });

  test('opened from elsewhere, previous / next walk your own analyses', async ({ page }) => {
    // The neighbours come from the caller's own list.
    const neighbours = page.waitForRequest(
      (r) => r.url().endsWith('/runs/query') && r.method() === 'POST',
    );
    await page.goto(RESUMED);
    expect((await neighbours).postDataJSON()).toMatchObject({ mine: true });
    const nav = page.getByRole('navigation', { name: 'Previous and next analysis' });
    await expect(nav).toContainText(/\d+ of \d+/);
    await expect(nav.getByRole('link').first()).toBeVisible();
  });

  test("the pipeline's friendly title names it on its card, page and analyses", async ({
    page,
  }) => {
    await page.goto('/workflows');
    const card = page.getByRole('article', { name: 'Bulk RNA-seq' });
    await expect(card).toBeVisible();
    await expect(card.getByTestId('pipeline-repo')).toHaveText('nf-core/rnaseq');
    await card.getByRole('link', { name: 'About Bulk RNA-seq' }).click();
    await expect(page.getByRole('heading', { level: 1, name: /Bulk RNA-seq/ })).toBeVisible();
    await expect(page.getByTestId('pipeline-repo')).toHaveText('nf-core/rnaseq');
    await page.goto(DONE);
    await expect(page.getByTestId('run-version')).toHaveText(/^Bulk RNA-seq · version /);
  });

  test('an unknown analysis is said plainly, with a way back and the details hidden', async ({
    page,
  }) => {
    await page.goto('/runs/does-not-exist');
    const error = page.getByRole('region', { name: "This analysis doesn't exist" });
    // What went wrong is what is announced.
    await expect(error.getByRole('alert')).toContainText('may have been removed');
    await expect(
      error.getByRole('heading', { level: 1, name: "This analysis doesn't exist" }),
    ).toBeVisible();
    await expect(error.getByRole('link', { name: 'Back to Analyses →' })).toHaveAttribute(
      'href',
      '/runs',
    );
    const details = error.getByText(/404/);
    await expect(details).toBeHidden();
    await error.getByText('Details').click();
    await expect(details).toBeVisible();
    // No Try again for a link that does not exist.
    await expect(error.getByRole('button', { name: 'Try again' })).toHaveCount(0);
  });

  test('no access, a server failure and no connection each say what to do', async ({ page }) => {
    let mode: 'forbidden' | 'server' | 'down' = 'forbidden';
    await onApi(page, 'GET', DONE, async (route) => {
      const cors = { 'access-control-allow-origin': '*' };
      if (mode === 'forbidden') return route.fulfill({ status: 403, body: '', headers: cors });
      if (mode === 'server') return route.fulfill({ status: 500, body: 'boom', headers: cors });
      return route.abort('internetdisconnected');
    });
    await page.goto(DONE);
    await expect(
      page.getByRole('heading', { name: "You don't have access to this analysis" }),
    ).toBeVisible();
    // Access is the role's, not project membership.
    await expect(page.getByText("Your account isn't allowed to see this analysis.")).toBeVisible();
    mode = 'server';
    await page.reload();
    await expect(
      page.getByRole('heading', { name: "biffix couldn't load this analysis" }),
    ).toBeVisible();
    mode = 'down';
    await page.getByRole('button', { name: 'Try again' }).click();
    await expect(page.getByRole('heading', { name: "biffix can't be reached" })).toBeVisible();
  });

  test('a pipeline the server failed to load offers Try again', async ({ page }) => {
    let fail = true;
    await onApi(page, 'GET', /^\/workflows\/aabbccddeeff(:|%3A)v1$/i, async (route) => {
      if (fail)
        return route.fulfill({
          status: 500,
          body: 'boom',
          headers: { 'access-control-allow-origin': '*' },
        });
      return route.fallback();
    });
    await page.goto('/workflows/aabbccddeeff:v1');
    await expect(
      page.getByRole('heading', { name: "biffix couldn't load this pipeline" }),
    ).toBeVisible();
    fail = false;
    await page.getByRole('button', { name: 'Try again' }).click();
    // The pipeline loads, and the error is gone.
    await expect(page.getByRole('heading', { level: 1, name: /Bulk RNA-seq/ })).toBeVisible();
    await expect(
      page.getByRole('heading', { name: "biffix couldn't load this pipeline" }),
    ).toHaveCount(0);
  });

  test('an unknown pipeline, experiment or page says so in Lab words', async ({ page }) => {
    await page.goto('/workflows/nope');
    await expect(page.getByRole('heading', { name: "This pipeline doesn't exist" })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Back to Pipelines →' })).toBeVisible();
    await page.goto('/experiments/exp_nope');
    await expect(
      page.getByRole('heading', { name: "This experiment doesn't exist" }),
    ).toBeVisible();
    await page.goto('/no-such-page');
    await expect(page.getByRole('heading', { name: "This page doesn't exist" })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Back to My work →' })).toHaveAttribute(
      'href',
      '/',
    );
  });

  for (const theme of THEMES) {
    test(`touched pages have no serious accessibility problems (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto(RESUMED);
      await expect(page.getByTestId('resumed-note')).toBeVisible();
      await expect(
        page.getByRole('navigation', { name: 'Previous and next analysis' }),
      ).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.getByRole('button', { name: /^Rename / }).click();
      await expect(page.getByRole('textbox', { name: 'Analysis name' })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.goto('/runs/does-not-exist');
      await expect(
        page.getByRole('heading', { name: "This analysis doesn't exist" }),
      ).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.goto('/workflows');
      await expect(page.getByRole('article', { name: 'Bulk RNA-seq' })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.setViewportSize({ width: 390, height: 844 });
      await page.goto('/runs');
      await expect(page.getByTestId('analyses-rows')).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }
});

test.describe('Lab polish: dark-mode previews', () => {
  test.use({ view: 'lab' });

  test('an image preview sits in a light frame on a neutral tile, a report on a light page', async ({
    page,
  }) => {
    await useTheme(page, 'dark');
    await page.goto(DONE);
    const results = page.getByRole('region', { name: /^Results/ });
    const img = results.getByRole('article', { name: 'PCA' }).getByRole('img', {
      name: 'Preview of PCA',
    });
    await expect(img).toBeVisible();
    const look = await img.evaluate((el) => {
      const tile = getComputedStyle(el.parentElement!);
      const card = getComputedStyle(el.closest('article')!);
      return {
        img: getComputedStyle(el).backgroundColor,
        tile: tile.backgroundColor,
        tileImage: tile.backgroundImage,
        card: card.backgroundColor,
      };
    });
    // The image in a white frame, on a tile that is neither white nor a bare copy of the card.
    expect(look.img).toBe('rgb(255, 255, 255)');
    expect(look.tile).not.toBe('rgb(255, 255, 255)');
    expect(look.tile).not.toBe(look.card);
    expect(look.tileImage).toContain('conic-gradient');
    // The page itself is dark, and its controls follow it.
    expect(await page.evaluate(() => getComputedStyle(document.documentElement).colorScheme)).toBe(
      'dark',
    );

    // The QC report opens as its own light page, light from the first paint (no dark canvas).
    await results
      .getByRole('article', { name: 'QC report' })
      .getByRole('button', { name: /^Open/ })
      .click();
    const frame = page.getByRole('dialog').locator('iframe');
    await expect(frame).toBeVisible();
    const frameLook = await frame.evaluate((el) => {
      const s = getComputedStyle(el);
      return { bg: s.backgroundColor, scheme: s.colorScheme };
    });
    expect(frameLook).toEqual({ bg: 'rgb(255, 255, 255)', scheme: 'light' });
  });

  test('the light theme keeps a light page', async ({ page }) => {
    await useTheme(page, 'light');
    await page.goto(DONE);
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
    expect(await page.evaluate(() => getComputedStyle(document.documentElement).colorScheme)).toBe(
      'light',
    );
  });
});
