import type { Page } from '@playwright/test';
import {
  API,
  test,
  expect,
  expectNoSeriousA11yViolations,
  useTheme,
  THEMES,
  CORS,
  preflight,
  nextFrame,
  pageCursors,
} from '../fixtures';

// Notify + share: the notification bell in both views, My work's three rows with
// their true counts, a run marked read when it is opened or tried again, and Share. The mock keeps
// one read state for every test, and Playwright runs them in parallel, so each test that reads or
// changes notifications answers those routes for its own page.

const DONE = '/runs/run_01HGWDdone';
const OOM = '/runs/run_01HGWEoom';
const CANCELLED = '/runs/run_01HGW0cancel';

interface Note {
  id: string;
  run_id: string;
  run_name?: string;
  workflow_name?: string;
  kind: string;
  at: number;
  read: boolean;
  samples_total?: number;
  samples_failed?: number;
}

const ago = (secs: number) => Math.floor(Date.now() / 1000) - secs;

/** A server error, as a test's own route answers one. */
const BOOM = { status: 500, headers: CORS, contentType: 'text/plain', body: 'boom' };

/** Two unread (a finished and a failed analysis) and one read. */
const NOTES: Note[] = [
  {
    id: 'n-done',
    run_id: 'run_01HGWDdone',
    run_name: 'batch-12',
    workflow_name: 'rnaseq',
    kind: 'succeeded',
    at: ago(2 * 3600),
    read: false,
    samples_total: 12,
    samples_failed: 0,
  },
  {
    id: 'n-oom',
    run_id: 'run_01HGWEoom',
    run_name: 'batch-11',
    workflow_name: 'rnaseq',
    kind: 'failed',
    at: ago(2 * 86400),
    read: false,
    samples_total: 12,
    samples_failed: 1,
  },
  {
    id: 'n-cancel',
    run_id: 'run_01HGW0cancel',
    run_name: 'trio-7',
    workflow_name: 'sarek',
    kind: 'cancelled',
    at: ago(4 * 86400),
    read: true,
  },
];

/**
 * Answer `/me/notifications` and its read for this page alone, from `items`, as the backend does
 * (exactly one of ids, run_ids or all). Returns the read bodies the page sent; `notes` is the
 * server's list, for a test to change between polls. `failReads` refuses that many reads first.
 */
async function useNotifications(
  page: Page,
  items: Note[] = NOTES,
  { failList = false, failReads = 0, notes = items.map((n) => ({ ...n })) } = {},
) {
  const reads: Record<string, unknown>[] = [];
  const counts = () => {
    const unread = notes.filter((n) => !n.read);
    return {
      unread: unread.length,
      unread_failed: unread.filter((n) => n.kind === 'failed').length,
    };
  };
  await page.route(
    (url) => url.origin === API && url.pathname.startsWith('/me/notifications'),
    async (route) => {
      if (await preflight(route)) return;
      const req = route.request();
      if (req.method() === 'GET') {
        if (failList) return route.fulfill(BOOM);
        return route.fulfill({ headers: CORS, json: { items: notes, ...counts() } });
      }
      const body = req.postDataJSON() as { ids?: string[]; run_ids?: string[]; all?: boolean };
      reads.push(body);
      if (failReads > 0) {
        failReads--;
        return route.fulfill(BOOM);
      }
      for (const n of notes)
        if (body.all || body.ids?.includes(n.id) || body.run_ids?.includes(n.run_id)) n.read = true;
      await route.fulfill({ headers: CORS, json: counts() });
    },
  );
  return reads;
}

/**
 * My work's three lists for this page alone: the mock's runs, with the totals set to `totals` (the
 * server's true counts, more than the five listed). Needs attention lists her failed runs.
 */
async function useWorkTotals(
  page: Page,
  totals: { attention: number; active: number; succeeded: number },
) {
  const bodies: Record<string, unknown>[] = [];
  await page.route(`${API}/runs/query`, async (route) => {
    if (await preflight(route)) return;
    const body = route.request().postDataJSON() as Record<string, unknown>;
    bodies.push(body);
    const key = body.attention ? 'attention' : (body.filter as string);
    if (!(key in totals)) return route.fallback();
    // Needs attention: the mock's failed runs, whatever another test has read meanwhile.
    const asked = body.attention ? { filter: 'failed', mine: true, limit: body.limit } : body;
    const response = await route.fetch({ postData: JSON.stringify(asked) });
    const json = await response.json();
    json.total = totals[key as keyof typeof totals];
    // The server never lists more than its total, and its cursors are its page's own.
    json.runs = json.runs.slice(0, json.total);
    await route.fulfill({ response, json: { ...json, ...pageCursors(json.runs) } });
  });
  return bodies;
}

/** Answer `path` on the mock API with the mock's own answer, changed by `edit`. */
async function patchAnswer(page: Page, path: string, edit: (json: Record<string, any>) => void) {
  await page.route(
    (url) => url.origin === API && url.pathname === path,
    async (route) => {
      if (await preflight(route)) return;
      const response = await route.fetch();
      const json = await response.json();
      edit(json);
      await route.fulfill({ response, json });
    },
  );
}

const bell = (page: Page) =>
  page.locator('.bar-toggle').getByRole('button', { name: /^Notifications/ });
const panel = (page: Page) => page.getByRole('region', { name: 'Notifications' });

/** The shared status poll's next beat now (as a tab shown again does), not in 15 s. */
const beat = (page: Page) =>
  page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
/** The bell's next list answer (start waiting before what asks for it). */
const listAnswered = (page: Page) =>
  page.waitForResponse(
    (r) => new URL(r.url()).pathname === '/me/notifications' && r.request().method() === 'GET',
  );
/** A beat, once the bell's refetch it starts has been answered and the page has had a turn: a
 * read the beat would send has been sent by then. */
async function afterBeat(page: Page): Promise<void> {
  const refetched = listAnswered(page);
  await beat(page);
  await refetched;
  await page.evaluate(() => 0);
}

test.describe('the notification bell (Lab view)', () => {
  test.use({ view: 'lab' });

  test('counts the unread, lists them in the bench’s words, and opening one reads it', async ({
    page,
  }) => {
    const reads = await useNotifications(page);
    await page.goto('/');
    await expect(bell(page)).toHaveAccessibleName(
      'Notifications, 2 unread, 1 stopped with a problem',
    );
    await bell(page).click();
    await expect(bell(page)).toHaveAttribute('aria-expanded', 'true');
    await expect(panel(page)).toContainText('2 unread, 1 stopped with a problem');
    const items = panel(page).getByRole('listitem');
    await expect(items).toHaveCount(3);
    await expect(items.nth(0)).toContainText('Unread: rnaseq · batch-12 finished');
    await expect(items.nth(0)).toContainText('12 samples · 2 h ago');
    await expect(items.nth(1)).toContainText('rnaseq · batch-11 stopped with a problem');
    await expect(items.nth(1)).toContainText('1 sample failed · 2 days ago');
    await expect(items.nth(2)).toContainText('sarek · trio-7 was cancelled');
    await expect(items.nth(2)).not.toContainText('Unread');

    await items.nth(0).getByRole('link').click();
    await expect(page).toHaveURL(new RegExp(`${DONE}$`));
    await expect(panel(page)).toBeHidden();
    // One read: the run's page sends it (the bell sends none of its own).
    await expect.poll(() => reads).toEqual([{ run_ids: ['run_01HGWDdone'] }]);
    await expect(bell(page)).toHaveAccessibleName(
      'Notifications, 1 unread, 1 stopped with a problem',
    );
    await afterBeat(page);
    expect(reads).toHaveLength(1);
  });

  test('a read failure stopped with a problem, and the bell no longer counts it', async ({
    page,
  }) => {
    await useNotifications(page, [{ ...NOTES[1], read: true }]);
    await page.goto('/');
    await expect(bell(page)).toHaveAccessibleName('Notifications');
    await bell(page).click();
    await expect(panel(page).getByRole('listitem')).toContainText(
      'rnaseq · batch-11 stopped with a problem',
    );
  });

  test('announces newly arrived notifications, not the first load', async ({ page }) => {
    const notes = NOTES.map((n) => ({ ...n }));
    await useNotifications(page, NOTES, { notes });
    await page.goto('/');
    await expect(bell(page)).toHaveAccessibleName(
      'Notifications, 2 unread, 1 stopped with a problem',
    );
    const live = page.locator('.bar-toggle [aria-live="polite"]');
    await expect(live).toHaveText('');
    notes.unshift({
      id: 'n-new',
      run_id: 'run_01HGWAimgfail',
      kind: 'failed',
      at: ago(30),
      read: false,
    });
    await beat(page);
    await expect(live).toHaveText('1 new notification; 3 unread, 2 stopped with a problem');
    await expect(bell(page)).toHaveAccessibleName(
      'Notifications, 3 unread, 2 stopped with a problem',
    );
  });

  test('Mark all read clears the count and every dot', async ({ page }) => {
    const reads = await useNotifications(page);
    await page.goto('/');
    await bell(page).click();
    await panel(page).getByRole('button', { name: 'Mark all read' }).click();
    await expect(bell(page)).toHaveAccessibleName('Notifications');
    await expect(panel(page).getByText('Unread:')).toHaveCount(0);
    await expect(panel(page).getByRole('button', { name: 'Mark all read' })).toHaveCount(0);
    await expect.poll(() => reads).toEqual([{ all: true }]);
  });

  test('opens from the keyboard; Escape closes it and returns focus to the bell', async ({
    page,
  }) => {
    await useNotifications(page);
    await page.goto('/');
    await bell(page).focus();
    await page.keyboard.press('Enter');
    await expect(panel(page)).toBeVisible();
    await page.keyboard.press('Tab');
    await expect(panel(page).getByRole('button', { name: 'Mark all read' })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(panel(page)).toBeHidden();
    await expect(bell(page)).toBeFocused();
    await expect(bell(page)).toHaveAttribute('aria-expanded', 'false');
    // Mark all read from the keyboard: the button goes away, focus stays in the panel (its title),
    // and Escape still closes it.
    await page.keyboard.press('Enter');
    await page.keyboard.press('Tab');
    await page.keyboard.press('Enter');
    await expect(panel(page).getByRole('button', { name: 'Mark all read' })).toHaveCount(0);
    await expect(panel(page).getByText('Notifications', { exact: true })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(panel(page)).toBeHidden();
    await expect(bell(page)).toBeFocused();
    // A click elsewhere closes it too.
    await bell(page).click();
    await expect(panel(page)).toBeVisible();
    await page.getByRole('heading', { name: 'My work' }).click();
    await expect(panel(page)).toBeHidden();
  });

  test('says what will appear when there is nothing yet', async ({ page }) => {
    await useNotifications(page, []);
    await page.goto('/');
    await expect(bell(page)).toHaveAccessibleName('Notifications');
    await bell(page).click();
    await expect(panel(page)).toContainText("You'll see finished and failed analyses here.");
  });

  test('a list that cannot load says so only in the open dropdown', async ({ page }) => {
    await useNotifications(page, NOTES, { failList: true });
    const failed = listAnswered(page);
    await page.goto('/');
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    // Once the list has failed and the page has handled it: nothing said outside the dropdown.
    await failed;
    await nextFrame(page);
    await expect(page.getByText(/Couldn't refresh notifications/)).toHaveCount(0);
    await bell(page).click();
    await expect(panel(page).getByRole('alert')).toContainText("Couldn't refresh notifications");
  });

  for (const theme of THEMES) {
    test(`My work and the open bell are accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await useNotifications(page);
      await page.goto('/');
      await expect(page.getByRole('button', { name: /^Needs attention/ })).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await bell(page).click();
      await expect(panel(page).getByRole('listitem').first()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }

  test('My work and the open dropdown fit a phone', async ({ page }) => {
    await useNotifications(page);
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/');
    await expect(page.getByRole('button', { name: /^Needs attention/ })).toBeVisible();
    const overflow = () =>
      page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
    expect(await overflow()).toBeLessThanOrEqual(0);
    const topBell = page.locator('header.topbar').getByRole('button', { name: /^Notifications/ });
    await topBell.click();
    const open = panel(page);
    await expect(open.getByRole('listitem').first()).toBeVisible();
    const box = (await open.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
    expect(await overflow()).toBeLessThanOrEqual(0);
  });
});

test.describe('the notification bell (Expert view)', () => {
  test('is the same bell, in run words', async ({ page }) => {
    await useNotifications(page);
    await page.goto('/');
    await expect(bell(page)).toHaveAccessibleName('Notifications, 2 unread, 1 failed');
    await bell(page).click();
    const items = panel(page).getByRole('listitem');
    await expect(items.nth(0)).toContainText('rnaseq · batch-12 succeeded');
    await expect(items.nth(1)).toContainText('rnaseq · batch-11 failed');
  });

  test('says what will appear, in run words', async ({ page }) => {
    await useNotifications(page, []);
    await page.goto('/');
    await bell(page).click();
    await expect(panel(page)).toContainText("You'll see finished and failed runs here.");
  });

  for (const theme of THEMES) {
    test(`the open bell is accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await useNotifications(page);
      await page.goto('/');
      await bell(page).click();
      await expect(panel(page).getByRole('listitem').first()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }

  test('opening an ended run reads it; a live one does not', async ({ page }) => {
    const reads = await useNotifications(page);
    await page.goto('/runs/run_01HGW3align');
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
    await page.goto(OOM);
    await expect.poll(() => reads).toEqual([{ run_ids: ['run_01HGWEoom'] }]);
  });

  test('a run that ends while its page is open is read once', async ({ page }) => {
    const reads = await useNotifications(page);
    const LIVE = '/runs/run_01HGW3align';
    let ended = false;
    await patchAnswer(page, LIVE, (run) => {
      if (ended) Object.assign(run, { state: 'failed', terminal: true, completed_at: ago(1) });
    });
    await page.goto(LIVE);
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
    // A poll of the run after it opened (the page polls every 4 s), while it is live, reads nothing.
    await page.waitForResponse(
      (r) => new URL(r.url()).pathname === LIVE && r.request().method() === 'GET',
    );
    await page.evaluate(() => 0);
    expect(reads).toEqual([]);
    ended = true;
    // The next poll sees the end, and reads the run.
    await expect.poll(() => reads, { timeout: 10_000 }).toEqual([{ run_ids: ['run_01HGW3align'] }]);
    await afterBeat(page);
    expect(reads).toHaveLength(1);
  });

  test('a refused read is tried again on the next beat', async ({ page }) => {
    const reads = await useNotifications(page, NOTES, { failReads: 1 });
    await page.goto(OOM);
    await expect.poll(() => reads).toEqual([{ run_ids: ['run_01HGWEoom'] }]);
    // Refused, so still unread; the status poll's next beat tries again, and it is taken.
    await expect(bell(page)).toHaveAccessibleName('Notifications, 2 unread, 1 failed');
    await beat(page);
    await expect.poll(() => reads).toHaveLength(2);
    expect(reads[1]).toEqual({ run_ids: ['run_01HGWEoom'] });
    await expect(bell(page)).toHaveAccessibleName('Notifications, 1 unread');
  });
});

test.describe('My work', () => {
  test.use({ view: 'lab' });

  test('three rows with the server’s true counts; Needs attention open, the rest closed', async ({
    page,
  }) => {
    await useNotifications(page);
    const bodies = await useWorkTotals(page, { attention: 7, active: 12, succeeded: 30 });
    await page.goto('/');
    const attention = page.getByRole('button', { name: /^Needs attention/ });
    const running = page.getByRole('button', { name: /^In progress/ });
    const results = page.getByRole('button', { name: /^Recent results/ });
    await expect(attention).toHaveAccessibleName('Needs attention 7');
    await expect(running).toHaveAccessibleName('In progress 12');
    await expect(results).toHaveAccessibleName('Recent results 30');
    await expect(page.getByText('Your analyses: 12 running, 7 need a look.')).toBeVisible();
    await expect(attention).toHaveAttribute('aria-expanded', 'true');
    await expect(running).toHaveAttribute('aria-expanded', 'false');
    await expect(results).toHaveAttribute('aria-expanded', 'false');
    // The open row lists its newest, and sees all of them in the filtered list.
    const attentionBody = page.locator('#attention-body');
    await expect(attentionBody.getByRole('listitem').first()).toBeVisible();
    await expect(attentionBody.getByRole('link', { name: 'See all 7 →' })).toHaveAttribute(
      'href',
      '/runs?filter=attention',
    );
    await expect(page.locator('#running-body')).toBeHidden();
    // Needs attention reads only the five it lists (and the true count).
    expect(bodies).toContainEqual(expect.objectContaining({ attention: true, limit: 5 }));
    expect(bodies).not.toContainEqual(expect.objectContaining({ attention: true, limit: 200 }));

    await running.click();
    await expect(page.getByRole('link', { name: 'See all 12 →' })).toHaveAttribute(
      'href',
      '/runs?filter=active',
    );
    await results.click();
    await expect(page.getByRole('link', { name: 'See all 30 →' })).toHaveAttribute(
      'href',
      '/runs?filter=succeeded',
    );
  });

  test('remembers which rows were left open', async ({ page }) => {
    await useNotifications(page);
    await useWorkTotals(page, { attention: 2, active: 3, succeeded: 4 });
    await page.goto('/');
    await page.getByRole('button', { name: /^Needs attention/ }).click();
    await page.getByRole('button', { name: /^In progress/ }).click();
    await page.reload();
    await expect(page.getByRole('button', { name: /^Needs attention/ })).toHaveAttribute(
      'aria-expanded',
      'false',
    );
    await expect(page.getByRole('button', { name: /^In progress/ })).toHaveAttribute(
      'aria-expanded',
      'true',
    );
    await expect(page.getByRole('button', { name: /^Recent results/ })).toHaveAttribute(
      'aria-expanded',
      'false',
    );
  });

  test('See all opens the analyses that need attention: yours alone', async ({ page }) => {
    await useNotifications(page);
    const bodies = await useWorkTotals(page, { attention: 7, active: 1, succeeded: 1 });
    await page.goto('/');
    await page.getByRole('link', { name: 'See all 7 →' }).click();
    await expect(page).toHaveURL(/\/runs\?filter=attention$/);
    await expect(
      page.getByRole('button', { name: 'Needs attention', exact: true }),
    ).toHaveAttribute('aria-pressed', 'true');
    await expect.poll(() => bodies.at(-1)).toMatchObject({ attention: true, mine: true });
    // Always the caller's own: no Everyone's, no Started by, and it says so.
    await expect(page.getByText('Your analyses that need attention')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Everyone’s' })).toHaveCount(0);
    await expect(page.getByRole('columnheader', { name: /Started by/ })).toHaveCount(0);
    // Another tab brings the choice back.
    await page.getByRole('button', { name: 'Failed' }).click();
    await expect(page.getByRole('button', { name: 'Everyone’s' })).toBeVisible();
  });

  test('offers See all only when there are more than the row lists', async ({ page }) => {
    await useNotifications(page);
    await useWorkTotals(page, { attention: 1, active: 0, succeeded: 0 });
    await page.goto('/');
    const attentionBody = page.locator('#attention-body');
    await expect(attentionBody.getByRole('listitem')).toHaveCount(1);
    await expect(attentionBody.getByRole('link', { name: /^See all/ })).toHaveCount(0);
  });

  test('opens Needs attention again when more need a look than when it was closed', async ({
    page,
  }) => {
    await useNotifications(page);
    const totals = { attention: 2, active: 0, succeeded: 0 };
    await useWorkTotals(page, totals);
    await page.goto('/');
    const attention = page.getByRole('button', { name: /^Needs attention/ });
    await expect(attention).toHaveAttribute('aria-expanded', 'true');
    await attention.click();
    await page.reload();
    await expect(attention).toHaveAccessibleName('Needs attention 2');
    await expect(attention).toHaveAttribute('aria-expanded', 'false');
    totals.attention = 3;
    await page.reload();
    await expect(attention).toHaveAccessibleName('Needs attention 3');
    await expect(attention).toHaveAttribute('aria-expanded', 'true');
  });

  /** Answer every `/runs/query` with `total` (`mineTotal` for the "has she any" one). */
  async function useNoRuns(page: Page, mineTotal: number) {
    await page.route(`${API}/runs/query`, async (route) => {
      if (await preflight(route)) return;
      const body = route.request().postDataJSON() as Record<string, unknown>;
      const any = body.mine === true && body.limit === 1 && !body.attention;
      await route.fulfill({
        headers: CORS,
        json: { runs: [], total: any ? mineTotal : 0, first_cursor: null, last_cursor: null },
      });
    });
  }

  test('welcomes someone who has never started an analysis', async ({ page }) => {
    await useNotifications(page, []);
    await useNoRuns(page, 0);
    await page.goto('/');
    await expect(
      page.getByRole('heading', { name: "You haven't started an analysis yet" }),
    ).toBeVisible();
    await expect(page.getByRole('button', { name: /^Needs attention/ })).toHaveCount(0);
  });

  test('shows the three rows to someone with analyses in none of them', async ({ page }) => {
    await useNotifications(page, []);
    await useNoRuns(page, 1);
    await page.goto('/');
    await expect(page.getByRole('button', { name: /^Needs attention/ })).toBeVisible();
    await expect(page.getByRole('button', { name: /^In progress/ })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Recent results/ })).toBeVisible();
    await expect(
      page.getByRole('heading', { name: "You haven't started an analysis yet" }),
    ).toHaveCount(0);
  });
});

test.describe('the analysis page', () => {
  test.use({ view: 'lab' });

  test('opening a failed analysis reads its notifications', async ({ page }) => {
    const reads = await useNotifications(page);
    await page.goto('/');
    await expect(bell(page)).toHaveAccessibleName(
      'Notifications, 2 unread, 1 stopped with a problem',
    );
    await page.goto(OOM);
    await expect(page.getByRole('region', { name: 'What went wrong' })).toBeVisible();
    await expect.poll(() => reads).toEqual([{ run_ids: ['run_01HGWEoom'] }]);
    await expect(bell(page)).toHaveAccessibleName('Notifications, 1 unread');
  });

  test('Try again names the run it tries again, as the launch form’s re-run does', async ({
    page,
  }) => {
    await useNotifications(page);
    const launched: Record<string, unknown>[] = [];
    await page.route(`${API}/runs`, async (route) => {
      if (await preflight(route)) return;
      if (route.request().method() !== 'POST') return route.fallback();
      launched.push(route.request().postDataJSON());
      await route.fulfill({
        status: 202,
        headers: CORS,
        json: { run_id: 'run_01HGWDdone', state: 'submitted' },
      });
    });
    // A failure the server would only re-run (not resume).
    await patchAnswer(page, `${OOM}/failure-summary`, (summary) => {
      summary.items[0].try_again = 'rerun';
    });
    await page.goto(OOM);
    await page
      .getByRole('region', { name: 'What went wrong' })
      .getByRole('button', { name: 'Try again' })
      .click();
    await page
      .getByRole('dialog', { name: 'Run this analysis again?' })
      .getByRole('button', { name: 'Run again' })
      .click();
    await expect(page).toHaveURL(new RegExp(`${DONE}$`));
    await expect.poll(() => launched[0]).toMatchObject({ retry_of: 'run_01HGWEoom' });
    expect(launched[0]).not.toHaveProperty('resume_from');

    // The launch form pre-filled from the same run and launched untouched sends the same request.
    await page.goto('/workflows/c4d5e6f7a8b9%3Av1/run?from=run_01HGWEoom');
    await page
      .getByRole('group', { name: 'View' })
      .first()
      .getByRole('button', { name: 'Expert' })
      .click();
    await expect(page.getByRole('status')).toContainText('Pre-filled from run_01HGWEoom');
    await page.getByRole('button', { name: 'Launch run →' }).click();
    await expect.poll(() => launched.length).toBe(2);
    expect(launched[1]).toEqual(launched[0]);
  });

  test('Share copies the page’s link and says who can open it', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await useNotifications(page);
    await page.goto(DONE);
    const share = page.getByRole('button', { name: 'Share' });
    await share.click();
    await expect(share).toHaveAttribute('aria-expanded', 'true');
    const popover = page.getByRole('region', { name: 'Share this analysis' });
    const url = `http://localhost:5174${DONE}`;
    await expect(popover.getByRole('textbox', { name: 'Link to this analysis' })).toHaveValue(url);
    await expect(popover).toContainText(
      'Anyone signed in to biffix with access to project genomics can open it.',
    );
    await popover.getByRole('button', { name: 'Copy link' }).click();
    await expect(popover.getByRole('button', { name: 'Copied' })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(url);
    await page.keyboard.press('Escape');
    await expect(popover).toBeHidden();
    await expect(share).toBeFocused();
  });

  test('a cancelled analysis says who cancelled it', async ({ page }) => {
    await useNotifications(page);
    await page.goto(CANCELLED);
    await expect(page.getByTestId('analysis-status')).toContainText(
      /Cancelled by alice@lab\.example\.org · \S/,
    );
  });

  test('without who, a cancelled analysis says when it stopped', async ({ page }) => {
    await useNotifications(page);
    await patchAnswer(page, CANCELLED, (run) => {
      delete run.cancelled_by;
    });
    await page.goto(CANCELLED);
    const status = page.getByTestId('analysis-status');
    await expect(status).toContainText('Cancelled · stopped');
    await expect(status).not.toContainText('Cancelled by');
  });
});

/**
 * Needs attention for this page alone: three of the mock's analyses of hers, as failed runs, less
 * those dismissed here; `POST /me/attention/dismiss` takes exactly the runs it names (refusing the
 * first `failDismiss`). Returns the dismissal bodies the page sent and the runs listed.
 */
async function useAttention(page: Page, { failDismiss = 0, count = 3 } = {}) {
  const dismissals: { run_ids: string[] }[] = [];
  const dismissed = new Set<string>();
  const queries: Record<string, unknown>[] = [];
  let runs: { run_id: string; run_name?: string }[] | null = null;
  await page.route(`${API}/runs/query`, async (route) => {
    if (await preflight(route)) return;
    const body = route.request().postDataJSON() as Record<string, unknown>;
    if (!body.attention) return route.fallback();
    queries.push(body);
    if (!runs) {
      const response = await page.request.post(`${API}/runs/query`, {
        data: { mine: true, limit: 200 },
      });
      const all = (await response.json()).runs as { run_id: string; state: string }[];
      // Ended ones (their page reads them when opened), named so a label can say which.
      const ended = all.filter((r) => ['succeeded', 'failed', 'cancelled'].includes(r.state));
      runs = ended.slice(0, count).map((r, i) => ({
        ...r,
        run_name: `failed-${i + 1}`,
        state: 'failed',
        terminal: true,
      }));
    }
    const left = runs.filter((r) => !dismissed.has(r.run_id));
    // A page of them, as the server answers `limit`, with the true total.
    const limit = typeof body.limit === 'number' ? body.limit : left.length;
    const listed = left.slice(0, limit);
    await route.fulfill({
      headers: CORS,
      json: { runs: listed, total: left.length, ...pageCursors(listed) },
    });
  });
  await page.route(`${API}/me/attention/dismiss`, async (route) => {
    if (await preflight(route)) return;
    const body = route.request().postDataJSON() as { run_ids: string[] };
    dismissals.push(body);
    if (failDismiss > 0) {
      failDismiss--;
      return route.fulfill({ status: 503, headers: CORS, contentType: 'text/plain', body: 'boom' });
    }
    for (const id of body.run_ids) dismissed.add(id);
    const left = (runs ?? []).filter((r) => !dismissed.has(r.run_id)).length;
    await route.fulfill({ headers: CORS, json: { attention: left } });
  });
  return { dismissals, queries, listed: () => runs ?? [] };
}

test.describe('dismissing from Needs attention', () => {
  test.use({ view: 'lab' });

  const attentionRows = (page: Page) => page.locator('#attention-body').getByRole('listitem');
  const attentionToggle = (page: Page) => page.getByRole('button', { name: /^Needs attention/ });

  test('opening one reads it but leaves it there', async ({ page }) => {
    const reads = await useNotifications(page);
    const { dismissals, listed } = await useAttention(page);
    await page.goto('/');
    await expect(attentionRows(page)).toHaveCount(3);
    const [first] = listed();
    await attentionRows(page)
      .first()
      .getByRole('link', { name: first.run_name, exact: true })
      .click();
    await expect(page).toHaveURL(new RegExp(`/runs/${first.run_id}$`));
    // The run's page reads its notification (the bell's state)...
    await expect.poll(() => reads).toContainEqual({ run_ids: [first.run_id] });
    // ...and it still needs attention: nothing was dismissed.
    await page.goBack();
    await expect(attentionToggle(page)).toHaveAccessibleName('Needs attention 3');
    await expect(attentionRows(page)).toHaveCount(3);
    expect(dismissals).toEqual([]);
  });

  test('Dismiss takes one row out, at once and for good, without navigating', async ({ page }) => {
    await useNotifications(page);
    const { dismissals, listed } = await useAttention(page);
    await page.goto('/');
    await expect(attentionRows(page)).toHaveCount(3);
    const [first] = listed();
    await page.getByRole('button', { name: `Dismiss ${first.run_name}`, exact: true }).click();
    await expect(page).toHaveURL(/\/$/);
    await expect(attentionRows(page)).toHaveCount(2);
    await expect(attentionToggle(page)).toHaveAccessibleName('Needs attention 2');
    await expect(attentionToggle(page)).toBeFocused();
    await expect(page.getByText(`Dismissed ${first.run_name}.`)).toBeAttached();
    await expect.poll(() => dismissals).toEqual([{ run_ids: [first.run_id] }]);
    await page.reload();
    await expect(attentionRows(page)).toHaveCount(2);
  });

  test('Dismiss all dismisses exactly the analyses it shows', async ({ page }) => {
    await useNotifications(page);
    const { dismissals, listed } = await useAttention(page);
    await page.goto('/');
    await page.getByRole('button', { name: 'Dismiss all 3 that need attention' }).click();
    await expect(page.locator('#attention-body')).toContainText('Nothing needs a look.');
    await expect(attentionToggle(page)).toHaveAccessibleName('Needs attention 0');
    await expect.poll(() => dismissals).toEqual([{ run_ids: listed().map((r) => r.run_id) }]);
  });

  test('with more than it lists, Dismiss these 5 dismisses exactly the five shown', async ({
    page,
  }) => {
    await useNotifications(page);
    const { dismissals, queries, listed } = await useAttention(page, { count: 6 });
    await page.goto('/');
    await expect(attentionRows(page)).toHaveCount(5);
    await expect(attentionToggle(page)).toHaveAccessibleName('Needs attention 6');
    const shown = listed().slice(0, 5);
    const asked = queries.length;
    await page.getByRole('button', { name: 'Dismiss these 5 that need attention' }).click();
    await expect.poll(() => dismissals).toEqual([{ run_ids: shown.map((r) => r.run_id) }]);
    // Listed again at once (not at the next poll): the one left, and the count.
    await expect(attentionRows(page)).toHaveCount(1);
    await expect(attentionToggle(page)).toHaveAccessibleName('Needs attention 1');
    expect(queries.length).toBeGreaterThan(asked);
    await expect(
      page.getByRole('button', { name: 'Dismiss all 1 that need attention' }),
    ).toBeVisible();
  });

  test('a refused dismissal brings the row back and says why', async ({ page }) => {
    await useNotifications(page);
    const { listed } = await useAttention(page, { failDismiss: 1 });
    await page.goto('/');
    await expect(attentionRows(page)).toHaveCount(3);
    const [first] = listed();
    await page.getByRole('button', { name: `Dismiss ${first.run_name}`, exact: true }).click();
    await expect(page.getByRole('alert')).toContainText("Couldn't dismiss: ");
    await expect(attentionRows(page)).toHaveCount(3);
  });

  test('the Analyses list’s Needs attention dismisses too', async ({ page }) => {
    await useNotifications(page);
    const { dismissals, listed } = await useAttention(page);
    await page.goto('/runs?filter=attention');
    const rows = page.locator('tbody tr');
    await expect(rows).toHaveCount(3);
    const [first, ...rest] = listed();
    await page.getByRole('button', { name: `Dismiss ${first.run_name}`, exact: true }).click();
    await expect(page).toHaveURL(/\/runs\?filter=attention$/);
    await expect(rows).toHaveCount(2);
    await page.getByRole('button', { name: 'Dismiss all 2 that need attention' }).click();
    await expect(page.getByText('No analyses match this filter.')).toBeVisible();
    await expect
      .poll(() => dismissals)
      .toEqual([{ run_ids: [first.run_id] }, { run_ids: rest.map((r) => r.run_id) }]);
  });

  for (const theme of THEMES) {
    test(`My work with Dismiss is accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await useNotifications(page);
      await useAttention(page);
      await page.goto('/');
      await expect(attentionRows(page)).toHaveCount(3);
      await expectNoSeriousA11yViolations(page);
    });
  }
});

test.describe('dismissing from the Expert attention list', () => {
  test('a row and the page dismiss, in run words', async ({ page }) => {
    await useNotifications(page);
    const { dismissals, listed } = await useAttention(page);
    await page.goto('/runs?filter=attention');
    await expect(page.locator('tbody tr')).toHaveCount(3);
    // In run words: the Expert view's, never the Lab's.
    await expect(page.getByText('Your runs that need attention')).toBeVisible();
    await expect(page.getByText('Your analyses that need attention')).toHaveCount(0);
    const [first, ...rest] = listed();
    await page.getByRole('button', { name: `Dismiss ${first.run_name}`, exact: true }).click();
    await expect(page.locator('tbody tr')).toHaveCount(2);
    await expect(page.getByText('Dismissed ')).toBeAttached();
    await expect.poll(() => dismissals).toEqual([{ run_ids: [first.run_id] }]);
    // And the page's own Dismiss all.
    await page.getByRole('button', { name: 'Dismiss all 2 that need attention' }).click();
    await expect(page.locator('tbody tr')).toHaveCount(0);
    await expect
      .poll(() => dismissals)
      .toEqual([{ run_ids: [first.run_id] }, { run_ids: rest.map((r) => r.run_id) }]);
  });
});
