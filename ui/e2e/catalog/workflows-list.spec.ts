import type { Page } from '@playwright/test';
import { test, expect, API } from '../fixtures';

// The Workflows list: versions grouped into (project, name) workflows, the latest shown, run counts
// summed, the sort in the URL, and a search that opens a group on a hit in an older version.

const day = 86_400_000_000;
const now = Date.now() * 1000;
const version = (
  project: string,
  name: string,
  sha: string,
  ageDays: number,
  runs: number,
  engine = 'snakemake',
) => ({
  id: `${sha}:v`,
  name,
  project,
  engine,
  engine_version: '9.0',
  git_sha: sha,
  image_digest: 'sha256:' + '0'.repeat(64),
  queue_classes: ['cpu-spot'],
  created_at_micros: now - ageDays * day,
  created_by: 'alice',
  run_count: runs,
});

// Two projects with a workflow of the same name, one with three versions listed out of order.
const LIST = [
  version('lab-a', 'align', 'aaaa000002', 5, 2),
  version('lab-a', 'align', 'aaaa000003', 1, 3),
  version('lab-a', 'align', 'oldhit0001', 9, 1),
  version('lab-b', 'align', 'bbbb000001', 3, 7, 'nextflow'),
  version('lab-a', 'call', 'cccc000001', 2, 0),
];

async function open(page: Page, query = '') {
  // None of these versions is released: Developer on lists them (`release.spec.ts` covers it off).
  await page.addInitScript(() => localStorage.setItem('biffix-show-developer', 'true'));
  // The API's list only (the mock, on :8080), not the page's own /workflows document.
  await page.route(`${API}/workflows`, (route) =>
    route.request().method() === 'GET' ? route.fulfill({ json: LIST }) : route.continue(),
  );
  await page.goto(`/workflows${query}`);
}
const rows = (page: Page) => page.locator('tbody tr:not(.sub-row)');

test('versions group by project and name, newest first, with their runs summed', async ({
  page,
}) => {
  await open(page);
  // Default order: project, then name.
  await expect(rows(page)).toHaveCount(3);
  await expect(rows(page).nth(0)).toContainText('align');
  await expect(rows(page).nth(0)).toContainText('lab-a');
  await expect(rows(page).nth(0)).toContainText('aaaa000003'); // the newest, not the first listed
  await expect(rows(page).nth(0).locator('td').last()).toHaveText('6');
  await expect(rows(page).nth(0)).toContainText('3 versions');
  await expect(rows(page).nth(1)).toContainText('call');
  await expect(rows(page).nth(2)).toContainText('lab-b');
  await expect(rows(page).nth(2).locator('td').last()).toHaveText('7');
});

test('the sort is read from the URL, a bad one falls back, and headers work from the keyboard', async ({
  page,
}) => {
  await open(page, '?sort=registered&dir=desc');
  // The row's version (the newest released, or with Developer on the newest).
  const latest = page.getByRole('columnheader', { name: 'Version', exact: true });
  await expect(latest).toHaveAttribute('aria-sort', 'descending');
  await expect(rows(page).nth(0)).toContainText('aaaa000003');
  await expect(rows(page).nth(2)).toContainText('bbbb000001');

  await page.goto('/workflows?sort=bogus&dir=desc');
  await expect(rows(page).nth(0)).toContainText('lab-a');
  await expect(rows(page).nth(2)).toContainText('lab-b');
  for (const name of ['Name', 'Project', 'Engine', 'Version'])
    await expect(page.getByRole('columnheader', { name, exact: true })).toHaveAttribute(
      'aria-sort',
      'none',
    );

  const header = page.getByRole('columnheader', { name: 'Engine' });
  await header.getByRole('button').focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/sort=engine&dir=asc/);
  await expect(header).toHaveAttribute('aria-sort', 'ascending');
  await expect(rows(page).nth(0)).toContainText('nextflow');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/sort=engine&dir=desc/);
  await expect(header).toHaveAttribute('aria-sort', 'descending');
  await expect(rows(page).nth(2)).toContainText('nextflow');
});

test('a search hit in an older version opens its group, which can still be closed', async ({
  page,
}) => {
  await open(page);
  const toggle = page.getByRole('button', { name: /3 versions/ });
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await page.getByRole('searchbox', { name: 'Search workflows' }).fill('oldhit');
  await expect(rows(page)).toHaveCount(1);
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  await expect(page.locator('tr.sub-row')).toHaveCount(2);
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(page.locator('tr.sub-row')).toHaveCount(0);
  await toggle.click();
  await expect(page.locator('tr.sub-row')).toHaveCount(2);
});
