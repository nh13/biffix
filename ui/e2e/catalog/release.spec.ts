import type { Page } from '@playwright/test';
import { test, expect, onApi, expectNoSeriousA11yViolations, CORS } from '../fixtures';

// Released vs developer versions: the Workflows pages list pipelines with a released version
// unless Developer is on (remembered), each version says which it is, a developer releases (and
// un-releases) a version from its page, and a launch from the catalog takes the newest released
// version. No test changes the shared mock's releases: each answers its own.

const DEV = '7a8b9c0d1e2f:dev1';
const DEV_RUN_TAB = `/workflows/${encodeURIComponent(DEV)}/run`;

type Change = { released: boolean; at_micros: number; by?: string; by_label?: string };

/** A version's current release as the server serves it (`VersionRelease::of`). */
const releaseOf = (history: Change[]) =>
  history[0]?.released
    ? {
        released: true,
        at_micros: history[0].at_micros,
        ...(history[0].by ? { by: history[0].by } : {}),
        ...(history[0].by_label ? { by_label: history[0].by_label } : {}),
      }
    : { released: false };

/**
 * Answers this page's `GET /workflows/{DEV}` (its `release` and `can_release`) and `PUT
 * /workflows/{DEV}/release` from a store of its own, starting from `history`, the caller allowed
 * to change it or not.
 */
async function releaseFor(page: Page, canChange: boolean, history: Change[] = []) {
  const detailPath = `/workflows/${encodeURIComponent(DEV)}`;
  const path = `${detailPath}/release`;
  await onApi(page, 'GET', detailPath, async (route) => {
    const detail = (await (await route.fetch()).json()) as Record<string, unknown>;
    await route.fulfill({
      json: { ...detail, release: releaseOf(history), can_release: canChange },
      headers: CORS,
    });
  });
  const puts: boolean[] = [];
  await onApi(page, 'PUT', path, async (route) => {
    const { released } = route.request().postDataJSON() as { released: boolean };
    puts.push(released);
    if ((history[0]?.released ?? false) !== released)
      history = [
        {
          released,
          at_micros: Date.now() * 1000,
          by: 'alice',
          by_label: 'alice@lab.example.org',
        },
        ...history,
      ];
    await route.fulfill({
      json: { version_id: DEV, release: releaseOf(history), history, can_change: true },
      headers: CORS,
    });
  });
  return puts;
}

/** The mock's list, nf-core/rnaseq given a newer developer version (`ffeeddccbbaa:v9`). */
async function withNewerDeveloperRnaseq(page: Page) {
  await onApi(page, 'GET', '/workflows', async (route) => {
    const list = (await (await route.fetch()).json()) as Record<string, unknown>[];
    const v1 = list.find((w) => w.id === 'aabbccddeeff:v1')!;
    const newer = {
      ...v1,
      id: 'ffeeddccbbaa:v9',
      git_sha: 'ffeeddccbbaa',
      created_at_micros: Date.now() * 1000,
      release: { released: false },
      run_count: 0,
    };
    delete newer.last_run_at;
    await route.fulfill({ json: [newer, ...list], headers: CORS });
  });
}

test.describe('the Lab catalog', () => {
  test.use({ view: 'lab' });

  test('lists released pipelines by default; Developer shows the rest, and is remembered', async ({
    page,
  }) => {
    await page.goto('/workflows');
    await expect(page.getByRole('article')).toHaveCount(2);
    await expect(page.getByRole('article', { name: 'wgs-germline' })).toHaveCount(0);
    await expect(page.getByTestId('developer-note')).toContainText('1 pipeline in development');
    const rnaseq = page.getByRole('article', { name: 'Bulk RNA-seq' });
    await expect(rnaseq.getByTestId('release-badge')).toHaveText('Released');

    await page.getByRole('checkbox', { name: 'Developer' }).check();
    await expect(page.getByRole('article')).toHaveCount(3);
    const dev = page.getByRole('article', { name: 'wgs-germline' });
    await expect(dev.getByTestId('release-badge')).toHaveText('Developer');
    await expect(page.getByTestId('developer-note')).toHaveCount(0);

    await page.reload();
    await expect(page.getByRole('checkbox', { name: 'Developer' })).toBeChecked();
    await expect(page.getByRole('article', { name: 'wgs-germline' })).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });

  test('Start launches the newest released version, not a newer developer one', async ({
    page,
  }) => {
    // nf-core/rnaseq gains a newer, developer version.
    await withNewerDeveloperRnaseq(page);
    await page.goto('/workflows');
    const card = page.getByRole('article', { name: 'Bulk RNA-seq' });
    await expect(card.getByTestId('release-badge')).toHaveText('Released');
    await expect(card.getByRole('link', { name: 'Start Bulk RNA-seq' })).toHaveAttribute(
      'href',
      '/workflows/aabbccddeeff%3Av1/run',
    );
    // New run's step 1 is the same catalog.
    await page.goto('/runs/new');
    await expect(
      page
        .getByRole('article', { name: 'Bulk RNA-seq' })
        .getByRole('link', { name: 'Start Bulk RNA-seq' }),
    ).toHaveAttribute('href', '/workflows/aabbccddeeff%3Av1/run');
  });

  test('with nothing released, the catalog says why and offers Developer', async ({ page }) => {
    await onApi(page, 'GET', '/workflows', async (route) => {
      const list = (await (await route.fetch()).json()) as Record<string, unknown>[];
      await route.fulfill({
        json: list.map((w) => ({ ...w, release: { released: false } })),
        headers: CORS,
      });
    });
    await page.goto('/workflows');
    const status = page.getByRole('status').filter({ hasText: 'No pipeline has a released' });
    await expect(status).toContainText('all 3 pipelines are in development');
    await expect(page.getByRole('article')).toHaveCount(0);
    await status.getByRole('button', { name: 'Show developer pipelines (3)' }).click();
    await expect(page.getByRole('article')).toHaveCount(3);
    await expect(page.getByRole('checkbox', { name: 'Developer' })).toBeChecked();
  });
});

test('the Expert list shows released workflows with their badges; Developer adds the rest', async ({
  page,
}) => {
  await page.goto('/workflows');
  const rows = page.locator('tbody tr:not(.sub-row)');
  await expect(rows).toHaveCount(2);
  await expect(rows.filter({ hasText: 'nf-core/rnaseq' }).getByTestId('release-badge')).toHaveText(
    'Released',
  );
  await expect(page.getByTestId('developer-note')).toContainText('1 workflow with no released');
  await page.getByRole('checkbox', { name: 'Developer' }).check();
  await expect(rows).toHaveCount(3);
  await expect(rows.filter({ hasText: 'wgs-germline' }).getByTestId('release-badge')).toHaveText(
    'Developer',
  );
  // Expert New run follows the same filter, and says which version a choice launches.
  await page.goto('/runs/new');
  const choice = page.getByRole('link', { name: /wgs-germline/ });
  await expect(choice).toContainText('Developer');
  await expect(choice).toHaveAttribute('href', DEV_RUN_TAB);
  await page.getByRole('checkbox', { name: 'Developer' }).uncheck();
  await expect(page.getByRole('link', { name: /wgs-germline/ })).toHaveCount(0);
  await expect(page.getByRole('link', { name: /nf-core\/rnaseq/ })).toContainText('Released');
});

test('a developer releases a version, with a confirmation, and can take it back', async ({
  page,
}) => {
  const puts = await releaseFor(page, true);
  await page.goto(DEV_RUN_TAB);
  const ctl = page.getByTestId('release-ctl');
  await expect(ctl.getByTestId('release-badge')).toHaveText('Developer');

  await ctl.getByRole('button', { name: 'Release this version' }).click();
  const confirm = ctl.getByRole('group', { name: 'Confirm the release change' });
  await expect(confirm).toContainText('Release this version?');
  // Cancel changes nothing.
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  expect(puts).toEqual([]);

  await ctl.getByRole('button', { name: 'Release this version' }).click();
  await ctl.getByRole('button', { name: 'Yes, release' }).click();
  await expect(ctl.getByTestId('release-badge')).toHaveText('Released');
  await expect(ctl).toContainText('by alice@lab.example.org');
  expect(puts).toEqual([true]);
  await expectNoSeriousA11yViolations(page);

  await ctl.getByRole('button', { name: 'Un-release this version' }).click();
  await expect(ctl.getByRole('group', { name: 'Confirm the release change' })).toContainText(
    'It becomes a developer version',
  );
  await ctl.getByRole('button', { name: 'Yes, un-release' }).click();
  await expect(ctl.getByTestId('release-badge')).toHaveText('Developer');
  expect(puts).toEqual([true, false]);
});

test('someone who may not release sees the badge only', async ({ page }) => {
  await releaseFor(page, false, [
    { released: true, at_micros: Date.now() * 1000, by: 'bob', by_label: 'bob@lab.example.org' },
  ]);
  await page.goto(DEV_RUN_TAB);
  const ctl = page.getByTestId('release-ctl');
  await expect(ctl.getByTestId('release-badge')).toHaveText('Released');
  await expect(ctl).toContainText('by bob@lab.example.org');
  await expect(ctl.getByRole('button')).toHaveCount(0);
});

test('an Expert row describes the version it opens, not a newer developer one', async ({
  page,
}) => {
  await withNewerDeveloperRnaseq(page);
  await page.goto('/workflows');
  const row = page.locator('tbody tr:not(.sub-row)').filter({ hasText: 'nf-core/rnaseq' });
  await expect(row.getByRole('link', { name: 'nf-core/rnaseq' })).toHaveAttribute(
    'href',
    '/workflows/aabbccddeeff%3Av1',
  );
  const version = row.getByTestId('row-version');
  await expect(version.getByTestId('release-badge')).toHaveText('Released');
  await expect(version).toContainText('aabbccddee');
  await expect(row.getByTestId('newer-developer')).toContainText('ffeeddccbb');
  // The other version is a sub-row, once.
  await row.getByRole('button', { name: /2 versions/ }).click();
  const subs = page.locator('tbody tr.sub-row');
  await expect(subs).toHaveCount(1);
  await expect(subs.getByTestId('release-badge')).toHaveText('Developer');
  // Developer on: the row is the newest version.
  await page.getByRole('checkbox', { name: 'Developer' }).check();
  await expect(row.getByRole('link', { name: 'nf-core/rnaseq' })).toHaveAttribute(
    'href',
    '/workflows/ffeeddccbbaa%3Av9',
  );
  await expect(row.getByTestId('row-version').getByTestId('release-badge')).toHaveText('Developer');
  await expect(row.getByTestId('newer-developer')).toHaveCount(0);
});

test('a version whose release cannot be read shows an error, not a developer version', async ({
  page,
}) => {
  await onApi(page, 'GET', `/workflows/${encodeURIComponent(DEV)}`, (route) =>
    route.fulfill({ status: 503, body: 'releases unavailable', headers: CORS }),
  );
  await page.goto(DEV_RUN_TAB);
  await expect(page.getByText(/couldn't load this workflow/)).toBeVisible();
  await expect(page.getByTestId('release-badge')).toHaveCount(0);
});
