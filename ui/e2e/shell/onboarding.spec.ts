import type { Page, Request } from '@playwright/test';
import {
  API,
  CORS,
  RUN_TAB,
  THEMES,
  expect,
  expectNoSeriousA11yViolations,
  nextFrame,
  onApi,
  preflight,
  test,
  useTheme,
} from '../fixtures';

// Onboarding: the first-visit tour on My work, the Learn panel on every
// page, and the empty states that name the next step and who takes it.

const tour = (page: Page) => page.getByRole('dialog', { name: /./ }).filter({ hasText: 'Tour ·' });
const learnButton = (page: Page) => page.getByRole('button', { name: 'Learn about this page' });
const learnPanel = (page: Page) => page.locator('#learn-panel');
const isPrefsPut = (r: Request) => r.method() === 'PUT' && r.url().endsWith('/me/prefs');
/** Once the preferences the tour waits on (`tour_done_at`) have been read and handled: the next
 * load's GET `/me/prefs` answer (start waiting before the load). */
const prefsRead = (page: Page) =>
  page
    .waitForResponse((r) => r.url().endsWith('/me/prefs') && r.request().method() === 'GET')
    .then(() => nextFrame(page));
/** Whether focus is inside the element `selector` matches. */
const focusIn = (page: Page, selector: string) =>
  page.evaluate((sel) => !!document.activeElement?.closest(sel), selector);

/** Answer every `/runs/query` with no analyses: a first visit. */
async function useNoRuns(page: Page) {
  await page.route(`${API}/runs/query`, async (route) => {
    if (await preflight(route)) return;
    await route.fulfill({
      headers: CORS,
      json: { runs: [], total: 0, first_cursor: null, last_cursor: null },
    });
  });
}

/** Sign in as `sub` (an ID token naming them; the mock reads only the access token). */
async function signInAs(page: Page, sub: string) {
  const payload = Buffer.from(JSON.stringify({ sub, email: `${sub}@lab.example.org` })).toString(
    'base64url',
  );
  await page.addInitScript((t) => sessionStorage.setItem('biffix-id-token', t), `h.${payload}.s`);
}

/** No horizontal page scroll. */
async function expectNoOverflow(page: Page) {
  const [scroll, client] = await page.evaluate(() => [
    document.documentElement.scrollWidth,
    document.documentElement.clientWidth,
  ]);
  expect(scroll).toBeLessThanOrEqual(client);
}

test.describe('the first-visit tour', () => {
  test.use({ view: 'lab', tourDone: false });

  test('walks its four stops with Next and Back, and Done ends it for good', async ({ page }) => {
    await page.goto('/');
    const stop = tour(page);
    await expect(stop).toBeVisible();
    await expect(stop).toBeFocused();
    await expect(stop.getByRole('heading', { name: 'Start an analysis' })).toBeVisible();
    await expect(stop).toContainText('Tour · 1 of 4');
    await expect(stop.getByRole('button', { name: 'Back' })).toHaveCount(0);
    // It points at its target, without moving it.
    await expect(page.locator('[data-tour="start"]')).toHaveAttribute('data-tour-active', '');

    await stop.getByRole('button', { name: 'Next' }).click();
    await expect(stop.getByRole('heading', { name: 'Follow what is running' })).toBeVisible();
    await expect(page.locator('[data-tour="running"]')).toHaveAttribute('data-tour-active', '');
    await stop.getByRole('button', { name: 'Back' }).click();
    await expect(stop.getByRole('heading', { name: 'Start an analysis' })).toBeVisible();
    await stop.getByRole('button', { name: 'Next' }).click();
    await stop.getByRole('button', { name: 'Next' }).click();
    await expect(stop.getByRole('heading', { name: 'Find a pipeline' })).toBeVisible();
    await expect(page.locator('[data-tour="nav-pipelines"]')).toHaveAttribute(
      'data-tour-active',
      '',
    );
    await stop.getByRole('button', { name: 'Next' }).click();
    await expect(stop.getByRole('heading', { name: 'Lab or Expert' })).toBeVisible();
    await expect(page.locator('[data-tour="view-toggle"][data-tour-active]')).toHaveCount(1);
    await expect(stop.getByRole('button', { name: 'Next' })).toHaveCount(0);
    await expect(stop.getByRole('button', { name: 'Skip tour' })).toBeVisible();

    const saved = page.waitForRequest(isPrefsPut);
    await stop.getByRole('button', { name: 'Done' }).click();
    expect((await saved).postDataJSON()).toEqual({ tour_done: true });
    await expect(stop).toHaveCount(0);
    await expect(page.locator('[data-tour-active]')).toHaveCount(0);

    // The stored preferences now say done: a reload shows no tour.
    const read = prefsRead(page);
    await page.reload();
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    await read;
    await expect(stop).toHaveCount(0);
  });

  test('Skip tour on any stop ends it', async ({ page }) => {
    await page.goto('/');
    await tour(page).getByRole('button', { name: 'Next' }).click();
    const saved = page.waitForRequest(isPrefsPut);
    await tour(page).getByRole('button', { name: 'Skip tour' }).click();
    expect((await saved).postDataJSON()).toEqual({ tour_done: true });
    await expect(tour(page)).toHaveCount(0);
  });

  test('Escape skips it and hands focus back to the page', async ({ page }) => {
    await page.goto('/');
    await expect(tour(page)).toBeFocused();
    const saved = page.waitForRequest(isPrefsPut);
    await page.keyboard.press('Escape');
    expect((await saved).postDataJSON()).toEqual({ tour_done: true });
    await expect(tour(page)).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Start an analysis →' }).first()).toBeFocused();
  });

  test('a refused save still ends it for this visit', async ({ page }) => {
    await onApi(page, 'PUT', '/me/prefs', (route) =>
      route.fulfill({ status: 500, headers: CORS, body: 'down' }),
    );
    await page.goto('/');
    await tour(page).getByRole('button', { name: 'Skip tour' }).click();
    await expect(tour(page)).toHaveCount(0);
    await page.getByRole('link', { name: 'Pipelines' }).first().click();
    await page.getByRole('link', { name: 'My work' }).first().click();
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    await expect(tour(page)).toHaveCount(0);
  });

  test('is not a trap: Tab and Shift+Tab leave it', async ({ page }) => {
    await page.goto('/');
    const stop = tour(page);
    await expect(stop).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(stop.getByRole('button', { name: 'Skip tour' })).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(stop.getByRole('button', { name: 'Next' })).toBeFocused();
    await page.keyboard.press('Tab');
    expect(await focusIn(page, '.tour')).toBe(false);
    await expect(stop).toBeVisible();

    await stop.focus();
    await page.keyboard.press('Shift+Tab');
    expect(await focusIn(page, '.tour')).toBe(false);
    expect(await focusIn(page, '.app')).toBe(true);
    await expect(stop).toBeVisible();
  });

  test('is not a trap: the page stays usable under it', async ({ page }) => {
    await page.goto('/');
    await expect(tour(page)).toBeVisible();
    await page.getByRole('link', { name: 'Analyses' }).first().click();
    await expect(page).toHaveURL(/\/runs$/);
    // Only My work shows it.
    await expect(tour(page)).toHaveCount(0);
  });

  test('leaving My work mid-tour counts as seen: it does not start over', async ({ page }) => {
    await page.goto('/');
    await tour(page).getByRole('button', { name: 'Next' }).click();
    const saved = page.waitForRequest(isPrefsPut);
    await page.getByRole('link', { name: 'Pipelines' }).first().click();
    await expect(page).toHaveURL(/\/workflows$/);
    expect((await saved).postDataJSON()).toEqual({ tour_done: true });
    await page.getByRole('link', { name: 'My work' }).first().click();
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    await expect(tour(page)).toHaveCount(0);
    const read = prefsRead(page);
    await page.reload();
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    await read;
    await expect(tour(page)).toHaveCount(0);
  });

  test('on a first visit, the second stop points at the welcome', async ({ page }) => {
    await useNoRuns(page);
    await page.goto('/');
    await expect(
      page.getByRole('heading', { name: "You haven't started an analysis yet" }),
    ).toBeVisible();
    await tour(page).getByRole('button', { name: 'Next' }).click();
    await expect(tour(page).getByRole('heading', { name: 'Follow what is running' })).toBeVisible();
    await expect(page.locator('[data-tour="welcome"]')).toHaveAttribute('data-tour-active', '');
  });

  test('with the sidebar collapsed, the Pipelines stop points at Open sidebar', async ({
    page,
  }) => {
    await page.goto('/');
    const stop = tour(page);
    await expect(stop).toBeVisible();
    await page.getByRole('button', { name: 'Collapse sidebar' }).click();
    await stop.getByRole('button', { name: 'Next' }).click();
    await stop.getByRole('button', { name: 'Next' }).click();
    await expect(stop.getByRole('heading', { name: 'Find a pipeline' })).toBeVisible();
    await expect(page.locator('[data-tour="reopen"]')).toHaveAttribute('data-tour-active', '');
    await expect(stop).not.toHaveClass(/centred/);
    // Opening the sidebar moves the stop to its link.
    await page.getByRole('button', { name: 'Open sidebar' }).click();
    await expect(page.locator('[data-tour="nav-pipelines"]')).toHaveAttribute(
      'data-tour-active',
      '',
    );
  });

  test('shows only in the Lab view', async ({ page }) => {
    await page.goto('/');
    await expect(tour(page)).toBeVisible();
    await page.getByRole('group', { name: 'View' }).getByRole('button', { name: 'Expert' }).click();
    await expect(tour(page)).toHaveCount(0);
  });

  for (const theme of THEMES) {
    test(`a tour stop is accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto('/');
      await expect(tour(page)).toBeVisible();
      await expectNoSeriousA11yViolations(page);
    });
  }

  test('fits a phone: every stop stays on screen, with no sideways scroll', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/');
    const stop = tour(page);
    for (let i = 0; i < 4; i++) {
      await expect(stop).toBeInViewport({ ratio: 1 });
      await expectNoOverflow(page);
      if (i < 3) await stop.getByRole('button', { name: 'Next' }).click();
    }
    // The nav is behind the menu on a phone: the Pipelines stop points at the menu button.
    await stop.getByRole('button', { name: 'Back' }).click();
    await expect(page.locator('[data-tour="menu"]')).toHaveAttribute('data-tour-active', '');
  });
});

test.describe('replaying the tour', () => {
  test.use({ view: 'lab' });

  test('Settings replays it on My work', async ({ page }) => {
    const read = prefsRead(page);
    await page.goto('/');
    await expect(page.getByRole('heading', { name: 'My work' })).toBeVisible();
    await read;
    await expect(tour(page)).toHaveCount(0);
    await page.goto('/settings');
    const saved = page.waitForRequest(isPrefsPut);
    await page.getByRole('button', { name: 'Replay the tour' }).click();
    expect((await saved).postDataJSON()).toEqual({ tour_done: false });
    await expect(page).toHaveURL(/\/$/);
    await expect(tour(page)).toBeVisible();
    await expect(tour(page)).toBeFocused();
    await expect(tour(page)).toContainText('1 of 4');
  });
});

test.describe('replaying from the Expert view', () => {
  test('says the tour lives in the Lab view', async ({ page }) => {
    await page.goto('/settings');
    await expect(page.getByText(/first-visit tour is part of the Lab view/)).toBeVisible();
    await expect(page.getByRole('button', { name: 'Replay the tour' })).toHaveCount(0);
  });
});

/** Open the Learn panel and check its title, three how-tos and the link to the user guide. */
async function openLearn(page: Page, title: string) {
  await learnButton(page).click();
  const panel = learnPanel(page);
  await expect(panel).toBeVisible();
  await expect(panel.getByRole('heading', { level: 2, name: title })).toBeFocused();
  await expect(panel.getByRole('heading', { level: 3 })).toHaveCount(3);
  await expect(panel.getByRole('link', { name: /Read the guide/ })).toHaveAttribute(
    'href',
    'https://github.com/nh13/biffix/blob/main/docs/user-guide.md',
  );
  await expect(learnButton(page)).toHaveAttribute('aria-expanded', 'true');
  return panel;
}

test.describe('the Learn panel (Lab)', () => {
  test.use({ view: 'lab' });

  for (const [path, title] of [
    ['/', 'About My work'],
    ['/runs', 'About Analyses'],
    ['/runs/run_01HGWDdone', 'About this analysis'],
    ['/workflows', 'About pipelines'],
    ['/workflows/aabbccddeeff%3Av1/about', 'About this pipeline'],
    [RUN_TAB, 'About starting an analysis'],
    ['/experiments', 'About experiments'],
    ['/data', 'About Data'],
    ['/settings', 'About Settings'],
  ] as const) {
    test(`${path} has its own Learn panel`, async ({ page }) => {
      await page.goto(path);
      await openLearn(page, title);
    });
  }

  test('closes with its button or Escape, handing focus back to "?"', async ({ page }) => {
    await page.goto('/workflows');
    const panel = await openLearn(page, 'About pipelines');
    // Not a modal: the page under it still works.
    await expect(page.getByRole('heading', { level: 1, name: 'Pipelines' })).toBeInViewport();
    await page.keyboard.press('Escape');
    await expect(panel).toHaveCount(0);
    await expect(learnButton(page)).toBeFocused();
    await expect(learnButton(page)).toHaveAttribute('aria-expanded', 'false');

    await openLearn(page, 'About pipelines');
    await panel.getByRole('button', { name: 'Close the Learn panel' }).click();
    await expect(panel).toHaveCount(0);
    await expect(learnButton(page)).toBeFocused();
  });

  test('follows the page while open', async ({ page }) => {
    await page.goto('/workflows');
    await openLearn(page, 'About pipelines');
    await page.getByRole('link', { name: 'Experiments' }).first().click();
    await expect(
      learnPanel(page).getByRole('heading', { level: 2, name: 'About experiments' }),
    ).toBeVisible();
  });

  for (const theme of THEMES) {
    test(`an open Learn panel is accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto('/');
      await openLearn(page, 'About My work');
      await expectNoSeriousA11yViolations(page);
    });
  }

  test('beside the page on a wide screen: Tab and Shift+Tab reach the page', async ({ page }) => {
    await page.goto('/workflows');
    const panel = await openLearn(page, 'About pipelines');
    await expect(panel).toHaveAttribute('role', 'complementary');
    expect(await page.locator('.app').getAttribute('inert')).toBeNull();
    await page.keyboard.press('Shift+Tab');
    expect(await focusIn(page, '#learn-panel')).toBe(false);
    expect(await focusIn(page, '.app')).toBe(true);
    // The page works with the panel open, and the panel stays.
    await page.getByRole('searchbox').first().fill('rna');
    await expect(panel).toBeVisible();
  });

  test('fits a phone as a modal sheet: focus stays in it, and Close hands it back', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto('/runs');
    const panel = await openLearn(page, 'About Analyses');
    await expect(panel).toBeInViewport();
    await expect(panel).toHaveAttribute('role', 'dialog');
    await expect(panel).toHaveAttribute('aria-modal', 'true');
    await expectNoOverflow(page);
    // The page under it is inert: neither Tab nor Shift+Tab walks into it.
    await expect(page.locator('.app')).toHaveAttribute('inert', '');
    for (const key of ['Shift+Tab', 'Shift+Tab', 'Tab', 'Tab', 'Tab']) {
      await page.keyboard.press(key);
      expect(await focusIn(page, '.app'), key).toBe(false);
    }
    await panel.getByRole('button', { name: 'Close the Learn panel' }).click();
    await expect(panel).toHaveCount(0);
    expect(await page.locator('.app').getAttribute('inert')).toBeNull();
    await expect(learnButton(page)).toBeFocused();
    await openLearn(page, 'About Analyses');
    await page.keyboard.press('Escape');
    await expect(learnButton(page)).toBeFocused();
  });

  test('leaves the upload tray in view beside it', async ({ page }) => {
    // An upload a reload interrupted: the tray offers to resume it.
    await page.addInitScript(() =>
      localStorage.setItem(
        'biffix:uploads',
        JSON.stringify([
          {
            upload_id: 'up-learn',
            key: 'uploads/2026-09-30/genome.fa',
            uri: 's3://genomics-project/uploads/2026-09-30/genome.fa',
            name: 'genome.fa',
            size: 3 * 1024 * 1024,
            last_modified: 1_700_000_000_000,
            part_size: 1024 * 1024,
            parts: 1,
          },
        ]),
      ),
    );
    await page.goto('/workflows');
    const tray = page.getByRole('region', { name: 'Uploads' });
    await expect(tray).toBeVisible();
    const panel = await openLearn(page, 'About pipelines');
    await expect(tray).toBeInViewport({ ratio: 1 });
    const [t, p] = [await tray.boundingBox(), await panel.boundingBox()];
    expect(t!.x + t!.width).toBeLessThanOrEqual(p!.x);
  });
});

test.describe('the Learn panel (Expert)', () => {
  for (const [path, title] of [
    ['/', 'About Home'],
    ['/runs', 'About Runs'],
    ['/runs/run_01HGWDdone', 'About this run'],
    ['/workflows', 'About Workflows'],
    [RUN_TAB, 'About the Run form'],
    ['/experiments', 'About Experiments'],
    ['/settings/references', 'About References'],
    ['/usage', 'About Usage'],
    ['/status', 'About Status'],
  ] as const) {
    test(`${path} has its own Learn panel`, async ({ page }) => {
      await page.goto(path);
      await openLearn(page, title);
    });
  }

  test('switches words with the view', async ({ page }) => {
    await page.goto('/runs');
    await openLearn(page, 'About Runs');
    await page.getByRole('group', { name: 'View' }).getByRole('button', { name: 'Lab' }).click();
    await expect(
      learnPanel(page).getByRole('heading', { level: 2, name: 'About Analyses' }),
    ).toBeVisible();
  });

  for (const theme of THEMES) {
    test(`an open Learn panel is accessible (${theme})`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto('/runs');
      await openLearn(page, 'About Runs');
      await expectNoSeriousA11yViolations(page);
    });
  }
});

/** The project's own bucket: always listed, so "no data folders" is a list of it alone. */
const PROJECT_ONLY = [
  {
    id: 'project',
    name: 'Project data',
    description: "This project's own bucket.",
    kind: 'project',
    uri: 's3://genomics-project/',
    writable: true,
  },
];
/** The Lab analyses' version, as the page asks for it. */
const LAB_VERSION = /^\/workflows\/c4d5e6f7a8b9(%3A|:)v1$/;

/** The Run tab's file picker for the samplesheet (wizard step 2), opened. */
async function openPicker(page: Page) {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Add samples ▾' }).click();
  await page.getByRole('menuitem', { name: 'Choose files…' }).click();
  return page.getByRole('dialog', { name: /Choose files for Samplesheet/ });
}

/** The Run tab's reference step (wizard step 3), reached from an example's samples. */
async function toReferenceStep(page: Page) {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Start from example ▾' }).click();
  await page.getByRole('menuitem', { name: /Human, 6 paired-end samples/ }).click();
  await page.getByRole('button', { name: /^Next: / }).click();
}

test.describe('empty states (Lab)', () => {
  test.use({ view: 'lab' });

  test('Pipelines, none registered', async ({ page }) => {
    await onApi(page, 'GET', '/workflows', (route) => route.fulfill({ json: [] }));
    await page.goto('/workflows');
    await expect(
      page.getByText(/No pipelines yet — a developer registers them with/),
    ).toBeVisible();
    await expect(page.locator('code', { hasText: 'biffix workflows register' })).toBeVisible();
  });

  test('My work, none pinned', async ({ page }) => {
    await page.goto('/');
    const pinned = page.getByRole('region', { name: 'Pinned pipelines' });
    await expect(pinned).toContainText('Pin a pipeline to keep it on My work');
    await expect(pinned.getByRole('link', { name: 'Pipelines' })).toHaveAttribute(
      'href',
      '/workflows',
    );
  });

  test('wizard step 2, no data folders: the project storage stays browsable', async ({ page }) => {
    await onApi(page, 'GET', '/data-sources', (route) => route.fulfill({ json: PROJECT_ONLY }));
    const picker = await openPicker(page);
    await expect(picker).toContainText(
      'No data folders are connected — an admin adds them in the deploy config',
    );
    await expect(picker).toContainText("Your project's own storage is below");
    await expect(picker.getByRole('alert')).toHaveCount(0);
    // The project's bucket is open under the note.
    await expect(picker.locator('.row').first()).toBeVisible();
  });

  test('wizard step 2, no data source at all', async ({ page }) => {
    await onApi(page, 'GET', '/data-sources', (route) => route.fulfill({ json: [] }));
    const picker = await openPicker(page);
    await expect(picker).toContainText('No data folders are connected');
    await expect(picker).toContainText('You can still paste');
    await expect(picker.getByText('Nothing here yet.')).toHaveCount(0);
  });

  test('wizard step 2, data folders connected: no note', async ({ page }) => {
    const picker = await openPicker(page);
    await expect(picker.locator('.row').first()).toBeVisible();
    await expect(picker).not.toContainText('No data folders are connected');
  });

  test('wizard step 3, no reference genomes', async ({ page }) => {
    await onApi(page, 'GET', '/references', (route) => route.fulfill({ json: [] }));
    await toReferenceStep(page);
    await expect(
      page.getByRole('heading', { name: 'Reference & options', exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText(/No reference genomes registered yet — an admin adds them under/),
    ).toBeVisible();
    await expect(page.getByRole('link', { name: 'Settings → References' })).toHaveAttribute(
      'href',
      '/settings/references',
    );
    // The link goes to an Expert page, and says so in the Lab view.
    await expect(page.getByText('Settings → References (Expert view).')).toBeVisible();
  });

  test('wizard step 3, references registered but none fits', async ({ page }) => {
    // The form's own query (its kind and keys) finds none; the unfiltered list has some.
    await onApi(page, 'GET', '/references', async (route) => {
      const q = new URL(route.request().url()).searchParams;
      if (q.has('kind') || q.has('keys')) return route.fulfill({ json: [], headers: CORS });
      return route.fallback();
    });
    await toReferenceStep(page);
    await expect(
      page.getByText(/No registered reference has .+; an admin can add one/),
    ).toBeVisible();
    await expect(page.getByText(/registered yet/)).toHaveCount(0);
  });

  /** The run's reports are none, and (when `undeclared`) its version declares none either. */
  async function useNoResults(page: Page, undeclared: boolean) {
    await onApi(page, 'GET', '/runs/run_01HGWDdone/reports', (route) =>
      route.fulfill({ json: { reports: [] }, headers: CORS }),
    );
    if (!undeclared) return;
    await onApi(page, 'GET', LAB_VERSION, async (route) => {
      const res = await route.fetch();
      const version = await res.json();
      delete version.form_schema.reports;
      await route.fulfill({ response: res, json: version });
    });
  }

  test('Results, none declared: the registrar sees how to declare them', async ({ page }) => {
    await signInAs(page, 'bob'); // who registered the version
    await useNoResults(page, true);
    await page.goto('/runs/run_01HGWDdone');
    const results = page.getByRole('region', { name: /^Results/ });
    await expect(results).toContainText("This pipeline doesn't declare key results");
    await expect(results.getByRole('link', { name: 'all files are here →' })).toBeVisible();
    await expect(results).toContainText('reports:');
  });

  test('Results, none declared: anyone else sees no author hint', async ({ page }) => {
    await signInAs(page, 'priya');
    await useNoResults(page, true);
    await page.goto('/runs/run_01HGWDdone');
    const results = page.getByRole('region', { name: /^Results/ });
    await expect(results).toContainText("This pipeline doesn't declare key results");
    await expect(results).not.toContainText('reports:');
  });

  test('Results, declared but none written', async ({ page }) => {
    await signInAs(page, 'bob');
    await useNoResults(page, false);
    await page.goto('/runs/run_01HGWDdone');
    const results = page.getByRole('region', { name: /^Results/ });
    await expect(results).toContainText('No results were written');
    await expect(results.getByRole('link', { name: 'all files are here →' })).toBeVisible();
    await expect(results).not.toContainText("doesn't declare");
    await expect(results).not.toContainText('reports:');
  });
});

test.describe('empty states (Expert)', () => {
  test('Workflows, none registered', async ({ page }) => {
    await onApi(page, 'GET', '/workflows', (route) => route.fulfill({ json: [] }));
    await page.goto('/workflows');
    await expect(
      page.getByText(/No workflows yet — a developer registers them with/),
    ).toBeVisible();
  });

  test('Data, no data folders: the project storage stays browsable', async ({ page }) => {
    await onApi(page, 'GET', '/data-sources', (route) =>
      route.fulfill({ json: PROJECT_ONLY, headers: CORS }),
    );
    await page.goto('/data');
    await expect(page.getByText(/No data folders are connected/)).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'Data sources' })).toContainText(
      'Project data',
    );
  });

  test('Data, data folders connected: no note', async ({ page }) => {
    await page.goto('/data');
    await expect(page.getByRole('navigation', { name: 'Data sources' })).toBeVisible();
    await expect(page.getByText(/No data folders are connected/)).toHaveCount(0);
  });
});
