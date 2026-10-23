import { test, expect, CORS, onApi, runTabs } from '../fixtures';

// The run's Outputs tab and its file viewer: keyboard use, and what the URL and the server see.

const OUTPUTS = '/runs/run_01HGW2wgs30x/outputs';

test('the viewer keeps focus inside while open and hands it back on close', async ({ page }) => {
  await page.goto(OUTPUTS);
  const opener = page.getByRole('button', { name: 'summary.tsv' });
  await opener.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('button', { name: /^mapped_pct/ })).toBeVisible();
  await expect(dialog).toBeFocused();
  // Tab and Shift+Tab never leave the dialog, however far they go.
  for (let i = 0; i < 12; i++) {
    await page.keyboard.press('Tab');
    expect(await dialog.evaluate((d) => d.contains(document.activeElement))).toBe(true);
  }
  for (let i = 0; i < 12; i++) {
    await page.keyboard.press('Shift+Tab');
    expect(await dialog.evaluate((d) => d.contains(document.activeElement))).toBe(true);
  }
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(opener).toBeFocused();
});

test('the tree/flat choice is one Tab stop moved by the arrow keys', async ({ page }) => {
  await page.goto(OUTPUTS);
  const tree = page.getByRole('radio', { name: 'tree' });
  const flat = page.getByRole('radio', { name: 'flat' });
  await expect(tree).toHaveAttribute('tabindex', '0');
  await expect(flat).toHaveAttribute('tabindex', '-1');
  await tree.focus();
  await page.keyboard.press('ArrowRight');
  await expect(flat).toBeFocused();
  await expect(flat).toHaveAttribute('aria-checked', 'true');
  await expect(page).toHaveURL(/outputs\?view=flat$/);
  await page.keyboard.press('ArrowLeft');
  await expect(tree).toBeFocused();
  await expect(tree).toHaveAttribute('aria-checked', 'true');
});

test('the filter box keeps what is typed through its URL round trip', async ({ page }) => {
  await page.goto(OUTPUTS);
  const box = page.getByLabel('Filter outputs');
  await box.pressSequentially('NA12878 ');
  // The URL keeps the trimmed text; the box keeps the trailing space the user is still typing on.
  // The search can name a sample, so it is in the fragment, not the query.
  await expect(page).toHaveURL(/\/outputs#q=NA12878$/);
  await expect(box).toHaveValue('NA12878 ');
  await box.pressSequentially('bam');
  await expect(page).toHaveURL(/\/outputs#q=NA12878\+bam$/);
  await expect(box).toHaveValue('NA12878 bam');
});

test('leaving the tab while a search is unsaved leaves the next page’s URL alone', async ({
  page,
}) => {
  // The search reports a quarter second after the last key: the tab is clicked before then, and
  // the report must neither win over the navigation nor land on the Jobs page's URL afterwards.
  await page.clock.install();
  await page.goto(OUTPUTS);
  await page.getByLabel('Filter outputs').pressSequentially('bam');
  await runTabs(page).getByRole('link', { name: 'Jobs' }).click();
  await expect(page).toHaveURL(/\/runs\/run_01HGW2wgs30x\/jobs$/);
  await page.clock.runFor(1_000);
  await expect(page).toHaveURL(/\/runs\/run_01HGW2wgs30x\/jobs$/);
});

test("the overview's outputs glance fills in sizes as the server checks them", async ({ page }) => {
  // The first glance has no sizes yet; the next, held until the test lets it go, has them.
  let reads = 0;
  let release = () => {};
  const held = new Promise<void>((r) => (release = r));
  await onApi(page, 'POST', '/runs/run_01HGW2wgs30x/outputs/query', async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    if (reads++ > 0) {
      await held;
      return route.fulfill({ response, json: body });
    }
    const outputs = body.outputs.map((o: object) => ({ ...o, size: undefined }));
    return route.fulfill({
      response,
      json: { ...body, outputs, enrichment_pending: outputs.length },
    });
  });
  await page.goto('/runs/run_01HGW2wgs30x');
  const glance = page.locator('section.latest');
  await expect(glance.locator('li').first()).toBeVisible();
  await expect(glance.locator('.size').first()).toHaveText('');
  release();
  await expect(glance.locator('.size').first()).not.toHaveText('');
});

test("a task's outputs are asked of the server by task, in a request body", async ({ page }) => {
  const urls: string[] = [];
  page.on('request', (r) => urls.push(r.url()));
  const asked = page.waitForRequest(
    (r) => r.method() === 'POST' && /\/runs\/run_01HGW2wgs30x\/outputs\/query$/.test(r.url()),
  );
  await page.goto(`${OUTPUTS}#task=${encodeURIComponent('call:sample=NA12878')}`);
  expect((await asked).postDataJSON()).toMatchObject({ task: 'call:sample=NA12878' });
  await expect(page.getByText('NA12878.vcf.gz', { exact: false }).first()).toBeVisible();
  await expect(page.getByRole('button', { name: 'summary.tsv' })).toHaveCount(0);
  // No request, to the API or the site, carried the task key.
  // The whole URL, path included: a page's own fragment is never part of a request URL.
  expect(urls.filter((u) => /NA12878/.test(decodeURIComponent(u)))).toEqual([]);
});

test('opening a file is a step Back undoes and Forward redoes', async ({ page }) => {
  await page.goto(OUTPUTS);
  await page.getByRole('button', { name: 'summary.tsv' }).click();
  const heading = page.getByRole('dialog').getByRole('heading', { name: 'summary.tsv' });
  await expect(heading).toBeVisible();
  await expect(page).toHaveURL(/\/outputs#file=out6$/);
  await page.goBack();
  await expect(page).toHaveURL(/\/outputs$/);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.goForward();
  await expect(page).toHaveURL(/\/outputs#file=out6$/);
  await expect(heading).toBeVisible();
});

test('a link to a file the list does not show still opens it, fetched by id', async ({ page }) => {
  // out11 is a work-dir intermediate: hidden from the default list, so the page asks for it alone.
  const asked: string[] = [];
  page.on('request', (r) => {
    if (/\/outputs\/out11$/.test(new URL(r.url()).pathname)) asked.push(r.url());
  });
  await page.goto(`${OUTPUTS}#file=out11`);
  await expect(
    page.getByRole('dialog').getByRole('heading', { name: 'NA12878.sorted.bam' }),
  ).toBeVisible();
  expect(asked).toHaveLength(1);
});

test('a linked file that cannot be fetched says so; a stale link opens nothing quietly', async ({
  page,
}) => {
  await onApi(page, 'GET', '/runs/run_01HGW2wgs30x/outputs/out11', (route) =>
    route.fulfill({ status: 503, headers: CORS, json: { error: 'unavailable' } }),
  );
  await page.goto(`${OUTPUTS}#file=out11`);
  await expect(page.getByRole('alert')).toContainText("Couldn't open the linked file");
  await page.goto(`${OUTPUTS}#file=no-such-output`);
  await expect(page.getByRole('heading', { name: /^Outputs/ })).toBeVisible();
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test('a resumed run lists the files it reused, each linking to the run that registered it', async ({
  page,
}) => {
  // A Snakemake resume: it re-ran the alignment and reused the failed run's FastQC report.
  await page.goto('/runs/run_01HGWMsmkresume/outputs?view=flat');
  await expect(page.getByText('2 files')).toBeVisible();
  await expect(page.getByText('1 reused from run_01HGW1failed')).toBeVisible();
  // The heading and the tab count what the list does, reused files included (the run registered
  // one file itself).
  await expect(page.getByRole('heading', { name: 'Outputs (2)' })).toBeVisible();
  await expect(
    runTabs(page)
      .getByRole('link', { name: /Outputs/ })
      .locator('.count'),
  ).toHaveText('2');
  const reused = page.getByRole('row').filter({ hasText: 'qc/BAD01_fastqc.html' });
  await expect(reused).toContainText('Reused from run_01HGW1failed');
  // The run's own file names no other run.
  await expect(page.getByRole('row').filter({ hasText: 'aligned/BAD01.bam' })).not.toContainText(
    'Reused from',
  );
  await reused.getByRole('link', { name: 'run_01HGW1failed' }).click();
  await expect(page).toHaveURL(/\/runs\/run_01HGW1failed$/);
});

test("a resumed run's overview counts the files it reused among its outputs", async ({ page }) => {
  await page.goto('/runs/run_01HGWMsmkresume');
  await expect(page.getByRole('link', { name: 'All 2 outputs →' })).toBeVisible();
  await expect(
    runTabs(page)
      .getByRole('link', { name: /Outputs/ })
      .locator('.count'),
  ).toHaveText('2');
});
