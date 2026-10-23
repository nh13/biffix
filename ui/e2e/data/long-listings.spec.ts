import { test, expect } from '../fixtures';

// Listings longer than one API page: the mock pages them as the backend does (a run's outputs by S3
// URI, 2,000 at a time; a data folder 1,000 entries at a time), so the UI's paging loops run.

test('a run with more outputs than one page lists them all, asking page by page', async ({
  page,
}) => {
  const bodies: Record<string, unknown>[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /\/runs\/run_01HGW9big\/outputs\/query$/.test(r.url()))
      bodies.push(r.postDataJSON() ?? {});
  });
  await page.goto('/runs/run_01HGW9big/outputs?view=flat');
  // The last output by URI is on the second page.
  await page.getByLabel('Filter outputs').fill('shard-2050');
  await expect(page.getByRole('button', { name: 'shard-2050.bed' })).toBeVisible();
  // The first two page requests (the run is running, so a slow runner may also see a refresh's).
  const paged = bodies.filter((b) => b.limit === 2000).slice(0, 2);
  expect(paged.map((b) => b.after ?? null)).toEqual([
    null,
    's3://genomics-project/biffix/runs/run_01HGW9big/results/shard-2000.bed',
  ]);
});

test('a folder with more entries than one page shows more on request', async ({ page }) => {
  await page.goto('/data/refs#path=panels%2F');
  await expect(page.getByRole('cell', { name: 'panel-1000.bed', exact: true })).toBeVisible();
  await expect(page.getByRole('cell', { name: 'panel-1001.bed', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Show more' }).click();
  await expect(page.getByRole('cell', { name: 'panel-1050.bed', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Show more' })).toHaveCount(0);
});
