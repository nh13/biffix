import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  RUN_TAB,
  chooseBatch12,
  importCsv,
} from '../fixtures';

// The samplesheet builder: samples from files, fixed and launched as rows.

test('files become samples, previewed before they are added', async ({ page }) => {
  await page.goto(RUN_TAB);
  await expect(page.getByText('No samples yet.')).toBeVisible();
  await chooseBatch12(page);
  const preview = page.getByRole('region', { name: 'Detected samples' });
  await expect(preview).toContainText('3 samples · 3 paired · 0 with problems');
  await preview.getByRole('button', { name: 'Add 3 samples' }).click();
  const table = page.getByRole('table', { name: 'Samplesheet rows' });
  await expect(table.getByRole('textbox', { name: 'Row 1 sample' })).toHaveValue('P01');
  await expect(table.getByRole('textbox', { name: 'Row 3 fastq_2' })).toHaveValue(
    's3://seq-core-deliveries/lab-x/2026/batch-12/P03_S3_L001_R2_001.fastq.gz',
  );
  await expect(table.getByRole('combobox', { name: 'Row 2 strandedness' })).toHaveValue('auto');
});

test('choosing name parts changes the sample ids live', async ({ page }) => {
  await page.goto(RUN_TAB);
  await chooseBatch12(page);
  const preview = page.getByRole('region', { name: 'Detected samples' });
  await preview.getByRole('button', { name: 'Fix naming' }).click();
  // A radio group: one Tab stop, the arrow keys move the choice.
  await preview.getByRole('radio', { name: 'Automatic' }).focus();
  await page.keyboard.press('ArrowRight');
  await expect(preview.getByRole('radio', { name: 'Choose parts' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  await preview.getByRole('button', { name: 'S1', exact: true }).click();
  await expect(preview).toContainText('Resulting sample id for P01_S1_L001_R1_001.fastq.gz: S1');
  await preview.getByRole('button', { name: 'P01', exact: true }).click();
  await expect(preview).toContainText('Resulting sample id for P01_S1_L001_R1_001.fastq.gz: P01S1');
});

test('a naming rule the server refuses keeps the editor, to fix it, and Cancel', async ({
  page,
}) => {
  await page.goto(RUN_TAB);
  await chooseBatch12(page);
  const preview = page.getByRole('region', { name: 'Detected samples' });
  await preview.getByRole('button', { name: 'Fix naming' }).click();
  // An empty pattern has no group to read the id from.
  await preview.getByRole('radio', { name: 'Regular expression' }).click();
  await expect(preview.getByRole('alert')).toContainText('needs a capture group');
  await expect(preview.getByRole('button', { name: /^Add / })).toBeDisabled();
  await expect(preview.getByRole('button', { name: 'Cancel' })).toBeVisible();
  await preview.getByRole('textbox', { name: /^Pattern/ }).fill('^(P\\d+)_');
  await expect(preview.getByRole('alert')).toHaveCount(0);
  await expect(preview).toContainText('Resulting sample id for P01_S1_L001_R1_001.fastq.gz: P01');
  await expect(preview.getByRole('button', { name: 'Add 3 samples' })).toBeEnabled();
});

test('Clear asks before dropping every row, and the menu closes on Escape', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await page.getByRole('menuitem', { name: 'Add an empty row' }).click();
  const sample1 = page.getByRole('textbox', { name: 'Row 1 sample' });
  await sample1.fill('KEEP');
  const asked: string[] = [];
  page.once('dialog', (d) => (asked.push(d.message()), d.dismiss()));
  await page.getByRole('button', { name: 'Clear' }).click();
  await expect.poll(() => asked).toEqual(['Remove all 1 row from the table?']);
  await expect(sample1).toHaveValue('KEEP');
  page.once('dialog', (d) => d.accept());
  await page.getByRole('button', { name: 'Clear' }).click();
  await expect(page.getByText('No samples yet.')).toBeVisible();

  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await expect(page.getByRole('menu')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
});

test('a split pair is offered as a merge', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await page.getByRole('menuitem', { name: 'Choose files…' }).click();
  const picker = page.getByRole('dialog');
  await picker.getByRole('button', { name: /Sequencing core/ }).click();
  await picker.getByRole('button', { name: '2026' }).click();
  await picker.getByRole('button', { name: 'batch-14' }).click();
  await picker.getByRole('button', { name: 'Select all 2' }).click();
  await picker.getByRole('button', { name: 'Use 2 files' }).click();
  const preview = page.getByRole('region', { name: 'Detected samples' });
  await expect(preview).toContainText('read 2 without read 1');
  await preview.getByRole('button', { name: 'Add 2 samples' }).click();
  await page.getByRole('button', { name: 'Merge as P07' }).click();
  const table = page.getByRole('table', { name: 'Samplesheet rows' });
  await expect(table.getByRole('textbox', { name: 'Row 1 sample' })).toHaveValue('P07');
  await expect(table.getByRole('textbox', { name: 'Row 1 fastq_1' })).toHaveValue(/P7_S7_L001_R1/);
  await expect(table.getByRole('textbox', { name: 'Row 1 fastq_2' })).toHaveValue(/P07_S7_L001_R2/);
  await expect(table.getByRole('textbox', { name: 'Row 2 sample' })).toHaveCount(0);
});

test('rows import from CSV, export, and launch as a samplesheet', async ({ page }) => {
  await page.goto(RUN_TAB);
  await importCsv(
    page,
    'samples.csv',
    'sample,fastq_1,fastq_2,batch\nA,s3://b/A_R1.fastq.gz,s3://b/A_R2.fastq.gz,1\nB,s3://b/B_R1.fastq.gz,,1\n',
  );
  await expect(page.getByText('Imported 2 rows from samples.csv.')).toContainText('batch');
  // The footer prices the two samples from the workflow's past runs.
  await expect(page.locator('footer.launch-bar')).toContainText(
    'About $4–$6 for 2 samples · usually ~2.5 h · from 6 runs',
  );
  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export CSV' }).click();
  expect((await download).suggestedFilename()).toBe('input.csv');

  await page.getByRole('combobox', { name: /^Reference genome/ }).selectOption('grch38-ensembl110');
  await expect(page.locator('footer.launch-bar')).toContainText('Ready to launch');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);

  // Re-run brings the rows back (opened directly: the mock run has not finished).
  const runId = page.url().split('/').pop();
  await page.goto(`${RUN_TAB}?from=${runId}`);
  await expect(
    page
      .getByRole('table', { name: 'Samplesheet rows' })
      .getByRole('textbox', { name: 'Row 2 sample' }),
  ).toHaveValue('B');
});

test('a bad row is named in the footer and flagged in the table', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await page.getByRole('menuitem', { name: 'Add an empty row' }).click();
  await page.getByRole('textbox', { name: 'Row 1 fastq_1' }).fill('local/a.fastq.gz');
  const footer = page.locator('footer.launch-bar');
  await expect(footer).toContainText('Row 1 · sample name missing');
  await expect(footer).toContainText('Row 1 · read 1: must be an s3:// path');
});

test('an import asks before replacing rows, and an empty file imports nothing', async ({
  page,
}) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await page.getByRole('menuitem', { name: 'Add an empty row' }).click();
  await page.getByRole('textbox', { name: 'Row 1 sample' }).fill('KEEP');
  const sample1 = page.getByRole('textbox', { name: 'Row 1 sample' });

  await importCsv(page, 'empty.csv', 'sample,fastq_1\n');
  await expect(page.getByText('empty.csv has no rows; nothing was imported.')).toBeVisible();
  await expect(sample1).toHaveValue('KEEP');

  await importCsv(
    page,
    'broken.csv',
    'sample,fastq_1\nA,"s3://b/a.fastq.gz\nB,s3://b/b.fastq.gz\n',
  );
  await expect(
    page.getByText(
      'broken.csv could not be read (a quoted field opened on line 2 is never closed); nothing was imported.',
    ),
  ).toBeVisible();
  await expect(sample1).toHaveValue('KEEP');

  const csv = 'sample,fastq_1\nNEW,s3://b/N_R1.fastq.gz\n';
  // Wait for each confirm explicitly: the table is unchanged until it is answered, so a value
  // check alone can pass before the dialog opens, and a second listener would then see it too.
  const declined = page.waitForEvent('dialog');
  await importCsv(page, 'new.csv', csv);
  await (await declined).dismiss();
  await expect(sample1).toHaveValue('KEEP');
  // Not imported, so not offered to match again.
  await expect(page.getByRole('button', { name: 'Change how its columns match' })).toHaveCount(0);

  const accepted = page.waitForEvent('dialog');
  await importCsv(page, 'new.csv', csv);
  const confirm = await accepted;
  expect(confirm.message()).toBe('Replace the 1 row in the table with the 1 in new.csv?');
  await confirm.accept();
  await expect(sample1).toHaveValue('NEW');
});

test("a removed row's edits do not flag the row that takes its place", async ({ page }) => {
  await page.goto(RUN_TAB);
  for (let i = 0; i < 2; i++) {
    await page.getByRole('button', { name: 'Add samples ▾' }).click();
    await page.getByRole('menuitem', { name: 'Add an empty row' }).click();
  }
  await page.getByRole('textbox', { name: 'Row 1 fastq_1' }).fill('local/a.fastq.gz');
  await expect(page.getByRole('textbox', { name: 'Row 1 fastq_1' })).toHaveAttribute(
    'aria-invalid',
    'true',
  );
  await page.getByRole('button', { name: 'Remove row 1' }).click();
  await expect(page.getByRole('textbox', { name: 'Row 1 sample' })).toHaveAttribute(
    'aria-invalid',
    'false',
  );
});

test('the builder has no serious accessibility violations', async ({ page }) => {
  await page.goto(RUN_TAB);
  await chooseBatch12(page);
  const preview = page.getByRole('region', { name: 'Detected samples' });
  await preview.getByRole('button', { name: 'Fix naming' }).click();
  await preview.getByRole('radio', { name: 'Choose parts' }).click();
  await preview.getByRole('button', { name: 'Add 3 samples' }).click();
  await expectNoSeriousA11yViolations(page);
});
