import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  useTheme,
  THEMES,
  withoutLaunchedRuns,
} from '../fixtures';

// The Lab view: the default for everyone, with a Lab | Expert switch on every page.

test.describe('Lab view', () => {
  test.use({ view: 'lab' });
  // My work and Analyses list the caller's runs: other tests' launches must not change them.
  test.beforeEach(async ({ page }) => {
    await withoutLaunchedRuns(page);
  });

  test('lands on My work with the bench’s words in the nav', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    const nav = page.getByRole('navigation').first();
    for (const item of ['My work', 'Analyses', 'Pipelines', 'Data', 'Settings'])
      await expect(nav.getByRole('link', { name: item })).toBeVisible();
    for (const expertOnly of ['Status', 'API', 'Usage'])
      await expect(nav.getByRole('link', { name: expertOnly })).toHaveCount(0);
    // Alice's failed analysis needs a look; her running one is in progress.
    await expect(page.getByRole('heading', { name: /Needs attention/ })).toBeVisible();
    await expect(page.getByRole('heading', { name: /In progress/ })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Start an analysis →' }).first()).toBeVisible();
  });

  test('the analyses list shows mine unless I ask for everyone’s', async ({ page }) => {
    const query = () =>
      page.waitForRequest((r) => r.url().endsWith('/runs/query') && r.method() === 'POST');
    const first = query();
    await page.goto('/runs');
    expect((await first).postDataJSON()).toMatchObject({ mine: true });
    await expect(page.getByRole('heading', { name: 'Analyses' })).toBeVisible();
    const rows = page.locator('tbody tr');
    await expect(rows.first()).toBeVisible();
    const mine = await rows.count();
    await expect(page.getByRole('columnheader', { name: 'Started by' })).toHaveCount(0);
    await expect(page.getByRole('columnheader', { name: 'Engine' })).toHaveCount(0);
    const everyone = query();
    await page.getByRole('button', { name: 'Everyone’s' }).click();
    expect((await everyone).postDataJSON()).not.toHaveProperty('mine', true);
    await expect(page.getByRole('columnheader', { name: 'Started by' })).toBeVisible();
    await expect.poll(() => rows.count()).toBeGreaterThan(mine);
  });

  test('an Expert page opened in Lab view renders under a banner that leads back', async ({
    page,
  }) => {
    await page.goto('/runs/run_01HGW3align/jobs');
    const banner = page.getByRole('note');
    await expect(banner).toContainText("You're looking at an Expert page");
    await banner.getByRole('link', { name: 'Back to the Lab view of this analysis' }).click();
    await expect(page).toHaveURL(/\/runs\/run_01HGW3align$/);
    // A pipeline's Expert tab leads back to the pipeline, in the Lab's word for it.
    await page.goto('/workflows/aabbccddeeff%3Av1/provenance');
    await page
      .getByRole('note')
      .getByRole('link', { name: 'Back to the Lab view of this pipeline' })
      .click();
    await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/run$/);
  });

  test('switching view keeps the object, and Lab leaves an Expert-only tab', async ({ page }) => {
    await page.goto('/runs/run_01HGW3align/jobs');
    const view = page.getByRole('group', { name: 'View' }).first();
    await view.getByRole('button', { name: 'Expert' }).click();
    await expect(page).toHaveURL(/\/runs\/run_01HGW3align\/jobs$/);
    await expect(page.getByRole('note')).toHaveCount(0);
    await expect(
      page.getByRole('navigation').first().getByRole('link', { name: 'Status' }),
    ).toBeVisible();
    await view.getByRole('button', { name: 'Lab' }).click();
    await expect(page).toHaveURL(/\/runs\/run_01HGW3align$/);
    // The choice is saved for the user: switch to Expert, forget the browser's cached view, and
    // reload. The fixture then primes this browser with Lab (it primes only when nothing is
    // cached), so only the saved preference can bring Expert back.
    const saved = page.waitForResponse(
      (r) => r.url().endsWith('/me/prefs') && r.request().method() === 'PUT',
    );
    await view.getByRole('button', { name: 'Expert' }).click();
    expect((await saved).request().postDataJSON()).toEqual({ view: 'expert' });
    await page.evaluate(() => {
      localStorage.removeItem('biffix-view');
      localStorage.removeItem('biffix-view-unsynced');
    });
    await page.reload();
    await expect(
      page.getByRole('group', { name: 'View' }).first().getByRole('button', { name: 'Expert' }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  test('Settings offers the view, and keeps admin settings to the Expert view', async ({
    page,
  }) => {
    await page.goto('/settings');
    await expect(page.getByRole('radio', { name: /^Lab/ })).toBeChecked();
    await expect(page.getByRole('heading', { name: 'Site' })).toHaveCount(0);
    await page.getByRole('radio', { name: /^Expert/ }).check();
    await expect(page.getByRole('heading', { name: 'Site' })).toBeVisible();
  });

  test('a phone-width Lab page carries the switch in the top bar', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/');
    const bar = page.locator('header.topbar');
    await expect(bar.getByRole('group', { name: 'View' })).toBeVisible();
    await expect(bar.getByRole('button', { name: 'Lab' })).toHaveAttribute('aria-pressed', 'true');
  });

  for (const theme of THEMES) {
    test(`My work and Analyses are accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto('/');
      await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.goto('/runs');
      await expect(page.locator('tbody tr').first()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }
});

test.describe('per-sample progress', () => {
  test.use({ view: 'lab' });

  test('a run page shows each sample’s steps, and the lists count samples', async ({ page }) => {
    // My work lists the caller's newest runs: other tests' launches must not push this one out.
    await withoutLaunchedRuns(page);
    await page.goto('/runs/run_01HGW3align');
    const grid = page.getByRole('region', { name: /Samples/ });
    await expect(grid).toBeVisible();
    await expect(grid.getByRole('columnheader', { name: 'align' })).toBeVisible();
    const row = grid.getByRole('row', { name: /HG00096/ });
    await expect(row).toContainText('running');
    await expect(grid.getByRole('row', { name: /Whole study/ })).toContainText('Joint call');
    await page.goto('/');
    // In progress starts closed (My work leads with what needs a look).
    await page.getByRole('button', { name: /^In progress/ }).click();
    await expect(page.getByText(/samples seen so far/).first()).toBeVisible();
  });
});
