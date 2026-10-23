import { test, expect, onApi } from '../fixtures';

// The in-app log viewer (LogStream), on the coordinator log of a live run. The mock serves the
// LogPage contract: a stream, `ts_ms` lines, and cursors that come back unchanged once paged.

test('the coordinator log shows timed lines, live, and pages to the start of the stream', async ({
  page,
}) => {
  await page.goto('/runs/run_01HGW3align/coordinator');
  const log = page.locator('section.coordinator-log');
  await expect(log.getByText('[mock] coordinator: log line 1')).toBeVisible();
  await expect(log.getByText('[mock] coordinator: log line 3')).toBeVisible();
  // Real timestamps (HH:MM:SS.mmm), not "Invalid Date".
  await expect(log.locator('.ts').first()).toHaveText(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
  await expect(log.getByText('● live')).toBeVisible();

  // The mock's backward cursor comes back unchanged: that is the start, and nothing is lost.
  await log.getByRole('button', { name: 'Load earlier' }).click();
  await expect(log.getByText('— start of stream —')).toBeVisible();
  await expect(log.locator('.ln')).toHaveCount(3);

  await log.getByRole('button', { name: 'From start' }).click();
  await expect(log.getByRole('button', { name: 'Jump to end' })).toBeVisible();
  await expect(log.locator('.ln')).toHaveCount(3);
  await expect(log.getByText('— start of stream —')).toBeVisible();
});

test('a live run whose coordinator log has no stream yet says it is coming', async ({ page }) => {
  await onApi(page, 'GET', '/runs/run_01HGW3align/coordinator-logs', (route) =>
    route.fulfill({ json: { events: [] } }),
  );
  await page.goto('/runs/run_01HGW3align/coordinator');
  const log = page.locator('section.coordinator-log');
  await expect(log).toContainText("The coordinator's log hasn't appeared yet.");
  await expect(log).not.toContainText('No log stream was recorded');
});

test("a job's pane shows its attempts and tails the display attempt's log, with console links in the deploy's region", async ({
  page,
}) => {
  await page.goto('/runs/run_01HGW3align/jobs#job=align%3Asample%3DHG00096');
  const pane = page.getByRole('complementary', { name: /Job align:sample=HG00096/ });
  const attempts = pane.getByRole('group', { name: 'Attempts' });
  await expect(attempts.getByRole('button')).toHaveCount(2);
  await expect(attempts.getByRole('button', { pressed: true })).toContainText('✓');
  await expect(pane.getByText('[mock] align:sample=HG00096: log line 1')).toBeVisible();
  await expect(pane.getByText('● live')).toBeVisible();
  // The region comes from /auth/config, not a constant.
  await expect(pane.getByRole('link', { name: 'Batch job ↗' })).toHaveAttribute(
    'href',
    /^https:\/\/us-east-1\.console\.aws\.amazon\.com\/batch\/.*#jobs\/ec2\/detail\/job-2$/,
  );
  await expect(pane.getByRole('link', { name: 'CloudWatch ↗' })).toHaveAttribute(
    'href',
    /region=us-east-1#logsV2:.*mock\$252Falign/,
  );
});

test('a live log stops polling while the tab is hidden and catches up when shown', async ({
  page,
}) => {
  // The page's timers run on a fake clock: the test steps through the 5 s poll interval instead of
  // waiting it out.
  await page.clock.install();
  const polls: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/coordinator-logs') && r.method() !== 'OPTIONS') polls.push(r.url());
  });
  const isPoll = (r: import('@playwright/test').Response) =>
    r.url().includes('/coordinator-logs') && r.request().method() !== 'OPTIONS';
  await page.goto('/runs/run_01HGW3align/coordinator');
  await expect(page.locator('section.coordinator-log').getByText('● live')).toBeVisible();
  // The coordinator log polls every 5 s: the first tick after the initial load.
  const loaded = polls.length;
  let polled = page.waitForResponse(isPoll);
  await page.clock.runFor(5_000);
  await polled;
  expect(polls.length).toBeGreaterThan(loaded);

  const setHidden = (hidden: boolean) =>
    page.evaluate((h) => {
      Object.defineProperty(document, 'visibilityState', {
        configurable: true,
        get: () => (h ? 'hidden' : 'visible'),
      });
      document.dispatchEvent(new Event('visibilitychange'));
    }, hidden);

  // A hidden tab sends nothing for longer than an interval.
  await setHidden(true);
  const whileHidden = polls.length;
  await page.clock.runFor(12_000);
  // Nothing was sent meanwhile (a round trip to the page lets any request it did send be seen).
  await page.evaluate(() => 0);
  expect(polls.length).toBe(whileHidden);

  // Shown again: one refresh right away, without the clock moving on.
  polled = page.waitForResponse(isPoll);
  await setHidden(false);
  await polled;
  expect(polls.length).toBeGreaterThan(whileHidden);
});
