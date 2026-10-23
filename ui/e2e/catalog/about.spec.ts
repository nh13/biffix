import { test, expect, expectNoSeriousA11yViolations, workflowTabs as tabs } from '../fixtures';

// A workflow version's About page, from the mock's nf-core/rnaseq README.

test('the About tab renders the Markdown safely', async ({ page }) => {
  await page.goto('/workflows/aabbccddeeff%3Av1/about');
  await expect(tabs(page).getByRole('link', { name: 'about' })).toHaveAttribute(
    'aria-current',
    'page',
  );
  const about = page.locator('article.about');
  await expect(about.getByRole('heading', { name: 'nf-core/rnaseq', level: 1 })).toBeVisible();
  await expect(about.getByRole('cell', { name: 'Read 1 FASTQ' })).toBeVisible();
  // Absolute links open in a new tab without an opener; relative ones are only text.
  await expect(about.getByRole('link', { name: 'nf-core docs' })).toHaveAttribute(
    'rel',
    'noopener noreferrer',
  );
  await expect(about.getByRole('link', { name: 'usage' })).toHaveCount(0);
  await expect(about).toContainText('See usage.');
  // The README's <script> was stripped, not just left inert ({@html} never runs a script, so only
  // its absence shows the sanitizer ran).
  await expect(about.locator('script')).toHaveCount(0);
  expect(
    await page.evaluate(() => (window as { __aboutScriptRan?: boolean }).__aboutScriptRan),
  ).toBe(undefined);
  await expectNoSeriousA11yViolations(page);
});

test('the Run tab introduces the workflow and links to About', async ({ page }) => {
  await page.goto('/workflows/aabbccddeeff%3Av1/run');
  const intro = page.locator('.intro');
  await expect(intro).toContainText('RNA-seq analysis: align reads, quantify genes and run QC.');
  await intro.getByRole('link', { name: 'More about this workflow →' }).click();
  await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/about$/);
});

test('a version without an About page has no About tab', async ({ page }) => {
  await page.goto('/workflows/112233445566%3Av2/run');
  await expect(tabs(page).getByRole('link', { name: 'run' })).toBeVisible();
  await expect(tabs(page).getByRole('link', { name: 'about' })).toHaveCount(0);
  await expect(page.locator('.intro')).toHaveCount(0);
});

test('New run shows each workflow’s summary', async ({ page }) => {
  await page.goto('/runs/new');
  await expect(
    page.getByRole('link', { name: /nf-core\/rnaseq/ }).getByText('RNA-seq analysis: align reads'),
  ).toBeVisible();
});
