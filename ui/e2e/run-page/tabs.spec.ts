import type { Page } from '@playwright/test';
import { API, CORS, test, expect, onApi, runTabs as tabs } from '../fixtures';

// The run page: a header that stays put, and one tab per view with its own URL.

test('a finished run opens on Overview under its header and tabs', async ({ page }) => {
  await page.goto('/runs/run_01HGW2wgs30x');
  await expect(page.getByRole('heading', { name: /run_01HGW2wgs30x/ })).toBeVisible();
  await expect(page.locator('.run-header .badge')).toHaveText('succeeded');
  await expect(tabs(page).getByRole('link', { name: 'Overview' })).toHaveAttribute(
    'aria-current',
    'page',
  );
  await expect(tabs(page).getByRole('link', { name: /^Jobs \d+$/ })).toBeVisible();
  await expect(tabs(page).getByRole('link', { name: 'Outputs 11', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Needs attention' })).toBeVisible();
  await expect(page.getByText('All 11 outputs →')).toBeVisible();
});

test('each tab has its own URL, and back/forward walk them', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align');
  for (const [name, path, heading] of [
    ['Jobs', '/jobs', null],
    ['Parameters', '/params', 'Parameters'],
    ['Cost', '/cost', /^Cost/],
    ['Coordinator', '/coordinator', 'Coordinator log'],
    ['Graph', '/graph', 'Graph'],
  ] as const) {
    await tabs(page)
      .getByRole('link', { name: new RegExp(`^${name}`) })
      .click();
    await expect(page).toHaveURL(new RegExp(`/runs/run_01HGW3align${path}$`));
    if (heading) await expect(page.getByRole('heading', { name: heading })).toBeVisible();
  }
  await page.goBack();
  await expect(page).toHaveURL(/\/coordinator$/);
  await page.goForward();
  await expect(page).toHaveURL(/\/graph$/);
});

test('job filters live in the URL', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  await expect(page.getByRole('button', { name: 'joint_call' })).toBeVisible();
  await page.getByRole('radio', { name: /^active/ }).click();
  await expect(page).toHaveURL(/jobs#state=active$/);
  await expect(page.getByRole('button', { name: 'fastqc:sample=HG00096' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'joint_call' })).toBeVisible();
  await page.getByLabel('Search task keys').fill('markdup');
  await expect(page).toHaveURL(/state=active&q=markdup/);
  await expect(page.getByRole('button', { name: 'joint_call' })).toHaveCount(0);
});

test('a job deep link opens its detail pane, and Esc closes it', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs#job=align%3Asample%3DHG00096');
  const pane = page.getByRole('complementary', { name: /Job align:sample=HG00096/ });
  await expect(pane).toBeVisible();
  await expect(pane.getByText('needs')).toBeVisible();
  await pane.getByRole('button', { name: 'markdup:sample=HG00096' }).click();
  await expect(page).toHaveURL(/job=markdup%3Asample%3DHG00096/);
  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/\/jobs$/);
  await expect(page.getByRole('complementary', { name: /^Job / })).toHaveCount(0);
});

test('a graph step opens its jobs', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/graph');
  await page
    .getByRole('button', { name: /^align/ })
    .first()
    .click();
  await expect(page).toHaveURL(/\/jobs#group=align$/);
  await expect(page.getByRole('button', { name: 'align:sample=HG00097' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'joint_call' })).toHaveCount(0);
});

test('a long step name fits its box, whole in its tooltip', async ({ page }) => {
  const long = 'NFCORE_RNASEQ:RNASEQ:QUANTIFY_PSEUDO_ALIGNMENT_WITH_SALMON';
  await onApi(page, 'GET', '/runs/run_01HGW3align', async (route) => {
    const response = await route.fetch();
    const run = await response.json();
    const rename = (id: string) => (id === 'align' ? long : id);
    const dag = {
      nodes: run.dag.nodes.map((n: { id: string }) => ({ ...n, id: rename(n.id) })),
      edges: run.dag.edges.map((e: string[]) => e.map(rename)),
    };
    await route.fulfill({ response, json: { ...run, dag } });
  });
  await page.goto('/runs/run_01HGW3align/graph');
  const node = page.getByRole('button', { name: new RegExp(`^${long}:`) });
  await expect(node.locator('text.label')).toHaveText('QUANTIFY_PSEUDO_ALI…');
  await expect(node.locator('title')).toHaveText(long);
});

test('a run that is not there is asked for once, not polled', async ({ page }) => {
  await page.clock.install();
  const asked: string[] = [];
  page.on('request', (r) => {
    if (r.url().startsWith(`${API}/runs/run_nope`)) asked.push(r.url());
  });
  await page.goto('/runs/run_nope');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
  await page.clock.runFor(20_000);
  expect(asked).toHaveLength(1);
});

test('a live run whose refresh fails says the page may be out of date', async ({ page }) => {
  await page.clock.install();
  await page.goto('/runs/run_01HGW3align');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
  let fail = true;
  await onApi(page, 'GET', '/runs/run_01HGW3align', (route) =>
    fail
      ? route.fulfill({ status: 503, headers: CORS, json: { error: 'unavailable' } })
      : route.fallback(),
  );
  await page.clock.runFor(5_000);
  const stale = page.getByText("Couldn't check for updates; this page may be out of date.");
  await expect(stale).toBeVisible();
  fail = false;
  await page.clock.runFor(5_000);
  await expect(stale).toHaveCount(0);
});

/** Launches a run of the test's own, straight through the mock API. */
const launchRun = (page: Page) =>
  page.request.post(`${API}/runs`, {
    headers: { authorization: 'Bearer mock' },
    data: { workflow_version_id: 'aabbccddeeff:v1', params: {} },
  });

test('a live run can be cancelled', async ({ page }) => {
  // A run of its own, launched for this attempt: a cancel sticks in the mock, so a shared run (or
  // this test's retry) would find nothing left to cancel.
  const launched = await launchRun(page);
  expect(launched.status()).toBe(202);
  const { run_id } = await launched.json();
  await page.goto(`/runs/${run_id}/params`);
  const badge = page.locator('.run-header .badge');
  await expect(badge).not.toHaveText('cancelled');
  page.on('dialog', (d) => d.accept());
  const cancelled = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === `/runs/${run_id}/cancel`,
  );
  await page.getByRole('button', { name: 'Cancel run' }).first().click();
  await cancelled;
  await expect(badge).toHaveText('cancelled', { timeout: 15_000 });
  await expect(page.getByRole('button', { name: 'Cancel run' })).toHaveCount(0);
});

test('a running run stays running after Cancel until its coordinator reports', async ({ page }) => {
  // The backend records a running run's cancel and ends it only once its coordinator's FAILED is
  // folded in, on a later read. A run of its own, answered as running until one poll after the
  // cancel was sent (the mock, which never runs a launch, ends it at once).
  const launched = await launchRun(page);
  const { run_id } = await launched.json();
  let cancelSent = false;
  let readsSince = 0;
  await onApi(page, 'GET', `/runs/${run_id}`, async (route) => {
    const response = await route.fetch();
    const run = await response.json();
    const stillRunning = !cancelSent || readsSince++ < 1;
    if (stillRunning) Object.assign(run, { state: 'running', terminal: false });
    await route.fulfill({ response, json: run });
  });
  await page.goto(`/runs/${run_id}/params`);
  const badge = page.locator('.run-header .badge');
  await expect(badge).toHaveText('running');
  page.on('dialog', (d) => d.accept());
  const cancelled = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === `/runs/${run_id}/cancel`,
  );
  await page.getByRole('button', { name: 'Cancel run' }).first().click();
  await cancelled;
  cancelSent = true;
  // Recorded, not ended: the run still reads as running until its coordinator reports.
  await expect(badge).toHaveText('running');
  await expect(badge).toHaveText('cancelled', { timeout: 15_000 });
  expect(readsSince).toBeGreaterThan(1);
});

test('an output deep link opens the file, and closing it leaves the URL', async ({ page }) => {
  // out6 is summary.tsv in the mock.
  await page.goto('/runs/run_01HGW2wgs30x/outputs#file=out6');
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('heading', { name: 'summary.tsv' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(page).toHaveURL(/\/outputs$/);
});

test('output view and filters live in the URL', async ({ page }) => {
  await page.goto('/runs/run_01HGW2wgs30x/outputs');
  await page.getByRole('radio', { name: 'flat' }).click();
  await expect(page).toHaveURL(/outputs\?view=flat$/);
  await page.getByLabel('Filter outputs').fill('metrics');
  await expect(page).toHaveURL(/outputs\?view=flat#q=metrics$/);
  await page.reload();
  await expect(page.getByLabel('Filter outputs')).toHaveValue('metrics');
  await expect(page.getByRole('button', { name: /umi_grouping\.metrics/ })).toBeVisible();
  await expect(page.getByRole('button', { name: 'summary.tsv' })).toHaveCount(0);
});

test('a finished run re-runs through the pre-filled launch form', async ({ page }) => {
  await page.goto('/runs/run_01HGW1failed');
  await page.getByRole('link', { name: 'Re-run' }).first().click();
  await expect(page).toHaveURL(/\/run\?from=run_01HGW1failed$/);
  await expect(page.getByRole('status')).toContainText('Pre-filled from run_01HGW1failed');
  // A re-run carries the run's submit-time image too, as given at the digest it ran.
  await expect(page.getByRole('status')).toContainText('ghcr.io/lab/bwa-mem3:dev@sha256:bwam3dev');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  await expect(page.locator('.run-header .badge')).toHaveText('preparing images');
  await expect(page.getByTestId('submit-images')).toContainText(
    'ghcr.io/lab/bwa-mem3:dev@sha256:bwam3dev',
  );
});

test('a run launched without images re-runs straight to submitted', async ({ page }) => {
  await page.goto('/runs/run_01HGW4resume');
  await page.getByRole('link', { name: 'Re-run' }).first().click();
  await expect(page.getByRole('status')).toContainText('Pre-filled from run_01HGW4resume');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  await expect(page.locator('.run-header .badge')).toHaveText('submitted');
});

test("a finished run's provenance downloads as an RO-Crate", async ({ page }) => {
  await page.goto('/runs/run_01HGW1failed');
  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Provenance' }).click();
  const file = await download;
  expect(file.suggestedFilename()).toBe('run_01HGW1failed-ro-crate-metadata.json');
  const body = JSON.parse(
    await (await file.createReadStream()).toArray().then((c) => Buffer.concat(c).toString()),
  );
  expect(body['@graph'][0]['@id']).toBe('ro-crate-metadata.json');
});

test('a finished Snakemake run resumes through the launch form', async ({ page }) => {
  await page.goto('/runs/run_01HGW1failed');
  await page.getByRole('link', { name: 'Resume' }).first().click();
  await expect(page).toHaveURL(/\/run\?from=run_01HGW1failed&resume=1$/);
  await expect(page.getByRole('status')).toContainText('Resuming run_01HGW1failed');
  await expect(page.getByRole('status')).toContainText('ghcr.io/lab/bwa-mem3:dev@sha256:bwam3dev');
  await page.getByRole('button', { name: 'Resume run →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  // It resubmits the run's pinned submit-time image, so it waits for that image first.
  await expect(page.locator('.run-header .badge')).toHaveText('preparing images');
  await expect(page.getByTestId('preparing')).toContainText(
    'image `ghcr.io/lab/bwa-mem3:dev@sha256:bwam3dev` is being imported',
  );
});

test('a finished Nextflow run resumes through the launch form', async ({ page }) => {
  await page.goto('/runs/run_01HGW4resume');
  await page.getByRole('link', { name: 'Resume' }).first().click();
  await expect(page).toHaveURL(/\/run\?from=run_01HGW4resume&resume=1$/);
  await expect(page.getByRole('status')).toContainText('Resuming run_01HGW4resume');
  await page.getByRole('button', { name: 'Resume run →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  await expect(page.getByRole('link', { name: 'run_01HGW4resume' })).toBeVisible();
});

test('j and k move the selected job', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  await expect(page.getByRole('button', { name: 'joint_call' })).toBeVisible();
  await page.keyboard.press('j');
  await expect(page).toHaveURL(/job=align%3Asample%3DHG00096/);
  await page.keyboard.press('j');
  await expect(page).toHaveURL(/job=align%3Asample%3DHG00097/);
  await page.keyboard.press('k');
  await expect(page).toHaveURL(/job=align%3Asample%3DHG00096/);
  await expect(page.getByRole('complementary', { name: /Job align:sample=HG00096/ })).toBeVisible();
  // Typing in the search box does not move the selection.
  await page.getByLabel('Search task keys').press('j');
  await expect(page).toHaveURL(/job=align%3Asample%3DHG00096/);
});

test('a running Nextflow run counts the tasks known so far, not a filled bar', async ({ page }) => {
  await page.goto('/runs/run_01HGW8nflive');
  const progress = page.locator('.progress').first();
  await expect(progress.locator('.indeterminate')).toBeVisible();
  await expect(progress).toContainText('2 of 5+ tasks');
  await expect(progress.locator('.fill')).toHaveCount(0);
});

test('the overview shows what the run ran with', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align');
  const ranWith = page.locator('section.ran-with');
  await expect(ranWith.getByRole('heading', { name: 'Ran with' })).toBeVisible();
  await expect(ranWith.getByText('aabbccddeeff', { exact: true })).toBeVisible();
  await expect(ranWith.getByText('snakemake 9.27.0 · 4dbb7d79f6bc')).toBeVisible();
  await expect(ranWith.getByText('derived from sha256:1mp0rted')).toBeVisible();
  await ranWith.getByText('Image map (2 images)').click();
  const row = ranWith.getByRole('row', { name: /seqkit/ });
  await expect(row).toContainText('workflows@sha256:s3qk1t');
  await expect(
    ranWith.getByTestId('submit-images').getByRole('row', { name: /umi-tools/ }),
  ).toContainText('ghcr.io/lab/umi-tools:dev@sha256:um170015');
});

test('a run waiting for its submit-time images says why, and what it will run with', async ({
  page,
}) => {
  await page.goto('/runs/run_01HGW7preparing');
  await expect(page.locator('.run-header .badge')).toHaveText('preparing images');
  const panel = page.getByTestId('preparing');
  await expect(panel.getByRole('heading', { name: 'Preparing images' })).toBeVisible();
  await expect(panel).toContainText('image `ghcr.io/lab/bwa-mem3:feature-x` is being imported');
  await expect(panel).toContainText('adding the Snakemake 9.27.0 runtime');
  await expect(panel.getByText('importing…')).toBeVisible();
  await expect(panel.getByText('adding runtime…')).toBeVisible();
  await expect(panel.getByRole('link', { name: 'Build log ↗' })).toBeVisible();
  const submitImages = page.getByTestId('submit-images');
  await expect(submitImages).toContainText('ghcr.io/lab/bwa-mem3:feature-x@sha256:f3a7ure');
  await expect(page.getByRole('heading', { name: 'Will run with' })).toBeVisible();
});
