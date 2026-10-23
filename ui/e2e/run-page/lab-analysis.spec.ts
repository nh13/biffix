import type { Page } from '@playwright/test';
import {
  API,
  test,
  expect,
  expectNoSeriousA11yViolations,
  runTabs,
  useTheme,
  THEMES,
  CORS,
  onApi,
} from '../fixtures';

// The Lab view's analysis page: one results-first page, and what went wrong
// with a failed analysis, in plain words, with Try again, a report for support and who to ask.

const DONE = '/runs/run_01HGWDdone';
const OOM = '/runs/run_01HGWEoom';
const BAD_INPUT = '/runs/run_01HGWFbadin';
const ENGINE = '/runs/run_01HGWGengine';
const CANCELLED = '/runs/run_01HGW0cancel';

/** Rewrite `runPath`'s failure summary for this page only. */
async function withFailures(
  page: Page,
  runPath: string,
  edit: (summary: { items: Record<string, unknown>[] }) => void,
) {
  await onApi(page, 'GET', `${runPath}/failure-summary`, async (route) => {
    const response = await route.fetch();
    const summary = await response.json();
    edit(summary);
    await route.fulfill({ response, json: summary });
  });
}

/** The ids of the page's sections, in order. */
const sectionIds = (page: Page) =>
  page.locator('section[id]').evaluateAll((els) => els.map((e) => e.id));

test.describe('Lab analysis page', () => {
  test.use({ view: 'lab' });

  test('a result a resumed analysis reused names the run it came from', async ({ page }) => {
    // The PCA as a resume that skipped it lists it.
    await onApi(page, 'GET', `${DONE}/reports`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      for (const r of body.reports) if (r.name === 'PCA') r.output.reused_from = 'run_01HGW0cancel';
      await route.fulfill({ response, json: body });
    });
    await page.goto(DONE);
    const results = page.getByRole('region', { name: /^Results/ });
    await expect(results.getByRole('article', { name: 'PCA' })).toContainText(
      'Reused from run_01HGW0cancel',
    );
    await expect(results.getByRole('article', { name: 'Gene counts' })).not.toContainText(
      'Reused from',
    );
  });

  test('a finished analysis leads with its results, one card per result', async ({ page }) => {
    await page.goto(DONE);
    await expect(page.getByRole('heading', { level: 1, name: 'rnaseq · batch-10' })).toBeVisible();
    await expect(page.getByTestId('analysis-status')).toContainText('Finished');
    await expect(page.getByTestId('analysis-status')).toContainText('$13.80');
    // One page: no Expert tabs.
    await expect(runTabs(page)).toHaveCount(0);
    await expect(page.getByRole('heading', { name: /QC at a glance/ })).toBeVisible();
    await expect(page.getByRole('region', { name: /Samples/ })).toBeVisible();
    expect(await sectionIds(page)).toEqual(['results', 'qc', 'samples', 'inputs', 'cost']);

    const results = page.getByRole('region', { name: /^Results/ });
    const card = (name: string) => results.getByRole('article', { name });
    await expect(card('Gene counts').getByRole('cell', { name: 'Alb' })).toBeVisible();
    await expect(card('PCA').getByRole('img', { name: 'Preview of PCA' })).toBeVisible();
    await expect(card('QC report')).toContainText('Report');
    // A per-sample result is one card with a sample picker.
    const perSample = card('Per-sample QC');
    await expect(results.getByRole('article', { name: 'Per-sample QC' })).toHaveCount(1);
    // Its preview names what it is and how many samples it has (not a bare number).
    await expect(perSample).toContainText('FastQC · Normal_1');
    await expect(perSample).toContainText('4 samples');
    const picker = perSample.getByRole('combobox', { name: 'Sample' });
    await expect(picker.locator('option')).toHaveText([
      'Normal_1',
      'Normal_2',
      'Tumor_1',
      'Tumor_2',
    ]);
    await picker.selectOption('Tumor_2');
    await perSample.getByRole('button', { name: /^Open/ }).click();
    const viewer = page.getByRole('dialog');
    await expect(viewer.getByRole('heading', { name: 'Per-sample QC · Tumor_2' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(viewer).toHaveCount(0);
    await expect(results.getByRole('link', { name: 'All files →' })).toHaveAttribute(
      'href',
      `${DONE}/files`,
    );

    const inputs = page.getByRole('region', { name: 'Inputs used' });
    await expect(inputs).toContainText('4 samples');
    await expect(inputs).toContainText('grcm39');
    await expect(inputs).toContainText('Aligner: STAR + RSEM');
    await expect(inputs).toContainText('Minimum trimmed reads: Strict (100,000)');
    await expect(inputs.getByRole('button', { name: /Download the samplesheet/ })).toBeVisible();
    await expect(page.getByRole('region', { name: 'Cost' })).toContainText('$13.80');
    // Nothing to try again on a finished analysis.
    await expect(page.getByRole('button', { name: 'Try again' })).toHaveCount(0);
  });

  test('a result whose file is gone offers neither Open nor Download, and says why', async ({
    page,
  }) => {
    await onApi(page, 'GET', `${DONE}/reports`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      const reports = body.reports.map((r: { name: string; output: object }) =>
        r.name === 'Gene counts' ? { ...r, output: { ...r.output, exists: false } } : r,
      );
      await route.fulfill({ response, json: { ...body, reports } });
    });
    await page.goto(DONE);
    const card = page.getByRole('region', { name: /^Results/ }).getByRole('article', {
      name: 'Gene counts',
    });
    await expect(card).toContainText('No longer stored');
    await expect(card.getByRole('button', { name: /^(Open|Download)/ })).toHaveCount(0);
  });

  test('asks for QC data only when the page has a QC section', async ({ page }) => {
    // The finished analysis, cancelled instead: its MultiQC report stays, its QC section goes.
    await onApi(page, 'GET', DONE, async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      await route.fulfill({ response, json: { ...run, state: 'cancelled' } });
    });
    const asked: string[] = [];
    page.on('request', (r) => {
      if (r.url().startsWith(API) && /\/qc\//.test(r.url())) asked.push(r.url());
    });
    await page.goto(DONE);
    await expect(page.getByRole('region', { name: 'Inputs used' })).toBeVisible();
    await page.waitForLoadState('networkidle');
    expect(asked).toEqual([]);
  });

  test('a failed analysis says which sample and step failed, and why', async ({ page }) => {
    await page.goto(OOM);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText(
      'Tumor_4 · STAR align · it ran out of memory (tried 2 times)',
    );
    await expect(wrong).toContainText('What to try:');
    await expect(page.getByTestId('analysis-status')).toContainText('Needs attention');
    await expect(page.getByRole('region', { name: /Samples/ })).toBeVisible();
    expect(await sectionIds(page)).toEqual([
      'what-went-wrong',
      'results',
      'samples',
      'inputs',
      'cost',
    ]);
    // Why? is open, with the likely cause and the last lines of the step.
    const why = wrong.locator('details');
    await expect(why).toHaveAttribute('open', '');
    await expect(why).toContainText('Which sample');
    await expect(why).toContainText('more memory than it was given');
    await expect(why.locator('pre')).toContainText('Killed');
    // Results so far: the per-sample QC of the finished samples.
    await expect(page.getByRole('region', { name: /^Results/ })).toContainText('partial');
    await why.getByRole('link', { name: /Open in Expert view/ }).click();
    await expect(page).toHaveURL(/\/runs\/run_01HGWEoom\/jobs#job=STAR_ALIGN/);
    await expect(page.getByRole('note')).toContainText("You're looking at an Expert page");
  });

  test('Try again resumes the analysis after asking, then opens the new one', async ({ page }) => {
    let body: Record<string, unknown> | null = null;
    await onApi(page, 'POST', '/runs', async (route) => {
      body = route.request().postDataJSON();
      await route.fulfill({
        status: 202,
        headers: CORS,
        json: { run_id: 'run_01HGW2wgs30x', state: 'submitted' },
      });
    });
    await page.goto(OOM);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(
      wrong.getByText('Picks up where it left off — finished samples are kept', { exact: true }),
    ).toBeVisible();
    await wrong.getByRole('button', { name: 'Try again' }).click();
    const dialog = page.getByRole('dialog', { name: 'Resume this analysis?' });
    await expect(dialog).toContainText('Picks up where it left off — finished samples are kept');
    await dialog.getByRole('button', { name: 'Resume' }).click();
    await expect(page).toHaveURL(/\/runs\/run_01HGW2wgs30x$/);
    // Exactly what the launch form sends for this run pre-filled and launched untouched: the
    // bundle's params and the row-built sheet left out, the shared outdir kept for a resume.
    const reads = (sample: string, n: number) =>
      `s3://seq-core-deliveries/lab-x/batch-11/${sample}_R${n}.fastq.gz`;
    expect(body).toEqual({
      workflow_version_id: 'c4d5e6f7a8b9:v1',
      resume_from: 'run_01HGWEoom',
      reference: 'grcm39',
      run_name: 'rnaseq · batch-11 (resumed)',
      params: {
        outdir: 's3://genomics-project/biffix/runs/run_01HGWEoom/results',
        aligner: 'star_rsem',
        min_trimmed_reads: 100000,
      },
      tables: {
        input: ['Tumor_1', 'Tumor_2', 'Tumor_3', 'Tumor_4'].map((sample) => ({
          sample,
          fastq_1: reads(sample, 1),
          fastq_2: reads(sample, 2),
          strandedness: 'auto',
        })),
      },
    });
  });

  test('Fix and try again opens the launch form at the step to fix', async ({ page }) => {
    await page.goto(BAD_INPUT);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText(
      'Normal_2 · FastQC · one of its input files looks damaged or is not in the format it expects',
    );
    await expect(wrong).toContainText(
      "Download or upload this sample's files again; the log below names the file.",
    );
    await wrong.getByRole('button', { name: 'Fix and try again' }).click();
    await expect(page).toHaveURL(
      /\/workflows\/c4d5e6f7a8b9%3Av1\/run\?from=run_01HGWFbadin&step=2&sample=Normal_2$/,
    );
    // The failing sample's row is pointed out: highlighted, and its first cell focused.
    const row = page.locator('tr.highlighted');
    await expect(row).toHaveCount(1);
    await expect(row.getByRole('textbox', { name: 'Row 2 sample' })).toBeFocused();
    await expect(row.getByRole('textbox', { name: 'Row 2 sample' })).toHaveValue('Normal_2');
  });

  test('a fix wins over a resume when the failures differ', async ({ page }) => {
    // The out-of-memory failure (resume) beside a damaged input (fix the samples).
    await withFailures(page, OOM, (summary) => {
      summary.items.push({
        task_key: 'FASTQC (Tumor_2) #2',
        sample: 'Tumor_2',
        step: 'FASTQC',
        cause: 'bad_input',
        message: 'one of its input files looks damaged or is not in the format it expects',
        advice: "Download or upload this sample's files again; the log below names the file.",
        attempts: 1,
        exit_code: 1,
        log_tail: ['gzip: Tumor_2_R1.fastq.gz: unexpected end of file'],
        try_again: 'fix_samples',
      });
    });
    await page.goto(OOM);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText('Tumor_2 · FastQC');
    await expect(page.getByRole('button', { name: 'Try again', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Fix and try again' })).toHaveCount(2);
    await wrong.getByRole('button', { name: 'Fix and try again' }).click();
    await expect(page).toHaveURL(/\?from=run_01HGWEoom&step=2&sample=Tumor_2$/);
  });

  test('a resume the server would refuse is a re-run', async ({ page }) => {
    await withFailures(page, OOM, (summary) => {
      summary.items[0].try_again = 'rerun';
    });
    await page.goto(OOM);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(
      wrong.getByText('Starts the analysis again with the same samples and settings', {
        exact: true,
      }),
    ).toBeVisible();
    await wrong.getByRole('button', { name: 'Try again' }).click();
    await expect(page.getByRole('dialog', { name: 'Run this analysis again?' })).toBeVisible();
  });

  test('the report for support copies what went wrong, never a secret', async ({
    page,
    context,
  }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    // The run carries a secret the version marks secret (the server would have redacted it; the
    // report keeps it out regardless).
    await onApi(page, 'GET', OOM, async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      run.params = { ...run.params, api_token: 'sk-live-TOPSECRET' };
      await route.fulfill({ response, json: run });
    });
    await onApi(page, 'GET', /^\/workflows\/c4d5e6f7a8b9(:|%3A)v1$/i, async (route) => {
      const response = await route.fetch();
      const detail = await response.json();
      detail.form_schema.fields.push({
        name: 'api_token',
        widget: 'text',
        required: false,
        is_output: false,
        is_secret: true,
        is_path_input: false,
      });
      await route.fulfill({ response, json: detail });
    });
    await page.goto(OOM);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText('Tumor_4');
    // The version (with its secret field) has loaded: the options it changed are listed.
    await expect(page.getByRole('region', { name: 'Inputs used' })).toContainText('Api token: Set');
    await wrong.getByRole('button', { name: 'Copy report for support' }).click();
    await expect(wrong.getByRole('button', { name: 'Copied' })).toBeVisible();
    const report = await page.evaluate(() => navigator.clipboard.readText());
    expect(report).toContain('Run id: run_01HGWEoom');
    expect(report).toContain('Failure 1: Tumor_4 · STAR align · it ran out of memory');
    expect(report).toContain('  Exit code: 137');
    expect(report).toContain('  Batch job: 5f0c6a2e-7b1d-4c9e-9a51-2f3e8d4b6c01');
    expect(report).toContain('Killed');
    expect(report).toContain('api_token: [secret]');
    expect(report).not.toContain('sk-live-TOPSECRET');
  });

  test('Ask emails the pipeline’s contact about this analysis', async ({ page }) => {
    await page.goto(OOM);
    const ask = page
      .getByRole('region', { name: 'What went wrong' })
      .getByRole('link', { name: 'Ask Sam Okafor' });
    const href = new URL((await ask.getAttribute('href'))!);
    expect(href.protocol).toBe('mailto:');
    expect(href.pathname).toBe('sam.okafor@lab.example.org');
    expect(href.searchParams.get('subject')).toBe('biffix: rnaseq · batch-11 needs attention');
    const body = href.searchParams.get('body')!;
    expect(body).toContain('/runs/run_01HGWEoom');
    expect(body).toContain('Tumor_4 · STAR align · it ran out of memory');
    expect(body).toContain('Paste the copied report here.');
    await expect(page.getByRole('link', { name: 'Help page ↗' })).toHaveAttribute(
      'href',
      'https://help.lab.example.org/rnaseq',
    );
  });

  test('without a contact, it says who to ask instead', async ({ page }) => {
    await page.goto(ENGINE);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText(
      'Workflow engine · the workflow engine stopped with an error before any step failed',
    );
    await expect(wrong).toContainText(
      "The engine's log, below, says why: often a parameter or input it could not use.",
    );
    await expect(wrong).toContainText('Ask whoever runs biffix for your lab.');
    await expect(wrong.getByRole('link', { name: /^Ask / })).toHaveCount(0);
    // An engine failure has no step to open in the Expert view.
    await expect(wrong.getByRole('link', { name: /Open in Expert view/ })).toHaveCount(0);
    // A setting it could not use: the fix is in the options (step 3).
    await wrong.getByRole('button', { name: 'Fix and try again' }).click();
    await expect(page).toHaveURL(/\/run\?from=run_01HGWGengine&step=3$/);
  });

  test('an engine failure with no log to read says so', async ({ page }) => {
    // As the server words an engine failure whose coordinator log was empty.
    await withFailures(page, ENGINE, (summary) => {
      Object.assign(summary.items[0], {
        cause: 'unknown',
        message: 'the workflow engine stopped, and biffix could not read its log',
        advice: "Open the coordinator's log in Expert view to see what happened.",
        log_tail: [],
        try_again: 'rerun',
      });
    });
    await page.goto(ENGINE);
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText(
      'Workflow engine · the workflow engine stopped, and biffix could not read its log',
    );
    await expect(wrong).toContainText(
      "Open the coordinator's log in Expert view to see what happened.",
    );
    await expect(wrong.locator('details')).toContainText(
      'biffix could not tell why from what it recorded.',
    );
    await expect(wrong.getByRole('link', { name: /Open in Expert view/ })).toHaveCount(0);
    await expect(wrong.getByRole('button', { name: 'Try again' })).toBeVisible();
  });

  test('a failure before launch is worded by where it stopped', async ({ page }) => {
    await page.goto('/runs/run_01HGWAimgfail');
    const wrong = page.getByRole('region', { name: 'What went wrong' });
    await expect(wrong).toContainText('Image preparation');
    await expect(wrong.locator('details')).toContainText(
      "biffix could not prepare the pipeline's software.",
    );
  });

  test('a cancelled analysis offers to run it again', async ({ page }) => {
    await page.goto(CANCELLED);
    await expect(page.getByTestId('analysis-status')).toContainText('Cancelled');
    await expect(page.getByRole('region', { name: 'What went wrong' })).toHaveCount(0);
    await page.getByRole('button', { name: 'Try again' }).click();
    const dialog = page.getByRole('dialog', { name: 'Run this analysis again?' });
    await expect(dialog).toContainText(
      'Starts the analysis again with the same samples and settings',
    );
    await dialog.getByRole('button', { name: 'Not now' }).click();
    await expect(dialog).toBeHidden();
  });

  test('a live analysis leads with its samples, and can be cancelled', async ({ page }) => {
    await page.goto('/runs/run_01HGW3align');
    await expect(page.getByTestId('analysis-status')).toContainText('Running');
    await expect(page.getByRole('progressbar', { name: 'Progress' })).toBeVisible();
    await expect(page.getByRole('region', { name: /Samples/ })).toBeVisible();
    expect((await sectionIds(page))[0]).toBe('samples');
    await expect(page.getByRole('region', { name: /^Results/ })).toContainText(
      'Results appear here as the analysis finishes',
    );
    await expect(page.getByRole('region', { name: 'Cost' })).toContainText('≈ $3.84 so far');
    // Cancel asks first, then sends the cancel (answered here: the shared run stays running).
    const cancels: string[] = [];
    await onApi(page, 'POST', '/runs/run_01HGW3align/cancel', async (route) => {
      cancels.push(route.request().url());
      await route.fulfill({ status: 202, headers: CORS });
    });
    page.once('dialog', (d) => d.accept());
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect.poll(() => cancels.length).toBe(1);
  });

  for (const theme of THEMES) {
    test(`the finished and failed pages are accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto(DONE);
      await expect(page.getByRole('cell', { name: 'Alb' })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.goto(OOM);
      await expect(page.getByRole('region', { name: 'What went wrong' })).toContainText('Tumor_4');
      await expectNoSeriousA11yViolations(page);
    });
  }

  test('fits a phone without scrolling sideways', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    for (const path of [OOM, DONE]) {
      await page.goto(path);
      await expect(page.getByRole('region', { name: /^Results/ })).toBeVisible();
      await expect(page.getByRole('region', { name: 'Inputs used' })).toContainText('samples');
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      expect(overflow, path).toBeLessThanOrEqual(0);
    }
  });
});
