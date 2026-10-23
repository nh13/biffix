import { test, expect, API, CORS, nextFrame, onApi, workflowTabs } from '../fixtures';

// The workflow page's tabs are routes, like the run page's.

test('workflow tabs have their own URLs', async ({ page }) => {
  await page.goto('/workflows/aabbccddeeff%3Av1/parameters');
  const tabs = workflowTabs(page);
  await expect(tabs.getByRole('link', { name: 'parameters' })).toHaveAttribute(
    'aria-current',
    'page',
  );
  await tabs.getByRole('link', { name: 'versions' }).click();
  await expect(page).toHaveURL(/\/workflows\/aabbccddeeff%3Av1\/versions$/);
  await expect(page.locator('th', { hasText: 'Registered' })).toBeVisible();
  await tabs.getByRole('link', { name: 'provenance' }).click();
  await expect(page).toHaveURL(/\/provenance$/);
  await expect(page.getByRole('heading', { name: 'Frozen id' })).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/\/versions$/);
});

test('a version that is not there is asked for once, not polled', async ({ page }) => {
  await page.clock.install();
  const asked: string[] = [];
  page.on('request', (r) => {
    if (r.url().startsWith(`${API}/workflows/nope%3Av1`)) asked.push(r.url());
  });
  await page.goto('/workflows/nope%3Av1');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
  await page.clock.runFor(30_000);
  expect(asked).toHaveLength(1);
});

test('the Versions tab says when the versions cannot be loaded, not an empty table', async ({
  page,
}) => {
  await onApi(page, 'GET', '/workflows', (route) =>
    route.fulfill({ status: 500, headers: CORS, json: { error: 'registry unavailable' } }),
  );
  await page.goto('/workflows/aabbccddeeff%3Av1/versions');
  await expect(page.getByRole('alert')).toContainText("Couldn't load this workflow's versions");
  await expect(page.locator('th', { hasText: 'Registered' })).toHaveCount(0);
});

test('a version still being built says why it cannot run and shows its images', async ({
  page,
}) => {
  await page.goto('/workflows/ccddeeff0011%3Apending/run');
  await expect(page.getByTestId('version-state')).toHaveText('building…');
  const notice = page.getByTestId('not-ready');
  await expect(notice).toContainText('being prepared');
  await expect(notice).toContainText('image `fastqc` is being imported');
  // No launch form until it can run.
  await expect(page.getByRole('button', { name: /launch/i })).toHaveCount(0);

  await notice.getByRole('link', { name: /engine and images/i }).click();
  await expect(page).toHaveURL(/\/provenance$/);
  await expect(page.getByRole('heading', { name: 'Engine' })).toBeVisible();
  const table = page.getByTestId('images-table');
  // The state is its own element: match it exactly, not anywhere in the row.
  await expect(table.getByRole('row', { name: /fastqc/ }).locator('.state')).toHaveText(
    'importing…',
  );
  await expect(table.getByRole('row', { name: /default/ }).locator('.state')).toHaveText(
    'imported',
  );
});

test('a Snakemake version shows the runtime being added to its images', async ({ page }) => {
  await page.goto('/workflows/ddeeff001122%3Aderiving/provenance');
  const table = page.getByTestId('images-table');
  const deriving = table.getByRole('row', { name: /default/ });
  await expect(deriving.locator('.state')).toHaveText('adding runtime…');
  await expect(deriving.getByTestId('derived-image')).toContainText('tasks run --');
  const derived = table.getByRole('row', { name: /bwa/ });
  await expect(derived.locator('.state')).toHaveText('ready');
  await expect(derived.getByTestId('derived-image')).toContainText('sha256:777777777777');
  await expect(derived.getByTestId('derived-image')).toContainText('snakemake 9.27.0 runtime');
});

test('a runnable version shows no state chip', async ({ page }) => {
  await page.goto('/workflows/aabbccddeeff%3Av1/run');
  await expect(page.getByRole('heading', { name: 'nf-core/rnaseq' })).toBeVisible();
  await expect(page.getByTestId('version-state')).toHaveCount(0);
  await expect(page.getByTestId('not-ready')).toHaveCount(0);
});

test('an image built from the workflow shows its recipe and each architecture', async ({
  page,
}) => {
  await page.goto('/workflows/eeff00112233%3Abuilding/provenance');
  const table = page.getByTestId('images-table');
  const pixi = table.getByRole('row', { name: /default/ }).first();
  await expect(pixi.getByTestId('image-kind')).toHaveText('pixi');
  await expect(pixi).toContainText('env');
  await expect(pixi.locator('.state')).toHaveText('failed');
  const archs = pixi.getByTestId('arch-builds');
  await expect(archs).toContainText('amd64');
  await expect(archs.getByRole('link', { name: '✗' })).toHaveAttribute('href', /build\/arm$/);
  await table.getByText('Generated Dockerfile').click();
  await expect(table.getByText(/pixi generator 1/)).toBeVisible();
  const align = table.getByRole('row', { name: /align/ });
  await expect(align.getByTestId('image-kind')).toHaveText('dockerfile');
  await expect(align).toContainText('docker/align/Dockerfile');
  await expect(align.locator('.state')).toHaveText('ready');
});

test('a re-registered version links the version it came from', async ({ page }) => {
  await page.goto('/workflows/eeff00112233%3Abuilding/provenance');
  const from = page.getByTestId('reregistered-from');
  await expect(from.getByRole('link', { name: 'aabbccddeeff:v1' })).toHaveAttribute(
    'href',
    '/workflows/aabbccddeeff%3Av1',
  );
  await expect(page.getByText('Engine release given')).toBeVisible();
});

test('while a version builds its detail is polled, but its version list is fetched once', async ({
  page,
}) => {
  await page.clock.install();
  const DETAIL = '/workflows/ccddeeff0011%3Apending';
  const lists: string[] = [];
  page.on('request', (r) => {
    if (new URL(r.url()).pathname === '/workflows') lists.push(r.url());
  });
  await page.goto(`${DETAIL}/run`);
  await expect(page.getByTestId('version-state')).toHaveText('building…');
  await expect.poll(() => lists.length).toBe(1);
  // The pending version is polled every 10 s, a new detail object each time with the same
  // (project, name).
  // After each of three polls has been answered and handled, still the one list read.
  for (let i = 0; i < 3; i++) {
    const polled = page.waitForResponse((r) => new URL(r.url()).pathname === DETAIL);
    await page.clock.runFor(10_000);
    await polled;
    await nextFrame(page);
    expect(lists).toHaveLength(1);
  }
});

test('a later poll that loads the version replaces a failed first load', async ({ page }) => {
  await page.clock.install();
  let failed = false;
  await page.route(/\/workflows\/ccddeeff0011%3Apending$/, (route) => {
    if (failed) return route.fallback();
    failed = true;
    return route.fulfill({ status: 503, body: 'try later' });
  });
  await page.goto('/workflows/ccddeeff0011%3Apending/run');
  await expect(page.getByText(/couldn't load this workflow/)).toBeVisible();
  await page.clock.runFor(11_000);
  await expect(page.getByTestId('version-state')).toHaveText('building…');
  await expect(page.getByText(/couldn't load this workflow/)).toHaveCount(0);
});

test('a copy button says when it copied', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/workflows/aabbccddeeff%3Av1/provenance');
  const frozen = page.locator('h3:text-is("Frozen id") + .prov-row');
  await frozen.getByRole('button', { name: 'copy' }).click();
  await expect(frozen.getByRole('button', { name: 'copied' })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('aabbccddeeff:v1');
});
