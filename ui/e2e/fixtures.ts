import AxeBuilder from '@axe-core/playwright';
import { test as base, expect, type Page, type Route } from '@playwright/test';

/** The mock API (`mock/server.mjs`), as the dev server is pointed at it. */
export const API = 'http://localhost:8080';
/** The Run tab of the mock's curated nf-core/rnaseq version. */
export const RUN_TAB = '/workflows/aabbccddeeff%3Av1/run';
export const THEMES = ['light', 'dark'] as const;

/**
 * Signs a page in before it loads: the app keeps its token in sessionStorage, and the mock API
 * accepts any bearer token.
 */
export async function signIn(page: Page): Promise<void> {
  await page.addInitScript(() => {
    sessionStorage.setItem('biffix-access-token', 'mock');
    sessionStorage.setItem('biffix-access-token-expiry', String(Date.now() + 3600e3));
  });
}

/** Opens a page in a colour theme (call before `goto`). */
export async function useTheme(page: Page, theme: (typeof THEMES)[number]): Promise<void> {
  await page.addInitScript((t) => localStorage.setItem('biffix-theme', t), theme);
}

/**
 * No serious or critical accessibility violations on the page as it is (axe-core, WCAG A/AA).
 */
export async function expectNoSeriousA11yViolations(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze();
  const serious = results.violations.filter(
    (v) => v.impact === 'serious' || v.impact === 'critical',
  );
  expect(
    serious.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(' ')).join(', ')})`),
  ).toEqual([]);
}

/** The CORS headers a test's own answer to the mock API's origin carries. */
export const CORS = {
  'access-control-allow-origin': '*',
  'access-control-allow-headers': 'authorization, content-type',
  'access-control-allow-methods': 'GET, POST, PUT, DELETE, OPTIONS',
};

/** Answer a preflight; true when `route` was one. */
export async function preflight(route: Route): Promise<boolean> {
  if (route.request().method() !== 'OPTIONS') return false;
  await route.fulfill({ status: 204, headers: CORS });
  return true;
}

/** Route `method` requests to `path` (on the mock API) to `handle`; the rest go on to the mock. */
export async function onApi(
  page: Page,
  method: string,
  path: string | RegExp,
  handle: (route: Route) => Promise<void>,
): Promise<void> {
  await page.route(
    (url) =>
      url.origin === API &&
      (typeof path === 'string' ? url.pathname === path : path.test(url.pathname)),
    async (route) => {
      if (await preflight(route)) return;
      if (route.request().method() !== method) return route.fallback();
      await handle(route);
    },
  );
}

/** Resolves a frame from now: by then the page has handled an answer that has already arrived
 * (its handlers run before the next frame). A signal to check that something did not happen. */
export const nextFrame = (page: Page): Promise<void> =>
  page.evaluate(() => new Promise<void>((done) => requestAnimationFrame(() => done())));

/** Fill the open Run tab from one of its form's examples (by default rnaseq's human one). */
export async function startFromExample(
  page: Page,
  example: RegExp = /Human, 6 paired-end samples/,
): Promise<void> {
  await page.getByRole('button', { name: 'Start from example ▾' }).click();
  await page.getByRole('menuitem', { name: example }).click();
}

/** Import `text` into the open Run tab's samplesheet as the CSV file `name`. */
export async function importCsv(page: Page, name: string, text: string): Promise<void> {
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  const chooser = page.waitForEvent('filechooser');
  await page.getByRole('menuitem', { name: 'Import CSV…' }).click();
  await (await chooser).setFiles({ name, mimeType: 'text/csv', buffer: Buffer.from(text) });
}

/** Choose the mock sequencing core's batch-12 FASTQs (P01–P03) in the samplesheet builder. */
export async function chooseBatch12(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await page.getByRole('menuitem', { name: 'Choose files…' }).click();
  const picker = page.getByRole('dialog', { name: /Choose files for Samplesheet/ });
  await picker.getByRole('button', { name: /Sequencing core/ }).click();
  await picker.getByRole('button', { name: '2026' }).click();
  await picker.getByRole('button', { name: 'batch-12' }).click();
  // SampleSheet.csv is hidden by the FASTQ filter; select all six FASTQs.
  await picker.getByRole('button', { name: 'Select all 6' }).click();
  await picker.getByRole('button', { name: 'Use 6 files' }).click();
}

/** Open the Run tab with own files, and the file picker of its Genome FASTA field. */
export async function browseGenomeFasta(page: Page): Promise<void> {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use my own files…' }).click();
  await page.getByRole('button', { name: 'Browse for Genome FASTA' }).click();
}

/**
 * Holds the page's part uploads (the PUTs to the mock's stand-in for S3) after the first `allow`,
 * until the returned release is called; from then on parts go straight through. A part the page
 * gives up on meanwhile (a pause or cancel aborts it) is simply dropped.
 */
export async function holdParts(page: Page, allow = 0): Promise<() => void> {
  let release!: () => void;
  const released = new Promise<void>((r) => (release = r));
  let passed = 0;
  await page.route(`${API}/mock-s3/**`, async (route) => {
    if (route.request().method() === 'PUT' && passed++ >= allow) await released;
    await route.fallback().catch(() => {});
  });
  return release;
}

/** The run page's tab bar. */
export const runTabs = (page: Page) => page.getByRole('navigation', { name: 'Run views' });
/** The workflow page's tab bar. */
export const workflowTabs = (page: Page) =>
  page.getByRole('navigation', { name: 'Workflow views' });

/**
 * Answers `/me/prefs` for this page alone, starting from `view` (so one test's switch never leaks
 * into another's, as the shared mock's state would), and primes the browser's cached view on the
 * first page load only: an init script runs on every load, and re-priming would undo a switch the
 * test made before a `goto`. The first-visit tour counts as done unless `tourDone` is false (a spec
 * of the tour itself).
 */
export async function useView(page: Page, view: 'lab' | 'expert', tourDone = true): Promise<void> {
  const prefs: Record<string, unknown> = { view, pinned_workflows: [] };
  if (tourDone) prefs.tour_done_at = 1_700_000_000;
  await page.addInitScript((v) => {
    if (localStorage.getItem('biffix-view') === null) localStorage.setItem('biffix-view', v);
  }, view);
  await page.route(`${API}/me/prefs`, async (route) => {
    if (await preflight(route)) return;
    const req = route.request();
    if (req.method() === 'PUT') {
      // As `/me/prefs` does: a partial update, with `pin`/`unpin` as operations on the stored list.
      const { pin, unpin, tour_done, ...rest } = req.postDataJSON() as Record<string, unknown>;
      Object.assign(prefs, rest);
      if (tour_done === true) prefs.tour_done_at = Date.now() / 1000;
      if (tour_done === false) delete prefs.tour_done_at;
      const pins = prefs.pinned_workflows as string[];
      if (typeof pin === 'string' && !pins.includes(pin)) prefs.pinned_workflows = [...pins, pin];
      if (typeof unpin === 'string') prefs.pinned_workflows = pins.filter((k) => k !== unpin);
    }
    await route.fulfill({ json: prefs, headers: CORS });
  });
}

interface Notification {
  id: string;
  run_id: string;
  kind: string;
  read: boolean;
}
interface Notifications {
  items: Notification[];
  unread: number;
  unread_failed: number;
}

/**
 * Keeps the notifications this page reads to itself. Opening an ended run of the caller's reads it
 * (`POST /me/notifications/read`), and the mock keeps one read state for every test: a read sent
 * there would change the bell of every test that runs after it. Here a read is answered for this
 * page alone, and the mock's list is shown with it applied. A spec that answers the notification
 * routes itself (`useNotifications` in shell/notifications.spec.ts) takes precedence.
 */
export async function keepNotificationReads(page: Page): Promise<void> {
  const ids = new Set<string>();
  const runIds = new Set<string>();
  let all = false;
  const withReads = (list: Notifications): Notifications => {
    const items = list.items.map((n) =>
      !n.read && (all || ids.has(n.id) || runIds.has(n.run_id)) ? { ...n, read: true } : n,
    );
    const newlyRead = items.filter((n, i) => n.read && !list.items[i].read);
    return {
      items,
      unread: list.unread - newlyRead.length,
      unread_failed: list.unread_failed - newlyRead.filter((n) => n.kind === 'failed').length,
    };
  };
  await page.route(
    (url) => url.origin === API && url.pathname.startsWith('/me/notifications'),
    async (route) => {
      if (await preflight(route)) return;
      const req = route.request();
      if (req.method() === 'GET') {
        const response = await route.fetch();
        if (!response.ok()) return route.fulfill({ response });
        return route.fulfill({ response, json: withReads(await response.json()) });
      }
      // As the backend: exactly one of `ids`, `run_ids` or `all: true`.
      const body = req.postDataJSON() as { ids?: string[]; run_ids?: string[]; all?: boolean };
      const given = [body.ids != null, body.run_ids != null, body.all === true].filter(Boolean);
      if (given.length !== 1)
        return route.fulfill({
          status: 400,
          headers: CORS,
          contentType: 'text/plain',
          body: 'give exactly one of ids, run_ids or all',
        });
      for (const id of body.ids ?? []) ids.add(id);
      for (const id of body.run_ids ?? []) runIds.add(id);
      if (body.all === true) all = true;
      const list = await page.request.get(`${API}/me/notifications?limit=100`);
      const { unread, unread_failed } = withReads(await list.json());
      await route.fulfill({ headers: CORS, json: { unread, unread_failed } });
    },
  );
}

type CursorRow = { run_id: string; submitted_at?: number | null };
/** A page of runs' own edge cursors, in the mock's spelling (`runCursor` in
 * mock/handlers/runs.mjs), for a test that answers or cuts a runs page itself. */
export function pageCursors(runs: CursorRow[]): {
  first_cursor: string | null;
  last_cursor: string | null;
} {
  const cursor = (r?: CursorRow) =>
    r ? `${r.submitted_at ?? Number.MAX_SAFE_INTEGER}|${r.run_id}` : null;
  return { first_cursor: cursor(runs[0]), last_cursor: cursor(runs.at(-1)) };
}

/**
 * Lists the mock's runs (`POST /runs/query`) for this page without the ones other tests launched
 * (`run_mock…`): a test that reads a list of the caller's runs (My work's *In progress*) sees the
 * fixtures', whichever launches ran before it. A first page is asked for in full and cut to its
 * limit after, so a launched run never takes a fixture run's place. Only a first page: one asked
 * for by a cursor (`after`, `before`) goes to the mock as it is, since its cursors would not match
 * the rows cut here.
 */
export async function withoutLaunchedRuns(page: Page): Promise<void> {
  await page.route(`${API}/runs/query`, async (route) => {
    if (await preflight(route)) return;
    const asked = route.request().postDataJSON() as {
      limit?: number;
      after?: string;
      before?: string;
    };
    if (asked.after || asked.before) return route.fallback();
    const response = await route.fetch({ postData: JSON.stringify({ ...asked, limit: 200 }) });
    if (!response.ok()) return route.fulfill({ response });
    const list = (await response.json()) as { runs: CursorRow[]; total: number };
    const runs = list.runs
      .filter((r) => !r.run_id.startsWith('run_mock'))
      .slice(0, asked.limit ?? 50);
    const launched = list.runs.filter((r) => r.run_id.startsWith('run_mock')).length;
    await route.fulfill({
      response,
      json: {
        ...list,
        runs,
        total: list.total - launched,
        limit: asked.limit ?? 50,
        ...pageCursors(runs),
      },
    });
  });
}

// Every page needs a signed-in session. Specs written for the Expert view (most of them) get it by
// default; a Lab-view spec says `test.use({ view: 'lab' })`.
// The first-visit tour is done unless a spec says `test.use({ tourDone: false })`.
//
// A route handler still running when a test ends (a poll's `route.fetch()` in flight) would read a
// response the closing context has already disposed, and its error would fail whichever test the
// worker runs then, or leave the rest of the file not run: so a test's routes are removed, their
// late errors ignored, before its page closes.
export const test = base.extend<{ view: 'lab' | 'expert'; tourDone: boolean }>({
  view: ['expert', { option: true }],
  tourDone: [true, { option: true }],
  page: async ({ page, view, tourDone }, use) => {
    await signIn(page);
    await useView(page, view, tourDone);
    await keepNotificationReads(page);
    await use(page);
    await page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});

export { expect };
