import { test, expect, onApi } from '../fixtures';

// Links out of an analysis: the analyses list's Pipeline column opens the version it ran, the
// analysis page names that version (and links it), and every S3 URI it shows for its results (the
// output prefix, each output, each key result) opens in the Data browser, beside its copy button,
// when the Data browser can show it.

const DONE = '/runs/run_01HGWDdone';
const DONE_VERSION = /\/workflows\/c4d5e6f7a8b9(%3A|:)v1$/;
const DONE_PREFIX = 's3://genomics-project/biffix/runs/run_01HGWDdone/';
const WGS_OUTPUTS = '/runs/run_01HGW2wgs30x/outputs';

test.describe('Analysis links (Lab view)', () => {
  test.use({ view: 'lab' });

  test('the Pipeline column links the pipeline version each analysis ran', async ({ page }) => {
    await page.goto('/runs?experiment=exp_liver_tumor');
    const row = page.getByRole('row', { name: /rnaseq · batch-10/ });
    // Named by its friendly title.
    const pipeline = row.getByRole('link', { name: 'Bulk RNA-seq' });
    await expect(pipeline).toHaveAttribute('href', DONE_VERSION);
    await pipeline.click();
    await expect(page).toHaveURL(DONE_VERSION);
  });

  test('the analysis page names the version it ran and links it', async ({ page }) => {
    await page.goto(DONE);
    const version = page.getByTestId('run-version');
    // The version's label (as its own page labels it) and the day it was registered (20 days
    // before the mock's now).
    await expect(version).toHaveText(
      /^Bulk RNA-seq · version c4d5e6f7a8:v1 · registered \d{4}-\d{2}-\d{2}$/,
    );
    await expect(version).toHaveAttribute('href', DONE_VERSION);
    const inputs = page.getByRole('region', { name: 'Inputs used' });
    await expect(inputs.getByTestId('inputs-version')).toHaveAttribute('href', DONE_VERSION);
    await expect(inputs.getByTestId('inputs-version')).toContainText('version c4d5e6f7a8:v1');
  });

  test('where the results are opens in the Data browser, and copies', async ({ page }) => {
    await page.goto(DONE);
    const where = page.getByTestId('results-folder');
    const link = where.getByRole('link', { name: DONE_PREFIX });
    await expect(link).toHaveAttribute(
      'href',
      '/data/project?project=genomics#path=biffix%2Fruns%2Frun_01HGWDdone%2F',
    );
    await expect(
      where.getByRole('button', { name: 'copy the URI of the results folder' }),
    ).toBeVisible();
    await link.click();
    // The run's folder in the project bucket, listed.
    await expect(page.getByRole('link', { name: /star_salmon/ })).toBeVisible();
    // Its trail: `biffix` (hidden by the source) is left out, never a link onto a 404; the runs'
    // folder opens, as *Analysis results*, each analysis's folder by its name.
    const trail = page.getByRole('navigation', { name: 'Folder path' });
    await expect(trail).not.toContainText('biffix');
    await trail.getByRole('link', { name: 'Analysis results', exact: true }).click();
    await expect(page).toHaveURL(/#path=biffix%2Fruns%2F$/);
    await expect(page.getByRole('link', { name: /rnaseq · batch-10/ })).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
    // And back up to the project's root, once its listing is in.
    await page
      .getByRole('navigation', { name: 'Folder path' })
      .getByRole('link', { name: 'Project data' })
      .click();
    await expect(page.getByRole('link', { name: 'Analysis results', exact: true })).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
  });

  test('a key result opens in the Data browser, its file open there', async ({ page }) => {
    await page.goto(DONE);
    const card = page.getByRole('article', { name: 'Gene counts' });
    const link = card.getByRole('link', { name: /^Show in Data browser/ });
    await expect(link).toHaveAttribute(
      'href',
      '/data/project?project=genomics#path=biffix%2Fruns%2Frun_01HGWDdone%2Fstar_salmon%2F&file=salmon.merged.gene_counts.tsv',
    );
    await link.click();
    await expect(page).toHaveURL(/\/data\/project\?project=genomics#path=/);
    await expect(page.getByRole('dialog')).toContainText('salmon.merged.gene_counts.tsv');
  });

  test('a running resume shows the results it reused before it has any of its own', async ({
    page,
  }) => {
    // batch-10 as a resume still running, with no outputs registered by itself yet. One route,
    // reading `resumed`, so the fixture's own routes (prefs, notifications) stay in place.
    let resumed = true;
    await onApi(page, 'GET', '/experiments/exp_liver_tumor', async (route) => {
      const response = await route.fetch();
      const detail = await response.json();
      detail.runs = detail.runs.map((r: Record<string, unknown>) =>
        r.run_id === 'run_01HGWDdone'
          ? { ...r, state: 'running', terminal: false, outputs_total: 0, resumed }
          : r,
      );
      await route.fulfill({ response, json: detail });
    });
    await page.goto('/experiments/exp_liver_tumor');
    const results = page.getByRole('region', { name: 'Results across analyses' });
    await results.getByRole('button', { name: 'By analysis' }).click();
    await expect(
      results
        .getByRole('region', { name: 'rnaseq · batch-10' })
        .getByRole('article', { name: 'Gene counts' }),
    ).toBeVisible();

    // The same run, not a resume, has none to show yet.
    resumed = false;
    await page.reload();
    await expect(results.getByRole('region', { name: 'rnaseq · batch-12' })).toBeVisible();
    await expect(results.getByRole('region', { name: 'rnaseq · batch-10' })).toHaveCount(0);
  });
});

test.describe('Analysis links (Expert view)', () => {
  test('the run header names the version it ran once, links it, and shows its id', async ({
    page,
  }) => {
    await page.goto(DONE);
    const version = page.getByTestId('run-version');
    await expect(version).toHaveText(
      /^lab-rnaseq · version c4d5e6f7a8:v1 · registered \d{4}-\d{2}-\d{2}$/,
    );
    await expect(version).toHaveAttribute('href', DONE_VERSION);
    // One link to the version (no second tab stop); the full id is text to copy.
    await expect(page.locator('.run-header .tags a[href*="/workflows/"]')).toHaveCount(1);
    await expect(page.getByTestId('run-version-id')).toHaveText('c4d5e6f7a8b9:v1');
  });

  test('the output prefix opens in the Data browser, copies, and opens in the S3 console', async ({
    page,
  }) => {
    await page.goto(DONE);
    const prefix = page.getByTestId('output-prefix');
    await expect(prefix.getByRole('link', { name: DONE_PREFIX })).toHaveAttribute(
      'href',
      '/data/project?project=genomics#path=biffix%2Fruns%2Frun_01HGWDdone%2F',
    );
    await expect(
      prefix.getByRole('button', { name: 'copy the URI of the output prefix' }),
    ).toBeVisible();
    await expect(
      prefix.getByRole('link', { name: /S3 console: the output prefix/ }),
    ).toHaveAttribute('href', /s3\.console\.aws\.amazon\.com/);
  });

  test('each output links into the Data browser beside its copy button', async ({ page }) => {
    await page.goto(WGS_OUTPUTS);
    const link = page.getByRole('link', { name: 'in Data browser: summary.tsv' });
    await expect(link).toHaveAttribute(
      'href',
      '/data/project?project=imported#path=biffix%2Fruns%2Frun_01HGW2wgs30x%2F&file=summary.tsv',
    );
    await expect(
      page.getByRole('row', { name: /summary\.tsv/ }).getByRole('button', { name: 'copy URI' }),
    ).toBeVisible();
    await link.click();
    await expect(page.getByRole('dialog')).toContainText('summary.tsv');
  });

  test('an output the Data browser cannot show is copied only', async ({ page }) => {
    // The run's outputs written to a bucket no data source of its project holds.
    await onApi(page, 'POST', `${WGS_OUTPUTS}/query`, async (route) => {
      const response = await route.fetch();
      const list = await response.json();
      list.outputs = list.outputs.map((o: { s3_uri: string }) => ({
        ...o,
        s3_uri: o.s3_uri.replace('s3://imported-project/', 's3://elsewhere/'),
      }));
      await route.fulfill({ response, json: list });
    });
    await page.goto(WGS_OUTPUTS);
    await expect(
      page.getByRole('row', { name: /summary\.tsv/ }).getByRole('button', { name: 'copy URI' }),
    ).toBeVisible();
    await expect(page.getByRole('link', { name: /in Data browser/ })).toHaveCount(0);
  });

  test('an output gone from S3 has no Data link', async ({ page }) => {
    await onApi(page, 'POST', `${WGS_OUTPUTS}/query`, async (route) => {
      const response = await route.fetch();
      const list = await response.json();
      list.outputs = list.outputs.map((o: { path: string }) =>
        o.path.endsWith('summary.tsv') ? { ...o, exists: false } : o,
      );
      await route.fulfill({ response, json: list });
    });
    await page.goto(WGS_OUTPUTS);
    const row = page.getByRole('row', { name: /summary\.tsv/ });
    await expect(row).toContainText('gone');
    await expect(row.getByRole('link', { name: /in Data browser/ })).toHaveCount(0);
    await expect(row.getByRole('button', { name: 'copy URI' })).toBeVisible();
  });
});
