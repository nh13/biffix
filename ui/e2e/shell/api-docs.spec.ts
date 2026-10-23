import { test, expect } from '../fixtures';

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
