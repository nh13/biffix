import { test, expect, expectNoSeriousA11yViolations } from '../fixtures';

// Why a run failed, on the failed run's Overview.

test('a failed run explains its failed step in plain words', async ({ page }) => {
  await page.goto('/runs/run_01HGW1failed');
  const why = page.getByRole('region', { name: 'Why this run failed' });
  await expect(why).toContainText('align (align:sample=BAD01) failed: it ran out of memory');
  await expect(why).toContainText('after 3 tries');
  await expect(why).toContainText('Give this step more memory');
  await why.getByText('Last 3 log lines').click();
  await expect(why.locator('pre')).toContainText('Killed');
  await expectNoSeriousA11yViolations(page);
  await why.getByRole('link', { name: 'Open the job →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_01HGW1failed\/jobs#job=align%3Asample%3DBAD01$/);
});

test('a run that failed preparing its images names that step, with nothing to open', async ({
  page,
}) => {
  await page.goto('/runs/run_01HGWAimgfail');
  const why = page.getByRole('region', { name: 'Why this run failed' });
  await expect(why).toContainText('image preparation failed: image `ghcr.io/lab/bwa-mem3:broken`');
  await expect(why).toContainText('Fix the image reference or its registry');
  await expect(why.getByRole('link', { name: /Open the/ })).toHaveCount(0);
  await expect(why.getByText('AWS says:')).toHaveCount(0);
  // Never launched: no provenance, but the images it was submitted with.
  await expect(page.getByRole('heading', { name: 'Submitted with' })).toBeVisible();
  await expect(page.getByTestId('submit-images')).toContainText(
    'ghcr.io/lab/bwa-mem3:broken@sha256:br0ken',
  );
});

test('a run that did not fail has no failure summary', async ({ page }) => {
  const asked: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/failure-summary')) asked.push(r.url());
  });
  await page.goto('/runs/run_01HGW2wgs30x');
  // The run has loaded (its state is known) before the panel's absence means anything; the
  // overview's own sections have rendered too.
  await expect(page.locator('.run-header .badge')).toHaveText('succeeded');
  await expect(page.getByRole('heading', { name: 'Needs attention' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Why this run failed' })).toHaveCount(0);
  // Nor was a summary asked for, so none can arrive late.
  expect(asked).toEqual([]);
});
