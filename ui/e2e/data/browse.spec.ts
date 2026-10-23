import { test, expect, expectNoSeriousA11yViolations, THEMES, useTheme } from '../fixtures';

// The Data page: browse the sources, view files in place, download.

// Folder and file names can name a sample: the URL keeps them in its fragment, never in the path
// or the query, and the API reads that name them are POSTs with the names in the body. Every page
// load and API request is watched (the dev server's own module requests are not what the app
// sends), over its whole decoded URL, for every folder and file name the tests open.
const NAMES = /GRCh38|stats|genome|karyotype|nope/;
const noNamesSent = (page: import('@playwright/test').Page) => {
  const sent: string[] = [];
  page.on('request', (r) => {
    if (['document', 'fetch', 'xhr'].includes(r.resourceType())) sent.push(r.url());
  });
  return () => {
    expect(sent.length).toBeGreaterThan(0);
    expect(sent.filter((u) => NAMES.test(decodeURIComponent(u)))).toEqual([]);
  };
};

test('the sidebar opens the project bucket, and folders are in the URL fragment', async ({
  page,
}) => {
  const check = noNamesSent(page);
  await page.goto('/');
  await page.getByRole('link', { name: 'Data' }).click();
  await expect(page).toHaveURL(/\/data$/);
  const sources = page.getByRole('navigation', { name: 'Data sources' });
  await expect(sources.getByRole('link', { name: /Project data/ })).toHaveAttribute(
    'aria-current',
    'page',
  );
  await sources.getByRole('link', { name: /Reference genomes/ }).click();
  await page.getByRole('link', { name: /GRCh38/ }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F$/);
  await expect(page.getByRole('navigation', { name: 'Folder path' })).toContainText('GRCh38');
  await expect(page.getByRole('cell', { name: 'genome.fa', exact: true })).toBeVisible();
  // Back returns to the source's root, forward to the folder.
  await page.goBack();
  await expect(page).toHaveURL(/\/data\/refs$/);
  await expect(page.getByRole('link', { name: /GRCh38/ })).toBeVisible();
  await page.goForward();
  await expect(page.getByRole('cell', { name: 'genome.fa', exact: true })).toBeVisible();
  // A deep link lands in the same place.
  await page.goto('/data/refs#path=GRCh38%2F');
  await expect(page.getByRole('cell', { name: 'genome.fa', exact: true })).toBeVisible();
  check();
});

test('a table and an image open in place; the URL keeps the open file', async ({ page }) => {
  const check = noNamesSent(page);
  await page.goto('/data/refs#path=GRCh38%2F');
  await page.getByRole('button', { name: 'stats.tsv', exact: true }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F&file=stats\.tsv$/);
  const viewer = page.getByRole('dialog');
  await expect(viewer.getByRole('cell', { name: 'chr1', exact: true })).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole('dialog').getByRole('cell', { name: 'chr2', exact: true }),
  ).toBeVisible();
  // Selecting text in it and letting go past its edge keeps it open; a click outside closes it.
  const cell = page.getByRole('dialog').getByRole('cell', { name: 'chr2', exact: true });
  const box = (await cell.boundingBox())!;
  await page.mouse.move(box.x + 2, box.y + 2);
  await page.mouse.down();
  await page.mouse.move(2, 2, { steps: 5 });
  await page.mouse.up();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.mouse.click(2, 2);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByRole('button', { name: 'stats.tsv', exact: true }).click();
  await page.getByRole('button', { name: 'Close' }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F$/);
  await expect(page.getByRole('dialog')).toHaveCount(0);

  await page.getByRole('button', { name: 'karyotype.svg', exact: true }).click();
  await expect(page.getByRole('dialog').getByRole('img', { name: 'karyotype.svg' })).toBeVisible();
  // Back closes the viewer; forward opens it again.
  await page.goBack();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F$/);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.goForward();
  await expect(page).toHaveURL(/file=karyotype\.svg$/);
  await expect(page.getByRole('dialog').getByRole('img', { name: 'karyotype.svg' })).toBeVisible();
  check();
});

test('a file opened from a search below the folder opens again after a reload', async ({
  page,
}) => {
  const check = noNamesSent(page);
  await page.goto('/data/refs');
  await page.getByLabel('Filter this folder by name').fill('stats');
  await page.getByRole('button', { name: 'Search subfolders' }).click();
  await page.getByRole('button', { name: 'GRCh38/stats.tsv', exact: true }).click();
  await expect(page).toHaveURL(/\/data\/refs#file=GRCh38%2Fstats\.tsv$/);
  await expect(
    page.getByRole('dialog').getByRole('cell', { name: 'chr1', exact: true }),
  ).toBeVisible();
  // The search is gone after a reload; the file is looked up on its own.
  await page.reload();
  await expect(
    page.getByRole('dialog').getByRole('cell', { name: 'chr1', exact: true }),
  ).toBeVisible();
  check();
});

test("a link to a file past the folder's first page still opens it", async ({ page }) => {
  // The folder's listing (no name filter) has more pages, and the file isn't on the first.
  await page.route(/\/data\/list$/, (route) => {
    const body = route.request().postDataJSON();
    if (body?.path !== 'GRCh38/' || body?.starts_with) return route.fallback();
    return route.fulfill({
      json: { source: 'refs', path: 'GRCh38/', folders: [], files: [], next_cursor: 'page-2' },
    });
  });
  const asked = page.waitForRequest(
    (r) => /\/data\/list$/.test(r.url()) && r.postDataJSON()?.starts_with === 'stats.tsv',
  );
  await page.goto('/data/refs#path=GRCh38%2F&file=stats.tsv');
  expect((await asked).postDataJSON()).toMatchObject({ source: 'refs', path: 'GRCh38/' });
  await expect(
    page.getByRole('dialog').getByRole('cell', { name: 'chr1', exact: true }),
  ).toBeVisible();
});

test('a link to a file that is not there says so', async ({ page }) => {
  const check = noNamesSent(page);
  await page.goto('/data/refs#path=GRCh38%2F&file=nope.tsv');
  const alert = page.getByRole('alert');
  await expect(alert).toContainText('There is no file “nope.tsv” in this folder.');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await alert.getByRole('button', { name: 'Dismiss' }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F$/);
  await expect(page.getByRole('alert')).toHaveCount(0);
  check();
});

test('a file with no viewer downloads', async ({ page }) => {
  await page.goto('/data/refs#path=GRCh38%2F');
  // genome.2bit has no viewer: plain text in the list, not a link to one (once the list is shown).
  await expect(page.getByRole('cell', { name: 'genome.2bit', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'genome.2bit', exact: true })).toHaveCount(0);
  const asked = page.waitForRequest(
    (r) =>
      r.url().endsWith('/data/download-url') &&
      r.method() === 'POST' &&
      r.postDataJSON()?.path === 'GRCh38/genome.2bit',
  );
  await page.getByRole('button', { name: 'Download genome.2bit' }).click();
  await asked;
});

test('an unknown source says so', async ({ page }) => {
  await page.goto('/data/nope');
  await expect(page.getByRole('alert')).toContainText('There is no data source “nope”.');
});

for (const theme of THEMES) {
  test(`the Data page (${theme}) has no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto('/data/refs#path=GRCh38%2F');
    await expect(page.getByRole('cell', { name: 'genome.fa', exact: true })).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}
