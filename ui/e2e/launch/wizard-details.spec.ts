import type { Page } from '@playwright/test';
import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  THEMES,
  useTheme,
  RUN_TAB,
  importCsv,
  startFromExample,
} from '../fixtures';

// The launch wizard: per-parameter help and units, per-row
// samplesheet validity, a trimmed Review, several named examples with overwrite protection, CSV
// column mapping, and problems that name the sample.

test.use({ view: 'lab' });

const heading = (page: Page, name: string) => page.getByRole('heading', { name, exact: true });
const next = (page: Page) => page.getByRole('button', { name: /^Next: / });
const table = (page: Page) => page.getByRole('table', { name: 'Samplesheet rows' });
const footer = (page: Page) => page.locator('#wizard-problems');

/** Three samples: A is ready, B lacks read 1, C's read 1 is not an s3:// path. */
const MIXED =
  'sample,fastq_1,fastq_2\n' +
  'A,s3://b/A_R1.fastq.gz,s3://b/A_R2.fastq.gz\n' +
  'B,,s3://b/B_R2.fastq.gz\n' +
  'C,local/C_R1.fastq.gz,\n';

test.describe('per-parameter help', () => {
  test('an ⓘ opens the long help from the keyboard, and Escape closes it', async ({ page }) => {
    await page.goto(RUN_TAB);
    await startFromExample(page);
    await next(page).click();
    await expect(heading(page, 'Reference & options')).toBeVisible();

    // The short description is inline; the unit rides with the label.
    await expect(
      page.getByText('How reads are matched to the genome and genes counted.'),
    ).toBeVisible();
    await expect(page.locator('#field-min_trimmed_reads-label')).toHaveText(
      /^Minimum trimmed reads\s*\[reads\]/,
    );

    const info = page.getByRole('button', { name: 'About Aligner' });
    await info.focus();
    await expect(info).toHaveAttribute('aria-expanded', 'false');
    await page.keyboard.press('Enter');
    await expect(info).toHaveAttribute('aria-expanded', 'true');
    const help = page.locator('#field-aligner-help');
    await expect(help).toBeVisible();
    // Rendered as Markdown: a list, with bold.
    await expect(help.getByRole('listitem')).toHaveCount(2);
    await expect(help.locator('strong').first()).toHaveText('STAR + RSEM');
    await expect(info).toHaveAttribute('aria-controls', 'field-aligner-help');
    await page.keyboard.press('Escape');
    await expect(help).toBeHidden();
    await expect(info).toBeFocused();

    // The reference dropdown has its own help.
    const refInfo = page.getByRole('button', { name: 'About Reference genome' });
    await refInfo.press('Enter');
    await expect(
      page.getByText('Pick the assembly your samples were sequenced against'),
    ).toBeVisible();
  });
});

test.describe('per-row samplesheet validity and problems that name the sample', () => {
  test('each row has a status, problem rows say why, and a filter shows only them', async ({
    page,
  }) => {
    await page.goto(RUN_TAB);
    await importCsv(page, 'mixed.csv', MIXED);
    const rows = table(page);
    await expect(rows.locator('tr[data-row="0"] td.flag')).toContainText('✓');
    await expect(rows.locator('tr[data-row="1"] td.flag')).toContainText('⚠ 1');
    await expect(rows).toContainText('Sample B: read 1 missing');
    await expect(rows).toContainText('Sample C: read 1: must be an s3:// path');
    await expect(page.getByText('⚠ Problems in rows 2 and 3')).toBeVisible();

    // Only the rows with problems.
    const only = page.getByRole('checkbox', { name: 'Only rows with problems (2)' });
    await only.check();
    await expect(rows.getByRole('textbox', { name: 'Row 1 sample' })).toHaveCount(0);
    await expect(rows.getByRole('textbox', { name: 'Row 2 sample' })).toHaveValue('B');

    // A row being fixed stays in view, keystroke by keystroke, with the focus and the whole value.
    const cell = rows.getByRole('textbox', { name: 'Row 2 fastq_1' });
    await cell.click();
    await cell.pressSequentially('s3://b/B_R1.fastq.gz');
    await expect(cell).toBeFocused();
    await expect(cell).toHaveValue('s3://b/B_R1.fastq.gz');
    await expect(rows.locator('tr[data-row="1"] td.flag')).toContainText('✓');
    // Fixed, it still shows until the filter is switched again; then it goes.
    const filter = page.getByRole('checkbox', { name: /^Only rows with problems/ });
    await filter.uncheck();
    await filter.check();
    await expect(rows.getByRole('textbox', { name: 'Row 2 sample' })).toHaveCount(0);
    // Put the problem back for the rest of the test.
    await page.getByRole('checkbox', { name: /^Only rows with problems/ }).uncheck();
    await rows.getByRole('textbox', { name: 'Row 2 fastq_1' }).fill('');
    await page.getByRole('checkbox', { name: 'Only rows with problems (2)' }).check();

    // The footer names the samples; a problem goes to its row's cell.
    await expect(footer(page)).toContainText('2 problems');
    const problem = footer(page).getByRole('button', { name: 'Sample B · read 1 missing' });
    await problem.click();
    await expect(rows.getByRole('textbox', { name: 'Row 2 fastq_1' })).toBeFocused();

    // Fixing both rows: every row is ready, and the filter turns itself off.
    await rows.getByRole('textbox', { name: 'Row 2 fastq_1' }).fill('s3://b/B_R1.fastq.gz');
    await rows.getByRole('textbox', { name: 'Row 3 fastq_1' }).fill('s3://b/C_R1.fastq.gz');
    await expect(page.getByText('✓ Every row is ready')).toBeVisible();
    await expect(rows.getByRole('textbox', { name: 'Row 1 sample' })).toHaveValue('A');
    await expect(footer(page)).toContainText('Samples ready');
  });

  test('Next goes to the first problem row, and Show the first finds it', async ({ page }) => {
    await page.goto(RUN_TAB);
    await importCsv(page, 'mixed.csv', MIXED);
    // The import is read asynchronously: wait for its last row, so Next sees its problems.
    await expect(table(page).getByRole('textbox', { name: 'Row 3 sample' })).toHaveValue('C');
    // Next is aria-disabled while the step has problems, yet still answers: it goes to the first.
    await next(page).focus();
    await page.keyboard.press('Enter');
    await expect(heading(page, 'Add samples')).toBeVisible();
    await expect(table(page).getByRole('textbox', { name: 'Row 2 fastq_1' })).toBeFocused();
    await page.getByRole('textbox', { name: 'Row 1 sample' }).focus();
    await page.getByRole('button', { name: 'Show the first →' }).click();
    await expect(table(page).getByRole('textbox', { name: 'Row 2 fastq_1' })).toBeFocused();
  });
});

test.describe('the Review step', () => {
  test('where results go is one line, opened by Change', async ({ page }) => {
    await page.goto(RUN_TAB);
    await startFromExample(page);
    await next(page).click();
    await next(page).click();
    await expect(heading(page, 'Review & launch')).toBeVisible();
    const more = page.locator('details.review-more');
    await expect(more.locator('summary')).toContainText(
      'Results folder: this analysis’s own · Intermediate files: keep for 30 days (default) · Notify: in the app',
    );
    const retention = page.getByRole('combobox', { name: 'Intermediate files' });
    await expect(retention).toBeHidden();
    // From the keyboard: the summary is a disclosure.
    await more.locator('summary').focus();
    await page.keyboard.press('Enter');
    await retention.selectOption('delete_on_success');
    await expect(more.locator('summary')).toContainText(
      'Intermediate files: delete when the run succeeds',
    );
    await expect(page.getByText('In the app: the bell at the top of the page')).toBeVisible();
    // The estimate, samples and options stay in view.
    await expect(page.getByRole('region', { name: 'Samples', exact: true })).toBeVisible();
    await expect(page.locator('.estimate-line')).toBeVisible();
  });
});

test.describe('named examples', () => {
  test('a new form invites an example; several are offered', async ({ page }) => {
    await page.goto(RUN_TAB);
    await expect(page.getByText(/New here\? Start from an example/)).toBeVisible();
    await page.getByRole('button', { name: 'Show the examples' }).click();
    const menu = page.getByRole('menu');
    await expect(menu.getByRole('menuitem')).toHaveCount(2);
    await expect(menu.getByRole('menuitem', { name: /Mouse, 2 single-end samples/ })).toContainText(
      'about 15 minutes',
    );
    await menu.getByRole('menuitem', { name: /Mouse, 2 single-end samples/ }).click();
    // Nothing was entered, so nothing to confirm or undo.
    await expect(page.getByRole('status')).toContainText('Filled from the example');
    await expect(page.getByRole('button', { name: /^Undo/ })).toHaveCount(0);
    await expect(page.getByText(/New here\?/)).toHaveCount(0);
  });

  test('an example over entered rows asks first, and can be undone', async ({ page }) => {
    await page.goto(RUN_TAB);
    await importCsv(page, 'mine.csv', 'sample,fastq_1\nMINE,s3://b/M_R1.fastq.gz\n');
    await startFromExample(page);
    await expect(page.getByText("Replace everything you've entered?")).toBeVisible();
    await page.getByRole('button', { name: 'Replace' }).click();
    await expect(page.getByRole('textbox', { name: /^Samplesheet/ })).toHaveValue(
      's3://biffix-examples/rnaseq/samplesheet.csv',
    );
    await page.getByRole('button', { name: 'Undo, and put back what you had entered' }).click();
    await expect(table(page).getByRole('textbox', { name: 'Row 1 sample' })).toHaveValue('MINE');
  });
});

test.describe('CSV column mapping', () => {
  test("a file whose headers are not the sheet's is matched before it imports", async ({
    page,
  }) => {
    await page.goto(RUN_TAB);
    const csv =
      'Sample,R1,R2,Lane\nP1,s3://b/P1_R1.fastq.gz,s3://b/P1_R2.fastq.gz,1\nP2,s3://b/P2_R1.fastq.gz,,1\n';
    await importCsv(page, 'core.csv', csv);
    const match = page.getByRole('region', { name: 'Match columns' });
    await expect(match).toBeVisible();
    await expect(match.getByRole('combobox', { name: /^sample \*/ })).toHaveValue('0');
    await expect(match.getByRole('combobox', { name: /^fastq_1 \*/ })).toHaveValue('1');
    await expect(match.getByRole('combobox', { name: /^fastq_2/ })).toHaveValue('2');
    const preview = match.getByRole('table', { name: 'Import preview' });
    await expect(preview).toContainText('s3://b/P1_R2.fastq.gz');
    // Nothing is imported until it is confirmed.
    await expect(table(page)).toHaveCount(0);

    // Changed from the keyboard: read 2 left empty.
    await match.getByRole('combobox', { name: /^fastq_2/ }).focus();
    await match.getByRole('combobox', { name: /^fastq_2/ }).selectOption('');
    await expect(preview).not.toContainText('s3://b/P1_R2.fastq.gz');
    await match.getByRole('button', { name: 'Import 2 rows' }).click();
    await expect(table(page).getByRole('textbox', { name: 'Row 1 fastq_1' })).toHaveValue(
      's3://b/P1_R1.fastq.gz',
    );
    await expect(table(page).getByRole('textbox', { name: 'Row 1 fastq_2' })).toHaveValue('');
    const note = page.getByText('Imported 2 rows from core.csv.');
    // Lane is not the workflow's; R2 is read 2, left out by choice.
    await expect(note).toContainText('Skipped columns this workflow does not use: Lane.');
    await expect(note).toContainText('Left out, as you chose: R2.');

    // The match is remembered, but leaving out a column that names read 2 is never repeated
    // quietly: the same headers show the match again, with the choice made before.
    await importCsv(page, 'core2.csv', csv);
    const again = page.getByRole('region', { name: 'Match columns' });
    await expect(again).toBeVisible();
    await expect(again.getByRole('combobox', { name: /^fastq_2/ })).toHaveValue('');
    await expect(again.getByRole('combobox', { name: /^sample \*/ })).toHaveValue('0');
    page.once('dialog', (d) => d.accept());
    await again.getByRole('button', { name: 'Import 2 rows' }).click();
    await expect(page.getByText('Imported 2 rows from core2.csv.')).toBeVisible();
    await page.getByRole('button', { name: 'Change how its columns match' }).click();
    await expect(page.getByRole('region', { name: 'Match columns' })).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect(page.getByRole('region', { name: 'Match columns' })).toHaveCount(0);

    // A remembered match the file's headers agree with imports at once.
    const plain = 'Sample,R1\nP1,s3://b/P1_R1.fastq.gz\n';
    page.once('dialog', (d) => d.accept());
    await importCsv(page, 'core3.csv', plain);
    await expect(page.getByRole('region', { name: 'Match columns' })).toHaveCount(0);
    await expect(page.getByText('Imported 1 row from core3.csv.')).toBeVisible();
  });
});

for (const theme of THEMES) {
  test(`the new wizard pieces are accessible (${theme})`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto(RUN_TAB);
    await expect(heading(page, 'Add samples')).toBeVisible();
    await importCsv(page, 'core.csv', 'Sample,R1\nP1,s3://b/P1_R1.fastq.gz\n');
    await expect(page.getByRole('region', { name: 'Match columns' })).toBeVisible();
    await expectNoSeriousA11yViolations(page);
    await page.getByRole('button', { name: 'Cancel' }).click();
    await importCsv(page, 'mixed.csv', MIXED);
    await expect(page.getByText('⚠ Problems in rows 2 and 3')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
    page.once('dialog', (d) => d.accept()); // Clear asks first
    await page.getByRole('button', { name: 'Clear' }).click();
    await startFromExample(page);
    await next(page).click();
    await page.getByRole('button', { name: 'About Aligner' }).click();
    await expectNoSeriousA11yViolations(page);
    await next(page).click();
    await page.locator('details.review-more > summary').click();
    await expectNoSeriousA11yViolations(page);
  });
}
