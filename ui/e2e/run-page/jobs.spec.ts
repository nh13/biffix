import type { Page } from '@playwright/test';
import { test, expect, CORS, onApi, runTabs } from '../fixtures';

// The Jobs view keeps its state (search text, the selected job: a task key names a sample) in the
// URL fragment, which the browser never sends to a server, and asks the API with a POST body.

const pane = (page: Page, key: string) => page.getByRole('complementary', { name: `Job ${key}` });

test('a link with the view in the fragment opens that view', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs#q=align&job=align%3Asample%3DHG00096');
  await expect(pane(page, 'align:sample=HG00096')).toBeVisible();
});

test('j, k and Escape leave the jobs alone while the nav drawer is open', async ({ page }) => {
  await page.setViewportSize({ width: 420, height: 800 });
  await page.goto('/runs/run_01HGW3align/jobs#job=align%3Asample%3DHG00096');
  await expect(pane(page, 'align:sample=HG00096')).toBeVisible();
  const menu = page.getByRole('button', { name: 'Menu' });
  await menu.click();
  await page.keyboard.press('j');
  await page.keyboard.press('Escape');
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
  // Escape closed the drawer, not the job's pane; j moved nothing.
  await expect(page).toHaveURL(/#job=align%3Asample%3DHG00096$/);
  await expect(pane(page, 'align:sample=HG00096')).toBeVisible();
});

test('a job cheaper than a cent reads as the table words it', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs#job=fastqc%3Asample%3DHG00096');
  const jobPane = pane(page, 'fastqc:sample=HG00096');
  await expect(jobPane.getByTitle('Estimated, all attempts')).toHaveText('<$0.01');
});

test('back and forward walk the selected jobs through the fragment', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  await page.getByRole('button', { name: 'align:sample=HG00096' }).click();
  await expect(page).toHaveURL(/#job=align%3Asample%3DHG00096$/);
  await page.getByRole('button', { name: 'align:sample=HG00097' }).click();
  await expect(page).toHaveURL(/#job=align%3Asample%3DHG00097$/);
  await expect(pane(page, 'align:sample=HG00097')).toBeVisible();

  await page.goBack();
  await expect(page).toHaveURL(/#job=align%3Asample%3DHG00096$/);
  await expect(pane(page, 'align:sample=HG00096')).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/\/jobs$/);
  await expect(page.getByRole('complementary', { name: /^Job / })).toHaveCount(0);
  await page.goForward();
  await expect(pane(page, 'align:sample=HG00096')).toBeVisible();
});

test('leaving the tab while a search is unsaved leaves the next page’s URL alone', async ({
  page,
}) => {
  // As on the Outputs tab: the search reports a quarter second after the last key.
  await page.clock.install();
  await page.goto('/runs/run_01HGW3align/jobs');
  await page.getByLabel('Search task keys').pressSequentially('align');
  await runTabs(page).getByRole('link', { name: 'Outputs' }).click();
  await expect(page).toHaveURL(/\/runs\/run_01HGW3align\/outputs$/);
  await page.clock.runFor(1_000);
  await expect(page).toHaveURL(/\/runs\/run_01HGW3align\/outputs$/);
});

test("a job's pane names the image it ran in when the engine reported one", async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs#job=align%3Asample%3DHG00097');
  await expect(pane(page, 'align:sample=HG00097')).toContainText(
    '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:d3r1ved',
  );
  await page.getByRole('button', { name: 'align:sample=HG00096' }).click();
  await expect(pane(page, 'align:sample=HG00096')).not.toContainText('@sha256:');
});

test('back to an earlier search sets the search box again', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  const search = page.getByLabel('Search task keys');
  await search.fill('markdup');
  await expect(page).toHaveURL(/#q=markdup$/);
  await page.getByRole('button', { name: 'markdup:sample=HG00096' }).click();
  await expect(page).toHaveURL(/#q=markdup&job=/);
  // Leave the Jobs view and come back through history: the fragment restores the view.
  await runTabs(page).getByRole('link', { name: /^Cost/ }).click();
  await expect(page).toHaveURL(/\/cost$/);
  await page.goBack();
  await expect(search).toHaveValue('markdup');
  await expect(pane(page, 'markdup:sample=HG00096')).toBeVisible();
});

test('the search box keeps what is typed while the URL catches up', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  const search = page.getByLabel('Search task keys');
  await search.pressSequentially('align ');
  await expect(page).toHaveURL(/#q=align$/);
  // The URL's trimmed echo (landed: the URL says so) does not take the trailing space back out.
  await expect(search).toHaveValue('align ');
  await search.pressSequentially('sample=HG00097');
  await expect(page).toHaveURL(/#q=align\+sample%3DHG00097$/);
  await expect(search).toHaveValue('align sample=HG00097');
});

test('no task key or search text reaches a request URL', async ({ page }) => {
  const tasksRequests: { method: string; url: string }[] = [];
  page.on('request', (r) => {
    // A CORS preflight carries no body; only the request it clears counts.
    if (/\/runs\/[^/]+\/tasks/.test(r.url()) && r.method() !== 'OPTIONS')
      tasksRequests.push({ method: r.method(), url: r.url() });
  });
  await page.goto('/runs/run_01HGW9big/jobs#job=markdup%3Asample%3DS0999');
  await expect(pane(page, 'markdup:sample=S0999')).toBeVisible();
  await page.getByLabel('Search task keys').fill('S0998');
  await expect(page.getByRole('button', { name: 'markdup:sample=S0998' })).toBeVisible();
  expect(tasksRequests.length).toBeGreaterThan(0);
  for (const r of tasksRequests) {
    expect(r.method).toBe('POST');
    expect(r.url).not.toMatch(/S099|markdup/);
  }
});

test('a big run pages its jobs from the server, and a filter change shows only the new filter', async ({
  page,
}) => {
  await page.goto('/runs/run_01HGW9big/jobs');
  await expect(page.getByText('Showing 200 of 3000')).toBeVisible();
  await expect(page.locator('tbody tr')).toHaveCount(200);
  await page.getByRole('button', { name: 'Show 200 more' }).click();
  await expect(page.getByText('Showing 400 of 3000')).toBeVisible();
  await expect(page.locator('tbody tr')).toHaveCount(400);
  await page.getByRole('radio', { name: /^failed/ }).click();
  await expect(page).toHaveURL(/state=failed/);
  await expect(page.getByText('31 jobs')).toBeVisible();
  await expect(page.locator('tbody tr')).toHaveCount(31);
  await expect(page.locator('tbody .badge').filter({ hasNotText: 'failed' })).toHaveCount(0);
});

test('the state filters are one tab stop, moved with the arrow keys', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  const all = page.getByRole('radio', { name: /^all/ });
  await all.focus();
  await page.keyboard.press('ArrowRight');
  await expect(page).toHaveURL(/#state=active$/);
  await expect(page.getByRole('radio', { name: /^active/ })).toBeFocused();
  await expect(page.getByRole('radio', { name: /^active/ })).toHaveAttribute('tabindex', '0');
  await expect(all).toHaveAttribute('tabindex', '-1');
  await page.keyboard.press('End');
  await expect(page).toHaveURL(/#state=failed$/);
});

test('a linked job opens even when its key is inside many other keys', async ({ page }) => {
  // `sample=P` is a substring of 250 other keys and sorts after them, so it is off the first page
  // and a substring search would bury it: the pane needs the exact-key lookup.
  await page.goto('/runs/run_01HGWBsubstr/jobs#job=sample%3DP');
  await expect(page.getByText('Showing 200 of 251')).toBeVisible();
  await expect(pane(page, 'sample=P')).toBeVisible();
});

test("a filter that cannot be loaded says so, and never shows the last filter's rows", async ({
  page,
}) => {
  await onApi(page, 'POST', '/runs/run_01HGW3align/tasks/query', async (route) => {
    if (route.request().postDataJSON()?.state === 'failed')
      return route.fulfill({ status: 500, headers: CORS, json: { error: 'tasks unavailable' } });
    return route.fallback();
  });
  await page.goto('/runs/run_01HGW3align/jobs');
  await expect(page.locator('tbody tr').first()).toBeVisible();
  await page.getByRole('radio', { name: /^failed/ }).click();
  await expect(page.getByText(/Couldn't load jobs/)).toBeVisible();
  await expect(page.locator('tbody tr')).toHaveCount(0);
});

test('a failed Show more says so and keeps the rows', async ({ page }) => {
  await onApi(page, 'POST', '/runs/run_01HGWBsubstr/tasks/query', async (route) => {
    if ((route.request().postDataJSON()?.limit ?? 0) > 200)
      return route.fulfill({ status: 500, headers: CORS, json: { error: 'tasks unavailable' } });
    return route.fallback();
  });
  await page.goto('/runs/run_01HGWBsubstr/jobs');
  await expect(page.locator('tbody tr')).toHaveCount(200);
  await page.getByRole('button', { name: /^Show \d+ more$/ }).click();
  await expect(page.getByRole('alert')).toContainText("Couldn't load jobs");
  await expect(page.locator('tbody tr')).toHaveCount(200);
});

test('a linked job off the loaded rows keeps up with a live run', async ({ page }) => {
  // The run is still going, and the job, off the first page, is running until a later poll.
  await onApi(page, 'GET', '/runs/run_01HGWBsubstr', async (route) => {
    const response = await route.fetch();
    const run = await response.json();
    await route.fulfill({
      response,
      json: { ...run, state: 'running', terminal: false, completed_at: null },
    });
  });
  let lookups = 0;
  await onApi(page, 'POST', '/runs/run_01HGWBsubstr/tasks/query', async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    if (route.request().postDataJSON()?.task_key !== 'sample=P' || lookups++ > 0)
      return route.fulfill({ response, json: body });
    const tasks = body.tasks.map((t: object) => ({ ...t, state: 'running' }));
    return route.fulfill({ response, json: { ...body, tasks } });
  });
  await page.clock.install();
  await page.goto('/runs/run_01HGWBsubstr/jobs#job=sample%3DP');
  const badge = pane(page, 'sample=P').locator('.badge').first();
  await expect(badge).toHaveText('running');
  await page.clock.runFor(5_000);
  await expect(badge).toHaveText('succeeded');
});

test("a job's logs, attempts and outputs are asked for without its key in a request URL", async ({
  page,
}) => {
  const urls: { method: string; url: string }[] = [];
  page.on('request', (r) => {
    if (/\/runs\/[^/]+\/(task-logs|attempts|outputs)/.test(r.url()))
      urls.push({ method: r.method(), url: r.url() });
  });
  await page.goto('/runs/run_01HGW3align/jobs#job=align%3Asample%3DHG00096');
  const jobPane = pane(page, 'align:sample=HG00096');
  await expect(jobPane.getByText('[mock] align:sample=HG00096: log line 1')).toBeVisible();
  await expect.poll(() => urls.some((u) => u.url.includes('/task-logs'))).toBe(true);
  await expect.poll(() => urls.some((u) => u.url.includes('/attempts'))).toBe(true);

  // A job's outputs (a run that has some).
  await page.goto('/runs/run_01HGW2wgs30x/jobs#job=align%3Asample%3DNA12878');
  const outputsPane = pane(page, 'align:sample=NA12878');
  await outputsPane.getByRole('button', { name: 'Outputs' }).click();
  await expect(outputsPane.getByText('NA12878.bam', { exact: false }).first()).toBeVisible();
  await expect.poll(() => urls.some((u) => u.url.includes('/outputs'))).toBe(true);

  for (const u of urls.filter((x) => x.method !== 'OPTIONS')) {
    expect(u.method).toBe('POST');
    expect(u.url).not.toMatch(/HG00096|NA12878|align%3A|align:/);
  }
});

test('the jobs table and a job pane show what each job requested', async ({ page }) => {
  await page.goto('/runs/run_01HGW3align/jobs');
  const table = page.getByRole('table');
  await expect(table.locator('th', { hasText: 'Requested' })).toBeVisible();
  const row = (key: string) =>
    table.locator('tr').filter({ has: page.getByRole('button', { name: key, exact: true }) });
  const requested = (key: string) => row(key).locator('td.requested');
  await expect(requested('fastqc:sample=HG00096')).toHaveText('1 vCPU · 2.0 GiB');
  await expect(requested('align:sample=HG00096')).toHaveText('8 vCPU · 29 GiB');
  // Queued (RUNNABLE, no host yet): its Batch job's request is already known.
  await expect(requested('joint_call')).toHaveText('16 vCPU · 60 GiB');
  // A Batch job whose request is unknown (a run recorded before biffix kept it): a dash read out
  // as "unknown", never a guess.
  await expect(requested('markdup:sample=HG00096')).toHaveText('--unknown');
  await expect(requested('markdup:sample=HG00096').locator('.sr-only')).toHaveText('unknown');
  // Queued, its host not known yet: the Host cell says so too, never an empty cell.
  await expect(row('joint_call').locator('td.host .sr-only')).toHaveText('unknown');

  await page.getByRole('button', { name: 'align:sample=HG00096', exact: true }).click();
  const jobPane = pane(page, 'align:sample=HG00096');
  await expect(jobPane).toContainText('requested 8 vCPU · 29 GiB');
  // The host's capacity is shown beside the request, as text (not only on hover).
  await expect(jobPane.getByText('host has 16 vCPU · 30 GiB')).toBeVisible();
  // Each attempt says what it asked for: the retry asked for more memory.
  const attempts = jobPane.getByRole('group', { name: 'Attempts' });
  await expect(attempts.getByRole('button', { name: /^attempt 1 / })).toContainText(
    'requested 8 vCPU, 15 GiB',
  );
  await expect(attempts.getByRole('button', { name: /^attempt 2 / })).toContainText(
    'requested 8 vCPU, 29 GiB',
  );
});

test('a task with no Batch job shows not-applicable, not an unknown request', async ({ page }) => {
  await page.goto('/runs/run_01HGW4resume/jobs');
  const table = page.getByRole('table');
  const row = table
    .locator('tr')
    .filter({ has: page.getByRole('button', { name: 'SAY (1)', exact: true }) });
  // Exit, Requested and Host all use the same marker, read out as not applicable.
  const na = row.locator('.sr-only', { hasText: 'not applicable: no Batch job' });
  await expect(na).toHaveCount(3);
  await expect(row.locator('td.requested')).toHaveText('--not applicable: no Batch job');

  await page.getByRole('button', { name: 'SAY (1)', exact: true }).click();
  await expect(pane(page, 'SAY (1)')).not.toContainText('requested');
});
