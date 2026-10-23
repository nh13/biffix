import { test, expect, RUN_TAB, startFromExample } from '../fixtures';

// Intermediate-file retention: a Nextflow launch chooses what happens to its work
// dir, and a run whose work dir was deleted can no longer be resumed.

test('a Nextflow launch can ask to delete intermediate files on success', async ({ page }) => {
  await page.goto(RUN_TAB);
  const retention = page.getByRole('combobox', { name: 'Intermediate files' });
  await expect(retention).toHaveValue('default');
  await expect(retention.locator('option').first()).toHaveText('Keep for 30 days (default)');

  await startFromExample(page);
  await retention.selectOption('delete_on_success');
  await expect(page.locator('footer.launch-bar')).toContainText('Ready to launch');
  const launched = page.waitForRequest((r) => r.url().endsWith('/runs') && r.method() === 'POST');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  expect((await launched).postDataJSON().retention).toBe('delete_on_success');
});

test('a run whose intermediate files were deleted offers Re-run but not Resume', async ({
  page,
}) => {
  await page.goto('/runs/run_01HGW6workgone');
  await expect(page.getByRole('link', { name: 'Re-run' }).first()).toBeVisible();
  await expect(page.getByRole('link', { name: 'Resume', exact: true })).toHaveCount(0);
  await expect(
    page.getByText(/results are kept, but the run can no longer be resumed/),
  ).toBeVisible();
});
