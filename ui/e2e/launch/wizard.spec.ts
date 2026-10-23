import type { Page } from '@playwright/test';
import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  THEMES,
  useTheme,
  RUN_TAB,
  API,
  chooseBatch12,
  importCsv,
  startFromExample,
} from '../fixtures';

// The Lab view's launch wizard: the Run form a step at a time, against the
// mock's curated nf-core/rnaseq form. The Expert view keeps the single long form (run-tab.spec.ts).

test.use({ view: 'lab' });

const steps = (page: Page) => page.getByRole('navigation', { name: 'Steps' });
const heading = (page: Page, name: string) => page.getByRole('heading', { name, exact: true });
const next = (page: Page) => page.getByRole('button', { name: /^Next: / });
const isPostRuns = (r: import('@playwright/test').Request) =>
  r.method() === 'POST' && new URL(r.url()).pathname === '/runs';
const card = (page: Page, name: string) => page.getByRole('region', { name, exact: true });
const changed = (page: Page) => page.getByRole('list', { name: 'Changed options' });

/** Build three samples (P01–P03) from the mock sequencing core's batch-12 with the builder. */
async function addBatch12(page: Page) {
  await chooseBatch12(page);
  await page
    .getByRole('region', { name: 'Detected samples' })
    .getByRole('button', { name: 'Add 3 samples' })
    .click();
}

/** Open the Review's collapsed *Where results go* (the results folder, intermediate files). */
async function openWhereResultsGo(page: Page) {
  const more = page.locator('details.review-more');
  if ((await more.getAttribute('open')) === null) await more.locator('summary').click();
  await expect(more).toHaveAttribute('open', '');
}

/** Fill step 2 from the form's example, and walk on to `step`. */
async function exampleTo(page: Page, step: 2 | 3 | 4) {
  await page.goto(RUN_TAB);
  await startFromExample(page);
  if (step >= 3) {
    await next(page).click();
    await expect(heading(page, 'Reference & options')).toBeVisible();
  }
  if (step >= 4) {
    await next(page).click();
    await expect(heading(page, 'Review & launch')).toBeVisible();
  }
}

test('the launch shortcut never fires from the new-experiment form', async ({ page }) => {
  await exampleTo(page, 4);
  let submits = 0;
  page.on('request', (r) => {
    if (r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/runs')) submits += 1;
  });
  await page.getByLabel('Experiment').selectOption({ label: 'New experiment…' });
  const name = page.getByRole('form', { name: 'New experiment' }).getByLabel('Name');
  await name.fill('Liver study');
  // No launch follows the shortcut: none within a second, and none counted.
  const launched = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/runs'),
    { timeout: 1_000 },
  );
  await name.press('Control+Enter');
  await expect(launched).rejects.toThrow();
  expect(submits).toBe(0);
  await expect(heading(page, 'Review & launch')).toBeVisible();
});

test('New run is step 1: the catalog, whose Start opens the wizard at Add samples', async ({
  page,
}) => {
  await page.goto('/runs/new');
  await expect(page.getByRole('heading', { name: 'Start an analysis' })).toBeVisible();
  await expect(page.getByText('Step 1 of 4 · Pick a pipeline')).toBeVisible();
  await expect(steps(page).locator('[aria-current="step"]')).toContainText('Pick a pipeline');
  await page
    .getByRole('article', { name: 'Bulk RNA-seq' })
    .getByRole('link', { name: 'Start Bulk RNA-seq' })
    .click();
  await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/run$/);
  await expect(heading(page, 'Add samples')).toBeVisible();
  await expect(steps(page).locator('[aria-current="step"]')).toContainText('Add samples');
  // Step 1 is done and links back to the catalog; later steps can't be reached yet.
  await expect(steps(page).getByRole('link', { name: /Pick a pipeline/ })).toHaveAttribute(
    'href',
    '/runs/new',
  );
  await expect(steps(page).getByRole('link', { name: /Review & launch/ })).toHaveCount(0);
});

test('the wizard walks samples → reference & options → review, and launches as the Expert form does', async ({
  page,
}) => {
  await page.goto(RUN_TAB);

  // Step 2: nothing to go on with until there are samples.
  await expect(heading(page, 'Add samples')).toBeVisible();
  await expect(next(page)).toHaveAttribute('aria-disabled', 'true');
  await expect(page.locator('footer.wizard-nav')).toContainText('Samplesheet: required');
  await addBatch12(page);
  await expect(next(page)).toHaveAttribute('aria-disabled', 'false');
  await next(page).click();

  // Step 3: the reference must be chosen; focus moves to the step's heading.
  await expect(page).toHaveURL(/\?step=3$/);
  await expect(heading(page, 'Reference & options')).toBeFocused();
  await expect(steps(page).getByRole('link', { name: /Add samples/ })).toBeVisible();
  await expect(next(page)).toHaveAttribute('aria-disabled', 'true');
  await page.getByRole('combobox', { name: /^Reference genome/ }).selectOption('grcm39');
  await page.getByRole('radio', { name: 'Strict (100,000)' }).click();
  // Advanced stays collapsed.
  await expect(page.locator('details.advanced')).not.toHaveAttribute('open', '');
  await expect(next(page)).toHaveAttribute('aria-disabled', 'false');
  await next(page).click();

  // Step 4: the review names what the launch runs with.
  await expect(page).toHaveURL(/\?step=4$/);
  await expect(heading(page, 'Review & launch')).toBeFocused();
  const card = (name: string) => page.getByRole('region', { name, exact: true });
  await expect(card('Pipeline')).toContainText('Bulk RNA-seq');
  await expect(card('Pipeline')).toContainText('nf-core/rnaseq');
  await expect(card('Samples')).toContainText('3 samples');
  await expect(card('Samples')).toContainText('P01, P02, P03');
  await expect(card('Reference genome')).toContainText('Mouse GRCm39 · Ensembl 110');
  // Only the option changed from its default is listed.
  const changed = page.getByRole('list', { name: 'Changed options' });
  await expect(changed.getByRole('listitem')).toHaveText([
    'Minimum trimmed reads: Strict (100,000)',
  ]);
  await expect(card('Options')).not.toContainText('Aligner');
  await expect(
    page.getByText('About $6–$9 for 3 samples · usually ~2.5 h · from 6 runs'),
  ).toBeVisible();
  const runName = page.getByRole('textbox', { name: 'Analysis name' });
  await expect(runName).toHaveValue(/^rnaseq · \d{4}-\d{2}-\d{2}$/);

  // Edit goes back to the step it names; a complete step links forward again.
  await card('Samples').getByRole('link', { name: 'Edit samples' }).click();
  await expect(page).toHaveURL(/\?step=2$/);
  await expect(heading(page, 'Add samples')).toBeFocused();
  await steps(page)
    .getByRole('link', { name: /Review & launch/ })
    .click();
  await expect(heading(page, 'Review & launch')).toBeVisible();
  await card('Options').getByRole('link', { name: 'Edit options' }).click();
  await expect(page).toHaveURL(/\?step=3$/);
  await expect(page.getByRole('radio', { name: 'Strict (100,000)' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  await next(page).click();
  await runName.fill('batch 12 check');

  // A reload keeps the step and the draft.
  await page.reload();
  await expect(heading(page, 'Review & launch')).toBeVisible();
  await expect(card('Samples')).toContainText('3 samples');
  await expect(card('Reference genome')).toContainText('Mouse GRCm39 · Ensembl 110');
  await expect(changed.getByRole('listitem')).toHaveText([
    'Minimum trimmed reads: Strict (100,000)',
  ]);
  await expect(runName).toHaveValue('batch 12 check');

  // The Expert form is the same draft: what it would send is captured (and refused, so the draft
  // stays), then the wizard's launch must send the same request.
  const view = page.getByRole('group', { name: 'View' }).first();
  await view.getByRole('button', { name: 'Expert' }).click();
  await expect(page.getByLabel('Run name')).toHaveValue('batch 12 check');
  let expertBody: unknown = null;
  await page.route(`${API}/runs`, async (route) => {
    if (route.request().method() !== 'POST') return route.continue();
    expertBody = route.request().postDataJSON();
    await route.abort();
  });
  await page.getByRole('button', { name: 'Launch run →' }).click();
  await expect(page.locator('.launch-error')).toBeVisible();
  await page.unroute(`${API}/runs`);
  expect(expertBody).not.toBeNull();

  await view.getByRole('button', { name: 'Lab' }).click();
  await expect(heading(page, 'Review & launch')).toBeVisible();
  const launched = page.waitForRequest(isPostRuns);
  await page.getByRole('button', { name: 'Launch analysis →' }).click();
  const body = (await launched).postDataJSON();
  expect(body).toEqual(expertBody);
  expect(body).toMatchObject({
    workflow_version_id: 'aabbccddeeff:v1',
    run_name: 'batch 12 check',
    reference: 'grcm39',
    params: { min_trimmed_reads: 100000 },
  });
  expect(body.tables.input.map((r: Record<string, string>) => r.sample)).toEqual([
    'P01',
    'P02',
    'P03',
  ]);
  // The reference's params and the row-built sheet are filled by biffix, not sent.
  expect(body.params).not.toHaveProperty('fasta');
  expect(body.params).not.toHaveProperty('input');
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  await expect(page.getByRole('heading', { name: 'batch 12 check' })).toBeVisible();
});

test('the example fills the draft from step 2, and Review says nothing changed', async ({
  page,
}) => {
  await exampleTo(page, 4);
  await expect(page.getByText('From a samplesheet file')).toBeVisible();
  await expect(
    page.getByText("Nothing changed from the pipeline's standard settings."),
  ).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'Analysis name' })).toHaveValue(
    'rnaseq example (human)',
  );
});

test('a launch refused by the server says why on the Review step', async ({ page }) => {
  await exampleTo(page, 4);
  await page.route(`${API}/runs`, async (route) => {
    if (route.request().method() !== 'POST') return route.continue();
    await route.fulfill({
      status: 400,
      headers: { 'access-control-allow-origin': '*' },
      contentType: 'text/plain',
      body: 'quota exceeded',
    });
  });
  await page.getByRole('button', { name: 'Launch analysis →' }).click();
  // The server's plain-text reason, shown as it is (as the API sends its errors).
  await expect(page.locator('.launch-error')).toHaveText('quota exceeded');
  await expect(page).toHaveURL(/\?step=4$/);
  // It can be tried again.
  await expect(page.getByRole('button', { name: 'Launch analysis →' })).toBeEnabled();
});

test('a pipeline without a samplesheet asks for its input files on step 2', async ({ page }) => {
  await page.goto('/workflows/112233445566%3Av2/run');
  await expect(heading(page, 'Add samples')).toBeVisible();
  await expect(page.getByRole('textbox', { name: /^Reads/ })).toBeVisible();
  await expect(next(page)).toHaveAttribute('aria-disabled', 'true');
  await page.getByRole('textbox', { name: /^Reads/ }).fill('s3://bucket/reads.fastq.gz');
  await expect(next(page)).toHaveAttribute('aria-disabled', 'false');
});

test('a pipeline without a form points to the Expert view', async ({ page }) => {
  await page.goto('/workflows/ddeeff001122%3Av3/run');
  await expect(heading(page, 'This pipeline has no launch form')).toBeVisible();
  await expect(steps(page)).toHaveCount(0);
  await page.getByRole('link', { name: 'Switch to the Expert view' }).click();
  await expect(
    page.getByRole('group', { name: 'View' }).first().getByRole('button', { name: 'Expert' }),
  ).toHaveAttribute('aria-pressed', 'true');
  await expect(page.getByText('This version declares no parameters.')).toBeVisible();
});

test('Next with a problem left goes to it rather than on', async ({ page }) => {
  await page.goto(RUN_TAB);
  await expect(heading(page, 'Add samples')).toBeVisible();
  // Next is marked disabled but still answers a click (it isn't removed from the tab order).
  await next(page).click({ force: true });
  await expect(page).toHaveURL(/\/run$/);
  await expect(page.locator('footer.wizard-nav')).toContainText('Samplesheet: required');
  await expect(page.locator('#field-input')).toBeFocused();
});

test('a step an earlier step blocks is not reached by its URL', async ({ page }) => {
  await page.goto(`${RUN_TAB}?step=4`);
  await expect(page).toHaveURL(/\?step=2$/);
  await expect(heading(page, 'Add samples')).toBeVisible();
  // With samples (kept as the draft) but no reference, Review sends back to Reference & options.
  await addBatch12(page);
  await page.goto(`${RUN_TAB}?step=4`);
  await expect(page).toHaveURL(/\?step=3$/);
  await expect(heading(page, 'Reference & options')).toBeVisible();
});

test("Review holds Nextflow's output folder, and its problem is fixed there", async ({ page }) => {
  // An author rule on the output folder, so it can have a problem.
  await page.route(/\/workflows\/aabbccddeeff%3Av1$/, async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    for (const f of body.form_schema.fields)
      if (f.name === 'outdir')
        f.rules = [{ pattern: '^s3://', message: 'must be an s3:// folder' }];
    await route.fulfill({ response: res, json: body });
  });
  await exampleTo(page, 4);
  const folder = card(page, 'Analysis details').getByRole('textbox', { name: /^Results folder/ });
  // Collapsed to one line that says where results go; Change opens it.
  await expect(folder).toBeHidden();
  await openWhereResultsGo(page);
  await folder.fill('results/here');
  // The settings close again; the folder's problem opens them to be fixed.
  await page.locator('details.review-more > summary').click();
  await expect(folder).toBeHidden();
  await page.getByRole('button', { name: 'Launch analysis →' }).click();
  const problem = page
    .locator('.banner.problems')
    .getByRole('button', { name: 'Outdir: must be an s3:// folder' });
  await expect(problem).toBeVisible();
  await expect(page).toHaveURL(/\?step=4$/);
  await problem.click();
  await expect(folder).toBeFocused();
  await expect(page.getByRole('combobox', { name: 'Intermediate files' })).toBeVisible();
});

test('a Re-run keeps its pre-fill across the steps, and Review shows what it inherits', async ({
  page,
}) => {
  // An analysis launched with its own results folder and a changed option.
  await exampleTo(page, 3);
  await page.getByRole('radio', { name: 'Strict (100,000)' }).click();
  await next(page).click();
  await openWhereResultsGo(page);
  await card(page, 'Analysis details')
    .getByRole('textbox', { name: /^Results folder/ })
    .fill('s3://lab-results/custom/');
  await page.getByRole('button', { name: 'Launch analysis →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  const runId = page.url().split('/').pop()!;
  // Off the new run's page first: its own polls of the run must not be counted below.
  await page.goto('about:blank');

  let fetched = 0;
  await page.route(`${API}/runs/${runId}*`, (route) => {
    if (route.request().method() === 'GET') fetched++;
    return route.continue();
  });
  await page.goto(`${RUN_TAB}?from=${runId}`);
  await expect(page.getByRole('status')).toContainText(`Pre-filled from ${runId}`);
  await next(page).click();
  await expect(page.getByRole('radio', { name: 'Strict (100,000)' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  await next(page).click();
  await expect(changed(page).getByRole('listitem')).toHaveText([
    'Minimum trimmed reads: Strict (100,000)',
  ]);
  await openWhereResultsGo(page);
  await expect(
    card(page, 'Analysis details').getByRole('textbox', { name: /^Results folder/ }),
  ).toHaveValue('s3://lab-results/custom/');
  await page.goBack();
  await expect(page.getByRole('radio', { name: 'Strict (100,000)' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  expect(fetched).toBe(1);
});

test('an option the author hid is shown and listed once this launch sets it', async ({ page }) => {
  await exampleTo(page, 2);
  const view = page.getByRole('group', { name: 'View' }).first();
  await view.getByRole('button', { name: 'Expert' }).click();
  await page.getByRole('switch', { name: 'Show all parameters' }).check();
  await page.getByLabel('Email').fill('me@lab.example.org');
  await page.getByRole('switch', { name: 'Show all parameters' }).uncheck();
  await view.getByRole('button', { name: 'Lab' }).click();
  await next(page).click();
  await expect(heading(page, 'Reference & options')).toBeVisible();
  // Advanced starts collapsed (Show all is the Expert form's); the field is in it, marked as
  // hidden by the author.
  await expect(page.locator('details.advanced')).not.toHaveAttribute('open', '');
  await page.locator('details.advanced > summary').click();
  await expect(page.getByLabel('Email')).toHaveValue('me@lab.example.org');
  await expect(page.getByText('hidden by author')).toBeVisible();
  await next(page).click();
  await expect(changed(page).getByRole('listitem')).toHaveText(['Email: me@lab.example.org']);
});

test('step 2 works at phone width', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(RUN_TAB);
  await expect(heading(page, 'Add samples')).toBeVisible();
  await importCsv(
    page,
    'samples.csv',
    'sample,fastq_1,fastq_2\nA,s3://b/A_R1.fastq.gz,s3://b/A_R2.fastq.gz\n',
  );
  await expect(page.getByRole('table', { name: 'Samplesheet rows' })).toBeVisible();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
  await next(page).click();
  await expect(heading(page, 'Reference & options')).toBeFocused();
});

for (const theme of THEMES) {
  test(`every wizard step is accessible (${theme})`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto(RUN_TAB);
    await expect(heading(page, 'Add samples')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
    await exampleTo(page, 3);
    await expectNoSeriousA11yViolations(page);
    await next(page).click();
    await expect(heading(page, 'Review & launch')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
    await page.goto('/runs/new');
    await expect(page.getByRole('article').first()).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}
