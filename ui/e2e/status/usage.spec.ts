import { readFile } from 'node:fs/promises';
import { test, expect, expectNoSeriousA11yViolations, API, THEMES } from '../fixtures';

// The Usage page: estimated spend over a period, grouped, with a CSV.

test('usage shows spend by month, regroups, and downloads what is shown', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Usage' }).click();
  await expect(page).toHaveURL(/\/usage$/);
  await expect(page.getByRole('heading', { name: 'Usage', level: 1 })).toBeVisible();
  await expect(page.locator('.summary')).toContainText('$144');
  await expect(page.locator('.summary')).toContainText('8 runs');
  const table = page.locator('table.usage');
  await expect(table.getByRole('rowheader', { name: '2026-09' })).toBeVisible();

  await page.getByLabel('Group by').selectOption('workflow');
  await expect(page).toHaveURL(/group_by=workflow/);
  await expect(table.locator('tbody th').first()).toHaveText('nf-core/sarek');
  await expect(table.getByRole('columnheader', { name: 'Workflow' })).toBeVisible();

  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download CSV' }).click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(/^biffix-usage-.*-by-workflow\.csv$/);
  const csv = await readFile((await file.path())!, 'utf8');
  expect(csv.split('\r\n')[0]).toBe('workflow,runs,compute_usd,storage_usd,total_usd');
  expect(csv).toContain('nf-core/sarek,4,102.30,5.20,107.50');
});

test('a custom period starts from the one shown and lives in the URL', async ({ page }) => {
  await page.goto('/usage?period=last_month');
  await page.getByLabel('Period').selectOption('custom');
  await expect(page).toHaveURL(/period=custom&from=\d{4}-\d{2}-01&to=\d{4}-\d{2}-\d{2}/);
  await page.getByLabel('From').fill('2026-01-01');
  await page.getByLabel('From').dispatchEvent('change');
  await expect(page).toHaveURL(/from=2026-01-01/);
});

test('while a new grouping loads or fails, the rows keep their own label and are not exported', async ({
  page,
}) => {
  let release: () => void = () => {};
  const held = new Promise<void>((r) => (release = r));
  let fail = false;
  await page.route(`${API}/usage?*`, async (route) => {
    const group = new URL(route.request().url()).searchParams.get('group_by');
    if (group === 'workflow') {
      await held;
      return route.fallback();
    }
    if (group === 'project' && fail) return route.fulfill({ status: 500, body: 'boom' });
    return route.fallback();
  });
  await page.goto('/usage');
  const table = page.locator('table.usage');
  await expect(table.getByRole('rowheader', { name: '2026-09' })).toBeVisible();
  const download = page.getByRole('button', { name: 'Download CSV' });
  await expect(download).toBeEnabled();

  await page.getByLabel('Group by').selectOption('workflow');
  // The month rows are still the ones shown: not downloadable as workflows (which says the new
  // grouping is pending), and still labelled as months.
  await expect(download).toBeDisabled();
  await expect(table.getByRole('columnheader', { name: 'Month' })).toBeVisible();
  release();
  await expect(table.getByRole('columnheader', { name: 'Workflow' })).toBeVisible();
  await expect(download).toBeEnabled();

  fail = true;
  await page.getByLabel('Group by').selectOption('project');
  await expect(page.getByRole('alert')).toContainText("Couldn't load usage");
  await expect(download).toBeDisabled();
});

for (const scheme of THEMES) {
  test(`usage (${scheme}) has no serious accessibility violations`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto('/usage');
    await expect(page.locator('table.usage')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}
