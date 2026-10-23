import { test, expect } from '../fixtures';

// Run facts the backend serves that only some runs have: queued jobs, the coordinator's host, the
// inputs declared at submit, and a Nextflow session other than the one biffix launched.

test("a running run shows its queued job, its coordinator's host and its inputs", async ({
  page,
}) => {
  await page.goto('/runs/run_01HGW3align');
  await expect(page.locator('.progress .chip.queued').first()).toHaveText('1 queued');
  const coordinator = page
    .locator('.meta-item')
    .filter({ has: page.locator('.meta-label', { hasText: /^Coordinator$/ }) });
  await expect(coordinator.locator('.instance')).toHaveText('m6i.large us-east-1b');
  await expect(coordinator.locator('.capacity')).toHaveText('on-demand');
  const inputs = page.locator('section.inputs');
  await expect(inputs.getByRole('heading', { name: 'Reads from' })).toBeVisible();
  await expect(inputs.getByRole('listitem')).toHaveText([
    's3://seq-core-deliveries/lab-x/2026/batch-12/',
    's3://shared-refs/GRCh38/genome.fa',
  ]);
});

test('a Nextflow run whose engine reported another session says so', async ({ page }) => {
  await page.goto('/runs/run_01HGW8nflive');
  await expect(page.getByText('session mismatch', { exact: true })).toBeVisible();
});
