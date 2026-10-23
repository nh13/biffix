import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  THEMES,
  useTheme,
  RUN_TAB,
  browseGenomeFasta,
} from '../fixtures';

// The file picker: browsing the data sources from a path field of the Run tab.

test('browse a data source and pick a file into a path field', async ({ page }) => {
  await browseGenomeFasta(page);
  const picker = page.getByRole('dialog', { name: /Choose a file for Genome FASTA/ });
  await expect(picker).toBeVisible();
  // The project bucket opens first; the registered sources are listed by name.
  await picker.getByRole('button', { name: /Reference genomes/ }).click();
  await picker.getByRole('button', { name: 'GRCh38' }).click();
  await expect(picker.getByRole('navigation', { name: 'Folder path' })).toContainText('GRCh38');
  await picker.getByText('genome.fa', { exact: true }).click();
  await picker.getByRole('button', { name: 'Use this file' }).click();
  await expect(picker).toBeHidden();
  await expect(page.getByRole('textbox', { name: /^Genome\ FASTA/ })).toHaveValue(
    's3://shared-refs/GRCh38/genome.fa',
  );
});

test('the picker reopens at the current value, and breadcrumbs go back up', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use my own files…' }).click();
  await page
    .getByRole('textbox', { name: /^Genome\ FASTA/ })
    .fill('s3://seq-core-deliveries/lab-x/2026/batch-12/x.fa');
  await page.getByRole('button', { name: 'Browse for Genome FASTA' }).click();
  const picker = page.getByRole('dialog');
  await expect(picker.getByText('P01_S1_L001_R1_001.fastq.gz')).toBeVisible();
  await picker
    .getByRole('navigation', { name: 'Folder path' })
    .getByRole('button', { name: '2026' })
    .click();
  await expect(picker.getByRole('button', { name: 'batch-13' })).toBeVisible();
});

test('search finds files in subfolders, and a pasted path is used as is', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use an existing samplesheet file' }).click();
  await page.getByRole('button', { name: 'Browse for Samplesheet' }).click();
  const picker = page.getByRole('dialog');
  await picker.getByRole('button', { name: /Sequencing core/ }).click();
  await picker.getByLabel('Filter this folder by name').fill('P04');
  await picker.getByRole('button', { name: 'Search subfolders' }).click();
  await expect(picker.getByText('2026/batch-13/P04_S1_L001_R1_001.fastq.gz')).toBeVisible();
  await picker.getByLabel('Paste an s3 path').fill('s3://somewhere/else/sheet.csv');
  await picker.getByRole('button', { name: 'Use this path' }).click();
  await expect(page.getByRole('textbox', { name: /^Samplesheet/ })).toHaveValue(
    's3://somewhere/else/sheet.csv',
  );
});

test('a folder field picks the folder being shown', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Browse for Results folder' }).click();
  const picker = page.getByRole('dialog', { name: /Choose a folder/ });
  // A search finds files, which a folder field cannot use: not offered.
  await expect(picker.getByRole('button', { name: 'Search subfolders' })).toHaveCount(0);
  await picker.getByRole('button', { name: 'uploads' }).click();
  await picker.getByRole('button', { name: 'Use this folder' }).click();
  await expect(page.getByRole('textbox', { name: /^Results\ folder/ })).toHaveValue(
    's3://genomics-project/uploads/',
  );
});

test('an unreadable source says what an admin should check', async ({ page }) => {
  await browseGenomeFasta(page);
  const picker = page.getByRole('dialog');
  await picker.getByRole('button', { name: /Locked archive/ }).click();
  await expect(picker.getByRole('alert')).toContainText('an admin should check its grant');
  await page.keyboard.press('Escape');
  await expect(picker).toBeHidden();
});

for (const theme of THEMES) {
  test(`the picker (${theme}) has no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    await browseGenomeFasta(page);
    const picker = page.getByRole('dialog');
    await picker.getByRole('button', { name: /Sequencing core/ }).click();
    await picker.getByRole('button', { name: '2026' }).click();
    await picker.getByRole('button', { name: 'batch-12' }).click();
    await expect(picker.getByText('SampleSheet.csv')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}
