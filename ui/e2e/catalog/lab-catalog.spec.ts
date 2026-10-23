import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  useTheme,
  THEMES,
  API,
  nextFrame,
  CORS,
  preflight,
  workflowTabs,
} from '../fixtures';
import type { Page } from '@playwright/test';

// The Lab view's pipeline catalog: cards, filters, pins, and the trimmed pipeline page.

/**
 * Answers this page's `GET /workflows` from the mock, with `hidden` set for the named pipelines
 * (and `PUT /workflows/listing` updating that set), so no test changes the shared mock's state.
 * `runCounts` sets the named pipelines' run counts (none: no newest run), for a test that reads
 * them: other tests launch runs of the mock's pipelines while it runs. `registered` sets the named
 * pipelines' registration times (`created_at_micros`).
 */
async function listingFor(
  page: Page,
  hidden: string[] = [],
  runCounts: Record<string, number> = {},
  registered: Record<string, number> = {},
): Promise<Set<string>> {
  const set = new Set(hidden);
  await page.route(`${API}/workflows`, async (route) => {
    if (await preflight(route)) return;
    const list = (await (await route.fetch()).json()) as Record<string, unknown>[];
    const counted = (listed: Record<string, unknown>) => {
      const at = registered[listed.name as string];
      const w = at === undefined ? listed : { ...listed, created_at_micros: at };
      const n = runCounts[w.name as string];
      if (n === undefined) return w;
      const { last_run_at, ...rest } = w;
      return n > 0 ? { ...rest, run_count: n, last_run_at } : { ...rest, run_count: 0 };
    };
    await route.fulfill({
      json: list.map((w) => ({ ...counted(w), hidden: set.has(w.name as string) })),
      headers: CORS,
    });
  });
  await page.route(`${API}/workflows/listing`, async (route) => {
    if (await preflight(route)) return;
    const body = route.request().postDataJSON() as {
      project: string;
      name: string;
      hidden: boolean;
    };
    if (body.hidden) set.add(body.name);
    else set.delete(body.name);
    await route.fulfill({
      json: { ...body, updated_at_micros: Date.now() * 1000 },
      headers: CORS,
    });
  });
  return set;
}

test.describe('pipeline catalog', () => {
  test.use({ view: 'lab' });

  test('cards say what each pipeline does, and filter by tag and text', async ({ page }) => {
    await listingFor(page);
    await page.goto('/workflows');
    await expect(page.getByRole('heading', { name: 'Pipelines' })).toBeVisible();
    const rnaseq = page.getByRole('article', { name: 'Bulk RNA-seq' });
    await expect(rnaseq).toContainText('Verified');
    await expect(rnaseq).toContainText('Gene expression from paired- or single-end FASTQ files');
    await expect(rnaseq.getByRole('list', { name: 'Tags' })).toContainText('RNA-seq');
    await expect(page.getByRole('article')).toHaveCount(2);
    const chips = page.getByRole('group', { name: 'Filter by tag' });
    await chips.getByRole('button', { name: 'QC' }).click();
    // Only the pipeline carrying the tag is left.
    await expect(page.getByRole('article')).toHaveCount(1);
    await expect(page.getByRole('article', { name: 'Bulk RNA-seq' })).toBeVisible();
    await chips.getByRole('button', { name: 'All' }).click();
    await page.getByRole('searchbox', { name: 'Search pipelines' }).fill('consensus');
    await expect(page.getByRole('article')).toHaveCount(1);
    await expect(page.getByRole('article', { name: 'umi-consensus' })).toBeVisible();
  });

  for (const path of ['/', '/workflows'])
    test(`${path} reads the pipeline list once, however late the pins arrive`, async ({ page }) => {
      // The pins (in the preferences) arrive after the pipeline list was asked for.
      await page.route(`${API}/me/prefs`, async (route) => {
        if (route.request().method() === 'GET') await new Promise((r) => setTimeout(r, 800));
        await route.fallback();
      });
      let lists = 0;
      await page.route(`${API}/workflows`, async (route) => {
        if (route.request().method() === 'GET') lists++;
        await route.fallback();
      });
      const pins = page.waitForResponse(
        (r) => r.url() === `${API}/me/prefs` && r.request().method() === 'GET',
      );
      await page.goto(path);
      await expect.poll(() => lists).toBeGreaterThan(0);
      // Once the late pins have arrived and been handled, still the one list read.
      await pins;
      await nextFrame(page);
      expect(lists).toBe(1);
    });

  test('sorting orders the cards by use, name or registration', async ({ page }) => {
    // umi-consensus is the newer registration, so each sort after the first changes the order.
    const day = 86_400_000_000;
    await listingFor(
      page,
      [],
      { 'umi-consensus': 1, 'nf-core/rnaseq': 0 },
      { 'umi-consensus': Date.now() * 1000 - day, 'nf-core/rnaseq': Date.now() * 1000 - 5 * day },
    );
    await page.goto('/workflows');
    const names = () => page.getByRole('article').getByRole('heading').allTextContents();
    // Most used first: umi-consensus has a run, nf-core/rnaseq (titled Bulk RNA-seq) none.
    await expect.poll(names).toEqual(['umi-consensus', 'Bulk RNA-seq']);
    // By name: the title the card shows.
    await page.getByLabel('Sort').selectOption('name');
    await expect.poll(names).toEqual(['Bulk RNA-seq', 'umi-consensus']);
    // Newest registration first.
    await page.getByLabel('Sort').selectOption('updated');
    await expect.poll(names).toEqual(['umi-consensus', 'Bulk RNA-seq']);
  });

  test('a pinned pipeline is saved as one operation and shows on My work', async ({ page }) => {
    await listingFor(page);
    await page.goto('/workflows');
    const saved = page.waitForRequest((r) => r.url().endsWith('/me/prefs') && r.method() === 'PUT');
    await page.getByRole('button', { name: 'Pin Bulk RNA-seq' }).click();
    expect((await saved).postDataJSON()).toEqual({
      pin: JSON.stringify(['genomics', 'nf-core/rnaseq']),
    });
    await expect(page.getByRole('button', { name: 'Unpin Bulk RNA-seq' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await page.goto('/');
    const pinned = page.getByRole('region', { name: 'Pinned pipelines' });
    await expect(pinned.getByRole('article', { name: 'Bulk RNA-seq' })).toBeVisible();
  });

  test('a hidden pipeline is left out of the catalog, My work and New run', async ({ page }) => {
    const hidden = await listingFor(page);
    // Both pinned while listed, then umi-consensus hidden: My work keeps only the listed pin.
    await page.goto('/workflows');
    for (const name of ['Bulk RNA-seq', 'umi-consensus']) {
      const saved = page.waitForResponse(
        (r) => r.url().endsWith('/me/prefs') && r.request().method() === 'PUT',
      );
      await page.getByRole('button', { name: `Pin ${name}` }).click();
      await saved;
    }
    hidden.add('umi-consensus');
    await page.goto('/');
    const pinned = page.getByRole('region', { name: 'Pinned pipelines' });
    await expect(pinned.getByRole('article', { name: 'Bulk RNA-seq' })).toBeVisible();
    await expect(pinned.getByRole('article', { name: 'umi-consensus' })).toHaveCount(0);
    await page.goto('/workflows');
    await expect(page.getByRole('article')).toHaveCount(1);
    await expect(page.getByRole('article', { name: 'umi-consensus' })).toHaveCount(0);
    await page.goto('/runs/new');
    await expect(page.getByText('nf-core/rnaseq').first()).toBeVisible();
    await expect(page.getByText('umi-consensus')).toHaveCount(0);
  });

  test('Start opens the Run tab and About the About tab; the page offers only those', async ({
    page,
  }) => {
    await listingFor(page);
    await page.goto('/workflows');
    const card = page.getByRole('article', { name: 'Bulk RNA-seq' });
    await card.getByRole('link', { name: 'About Bulk RNA-seq' }).click();
    await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/about$/);
    await page.goBack();
    await page
      .getByRole('article', { name: 'Bulk RNA-seq' })
      .getByRole('link', { name: 'Start Bulk RNA-seq' })
      .click();
    await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/run$/);
    const tabs = workflowTabs(page);
    await expect(tabs.getByRole('link')).toHaveText(['run', 'about']);
    await expect(
      page.getByText('Gene expression from paired- or single-end FASTQ files'),
    ).toBeVisible();
  });

  for (const theme of THEMES) {
    test(`the catalog is accessible (${theme})`, async ({ page }) => {
      await listingFor(page);
      await useTheme(page, theme);
      await page.goto('/workflows');
      await expect(page.getByRole('article').first()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }
});

test('a developer hides a pipeline from the Lab listing, and it leaves the Lab catalog', async ({
  page,
}) => {
  const hidden = await listingFor(page);
  await page.goto('/workflows/112233445566%3Av2/run');
  await page.getByRole('button', { name: 'Hide from the Lab catalog' }).click();
  await expect(page.getByText('hidden from the Lab catalog')).toBeVisible();
  expect(hidden.has('umi-consensus')).toBe(true);
  const toggle = page.getByRole('group', { name: 'View' }).first();
  await toggle.getByRole('button', { name: 'Lab' }).click();
  await expect(toggle.getByRole('button', { name: 'Lab' })).toHaveAttribute('aria-pressed', 'true');
  await page.goto('/workflows');
  await expect(page.getByRole('article', { name: 'Bulk RNA-seq' })).toBeVisible();
  await expect(page.getByRole('article', { name: 'umi-consensus' })).toHaveCount(0);
  await page.goto('/workflows/112233445566%3Av2/run');
  await page
    .getByRole('group', { name: 'View' })
    .first()
    .getByRole('button', { name: 'Expert' })
    .click();
  await page.getByRole('button', { name: 'Show in the Lab catalog' }).click();
  await expect(page.getByRole('button', { name: 'Hide from the Lab catalog' })).toBeVisible();
  expect(hidden.has('umi-consensus')).toBe(false);
});
