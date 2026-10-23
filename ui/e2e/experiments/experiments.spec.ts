import type { Page } from '@playwright/test';
import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  useTheme,
  THEMES,
  CORS,
  onApi,
  startFromExample,
} from '../fixtures';

// Experiments: named groups of a project's analyses — the list, an experiment's
// page, launching into one from the wizard, adding an analysis from its page, and the analyses
// list's filter. The mock's Liver tumor study holds Priya's batch-10 and batch-12. A test that
// changes an experiment does it in a `page.route` of its own, or on one it made under a name of its
// own, so parallel tests never see each other's changes.

const LIVER = '/experiments/exp_liver_tumor';
const OOM = '/runs/run_01HGWEoom';

const nav = (page: Page) => page.getByRole('navigation').first();

/** Fill the rnaseq Run tab from its human example (which keeps the form's experiment). */
async function fillFromExample(page: Page): Promise<void> {
  await startFromExample(page);
  await expect(page.getByLabel('Run name')).toHaveValue('rnaseq example (human)');
}
/** Launch the Run tab: the `POST /runs` body it sent, once the run page it lands on is open. */
async function launch(page: Page): Promise<Record<string, unknown>> {
  const launched = page.waitForRequest((r) => r.url().endsWith('/runs') && r.method() === 'POST');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  const body = (await launched).postDataJSON() as Record<string, unknown>;
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  return body;
}

test.describe('Experiments (Lab view)', () => {
  test.use({ view: 'lab' });

  test('the list: a card per open experiment, by project, archived ones on asking', async ({
    page,
  }) => {
    await page.goto('/');
    await nav(page).getByRole('link', { name: 'Experiments' }).click();
    await expect(page).toHaveURL(/\/experiments$/);
    await expect(page.getByRole('heading', { level: 1, name: 'Experiments' })).toBeVisible();
    const liver = page.getByRole('article', { name: 'Liver tumor study' });
    await expect(liver).toContainText('Tumor vs adjacent normal liver');
    await expect(liver).toContainText('genomics');
    await expect(liver).toContainText('2 analyses · last started');
    await expect(page.getByRole('article', { name: 'UMI consensus tuning' })).toContainText(
      '1 analysis',
    );
    await expect(page.getByRole('article', { name: 'Pilot 2025' })).toHaveCount(0);

    await page.getByLabel('Show archived').check();
    await expect(page).toHaveURL(/archived=true/);
    await expect(page.getByRole('article', { name: 'Pilot 2025' })).toContainText('archived');

    await page.getByLabel('Project').selectOption('imported');
    await expect(page).toHaveURL(/project=imported/);
    await expect(page.getByRole('article', { name: 'UMI consensus tuning' })).toBeVisible();
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toHaveCount(0);

    await page.getByRole('article', { name: 'UMI consensus tuning' }).getByRole('link').click();
    await expect(page).toHaveURL(/\/experiments\/exp_umi_tuning$/);
  });

  test('none yet: the empty state says what an experiment is for', async ({ page }) => {
    await onApi(page, 'GET', '/experiments', (route) =>
      route.fulfill({ headers: CORS, json: { experiments: [] } }),
    );
    await page.goto('/experiments');
    await expect(page.getByTestId('experiments-empty')).toContainText(
      'Group analyses under a name to read their results together.',
    );
    await expect(
      page.getByTestId('experiments-empty').getByRole('button', { name: 'New experiment' }),
    ).toBeVisible();
  });

  test('New experiment checks the name, shows the server’s refusal, and opens the new one', async ({
    page,
  }, info) => {
    await page.goto('/experiments');
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toBeVisible();
    await page.getByRole('button', { name: 'New experiment' }).click();
    const form = page.getByRole('form', { name: 'New experiment' });
    await form.getByLabel('Project').selectOption('genomics');
    // A name taken in the project is caught as it is typed (ignoring case and spaces).
    await form.getByLabel('Name').fill(' liver TUMOR study');
    await expect(form).toContainText(
      '“Liver tumor study” is already an experiment in project genomics.',
    );
    await expect(form.getByLabel('Name')).toHaveAttribute('aria-invalid', 'true');

    // One taken meanwhile: the server's 409, in its words.
    const name = `Kidney study ${info.workerIndex}-${info.retry}-${Date.now()}`;
    await form.getByLabel('Name').fill(name);
    await expect(form.getByLabel('Name')).not.toHaveAttribute('aria-invalid', 'true');
    let refuse = true;
    await onApi(page, 'POST', '/experiments', async (route) => {
      if (!refuse) return route.fallback();
      refuse = false;
      await route.fulfill({
        status: 409,
        headers: { ...CORS, 'content-type': 'text/plain' },
        body: `project genomics already has an experiment named \`${name}\``,
      });
    });
    await form.getByRole('button', { name: 'Create experiment' }).click();
    await expect(form.getByRole('alert')).toHaveText(
      `project genomics already has an experiment named \`${name}\``,
    );

    // Created for real: its page opens, empty.
    await form.getByLabel('Description').fill('Mouse kidney, two time points.');
    const created = page.waitForResponse(
      (r) => r.request().method() === 'POST' && new URL(r.url()).pathname === '/experiments',
    );
    await form.getByRole('button', { name: 'Create experiment' }).click();
    const body = (await created).request().postDataJSON();
    expect(body).toEqual({
      project: 'genomics',
      name,
      description: 'Mouse kidney, two time points.',
    });
    await expect(page).toHaveURL(/\/experiments\/exp_mock\d+$/);
    await expect(page.getByRole('heading', { level: 1, name })).toBeVisible();
    await expect(page.getByText('No analyses in it yet.')).toBeVisible();
  });

  test('an experiment’s page: its analyses, results across them, samples and one total', async ({
    page,
  }) => {
    await page.goto(LIVER);
    await expect(page.getByRole('heading', { level: 1, name: 'Liver tumor study' })).toBeVisible();
    await expect(page.getByText('Created')).toContainText('by priya@lab.example.org');
    await expect(page.getByText('Created')).toContainText('project genomics');
    // batch-12 has jobs not priced yet, so the total can still grow.
    await expect(page.getByTestId('experiment-cost')).toHaveText('$13.80 · estimate');

    const analyses = page.getByRole('region', { name: 'Analyses' });
    await expect(analyses.getByRole('listitem')).toHaveCount(2);
    await expect(analyses.getByRole('listitem').first()).toContainText('rnaseq · batch-12');
    await expect(analyses.getByRole('listitem').first()).toContainText('Needs attention');
    await expect(analyses.getByRole('listitem').last()).toContainText('Finished');

    // Each analysis's key-result cards under its name (two analyses start as a table).
    const results = page.getByRole('region', { name: 'Results across analyses' });
    await results.getByRole('button', { name: 'By analysis' }).click();
    const batch10 = results.getByRole('region', { name: 'rnaseq · batch-10' });
    await expect(batch10.getByRole('article', { name: 'Gene counts' })).toBeVisible();
    await expect(
      batch10.getByRole('article', { name: 'Gene counts' }).getByRole('cell', { name: 'Alb' }),
    ).toBeVisible();
    await expect(results.getByRole('region', { name: 'rnaseq · batch-12' })).toContainText(
      'No key results to show',
    );

    // Sample × analysis, oldest analysis first.
    const samples = page.getByRole('region', { name: /^Samples/ });
    await expect(samples.getByRole('heading')).toContainText('4 samples across 2 analyses');
    await expect(samples.getByRole('columnheader')).toHaveText([
      'Sample',
      'rnaseq · batch-10',
      'rnaseq · batch-12',
    ]);
    const normal2 = samples.getByRole('row', { name: /Normal_2/ });
    await expect(normal2.getByRole('cell')).toHaveText(['done', 'failed']);
    const tumor1 = samples.getByRole('row', { name: /Tumor_1/ });
    await expect(tumor1.getByRole('cell').last()).toHaveText('–not in this analysis');

    // Its runs in the analyses list.
    await analyses.getByRole('link', { name: /See them in Analyses/ }).click();
    await expect(page).toHaveURL(/\/runs\?experiment=exp_liver_tumor$/);
  });

  for (const running of [true, false])
    test(`${running ? 'while an analysis runs' : 'once every analysis ended and was priced'}, the total ${running ? 'is' : 'is not'} an estimate`, async ({
      page,
    }) => {
      await onApi(page, 'GET', LIVER, async (route) => {
        const response = await route.fetch();
        const detail = await response.json();
        // Every job priced and every analysis ended (other tests launch into this experiment):
        // only a running analysis can make the total an estimate.
        detail.runs = detail.runs.map((r: Record<string, unknown>) => ({
          ...r,
          ...(r.terminal ? {} : { state: 'cancelled', terminal: true }),
          cost_pending: false,
        }));
        if (running) detail.runs[0] = { ...detail.runs[0], state: 'running', terminal: false };
        await route.fulfill({ response, json: detail });
      });
      await page.goto(LIVER);
      await expect(page.getByTestId('experiment-cost')).toHaveText(
        running ? '$13.80 · estimate' : '$13.80',
      );
    });

  test('Download samples (CSV) saves one row per sample per analysis', async ({ page }) => {
    await page.goto(LIVER);
    const download = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Download samples (CSV)' }).click();
    const file = await download;
    expect(file.suggestedFilename()).toBe('Liver tumor study-samples.csv');
    const text = await (
      await file.createReadStream()
    )
      .toArray()
      .then((c) => Buffer.concat(c).toString('utf8'));
    // UTF-8 with a byte-order mark (for Excel), CRLF rows, a header first.
    expect(
      text.startsWith('\ufeffSample,Sample status,Reused,Analysis,Run ID,Analysis status,'),
    ).toBe(true);
    const rows = text.slice(1).trimEnd().split('\r\n').slice(1);
    // Normal_2 is in both analyses, so it has a row in each, the older analysis first.
    const normal2 = rows.filter((r) => r.startsWith('Normal_2,'));
    expect(normal2).toHaveLength(2);
    expect(normal2[0]).toMatch(/^Normal_2,Done,no,rnaseq · batch-10,run_01HGWDdone,Succeeded,/);
    expect(normal2[1]).toMatch(/^Normal_2,Failed,no,rnaseq · batch-12,run_01HGWFbadin,Failed,/);
    expect(rows[0]).toContain(',run_01HGWDdone,');
  });

  test('Download samples (CSV) is disabled, saying why, when there are no samples', async ({
    page,
  }) => {
    await onApi(page, 'GET', LIVER, async (route) => {
      const response = await route.fetch();
      const detail = await response.json();
      await route.fulfill({ response, json: { ...detail, samples: [] } });
    });
    let asked = false;
    await onApi(page, 'GET', `${LIVER}/samples.csv`, (route) => {
      asked = true;
      return route.fallback();
    });
    await page.goto(LIVER);
    const button = page.getByRole('button', { name: 'Download samples (CSV)' });
    // aria-disabled, not disabled: still reachable by keyboard, so the reason can be heard.
    await expect(button).toHaveAttribute('aria-disabled', 'true');
    await expect(button).toHaveAccessibleDescription(
      'No samples yet: none of its analyses has named a sample.',
    );
    await button.focus();
    await expect(button).toBeFocused();
    // Pressed from the keyboard and clicked (forced: Playwright waits for an aria-disabled button
    // to be enabled), it starts no download: no request for the CSV follows within a second.
    const csv = page.waitForRequest((r) => r.url().includes('/samples.csv'), { timeout: 1_000 });
    await page.keyboard.press('Enter');
    await button.click({ force: true });
    await expect(csv).rejects.toThrow();
    await expect(button).toHaveText('Download samples (CSV)');
    expect(asked).toBe(false);
  });

  test('Download samples (CSV) says what it holds, and that it is being prepared', async ({
    page,
  }) => {
    let release = () => {};
    const held = new Promise<void>((ok) => (release = ok));
    await onApi(page, 'GET', `${LIVER}/samples.csv`, async (route) => {
      await held;
      await route.fallback();
    });
    await page.goto(LIVER);
    // By its description, as its name changes while it prepares.
    const button = page.locator('button[aria-describedby="samples-csv-about"]');
    await expect(button).toHaveAccessibleName('Download samples (CSV)');
    await expect(button).toHaveAttribute('aria-disabled', 'false');
    await expect(button).toHaveAccessibleDescription(
      'One row per sample per analysis, for every analysis in it; analyses that name no samples yet are left out.',
    );
    const download = page.waitForEvent('download');
    await button.click();
    await expect(
      page.getByRole('status').filter({ hasText: 'Preparing the samples file…' }),
    ).toBeAttached();
    await expect(button).toHaveAttribute('aria-busy', 'true');
    await expect(button).toHaveAccessibleName('Preparing…');
    release();
    await download;
    await expect(button).toHaveAttribute('aria-busy', 'false');
  });

  test("a samples download too large for one file shows the server's reason", async ({ page }) => {
    const reason =
      'This experiment has 1001 runs (analyses), more than the 1000 one samples download can cover. Split the experiment into smaller ones (move some of its runs into another experiment) and download the samples of each.';
    await onApi(page, 'GET', `${LIVER}/samples.csv`, (route) =>
      route.fulfill({ status: 413, headers: CORS, contentType: 'text/plain', body: reason }),
    );
    await page.goto(LIVER);
    await page.getByRole('button', { name: 'Download samples (CSV)' }).click();
    await expect(page.getByRole('alert')).toHaveText(`Could not download the samples: ${reason}`);
  });

  test('a samples download the server refuses says so', async ({ page }) => {
    await onApi(page, 'GET', `${LIVER}/samples.csv`, (route) =>
      route.fulfill({ status: 500, headers: CORS, contentType: 'text/plain', body: 'store down' }),
    );
    await page.goto(LIVER);
    await page.getByRole('button', { name: 'Download samples (CSV)' }).click();
    await expect(page.getByRole('alert')).toContainText('Could not download the samples');
    await expect(page.getByRole('alert')).toContainText('store down');
  });

  test('the name and description are edited in place; archive and restore', async ({ page }) => {
    let stored: Record<string, unknown> | null = null;
    const puts: unknown[] = [];
    await onApi(page, 'PUT', LIVER, async (route) => {
      const update = route.request().postDataJSON();
      puts.push(update);
      const base = stored ?? (await (await route.fetch({ method: 'GET' })).json()).experiment;
      const next: Record<string, unknown> = { ...base };
      if (update.name) next.name = update.name;
      if ('description' in update) {
        if (update.description) next.description = update.description;
        else delete next.description;
      }
      if (update.archived === true) next.archived_at = 1_790_000_000;
      if (update.archived === false) delete next.archived_at;
      stored = next;
      await route.fulfill({ headers: CORS, json: next });
    });
    await page.goto(LIVER);
    await page.getByRole('button', { name: 'Edit name and description' }).click();
    const form = page.getByRole('form', { name: 'Edit experiment' });
    await form.getByLabel('Name').fill('');
    await expect(form).toContainText('Give the experiment a name.');
    await form.getByLabel('Name').fill('Liver tumour study');
    await form.getByLabel('Description').fill('');
    await form.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByRole('heading', { level: 1, name: 'Liver tumour study' })).toBeVisible();
    expect(puts[0]).toEqual({ name: 'Liver tumour study', description: null });
    await expect(page.getByText('Tumor vs adjacent normal liver')).toHaveCount(0);

    await page.getByRole('button', { name: 'Archive' }).click();
    await expect(page.getByRole('status').filter({ hasText: 'Archived' })).toBeVisible();
    await expect(
      page.getByRole('link', { name: /Start an analysis in this experiment/ }),
    ).toHaveCount(0);
    await page.getByRole('button', { name: 'Restore' }).click();
    await expect(
      page.getByRole('link', { name: /Start an analysis in this experiment/ }),
    ).toBeVisible();
    expect(puts.slice(1)).toEqual([{ archived: true }, { archived: false }]);
  });

  test('a change the server refuses shows its message', async ({ page }) => {
    await onApi(page, 'PUT', LIVER, (route) =>
      route.fulfill({
        status: 403,
        headers: { ...CORS, 'content-type': 'text/plain' },
        body: 'changing an experiment needs the Operator role',
      }),
    );
    await page.goto(LIVER);
    await page.getByRole('button', { name: 'Archive' }).click();
    await expect(page.getByRole('alert')).toHaveText(
      'changing an experiment needs the Operator role',
    );
    await expect(page.getByRole('button', { name: 'Archive' })).toBeVisible();
  });

  test('Start an analysis in this experiment: the wizard’s Review has it chosen, and the launch sends it', async ({
    page,
  }) => {
    await page.goto(LIVER);
    await page.getByRole('link', { name: 'Start an analysis in this experiment →' }).click();
    await expect(page).toHaveURL(/\/runs\/new\?experiment=exp_liver_tumor$/);
    await page
      .getByRole('article', { name: 'Bulk RNA-seq' })
      .getByRole('link', { name: 'Start Bulk RNA-seq' })
      .click();
    await expect(page).toHaveURL(
      /\/workflows\/aabbccddeeff%3Av1\/run\?experiment=exp_liver_tumor$/,
    );
    await startFromExample(page);
    await page.getByRole('button', { name: /^Next: / }).click();
    await page.getByRole('button', { name: /^Next: / }).click();
    await expect(page.getByRole('heading', { name: 'Review & launch', exact: true })).toBeVisible();
    const select = page.getByLabel('Experiment', { exact: true });
    await expect(select).toHaveValue('exp_liver_tumor');
    await expect(select.locator('option:checked')).toHaveText('Liver tumor study');

    let body: Record<string, unknown> | null = null;
    await onApi(page, 'POST', '/runs', async (route) => {
      body = route.request().postDataJSON();
      await route.fulfill({
        status: 202,
        headers: CORS,
        json: { run_id: 'run_01HGWDdone', state: 'submitted' },
      });
    });
    await page.getByRole('button', { name: 'Launch analysis →' }).click();
    await expect(page).toHaveURL(/\/runs\/run_01HGWDdone$/);
    expect(body).toMatchObject({
      workflow_version_id: 'aabbccddeeff:v1',
      experiment: 'exp_liver_tumor',
    });
  });

  test('the wizard’s Review makes a new experiment inline and keeps the choice in the draft', async ({
    page,
  }) => {
    await page.goto('/workflows/aabbccddeeff%3Av1/run');
    await startFromExample(page);
    await page.getByRole('button', { name: /^Next: / }).click();
    await page.getByRole('button', { name: /^Next: / }).click();
    const select = page.getByLabel('Experiment', { exact: true });
    await expect(select).toHaveValue('');
    const made = {
      id: 'exp_inline_1',
      project: 'genomics',
      name: 'Inline study',
      created_at: 1_790_000_000,
      run_count: 0,
    };
    let created: unknown = null;
    await onApi(page, 'POST', '/experiments', async (route) => {
      created = route.request().postDataJSON();
      await route.fulfill({ status: 201, headers: CORS, json: made });
    });
    // The project's experiments as the page sees them, the new one too once made.
    let list = false;
    await onApi(page, 'GET', '/experiments', async (route) => {
      const response = await route.fetch();
      const json = await response.json();
      if (list) json.experiments.unshift(made);
      await route.fulfill({ response, json });
    });
    await select.selectOption({ label: 'New experiment…' });
    const form = page.getByRole('form', { name: 'New experiment' });
    await form.getByLabel('Name').fill('Inline study');
    list = true;
    await form.getByRole('button', { name: 'Create experiment' }).click();
    await expect(select).toHaveValue('exp_inline_1');
    expect(created).toEqual({ project: 'genomics', name: 'Inline study' });
    // A reload restores the draft with the choice.
    await page.reload();
    await expect(page.getByLabel('Experiment', { exact: true })).toHaveValue('exp_inline_1');
  });

  test('the analysis page adds the analysis to an experiment, moves it and takes it out', async ({
    page,
  }) => {
    let current: string | null = null;
    const sets: unknown[] = [];
    await onApi(page, 'POST', `${OOM}/experiment`, async (route) => {
      const body = route.request().postDataJSON();
      sets.push(body);
      current = body.experiment_id;
      await route.fulfill({ status: 204, headers: CORS });
    });
    await onApi(page, 'GET', OOM, async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      if (current) run.experiment_id = current;
      await route.fulfill({ response, json: run });
    });
    const second = {
      id: 'exp_second',
      project: 'genomics',
      name: 'Second study',
      created_at: 1_790_000_000,
      run_count: 0,
    };
    await onApi(page, 'GET', '/experiments', async (route) => {
      const response = await route.fetch();
      const json = await response.json();
      json.experiments.push(second);
      await route.fulfill({ response, json });
    });
    await page.goto(OOM);
    await page.getByRole('button', { name: 'Add to experiment' }).click();
    const panel = page.getByRole('region', { name: 'Experiment' });
    await panel.getByLabel('Add it to').selectOption({ label: 'Liver tumor study' });
    await panel.getByRole('button', { name: 'Add', exact: true }).click();
    await expect(panel).toHaveCount(0);
    const inLink = page.locator('.sub').getByRole('link', { name: 'Liver tumor study' });
    await expect(inLink).toHaveAttribute('href', '/experiments/exp_liver_tumor');

    await page.getByRole('button', { name: 'Experiment', exact: true }).click();
    await expect(panel).toContainText('In Liver tumor study');
    await panel.getByLabel('Move it to').selectOption({ label: 'Second study' });
    await panel.getByRole('button', { name: 'Move', exact: true }).click();
    await expect(page.locator('.sub').getByRole('link', { name: 'Second study' })).toBeVisible();

    await page.getByRole('button', { name: 'Experiment', exact: true }).click();
    await panel.getByRole('button', { name: 'Remove from Second study' }).click();
    await expect(page.getByRole('button', { name: 'Add to experiment' })).toBeVisible();
    await expect(page.locator('.sub').getByRole('link', { name: 'Second study' })).toHaveCount(0);
    expect(sets).toEqual([
      { experiment_id: 'exp_liver_tumor' },
      { experiment_id: 'exp_second' },
      { experiment_id: null },
    ]);
  });

  test('adding to an experiment the server refuses shows its message', async ({ page }) => {
    await onApi(page, 'POST', `${OOM}/experiment`, (route) =>
      route.fulfill({
        status: 403,
        headers: { ...CORS, 'content-type': 'text/plain' },
        body: 'adding a run to an experiment needs the Operator role',
      }),
    );
    await page.goto(OOM);
    await page.getByRole('button', { name: 'Add to experiment' }).click();
    const panel = page.getByRole('region', { name: 'Experiment' });
    await panel.getByLabel('Add it to').selectOption({ label: 'Liver tumor study' });
    await panel.getByRole('button', { name: 'Add', exact: true }).click();
    await expect(panel.getByRole('alert')).toHaveText(
      'adding a run to an experiment needs the Operator role',
    );
  });

  test('the analyses list filters to an experiment under a chip that names it', async ({
    page,
  }) => {
    await page.goto('/runs?experiment=exp_liver_tumor');
    const chip = page.getByTestId('experiment-chip');
    await expect(chip).toContainText('In experiment Liver tumor study');
    // Everyone's analyses in it (Priya started them), with who started each.
    await expect(page.locator('tbody tr')).toHaveCount(2);
    await expect(page.getByRole('columnheader', { name: 'Started by' })).toBeVisible();
    await expect(page.getByRole('group', { name: 'Whose analyses' })).toHaveCount(0);
    await chip
      .getByRole('button', { name: 'Remove the filter: experiment Liver tumor study' })
      .click();
    await expect(page).toHaveURL(/\/runs$/);
    await expect(chip).toHaveCount(0);
    await expect(page.getByRole('group', { name: 'Whose analyses' })).toBeVisible();
  });

  test('New experiment opened before the projects load picks one once they do', async ({
    page,
  }) => {
    // Hold the pipelines and the experiments (the two sources of projects) until the form is open.
    let release!: () => void;
    const held = new Promise<void>((r) => (release = r));
    for (const path of ['/workflows', '/experiments'])
      await onApi(page, 'GET', path, async (route) => {
        await held;
        await route.fallback();
      });
    await page.goto('/experiments');
    await page.getByRole('button', { name: 'New experiment' }).first().click();
    const form = page.getByRole('form', { name: 'New experiment' });
    await expect(form).toContainText('There is no project to put it in yet');
    release();
    // The first project, once they are offered, is the one chosen; the form is usable.
    await expect(form.getByLabel('Project')).toHaveValue('genomics');
    await expect(form).not.toContainText('There is no project to put it in yet');
    let body: unknown = null;
    await onApi(page, 'POST', '/experiments', async (route) => {
      body = route.request().postDataJSON();
      await route.fulfill({
        status: 201,
        headers: CORS,
        json: { id: 'exp_late', project: 'genomics', name: 'Late', created_at: 1, run_count: 0 },
      });
    });
    await form.getByLabel('Name').fill('Late');
    await form.getByRole('button', { name: 'Create experiment' }).click();
    await expect(page).toHaveURL(/\/experiments\/exp_late$/);
    expect(body).toEqual({ project: 'genomics', name: 'Late' });
  });

  test('a capped experiment says how many analyses it holds, with the rest a link away', async ({
    page,
  }) => {
    await onApi(page, 'GET', LIVER, async (route) => {
      const response = await route.fetch();
      const detail = await response.json();
      await route.fulfill({ response, json: { ...detail, runs_total: 250 } });
    });
    await page.goto(LIVER);
    const capped = page.getByTestId('runs-capped');
    await expect(capped).toContainText('Showing the 2 newest of 250 analyses.');
    await expect(capped.getByRole('link', { name: 'See them all in Analyses →' })).toHaveAttribute(
      'href',
      '/runs?experiment=exp_liver_tumor',
    );
    await expect(page.locator('.stats').getByText('250')).toBeVisible();
  });

  test('Start an analysis in an experiment offers its project’s pipelines only, and says so', async ({
    page,
  }) => {
    await page.goto('/runs/new?experiment=exp_umi_tuning');
    const line = page.getByTestId('start-in-experiment');
    await expect(line).toContainText('In the experiment UMI consensus tuning');
    await expect(line).toContainText('imported');
    await expect(page.getByRole('article', { name: 'umi-consensus' })).toBeVisible();
    await expect(page.getByRole('article', { name: 'Bulk RNA-seq' })).toHaveCount(0);
  });

  test('Try again keeps the analysis in its experiment while that is open', async ({ page }) => {
    await onApi(page, 'GET', OOM, async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      await route.fulfill({ response, json: { ...run, experiment_id: 'exp_liver_tumor' } });
    });
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
    await page
      .getByRole('region', { name: 'What went wrong' })
      .getByRole('button', { name: 'Try again' })
      .click();
    await page
      .getByRole('dialog', { name: 'Resume this analysis?' })
      .getByRole('button', { name: 'Resume' })
      .click();
    await expect(page).toHaveURL(/\/runs\/run_01HGW2wgs30x$/);
    expect(body).toMatchObject({ resume_from: 'run_01HGWEoom', experiment: 'exp_liver_tumor' });
  });

  test('Try again with an archived experiment opens the launch form instead of a refused launch', async ({
    page,
  }) => {
    await onApi(page, 'GET', OOM, async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      await route.fulfill({ response, json: { ...run, experiment_id: 'exp_pilot_2025' } });
    });
    const posts: unknown[] = [];
    // Answered here, never by the shared mock: a launch that slipped through would resume (and so
    // dismiss) the shared run for every later test.
    await onApi(page, 'POST', '/runs', async (route) => {
      posts.push(route.request().postDataJSON());
      await route.fulfill({ status: 500, headers: CORS, contentType: 'text/plain', body: 'no' });
    });
    await page.goto(OOM);
    await page
      .getByRole('region', { name: 'What went wrong' })
      .getByRole('button', { name: 'Try again' })
      .click();
    await page
      .getByRole('dialog', { name: 'Resume this analysis?' })
      .getByRole('button', { name: 'Resume' })
      .click();
    await expect(page).toHaveURL(
      /\/workflows\/c4d5e6f7a8b9%3Av1\/run\?from=run_01HGWEoom&resume=1$/,
    );
    expect(posts).toEqual([]);
  });

  for (const theme of THEMES) {
    test(`the list and an experiment's page have no serious a11y violations (${theme})`, async ({
      page,
    }) => {
      await useTheme(page, theme);
      await page.goto('/experiments');
      await expect(page.getByRole('article', { name: 'Liver tumor study' })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await page.goto(LIVER);
      await expect(
        page.getByRole('table', { name: 'Key results of each analysis' }).getByRole('row').nth(2),
      ).toBeVisible();
      await expect(page.getByRole('region', { name: /^Samples/ })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }

  test('at 390px neither page scrolls sideways', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    for (const path of ['/experiments', LIVER]) {
      await page.goto(path);
      await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
      await expect(page.getByText('Loading…')).toHaveCount(0);
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
        ),
      ).toBeLessThanOrEqual(0);
    }
  });
});

test.describe('Experiments (Expert view)', () => {
  test('the same page in Expert words, with run ids', async ({ page }) => {
    await page.goto('/');
    await expect(nav(page).getByRole('link', { name: 'Experiments' })).toBeVisible();
    await page.goto(LIVER);
    await expect(page.getByRole('heading', { level: 1, name: 'Liver tumor study' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Runs', exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Results across runs' })).toBeVisible();
    await expect(page.getByText('run_01HGWDdone').first()).toBeVisible();
    await expect(page.getByText('exp_liver_tumor')).toBeVisible();
    await expect(page.getByRole('link', { name: 'New run in this experiment →' })).toBeVisible();
    await expect(page.getByRole('region', { name: /^Samples/ }).getByRole('heading')).toContainText(
      '4 samples across 2 runs',
    );
  });

  test('none yet: the empty state in Expert words', async ({ page }) => {
    await onApi(page, 'GET', '/experiments', (route) =>
      route.fulfill({ headers: CORS, json: { experiments: [] } }),
    );
    await page.goto('/experiments');
    const empty = page.getByTestId('experiments-empty');
    await expect(empty).toContainText('Group runs under a name to read their results together.');
    await expect(empty).toContainText('Anyone who can start runs can make one.');
    await expect(empty).not.toContainText('analyses');
  });

  test('the run page’s header adds the run to an experiment, and names the one it is in', async ({
    page,
  }) => {
    let current: string | null = null;
    const sets: unknown[] = [];
    await onApi(page, 'POST', `${OOM}/experiment`, async (route) => {
      const body = route.request().postDataJSON();
      sets.push(body);
      current = body.experiment_id;
      await route.fulfill({ headers: CORS, json: {} });
    });
    await onApi(page, 'GET', OOM, async (route) => {
      const response = await route.fetch();
      const run = await response.json();
      if (current) run.experiment_id = current;
      await route.fulfill({ response, json: run });
    });
    await page.goto(OOM);
    const header = page.locator('.run-header');
    await header.getByRole('button', { name: 'Add to experiment' }).click();
    const panel = page.getByRole('region', { name: 'Experiment' });
    await panel.getByLabel('Add it to').selectOption({ label: 'Liver tumor study' });
    await panel.getByRole('button', { name: 'Add', exact: true }).click();
    await expect(header.getByRole('link', { name: 'Liver tumor study' })).toHaveAttribute(
      'href',
      '/experiments/exp_liver_tumor',
    );
    await expect(header).toContainText('In Liver tumor study');
    await expect(header.getByRole('button', { name: 'Experiment', exact: true })).toBeVisible();
    expect(sets).toEqual([{ experiment_id: 'exp_liver_tumor' }]);
  });

  test('the form names an experiment it cannot launch into, and launches outside it', async ({
    page,
  }) => {
    await page.goto('/workflows/aabbccddeeff%3Av1/run?experiment=exp_pilot_2025');
    const notice = page.getByTestId('experiment-unavailable');
    await expect(notice).toContainText('exp_pilot_2025');
    await expect(notice).toContainText("isn't open in project genomics");
    await expect(page.getByText('Launching into the experiment')).toHaveCount(0);
    // It launches, and the launch names no experiment.
    await fillFromExample(page);
    expect(await launch(page)).not.toHaveProperty('experiment');
  });

  test('the form launching into an experiment says so, and can launch outside it', async ({
    page,
  }) => {
    await page.goto('/workflows/aabbccddeeff%3Av1/run?experiment=exp_liver_tumor');
    await fillFromExample(page);
    const banner = page.getByText('Launching into the experiment');
    await expect(banner).toContainText('Liver tumor study');
    await page.getByRole('button', { name: 'Launch outside it' }).click();
    await expect(banner).toHaveCount(0);
    await expect(page.getByTestId('experiment-unavailable')).toHaveCount(0);
    expect(await launch(page)).not.toHaveProperty('experiment');
  });

  test('New run in an experiment lists its project’s workflows only', async ({ page }) => {
    await page.goto('/runs/new?experiment=exp_umi_tuning');
    await expect(page.getByTestId('start-in-experiment')).toContainText('UMI consensus tuning');
    await expect(page.getByRole('link', { name: /umi-consensus/ })).toBeVisible();
    await expect(page.getByRole('link', { name: /nf-core\/rnaseq/ })).toHaveCount(0);
  });
});

// The caller's experiments: My work's Experiments row and the list's Mine | All. Other tests make
// experiments of the caller's in the shared mock, so a test that counts them says which are hers
// itself (`useMine`).

/** `n` open experiments of the caller's, newest activity first; the second has one of hers to look
 * at. */
const myExperiments = (n: number) =>
  Array.from({ length: n }, (_, i) => ({
    id: `exp_mine_${i}`,
    project: 'genomics',
    name: `My study ${i + 1}`,
    created_by: 'alice',
    created_at: 1_790_000_000 - i * 3600,
    run_count: i,
    needs_attention: i === 1 ? 1 : 0,
    mine: true,
  }));

/** An archived experiment of the caller's. */
const myArchived = {
  ...myExperiments(1)[0],
  id: 'exp_mine_old',
  name: 'My old study',
  archived_at: 1_790_000_100,
};

type Listed = { archived_at?: number | null; project: string; mine?: boolean };
/** `experiments` as `GET /experiments` with `params` lists them: archived ones only on asking, one
 * project's on asking. */
const listedAs = <E extends Listed>(experiments: E[], params: URLSearchParams) =>
  experiments.filter(
    (e) =>
      (params.get('archived') === 'true' || e.archived_at == null) &&
      (!params.get('project') || e.project === params.get('project')),
  );

/** Make `experiments` the caller's only ones: `GET /experiments?mine=true` lists them, and everyone's
 * list is the mock's (none of them the caller's) with them first. Returns the queries `?mine=true`
 * was asked with. */
async function useMine(page: Page, experiments: Listed[]): Promise<URLSearchParams[]> {
  const asked: URLSearchParams[] = [];
  await onApi(page, 'GET', '/experiments', async (route) => {
    const params = new URL(route.request().url()).searchParams;
    if (params.get('mine') === 'true') {
      asked.push(params);
      await route.fulfill({ headers: CORS, json: { experiments: listedAs(experiments, params) } });
      return;
    }
    const theirs = ((await (await route.fetch()).json()) as { experiments: Listed[] }).experiments;
    const all = [...listedAs(experiments, params), ...theirs.map((e) => ({ ...e, mine: false }))];
    await route.fulfill({ headers: CORS, json: { experiments: all } });
  });
  return asked;
}

/** Answer every analyses read (`POST /runs/query`) with `answer`. */
async function useRuns(page: Page, answer: { status: number } | { total: number }) {
  await onApi(page, 'POST', '/runs/query', (route) =>
    'status' in answer
      ? route.fulfill({ headers: CORS, status: answer.status, body: 'store down' })
      : route.fulfill({
          headers: CORS,
          json: { runs: [], total: answer.total, first_cursor: null, last_cursor: null },
        }),
  );
}

const experimentsRow = (page: Page) => page.getByRole('button', { name: /^Experiments/ });
/** The Experiments list's cards. */
const cards = (page: Page) => page.getByRole('list', { name: 'Experiments' }).getByRole('article');
const whoseGroup = (page: Page) => page.getByRole('group', { name: 'Whose experiments' });

test.describe('My experiments (Lab view)', () => {
  test.use({ view: 'lab' });

  test('My work counts the caller’s experiments, lists the five newest and sees all', async ({
    page,
  }) => {
    const asked = await useMine(page, [...myExperiments(7), myArchived]);
    await page.goto('/');
    const row = experimentsRow(page);
    // The open ones count; the archived ones are read only to say they exist.
    await expect(row).toHaveAccessibleName('Experiments 7');
    await expect(row).toHaveAttribute('aria-expanded', 'false');
    expect(asked[0].get('archived')).toBe('true');
    await row.click();
    const body = page.locator('#experiments-body');
    await expect(body.getByRole('listitem')).toHaveCount(5);
    await expect(body.getByRole('listitem').first()).toContainText('My study 1');
    await expect(body.getByRole('listitem').first()).toContainText('genomics · 0 analyses');
    await expect(body.getByRole('listitem').nth(1)).toContainText('1 needs a look');
    await expect(body.getByRole('link', { name: 'My study 2', exact: true })).toHaveAttribute(
      'href',
      '/experiments/exp_mine_1',
    );
    // Left open, it stays open.
    await page.reload();
    await expect(experimentsRow(page)).toHaveAttribute('aria-expanded', 'true');

    await page.getByRole('link', { name: 'See all 7 →' }).click();
    await expect(page).toHaveURL(/\/experiments\?mine=true$/);
    await expect(whoseGroup(page).getByRole('button', { name: 'Mine' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await expect(cards(page)).toHaveCount(7);
    await expect(page.getByRole('article', { name: 'My study 2' })).toContainText('1 needs a look');
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toHaveCount(0);
  });

  test('My work offers See all only when there are more than five', async ({ page }) => {
    await useMine(page, myExperiments(2));
    await page.goto('/');
    await experimentsRow(page).click();
    await expect(page.locator('#experiments-body').getByRole('listitem')).toHaveCount(2);
    await expect(
      page.locator('#experiments-body').getByRole('link', { name: /^See all/ }),
    ).toHaveCount(0);
  });

  test('My work with no experiment of the caller’s says how to make one', async ({ page }) => {
    await useMine(page, []);
    await page.goto('/');
    await expect(experimentsRow(page)).toHaveAccessibleName('Experiments 0');
    await experimentsRow(page).click();
    const empty = page.getByTestId('my-experiments-empty');
    await expect(empty).toContainText('None of your analyses is in an experiment yet.');
    await empty.getByRole('link', { name: 'Group analyses under an experiment →' }).click();
    await expect(page).toHaveURL(/\/experiments$/);
  });

  test('My work with only archived experiments says so, and links to them', async ({ page }) => {
    await useMine(page, [myArchived]);
    await page.goto('/');
    await expect(experimentsRow(page)).toHaveAccessibleName('Experiments 0');
    await experimentsRow(page).click();
    const empty = page.getByTestId('my-experiments-empty');
    await expect(empty).toContainText('No open experiments of yours (1 archived).');
    await empty.getByRole('link', { name: 'See your archived experiments →' }).click();
    await expect(page).toHaveURL(/\/experiments\?mine=true&archived=true$/);
    await expect(cards(page)).toHaveCount(1);
    await expect(page.getByRole('article', { name: 'My old study' })).toContainText('archived');
  });

  test('My work counts no experiments before they are read', async ({ page }) => {
    let answer!: () => void;
    const answered = new Promise<void>((resolve) => (answer = resolve));
    await onApi(page, 'GET', '/experiments', async (route) => {
      await answered;
      await route.fulfill({ headers: CORS, json: { experiments: myExperiments(2) } });
    });
    await page.goto('/');
    await expect(page.getByRole('button', { name: /^In progress/ })).toBeVisible();
    await expect(experimentsRow(page)).toHaveAccessibleName('Experiments loading');
    await experimentsRow(page).click();
    await expect(page.locator('#experiments-body')).toContainText('Loading…');
    await expect(page.getByTestId('my-experiments-empty')).toHaveCount(0);
    answer();
    await expect(experimentsRow(page)).toHaveAccessibleName('Experiments 2');
    await expect(page.locator('#experiments-body').getByRole('listitem')).toHaveCount(2);
  });

  test('My work says in the Experiments header when they could not be read, and keeps the analyses', async ({
    page,
  }) => {
    await onApi(page, 'GET', '/experiments', (route) =>
      route.fulfill({ headers: CORS, status: 500, body: 'store down' }),
    );
    await page.goto('/');
    // Visible with the row closed.
    const row = experimentsRow(page);
    await expect(row).toHaveAccessibleName("Experiments couldn't load");
    await expect(row).toHaveAttribute('aria-expanded', 'false');
    await expect(page.getByRole('button', { name: /^Needs attention/ })).toBeVisible();
    await expect(page.getByRole('button', { name: /^In progress/ })).toBeVisible();
    await row.click();
    await expect(page.locator('#experiments-body')).toContainText("Couldn't load your experiments");
  });

  test('My work keeps the Experiments row when the analyses cannot be read', async ({ page }) => {
    await useRuns(page, { status: 500 });
    await useMine(page, myExperiments(2));
    await page.goto('/');
    await expect(page.getByRole('alert')).toContainText("Couldn't load your analyses");
    await expect(experimentsRow(page)).toHaveAccessibleName('Experiments 2');
    await experimentsRow(page).click();
    await expect(page.locator('#experiments-body').getByRole('listitem')).toHaveCount(2);
  });

  test('a first-time user who made experiments sees them beside the welcome', async ({ page }) => {
    await useRuns(page, { total: 0 });
    await useMine(page, myExperiments(2));
    await page.goto('/');
    await expect(
      page.getByRole('heading', { name: "You haven't started an analysis yet" }),
    ).toBeVisible();
    await expect(page.getByRole('button', { name: /^Needs attention/ })).toHaveCount(0);
    await expect(experimentsRow(page)).toHaveAccessibleName('Experiments 2');
  });

  test('a first-time user with no experiments sees the welcome alone', async ({ page }) => {
    await useRuns(page, { total: 0 });
    const asked = await useMine(page, []);
    await page.goto('/');
    await expect(
      page.getByRole('heading', { name: "You haven't started an analysis yet" }),
    ).toBeVisible();
    await expect.poll(() => asked.length).toBeGreaterThan(0);
    await expect(page.getByRole('heading', { name: 'Pinned pipelines' })).toBeVisible();
    await page.waitForLoadState('networkidle');
    await expect(experimentsRow(page)).toHaveCount(0);
  });

  test('the list’s Mine | All lives in the URL', async ({ page }) => {
    await useMine(page, [myArchived]);
    await page.goto('/experiments');
    await expect(whoseGroup(page).getByRole('button', { name: 'All' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toBeVisible();
    // Priya's study is not hers; her one experiment is archived, so Mine says it hides it.
    await whoseGroup(page).getByRole('button', { name: 'Mine' }).click();
    await expect(page).toHaveURL(/[?&]mine=true/);
    const hidden = page.getByTestId('experiments-empty-filtered');
    await expect(hidden).toHaveText(/No experiments of yours \(archived ones are hidden\)\./);
    await expect(page.getByTestId('experiments-empty-mine')).toHaveCount(0);
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toHaveCount(0);
    await page.reload();
    await expect(whoseGroup(page).getByRole('button', { name: 'Mine' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    // Loaded (the message needs the list), so the absence below is not a page still loading.
    await expect(hidden).toBeVisible();
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toHaveCount(0);
    // Mine keeps the other filters: her archived one shows on asking.
    await hidden.getByRole('button', { name: 'Show archived' }).click();
    await expect(page).toHaveURL(/mine=true/);
    await expect(page).toHaveURL(/archived=true/);
    await expect(cards(page)).toHaveCount(1);
    await expect(page.getByRole('article', { name: 'My old study' })).toBeVisible();
    await whoseGroup(page).getByRole('button', { name: 'All' }).click();
    await expect(page).not.toHaveURL(/mine=/);
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toBeVisible();
  });

  test('Mine in a project with none of the caller’s says so', async ({ page }) => {
    await useMine(page, myExperiments(2));
    await page.goto('/experiments?mine=true&project=imported');
    await expect(page.getByTestId('experiments-empty-filtered')).toHaveText(
      /No experiments of yours in project imported\./,
    );
    await expect(page.getByRole('article', { name: 'UMI consensus tuning' })).toHaveCount(0);
  });

  test('Mine with no experiment of the caller’s says how one becomes hers', async ({ page }) => {
    await useMine(page, []);
    await page.goto('/experiments?mine=true');
    const empty = page.getByTestId('experiments-empty-mine');
    await expect(empty).toContainText("You're not in an experiment yet.");
    await expect(empty.getByRole('button', { name: 'New experiment' })).toBeVisible();
    await empty.getByRole('button', { name: 'See all experiments' }).click();
    await expect(page).not.toHaveURL(/mine=/);
    await expect(page.getByRole('article', { name: 'Liver tumor study' })).toBeVisible();
  });

  for (const theme of THEMES) {
    test(`My work’s Experiments and the caller’s list are accessible (${theme})`, async ({
      page,
    }) => {
      await useTheme(page, theme);
      await useMine(page, myExperiments(7));
      await page.goto('/');
      await experimentsRow(page).click();
      await expect(page.locator('#experiments-body').getByRole('listitem')).toHaveCount(5);
      await expectNoSeriousA11yViolations(page);
      await page.goto('/experiments?mine=true');
      await expect(cards(page)).toHaveCount(7);
      await expectNoSeriousA11yViolations(page);
    });
  }
});
