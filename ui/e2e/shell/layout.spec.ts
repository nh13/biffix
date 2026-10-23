import { test, expect, runTabs } from '../fixtures';

// The pinned run tab bar, and the narrow-screen app shell.

test('the tab bar stays pinned and takes over the header once it scrolls away', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1400, height: 260 });
  await page.goto('/runs/run_01HGW3align/jobs');
  const bar = runTabs(page);
  await expect(bar).toBeVisible();
  await expect(page.getByRole('button', { name: 'Cancel run' })).toHaveCount(1);

  await page.mouse.wheel(0, 2000);
  await expect(bar).toBeInViewport();
  // The header's Cancel scrolled away; the pinned bar now carries one.
  await expect(page.getByRole('button', { name: 'Cancel run' })).toHaveCount(2);
  await expect(page.getByRole('button', { name: 'Cancel run' }).last()).toBeInViewport();
});

test('narrow screens get a top bar and a drawer, and nothing scrolls sideways', async ({
  page,
}) => {
  await page.setViewportSize({ width: 420, height: 800 });
  await page.goto('/runs/run_01HGW3align/jobs');
  await expect(page.getByRole('button', { name: 'joint_call' })).toBeVisible();
  const width = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(width).toBeLessThanOrEqual(420);

  const menu = page.getByRole('button', { name: 'Menu' });
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
  await expect(page.getByRole('link', { name: 'Workflows' })).not.toBeInViewport();
  await menu.click();
  await expect(page.getByRole('link', { name: 'Workflows' })).toBeInViewport();
  await page.keyboard.press('Escape');
  await expect(menu).toHaveAttribute('aria-expanded', 'false');

  await menu.click();
  await page.getByRole('link', { name: 'Workflows' }).click();
  await expect(page).toHaveURL(/\/workflows$/);
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
});

test('the closed drawer is out of the tab order; opening and closing it moves focus', async ({
  page,
}) => {
  await page.setViewportSize({ width: 420, height: 800 });
  await page.goto('/workflows');
  const sidebar = page.locator('#app-sidebar');
  const menu = page.getByRole('button', { name: 'Menu' });
  await expect(menu).toBeVisible();
  // Closed: its off-screen links take no focus, however far Tab goes.
  await expect(sidebar).toHaveAttribute('inert', '');
  for (let i = 0; i < 15; i++) {
    await page.keyboard.press('Tab');
    expect(await sidebar.evaluate((s) => s.contains(document.activeElement))).toBe(false);
  }

  // Opening moves focus into the drawer; Escape closes it and hands focus back to the menu.
  await menu.focus();
  await page.keyboard.press('Enter');
  await expect(sidebar).not.toHaveAttribute('inert');
  await expect(sidebar.getByRole('link', { name: 'Home' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(menu).toBeFocused();
  await expect(sidebar).toHaveAttribute('inert', '');

  // So does a click on the backdrop.
  await menu.click();
  await page.mouse.click(400, 400);
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
  await expect(menu).toBeFocused();
});

test('on a wide screen the sidebar is always in the tab order', async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.goto('/workflows');
  const nav = page.locator('#app-sidebar');
  await expect(nav.getByRole('link', { name: 'Workflows' })).toHaveAttribute(
    'aria-current',
    'page',
  );
  // Checked once the sidebar has rendered: not inert.
  await expect(nav).not.toHaveAttribute('inert');
});
