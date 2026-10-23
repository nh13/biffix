import type { Page, Request } from '@playwright/test';
import { API, test, expect } from '../fixtures';

// The Runs list: keyset paging over the mock's `paging-demo:v1` runs (120 of them, newest first),
// the state filter in the URL, the search kept out of every URL, and the sortable headers.

const SCOPED = '/runs?workflow=paging-demo:v1';

/** Every runs-list request the page sends to the API (the mock, on :8080), with its JSON body. */
function runsRequests(page: Page) {
  const sent: { url: string; method: string; body: Record<string, unknown> }[] = [];
  page.on('request', (r: Request) => {
    const url = new URL(r.url());
    if (url.origin !== API || !/^\/runs(\/query)?$/.test(url.pathname)) return;
    if (r.method() === 'OPTIONS') return;
    sent.push({ url: r.url(), method: r.method(), body: r.postDataJSON() ?? {} });
  });
  return sent;
}

const range = (page: Page) => page.locator('.pager .range');
const firstRun = (page: Page) => page.locator('tbody tr').first().getByRole('link').first();

test('Next and Prev walk the pages, and Prev back to page one drops the cursor', async ({
  page,
}) => {
  const sent = runsRequests(page);
  await page.goto(SCOPED);
  await expect(range(page)).toContainText('1–50 of 120');
  await expect(firstRun(page)).toHaveText('paging run 1');
  const prev = page.getByRole('button', { name: '← Prev' });
  const next = page.getByRole('button', { name: 'Next →' });
  await expect(prev).toBeDisabled();

  await next.click();
  await expect(range(page)).toContainText('51–100 of 120');
  await expect(firstRun(page)).toHaveText('paging run 51');
  await next.click();
  await expect(range(page)).toContainText('101–120 of 120');
  await expect(firstRun(page)).toHaveText('paging run 101');
  await expect(next).toBeDisabled();

  await prev.click();
  await expect(range(page)).toContainText('51–100 of 120');
  await expect(firstRun(page)).toHaveText('paging run 51');
  expect(sent.at(-1)!.body.before).toBeTruthy();

  await prev.click();
  await expect(range(page)).toContainText('1–50 of 120');
  await expect(firstRun(page)).toHaveText('paging run 1');
  await expect(prev).toBeDisabled();
  // The true first page: no cursor, so a run submitted since would show at the top.
  const last = sent.at(-1)!;
  expect(last.body.before).toBeUndefined();
  expect(last.body.after).toBeUndefined();
});

test('the filter lives in the URL, and another URL change keeps the tab chosen', async ({
  page,
}) => {
  await page.goto('/');
  await page.goto(`${SCOPED}&filter=failed`);
  const tab = (name: string) => page.getByRole('button', { name, exact: true });
  await expect(tab('failed')).toHaveAttribute('aria-pressed', 'true');
  await expect(range(page)).toContainText('of 40');

  await tab('succeeded').click();
  await expect(page).toHaveURL(/filter=succeeded/);
  await expect(tab('succeeded')).toHaveAttribute('aria-pressed', 'true');
  await expect(tab('failed')).toHaveAttribute('aria-pressed', 'false');
  await expect(range(page)).toContainText('of 80');

  // Clearing the scope rewrites the URL; the tab stays the one picked.
  await page.getByRole('button', { name: /^Remove the filter: workflow / }).click();
  await expect(page).not.toHaveURL(/workflow=/);
  await expect(tab('succeeded')).toHaveAttribute('aria-pressed', 'true');

  // A tab replaces the history entry: Back leaves the list, Forward returns to the last tab.
  await page.goBack();
  await expect(page).toHaveURL(/\/$/);
  await page.goForward();
  await expect(page).toHaveURL(/\/runs\?filter=succeeded$/);
  await expect(tab('succeeded')).toHaveAttribute('aria-pressed', 'true');

  await tab('all').click();
  await expect(page).toHaveURL(/\/runs$/);
  await expect(tab('all')).toHaveAttribute('aria-pressed', 'true');
});

test('the search goes in a request body, never in a URL', async ({ page }) => {
  const sent = runsRequests(page);
  // Every API URL the page asks for, not only the runs list's.
  const apiUrls: string[] = [];
  page.on('request', (r) => {
    if (new URL(r.url()).origin === API) apiUrls.push(r.url());
  });
  await page.goto(SCOPED);
  await expect(range(page)).toContainText('of 120');
  await page.getByRole('searchbox', { name: 'Search runs' }).fill('paging run 7');
  // 7 and 70–79.
  await expect(range(page)).toContainText('1–11 of 11');
  const searched = sent.filter((r) => r.body.q === 'paging run 7');
  expect(searched.length).toBeGreaterThan(0);
  for (const r of sent) {
    expect(r.method).toBe('POST');
    expect(r.url).not.toContain('paging');
  }
  for (const url of apiUrls)
    expect(decodeURIComponent(url.replace(/\+/g, ' ')), url).not.toContain('paging run');
  // Nor in the page's own URL, however it is spelled there (`%20`, or `+` as URLSearchParams
  // writes a space), in the query or the fragment.
  expect(decodeURIComponent(page.url().replace(/\+/g, ' '))).not.toContain('paging run');
});

test('a column sorts from the keyboard and announces its direction', async ({ page }) => {
  await page.goto(SCOPED);
  await expect(range(page)).toContainText('of 120');
  const header = page.getByRole('columnheader', { name: 'Run' });
  await expect(header).toHaveAttribute('aria-sort', 'none');
  await header.getByRole('button').focus();
  await page.keyboard.press('Enter');
  await expect(header).toHaveAttribute('aria-sort', 'ascending');
  await expect(firstRun(page)).toHaveText('paging run 1');
  await page.keyboard.press('Space');
  await expect(header).toHaveAttribute('aria-sort', 'descending');
  await expect(firstRun(page)).toHaveText('paging run 50');
});

// Each run's cost, as its page has it: a plain right-aligned column, not sortable (paging is
// server-side, and the server does not sort by cost).
const LIVER = '/runs?experiment=exp_liver_tumor';
const LAB_RUNS = '/runs?workflow=c4d5e6f7a8b9:v1';
const LIVE_RUN = 'run_01HGW3align';
/** The cost cell of the row whose run link is exactly `name`. */
const costOf = (page: Page, name: string) =>
  page
    .getByRole('row')
    .filter({ has: page.getByRole('link', { name, exact: true }) })
    .locator('td.cost');
/** The cell shows `shown` and a screen reader hears `said`, exactly. */
async function expectCost(page: Page, name: string, shown: string, said = shown) {
  const cell = costOf(page, name);
  await expect(cell.locator('[aria-hidden="true"]')).toHaveText(shown);
  await expect(cell.locator('.sr-only')).toHaveText(said);
}

test.describe('the Lab analyses list', () => {
  test.use({ view: 'lab' });

  test('shows each analysis’s cost, a dash before any is priced, as its page does', async ({
    page,
  }) => {
    await page.goto(LIVER);
    const header = page.getByRole('columnheader', { name: 'Cost' });
    await expect(header).toBeVisible();
    await expect(header.getByRole('button')).toHaveCount(0);
    await expectCost(page, 'rnaseq · batch-10', '$13.80');
    await expect(costOf(page, 'rnaseq · batch-10')).toHaveCSS('text-align', 'right');
    // None of its jobs priced yet (two still to be): no cost yet, read out in words.
    await expectCost(page, 'rnaseq · batch-12', '—', 'No cost yet');
    await expect(costOf(page, 'rnaseq · batch-12')).toHaveAccessibleName('No cost yet');

    // The run pages say the same.
    await page.getByRole('link', { name: 'rnaseq · batch-10', exact: true }).click();
    await expect(page.locator('#cost .amount')).toHaveText('$13.80');
    await page.goto(LIVER);
    await page.getByRole('link', { name: 'rnaseq · batch-12', exact: true }).click();
    const cost = page.getByRole('region', { name: 'Cost' });
    await expect(cost.locator('.note')).toHaveText('No cost yet.');
    await expect(cost.locator('.amount')).toHaveCount(0);
    await expect(page.getByTestId('analysis-status')).not.toContainText('$');
  });

  test('marks an ended analysis whose machines are still being priced', async ({ page }) => {
    await page.goto(LAB_RUNS);
    await page.getByRole('button', { name: 'Everyone’s' }).click();
    await expectCost(
      page,
      'rnaseq · batch-11',
      '$11.40 + pending',
      '$11.40 so far; some machines not priced yet',
    );
    await page.getByRole('link', { name: 'rnaseq · batch-11', exact: true }).click();
    await expect(page.locator('#cost .amount')).toHaveText('$11.40 + pending');
  });
});

test('the Expert runs list shows each run’s cost too', async ({ page }) => {
  await page.goto(LIVER);
  await expect(page.getByRole('columnheader', { name: 'Cost' })).toBeVisible();
  await expectCost(page, 'rnaseq · batch-10', '$13.80');
  await expectCost(page, 'rnaseq · batch-12', '—', 'No cost yet');
  // A running run: its estimate so far.
  await page.goto('/runs?workflow=aabbccddeeff:smk1');
  await expectCost(page, LIVE_RUN, '≈ $3.84 so far', 'about $3.84 so far');
});

test('the Expert run header shows no amount before a job is priced, only what is pending', async ({
  page,
}) => {
  await page.goto('/runs/run_01HGWFbadin');
  const cost = page.locator('.meta-item').filter({ hasText: 'Est. cost' });
  await expect(cost.locator('.meta-value')).toHaveText('No cost yet 2 jobs pending');
  await page.goto('/runs/run_01HGWEoom');
  await expect(cost.locator('.meta-value')).toHaveText('$11.40 estimated +1 job pending');
});
