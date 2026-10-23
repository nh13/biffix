import { API, test, expect, nextFrame, preflight } from '../fixtures';

// A deploy of more than one project (the mock has `genomics`, the default, and `imported`, with
// its own bucket and data source, and uploads off): the Data page offers a choice of project, kept
// in the URL, and a version's launch form browses its own project's sources.

const UMI_RUN_TAB = '/workflows/112233445566%3Av2/run';

/** The `project` each data read the page sends names (`null`: none, the default project). */
function projectsNamed(page: import('@playwright/test').Page) {
  const named: (string | null)[] = [];
  page.on('request', (r) => {
    const path = new URL(r.url()).pathname;
    if (path === '/data-sources') named.push(new URL(r.url()).searchParams.get('project'));
    else if (path === '/data/list' || path.startsWith('/data/uploads'))
      named.push((r.postDataJSON() as { project?: string })?.project ?? null);
  });
  return named;
}

test('the Data page switches project, keeping it in the URL', async ({ page }) => {
  const named = projectsNamed(page);
  await page.goto('/data');
  const choice = page.getByLabel('Project');
  await expect(choice).toHaveValue('genomics');
  const sources = page.getByRole('navigation', { name: 'Data sources' });
  await expect(sources.getByRole('link', { name: /Sequencing core/ })).toBeVisible();
  // The default project is browsed without naming it, as on a deploy of one.
  expect(named.length).toBeGreaterThan(0);
  expect(named.every((p) => p === null)).toBe(true);

  await choice.selectOption('imported');
  await expect(page).toHaveURL(/\/data\?project=imported$/);
  await expect(sources.getByRole('link', { name: /Benchmark reads/ })).toBeVisible();
  await expect(sources.getByRole('link', { name: /Sequencing core/ })).toHaveCount(0);
  // Its own bucket, under the same source id as the default project's.
  await expect(page.getByRole('link', { name: /results/ })).toBeVisible();
  await sources.getByRole('link', { name: /Benchmark reads/ }).click();
  await page.getByRole('link', { name: /hg002/ }).click();
  await expect(page).toHaveURL(/\/data\/bench-data\?project=imported#path=hg002%2F$/);
  await expect(page.getByRole('cell', { name: 'HG002_R1.fastq.gz', exact: true })).toBeVisible();
  expect(named).toContain('imported');

  // A link to it lands in the same place.
  await page.goto('/data/bench-data?project=imported#path=hg002%2F');
  await expect(page.getByLabel('Project')).toHaveValue('imported');
  await expect(page.getByRole('cell', { name: 'HG002_R2.fastq.gz', exact: true })).toBeVisible();

  // Back to the default project: its URL names none.
  await page.getByLabel('Project').selectOption('genomics');
  await expect(page).toHaveURL(/\/data$/);
  await expect(sources.getByRole('link', { name: /Sequencing core/ })).toBeVisible();
});

test("a slow answer for the project left behind never replaces the current one's", async ({
  page,
}) => {
  // Hold the imported project's sources until the page is back on the default project.
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  await page.route(
    (url) =>
      url.href.startsWith(`${API}/data-sources`) && url.searchParams.get('project') === 'imported',
    async (route) => {
      if (await preflight(route)) return;
      await held;
      await route.continue();
    },
  );
  await page.goto('/data');
  const sources = page.getByRole('navigation', { name: 'Data sources' });
  await expect(sources.getByRole('link', { name: /Sequencing core/ })).toBeVisible();
  const choice = page.getByLabel('Project');
  await choice.selectOption('imported');
  await expect(page).toHaveURL(/\/data\?project=imported$/);
  await choice.selectOption('genomics');
  await expect(page).toHaveURL(/\/data$/);
  await expect(sources.getByRole('link', { name: /Sequencing core/ })).toBeVisible();

  // The imported answer lands last: the page stays on the default project's sources.
  const late = page.waitForResponse(
    (r) =>
      r.request().method() === 'GET' && new URL(r.url()).searchParams.get('project') === 'imported',
  );
  release();
  await late;
  await nextFrame(page);
  await expect(sources.getByRole('link', { name: /Sequencing core/ })).toBeVisible();
  await expect(sources.getByRole('link', { name: /Benchmark reads/ })).toHaveCount(0);
  await expect(page.getByText('Loading…')).toHaveCount(0);
});

test('a link to a project the deploy does not have says so', async ({ page }) => {
  await page.goto('/data?project=nope');
  await expect(page.getByRole('alert')).toContainText('project `nope` is not configured');
});

for (const view of ['expert', 'lab'] as const) {
  test.describe(`${view} view`, () => {
    test.use({ view });
    test("a version's file picker browses its own project, which allows no uploads", async ({
      page,
    }) => {
      const named = projectsNamed(page);
      await page.goto(UMI_RUN_TAB);
      await page.getByRole('button', { name: 'Browse for Reads' }).click();
      const picker = page.getByRole('dialog', { name: /Choose a file for Reads/ });
      await expect(picker.getByRole('button', { name: /Benchmark reads/ })).toBeVisible();
      await expect(picker.getByRole('button', { name: /Sequencing core/ })).toHaveCount(0);
      // The project's own bucket opens first, and uploads are off in this project.
      await expect(picker.getByRole('button', { name: 'results' })).toBeVisible();
      await expect(picker.getByRole('button', { name: 'Upload files…' })).toHaveCount(0);
      await picker.getByRole('button', { name: /Benchmark reads/ }).click();
      await picker.getByRole('button', { name: 'hg002' }).click();
      await picker.getByText('HG002_R1.fastq.gz', { exact: true }).click();
      await picker.getByRole('button', { name: 'Use this file' }).click();
      await expect(picker).toBeHidden();
      await expect(page.getByRole('textbox', { name: /^Reads/ })).toHaveValue(
        's3://acme-imported-data/hg002/HG002_R1.fastq.gz',
      );
      expect(named.length).toBeGreaterThan(0);
      expect(named.every((p) => p === 'imported')).toBe(true);
    });
  });
}
