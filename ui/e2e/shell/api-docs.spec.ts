import { readFileSync } from 'node:fs';
import { test, expect } from '../fixtures';
import { workspaceVersion } from '../../src/lib/util/workspaceVersion';

// The API reference: expanding an operation adds no history entry SvelteKit doesn't own, so Back
// after leaving the page goes back to the page, not just to its URL.

test('back from another page returns to the API reference', async ({ page }) => {
  await page.goto('/api-docs');
  const op = page.locator('.opblock-summary').first();
  await expect(op).toBeVisible();
  await op.click();
  await expect(page.locator('.opblock.is-open').first()).toBeVisible();
  expect(new URL(page.url()).hash).toBe('');

  await page.getByRole('link', { name: 'Runs' }).click();
  await expect(page).toHaveURL(/\/runs$/);
  await page.goBack();
  await expect(page).toHaveURL(/\/api-docs$/);
  await expect(page.getByRole('heading', { name: 'API', level: 1 })).toBeVisible();
});

test('the API reference shows the release version, not the document placeholder', async ({
  page,
}) => {
  // The committed document's version is fixed; the build supplies the workspace's (vite.config.ts,
  // through workspaceVersion, whose own tests cover reading it).
  const version = workspaceVersion(
    readFileSync(new URL('../../../Cargo.toml', import.meta.url), 'utf8'),
  );
  await page.goto('/api-docs');
  await expect(page.locator('.info .title small:not(.version-stamp) .version')).toHaveText(version);
});
