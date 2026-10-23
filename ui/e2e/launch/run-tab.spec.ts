import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  THEMES,
  useTheme,
  RUN_TAB,
  workflowTabs,
  onApi,
  CORS,
  startFromExample,
} from '../fixtures';

// The Run tab: launching a workflow from the UI, against the mock's curated
// nf-core/rnaseq form.

test('a workflow opens on its Run tab', async ({ page }) => {
  await page.goto('/workflows/aabbccddeeff%3Av1');
  await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/run$/);
  await expect(workflowTabs(page).getByRole('link', { name: 'run' })).toHaveAttribute(
    'aria-current',
    'page',
  );
});

test('the curated form shows its sections and keeps the rest under Advanced', async ({ page }) => {
  await page.goto(RUN_TAB);
  for (const title of ['Samples', 'Reference genome', 'Options', 'Run details'])
    await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
  // Advanced is collapsed, naming its groups.
  const advanced = page.locator('details.advanced');
  await expect(advanced).not.toHaveAttribute('open', '');
  await expect(advanced.locator('> summary')).toContainText('Trimming');
  await expect(advanced.locator('> summary')).toContainText('2 groups · 2 parameters');
  // Nextflow's outdir is offered as the results folder, not as a required input.
  await expect(page.getByRole('textbox', { name: /^Results\ folder/ })).toHaveAttribute(
    'placeholder',
    "Default: this run's own results folder",
  );
  // Author presets name the aligner's choices.
  await expect(page.getByRole('radio', { name: 'STAR + Salmon' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
});

/** The Run tab with its form rendered (it loads with the version, after the page itself). */
async function openRunTab(page: import('@playwright/test').Page) {
  await page.goto(RUN_TAB);
  await expect(page.getByRole('heading', { name: 'Options', exact: true })).toBeVisible();
}

test('a gated option appears only when its gate is on', async ({ page }) => {
  await openRunTab(page);
  // Its gate is rendered, so the option's absence is the gate's doing, not a form still loading.
  await expect(page.getByRole('switch', { name: 'Enable UMI' })).toBeVisible();
  await expect(page.getByLabel('UMI pattern')).toHaveCount(0);
  await page.getByRole('switch', { name: 'Enable UMI' }).click();
  await expect(page.getByLabel('UMI pattern')).toBeVisible();
  await page.getByRole('switch', { name: 'Enable UMI' }).click();
  await expect(page.getByLabel('UMI pattern')).toHaveCount(0);
});

test('show all parameters reveals params the author hid', async ({ page }) => {
  await openRunTab(page);
  await expect(page.getByRole('switch', { name: 'Show all parameters' })).toBeVisible();
  await expect(page.getByLabel('Email')).toHaveCount(0);
  await page.getByRole('switch', { name: 'Show all parameters' }).check();
  await expect(page.getByLabel('Email')).toBeVisible();
  await expect(page.getByText('hidden by author')).toBeVisible();
});

test('a preset chip sets the value and Custom starts from it', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('radio', { name: 'Strict (100,000)' }).click();
  await page.getByRole('radio', { name: 'Custom' }).click();
  const reads = page.getByRole('textbox', { name: /Minimum trimmed reads/ });
  await expect(reads).toHaveValue('100000');
  await reads.fill('2.5');
  await reads.blur();
  await expect(page.getByText('must be a whole number')).toBeVisible();
});

test('launch names what is missing, then launches from an example', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Launch run →' }).click();
  const footer = page.locator('footer.launch-bar');
  await expect(footer).toContainText('2 problems');
  await expect(footer.getByRole('button', { name: 'Samplesheet: required' })).toBeVisible();
  await expect(page.locator('#field-input')).toBeFocused();

  await startFromExample(page);
  await expect(page.getByRole('textbox', { name: /^Samplesheet/ })).toHaveValue(
    's3://biffix-examples/rnaseq/samplesheet.csv',
  );
  await expect(page.getByLabel('Run name')).toHaveValue('rnaseq example (human)');
  await expect(footer).toContainText('Ready to launch');

  await page.getByRole('button', { name: 'Launch run →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  await expect(page.getByRole('heading', { name: 'rnaseq example (human)' })).toBeVisible();
});

test('the launch shortcut launches once, however often it is pressed', async ({ page }) => {
  await page.goto(RUN_TAB);
  await startFromExample(page);
  await expect(page.locator('footer.launch-bar')).toContainText('Ready to launch');
  let submits = 0;
  page.on('request', (r) => {
    if (r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/runs')) submits += 1;
  });
  await page.keyboard.press('Control+Enter');
  await page.keyboard.press('Control+Enter');
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);
  expect(submits).toBe(1);
});

test('an example asks before replacing what was entered', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use my own files…' }).click();
  await page.getByRole('textbox', { name: /^Genome\ FASTA/ }).fill('s3://mine/genome.fa');
  await startFromExample(page);
  await expect(page.getByText("Replace everything you've entered?")).toBeVisible();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page.getByRole('textbox', { name: /^Genome\ FASTA/ })).toHaveValue(
    's3://mine/genome.fa',
  );
  await page.getByRole('menuitem', { name: /Human, 6 paired-end samples/ }).click();
  await page.getByRole('button', { name: 'Replace' }).click();
  await expect(page.getByRole('textbox', { name: /^Genome\ FASTA/ })).toHaveValue(
    's3://biffix-examples/rnaseq/genome.fa',
  );
});

test('a pattern rule is checked as you type', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use an existing samplesheet file' }).click();
  await page.getByRole('textbox', { name: /^Samplesheet/ }).fill('s3://bucket/samples.tsv');
  await page.getByRole('textbox', { name: /^Samplesheet/ }).blur();
  await expect(page.locator('#field-input-error')).toHaveText(
    'must be an s3:// path to a .csv file',
  );
});

test('params import from YAML, keeping unknown keys out', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Import YAML' }).click();
  await page
    .getByLabel('Params YAML')
    .fill('input: s3://b/sheet.csv\nfasta: s3://b/g.fa\nbogus: 1\n');
  await page.getByRole('button', { name: 'Apply' }).click();
  await expect(page.getByRole('textbox', { name: /^Samplesheet/ })).toHaveValue('s3://b/sheet.csv');
  await expect(page.getByRole('status')).toContainText('bogus');
});

test('a half-filled form survives a reload and can be discarded', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use my own files…' }).click();
  await page.getByRole('textbox', { name: /^Genome\ FASTA/ }).fill('s3://b/genome.fa');
  await page.reload();
  await expect(page.getByRole('textbox', { name: /^Genome\ FASTA/ })).toHaveValue(
    's3://b/genome.fa',
  );
  await page.getByRole('button', { name: 'Discard it' }).click();
  // Back to a fresh form: the reference dropdown, nothing chosen.
  await expect(page.getByRole('textbox', { name: /^Genome\ FASTA/ })).toHaveCount(0);
  await expect(page.getByRole('combobox', { name: /^Reference genome/ })).toHaveValue('');
});

test('a draft that only chose a reference survives a reload', async ({ page }) => {
  await openRunTab(page);
  const reference = page.getByRole('combobox', { name: /^Reference genome/ });
  await expect(reference.locator('option')).not.toHaveCount(1);
  const bundle = await reference.locator('option').nth(1).getAttribute('value');
  await reference.selectOption(bundle!);
  await page.reload();
  await expect(page.getByText('Your unsaved form was restored')).toBeVisible();
  await expect(reference).toHaveValue(bundle!);
});

test("an author's prefilled samplesheet opens as that file", async ({ page }) => {
  await onApi(page, 'GET', /^\/workflows\/aabbccddeeff(:|%3A)v1$/, async (route) => {
    const response = await route.fetch();
    const detail = await response.json();
    for (const f of detail.form_schema.fields) if (f.name === 'input') f.prefill = 's3://b/lab.csv';
    await route.fulfill({ response, json: detail });
  });
  await page.goto(RUN_TAB);
  await expect(page.getByRole('textbox', { name: /^Samplesheet/ })).toHaveValue('s3://b/lab.csv');
});

test('a param that takes a list of paths holds one per line', async ({ page }) => {
  await onApi(page, 'GET', /^\/workflows\/aabbccddeeff(:|%3A)v1$/, async (route) => {
    const response = await route.fetch();
    const detail = await response.json();
    detail.form_schema.fields.push({
      name: 'extra_reads',
      label: 'Extra reads',
      widget: 's3_file',
      multiple: true,
      required: false,
      section: 'options',
      is_output: false,
      is_secret: false,
      is_path_input: true,
    });
    await route.fulfill({ response, json: detail });
  });
  await page.goto(RUN_TAB);
  const list = page.getByRole('textbox', { name: /^Extra reads/ });
  await list.fill('s3://b/a.fq.gz\n\n  s3://b/b.fq.gz  ');
  // Kept as a list (in the saved draft too), not one string.
  await page.reload();
  await expect(list).toHaveValue('s3://b/a.fq.gz\ns3://b/b.fq.gz');
});

test('an Advanced group the user opened stays open when its change is undone', async ({ page }) => {
  await openRunTab(page);
  await page.locator('details.advanced > summary').click();
  const trimming = page.locator('details.group', {
    has: page.locator('summary', { hasText: 'Trimming' }),
  });
  await trimming.locator('summary').click();
  await expect(trimming).toHaveAttribute('open', '');
  await trimming.getByRole('radio', { name: 'fastp' }).click();
  await trimming.getByRole('radio', { name: 'trimgalore' }).click();
  await expect(trimming).toHaveAttribute('open', '');
  await expect(trimming.getByRole('radio', { name: 'trimgalore' })).toBeFocused();
});

test('the examples menu moves with the arrow keys and closes on Escape', async ({ page }) => {
  await openRunTab(page);
  const open = page.getByRole('button', { name: 'Start from example ▾' });
  await open.click();
  const items = page.getByRole('menuitem');
  await items.first().focus();
  await page.keyboard.press('ArrowDown');
  await expect(items.nth(1)).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  await expect(open).toBeFocused();
});

test('the arrow keys move a chip choice, one Tab stop for the group', async ({ page }) => {
  await openRunTab(page);
  const star = page.getByRole('radio', { name: 'STAR + Salmon' });
  await expect(star).toHaveAttribute('tabindex', '0');
  await expect(page.getByRole('radio', { name: 'HISAT2' })).toHaveAttribute('tabindex', '-1');
  await star.focus();
  await page.keyboard.press('ArrowRight');
  const rsem = page.getByRole('radio', { name: 'STAR + RSEM' });
  await expect(rsem).toHaveAttribute('aria-checked', 'true');
  await expect(rsem).toBeFocused();
});

test('with no reference to offer, a discarded draft opens on own files again', async ({ page }) => {
  await onApi(page, 'GET', '/references', (route) => route.fulfill({ json: [], headers: CORS }));
  await page.goto(RUN_TAB);
  const fasta = page.getByRole('textbox', { name: /^Genome\ FASTA/ });
  await fasta.fill('s3://b/genome.fa');
  await page.reload();
  await expect(fasta).toHaveValue('s3://b/genome.fa');
  await page.getByRole('button', { name: 'Discard it' }).click();
  await expect(fasta).toHaveValue('');
});

test('New run lists the workflows and opens the chosen one on its Run tab', async ({ page }) => {
  await page.goto('/runs');
  await page.getByRole('link', { name: 'New run' }).click();
  await expect(page).toHaveURL(/\/runs\/new$/);
  await page.getByLabel('Search workflows').fill('rnaseq');
  await page.getByRole('link', { name: /nf-core\/rnaseq/ }).click();
  await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/run$/);
});

for (const theme of THEMES) {
  test(`the Run tab (${theme}) has no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto(RUN_TAB);
    await page.getByRole('switch', { name: 'Enable UMI' }).click();
    await page.getByRole('button', { name: 'Launch run →' }).click();
    await page.locator('details.advanced > summary').click();
    await expect(page.locator('details.advanced[open]')).toBeVisible();
    await expect(page.getByLabel('UMI pattern')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}
