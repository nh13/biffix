import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  onApi,
  runTabs as tabs,
  API,
} from '../fixtures';

// A run's Reports tab: the workflow's declared reports and a MultiQC report,
// shown in the page; Overview points at the QC report.

const RUN = '/runs/run_01HGW2wgs30x';

test('Overview points at the QC report, which opens in the Reports tab', async ({ page }) => {
  await page.goto(RUN);
  await expect(
    tabs(page)
      .getByRole('link', { name: /Reports/ })
      .locator('.count'),
  ).toHaveText('2');
  await page.getByRole('link', { name: 'QC report available →' }).click();
  // The report is a file id: it is in the fragment, not the query.
  await expect(page).toHaveURL(/\/reports#report=out8$/);
  await expect(tabs(page).getByRole('link', { name: /Reports/ })).toHaveAttribute(
    'aria-current',
    'page',
  );
  await expect(page.getByRole('button', { name: 'MultiQC' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  const frame = page.frameLocator('iframe[title="MultiQC"]');
  await expect(frame.getByText('hello from a sandboxed frame')).toBeVisible();

  // Another report; the choice is in the URL.
  await page.getByRole('button', { name: 'Coverage' }).click();
  await expect(page).toHaveURL(/\/reports#report=out9$/);
  await expect(page.getByRole('img', { name: 'Coverage' })).toBeVisible();
  // Back and Forward walk the fragment too: Back leaves for the Overview, Forward returns to the
  // last report chosen (the choice replaced the entry).
  await page.goBack();
  await expect(page).toHaveURL(/\/runs\/run_01HGW2wgs30x$/);
  await page.goForward();
  await expect(page).toHaveURL(/\/reports#report=out9$/);
  await expect(page.getByRole('button', { name: 'Coverage' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );

  await expectNoSeriousA11yViolations(page);
});

test('a failed reports load says so and can be retried', async ({ page }) => {
  let failures = 1;
  // The API's reports list (the mock, on :8080) fails once; the page's own URL is left alone.
  await page.route(`${API}${RUN}/reports`, (route) =>
    route.request().method() === 'GET' && failures-- > 0
      ? route.fulfill({
          status: 500,
          body: 'boom',
          headers: { 'access-control-allow-origin': '*' },
        })
      : route.continue(),
  );
  await page.goto(`${RUN}/reports`);
  const alert = page.getByRole('alert');
  await expect(alert).toContainText("Couldn't load this run's reports");
  await expect(page.getByText('Loading reports…')).toHaveCount(0);
  await alert.getByRole('button', { name: 'Try again' }).click();
  await expect(page.getByRole('button', { name: 'MultiQC' })).toBeVisible();
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test("going to another run drops the previous run's reports at once", async ({ page }) => {
  // Run A (a resumed run) is given two reports; run B's (the run it resumed) are held back.
  const api = `${API}/runs`;
  await page.route(`${api}/run_01HGW4resume/reports`, async (route) =>
    route.fulfill({ response: await route.fetch({ url: `${api}/run_01HGW2wgs30x/reports` }) }),
  );
  let release!: () => void;
  const held = new Promise<void>((resolve) => (release = resolve));
  await page.route(`${api}/run_01HGW0cancel/reports`, async (route) => {
    await held;
    await route.continue();
  });
  await page.goto('/runs/run_01HGW4resume');
  const reportsTab = tabs(page).getByRole('link', { name: /Reports/ });
  await expect(reportsTab.locator('.count')).toHaveText('2');
  await expect(page.getByRole('link', { name: 'QC report available →' })).toBeVisible();

  // The "Resumed from" link reuses the run layout.
  await page.getByRole('link', { name: 'run_01HGW0cancel', exact: true }).click();
  await expect(page).toHaveURL(/\/runs\/run_01HGW0cancel$/);
  await expect(reportsTab).toBeVisible();
  // While B's reports load, nothing of A's is shown or linked under B.
  await expect(reportsTab.locator('.count')).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'QC report available →' })).toHaveCount(0);
  release();
  await expect(reportsTab).toHaveClass(/empty/);
});

test('a run without reports says where they come from', async ({ page }) => {
  await page.goto('/runs/run_01HGW1failed/reports');
  await expect(page.getByText('This run has no reports yet.')).toBeVisible();
  await expect(tabs(page).getByRole('link', { name: /Reports/ })).toHaveClass(/empty/);
  // The Overview has its reports (none) before their link's absence means anything.
  const loaded = page.waitForResponse(
    (r) => r.url() === `${API}/runs/run_01HGW1failed/reports` && r.request().method() === 'GET',
  );
  await page.goto('/runs/run_01HGW1failed');
  await loaded;
  await expect(tabs(page).getByRole('link', { name: /Reports/ })).toHaveClass(/empty/);
  await expect(page.getByRole('link', { name: 'QC report available →' })).toHaveCount(0);
});

test('a MultiQC report shows its General Statistics natively', async ({ page }) => {
  await page.goto(RUN);
  const line = page.locator('.reports-line');
  await expect(line).toContainText('2 samples');
  await expect(line).toContainText('1 failing QC: NA12891');

  // Choosing a report writes only the fragment's own params into it, never the query's.
  await page.goto(`${RUN}/reports?tab=x#report=out8`);
  await page.getByRole('button', { name: 'Coverage' }).click();
  await expect(page).toHaveURL(/\/reports\?tab=x#report=out9$/);
  await page.getByRole('button', { name: 'MultiQC' }).click();
  const table = page.locator('details.qc table');
  await expect(page.getByText('General statistics')).toBeVisible();
  // MultiQC's rules: 2.1M reads fails, with a symbol as well as a color, and says so in words.
  const failing = table.getByRole('row', { name: /NA12891/ });
  await expect(failing).toContainText('2.1M');
  await expect(failing.getByRole('cell', { name: /2\.1M.*\(fail\)/ })).toBeVisible();
  await expect(table.getByRole('row', { name: /NA12878/ })).toContainText('41.2%');

  // Sorting by a column, and the column MultiQC hides.
  await table.getByRole('button', { name: /^GC/ }).click();
  await expect(table.locator('tbody th').first()).toHaveText('NA12878');
  await table.getByRole('button', { name: /^GC/ }).click();
  await expect(table.locator('tbody th').first()).toHaveText('NA12891');
  await expect(table.getByRole('button', { name: /Avg len/ })).toHaveCount(0);
  await page.getByLabel(/Show all columns \(1 hidden by MultiQC\)/).check();
  await expect(table.getByRole('button', { name: /Avg len/ })).toBeVisible();
  // NA12878 fails only on that hidden column, so the Overview did not count it as failing.
  await expect(
    table.getByRole('row', { name: /NA12878/ }).getByRole('cell', { name: /150 bp.*\(fail\)/ }),
  ).toBeVisible();
  // Sorted by the hidden column, then hidden again: the table is sorted by sample, and says so.
  await table.getByRole('button', { name: /Avg len/ }).click();
  await page.getByLabel(/Show all columns \(1 hidden by MultiQC\)/).uncheck();
  await expect(table.locator('thead th').first()).toHaveAttribute('aria-sort', 'ascending');
  await page.getByLabel(/Show all columns \(1 hidden by MultiQC\)/).check();

  await expectNoSeriousA11yViolations(page);

  // A sample links to its jobs.
  await table.getByRole('link', { name: 'NA12878' }).click();
  await expect(page).toHaveURL(/\/jobs#q=NA12878$/);
});

test('a report a resumed run reused names the run that wrote it', async ({ page }) => {
  // The run's MultiQC report, as a resume that skipped MultiQC lists it.
  await onApi(page, 'GET', `${RUN}/reports`, async (route) => {
    const body = await (await route.fetch()).json();
    for (const r of body.reports)
      if (r.kind === 'multiqc') r.output.reused_from = 'run_01HGW0cancel';
    await route.fulfill({ json: body });
  });
  await page.goto(`${RUN}/reports`);
  await page.getByRole('button', { name: 'MultiQC' }).click();
  await expect(page.getByText('Reused from run_01HGW0cancel')).toBeVisible();
  await page.getByRole('button', { name: 'Coverage' }).click();
  await expect(page.getByText('Reused from')).toHaveCount(0);
});
