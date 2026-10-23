import { test, expect, expectNoSeriousA11yViolations, API, nextFrame, THEMES } from '../fixtures';

// The status page: one shared live poll with the layout's badge, an outage shown as one, the history
// window's bars, and the controls' pressed/expanded state.

const isStatus = (url: string) => url === `${API}/status`;

test('the page and the health badge share one status poll', async ({ page }) => {
  await page.clock.install();
  const probes: string[] = [];
  page.on('request', (r) => {
    if (isStatus(r.url()) && r.method() === 'GET') probes.push(r.url());
  });
  await page.goto('/status');
  await expect(page.locator('.banner')).toContainText('Partial degradation');
  expect(probes).toHaveLength(1);

  // After each poll, part of an interval more and a round trip to the page: a second poller's
  // request would have arrived by then, so the count must still be exact.
  const settled = async (n: number) => {
    await expect.poll(() => probes.length).toBe(n);
    await page.clock.runFor(1_000);
    await page.evaluate(() => 0);
    expect(probes).toHaveLength(n);
  };
  await page.clock.runFor(15_000);
  await settled(2);
  // Leaving the page keeps the layout's poll, still one request per interval.
  await page.getByRole('link', { name: 'Usage' }).click();
  await page.clock.runFor(14_000);
  await settled(3);
});

test('an outage after a good probe says so, keeping the last report as stale', async ({ page }) => {
  await page.clock.install();
  let down = false;
  await page.route(`${API}/status`, (route) =>
    down ? route.abort('connectionrefused') : route.fallback(),
  );
  await page.goto('/status');
  await expect(page.locator('.banner')).toContainText('Partial degradation');

  down = true;
  await page.clock.runFor(15_000);
  const banner = page.getByRole('alert');
  await expect(banner).toContainText("Couldn't reach the control plane");
  // The time it was last reached, not last tried.
  await expect(banner).toContainText('last reached');
  await expect(page.getByText('as of the last check')).toBeVisible();
  await expect(page.locator('.component').first()).toBeVisible();

  down = false;
  await page.clock.runFor(15_000);
  await expect(page.locator('.banner')).toContainText('Partial degradation');
});

test('the history window is pressed, and a failed window does not keep the old bars', async ({
  page,
}) => {
  await page.route(`${API}/status/history?window=1w`, (route) =>
    route.fulfill({ status: 500, body: 'boom' }),
  );
  await page.goto('/status');
  const windows = page.getByRole('group', { name: 'History window' });
  await expect(windows.getByRole('button', { name: '1d' })).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('.bars').first()).toBeVisible();

  // The bars are checked once the 1w window has failed, not while it is loading (which clears them
  // too).
  const failed = page.waitForResponse((r) => r.url().endsWith('/status/history?window=1w'));
  await windows.getByRole('button', { name: '1w' }).click();
  await failed;
  // A frame later, the page has handled the failure.
  await nextFrame(page);
  await expect(windows.getByRole('button', { name: '1w' })).toHaveAttribute('aria-pressed', 'true');
  await expect(windows.getByRole('button', { name: '1d' })).toHaveAttribute(
    'aria-pressed',
    'false',
  );
  await expect(page.locator('.bars')).toHaveCount(0);

  await windows.getByRole('button', { name: '1h' }).click();
  await expect(page.locator('.bars').first()).toBeVisible();
});

test('filters are pressed buttons and the info toggle is expandable', async ({ page }) => {
  await page.goto('/status');
  const filters = page.getByRole('group', { name: 'Filter by status' });
  await filters.getByRole('button', { name: 'Degraded' }).click();
  await expect(filters.getByRole('button', { name: 'Degraded' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await expect(page.locator('.component')).toHaveCount(1);

  await page.getByRole('searchbox', { name: 'Search services' }).fill('nothing-like-this');
  await expect(page.getByText('No services match.')).toBeVisible();
  await page.getByRole('searchbox', { name: 'Search services' }).fill('');

  const info = page.getByRole('button', { name: 'About Event Ingest' });
  await expect(info).toHaveAttribute('aria-expanded', 'false');
  await info.click();
  await expect(info).toHaveAttribute('aria-expanded', 'true');
  await expect(page.getByText('Folds engine and AWS events')).toBeVisible();
});

for (const scheme of THEMES) {
  test(`status (${scheme}) has no serious accessibility violations`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto('/status');
    await expect(page.locator('.bars').first()).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}
