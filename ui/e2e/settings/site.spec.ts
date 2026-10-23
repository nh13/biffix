import type { Page } from '@playwright/test';
import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  API,
  CORS,
  preflight,
  THEMES,
  useTheme,
} from '../fixtures';

// Settings: the site title and logo (partial saves, shown in the chrome at once), the logo size
// limit, a failed load, the alert recipients, and the support contact.

// A 1×1 PNG.
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
  'base64',
);

// The site settings as a server would store them, per test (the shared mock would leak one test's
// save into another running alongside it). A partial update, like the backend's.
async function fakeSettings(page: Page) {
  const stored: { title: string | null; logo_data_url: string | null } = {
    title: 'biffix (mock)',
    logo_data_url: null,
  };
  await page.route(`${API}/settings`, (route) => {
    const req = route.request();
    if (req.method() === 'PUT') {
      const u = req.postDataJSON();
      if ('title' in u) stored.title = (u.title ?? '').trim() || null;
      if ('logo_data_url' in u) stored.logo_data_url = u.logo_data_url || null;
    } else if (req.method() !== 'GET') return route.fallback();
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify(stored) });
  });
}

test('a saved title and logo show in the chrome at once, and only changed fields are sent', async ({
  page,
}) => {
  await fakeSettings(page);
  await page.goto('/settings');
  const titleInput = page.getByRole('textbox', { name: 'Title' });
  await expect(titleInput).toHaveValue('biffix (mock)');
  const save = page.getByRole('button', { name: 'Save' });
  await expect(save).toBeDisabled(); // nothing changed yet
  const saved = () =>
    page.waitForRequest((r) => r.url() === `${API}/settings` && r.method() === 'PUT');

  // A title-only edit sends only the title: the stored logo is kept, not cleared.
  await titleInput.fill('Acme Genomics');
  let put = saved();
  await save.click();
  expect((await put).postDataJSON()).toEqual({ title: 'Acme Genomics' });
  await expect(page.getByText('Saved ✓')).toBeVisible();
  const brand = page.locator('aside .brand');
  await expect(brand).toHaveText('Acme Genomics');
  await expect(page).toHaveTitle('Acme Genomics');

  // A logo shows in the sidebar and becomes the favicon; removing it restores the default icon.
  await page.getByLabel('Logo image').setInputFiles({
    name: 'logo.png',
    mimeType: 'image/png',
    buffer: PNG,
  });
  await expect(page.getByRole('img', { name: 'logo preview' })).toBeVisible();
  put = saved();
  await save.click();
  expect(Object.keys((await put).postDataJSON())).toEqual(['logo_data_url']);
  await expect(brand.locator('img')).toHaveAttribute('src', /^data:image\/png;base64,/);
  const icon = page.locator('link[rel="icon"]');
  await expect(icon).toHaveAttribute('href', /^data:image\/png;base64,/);

  await page.reload();
  await expect(page.locator('aside .brand')).toHaveText('Acme Genomics');
  await expect(titleInput).toHaveValue('Acme Genomics');

  await page.getByRole('button', { name: 'Remove', exact: true }).first().click();
  // Blanking the title clears it (null), and the default comes back.
  await titleInput.fill('  ');
  put = saved();
  await save.click();
  expect((await put).postDataJSON()).toEqual({ title: null, logo_data_url: null });
  await expect(page.locator('aside .brand')).toHaveText('biffix');
  await expect(page.locator('aside .brand img')).toHaveCount(0);
  await expect(icon).toHaveAttribute('href', '/favicon.svg');
  await expect(icon).toHaveAttribute('type', 'image/svg+xml');
});

test('a logo too large once encoded is refused before Save', async ({ page }) => {
  await page.goto('/settings');
  await expect(page.getByRole('textbox', { name: 'Title' })).toBeVisible();
  // 400 KB is under 512 KiB as a file but over it as a base64 data URL (what the server caps).
  await page.getByLabel('Logo image').setInputFiles({
    name: 'big.png',
    mimeType: 'image/png',
    buffer: Buffer.alloc(400 * 1024),
  });
  await expect(page.getByRole('alert')).toContainText('Image too large (400 KB');
  await expect(page.getByRole('img', { name: 'logo preview' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Save' })).toBeDisabled();
});

test('a failed load offers Retry, not a form that would save over the stored settings', async ({
  page,
}) => {
  let fail = true;
  await page.route(`${API}/settings`, (route) =>
    fail && route.request().method() === 'GET'
      ? route.fulfill({ status: 503, body: 'database resuming' })
      : route.fallback(),
  );
  await page.goto('/settings');
  await expect(page.getByRole('alert')).toContainText("Couldn't load the site settings");
  await expect(page.getByRole('button', { name: 'Save' })).toHaveCount(0);
  await expect(page.getByRole('textbox', { name: 'Title' })).toHaveCount(0);

  fail = false;
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByRole('textbox', { name: 'Title' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Save' })).toBeDisabled();
});

test('alert recipients can be added and removed', async ({ page }) => {
  // Per-test recipients, like the mock's (a removal must not leak into a retry or another test).
  let recipients = [
    { email: 'ops@lab.example.org', status: 'confirmed' },
    { email: 'oncall@lab.example.org', status: 'pending' },
  ];
  const body = () => JSON.stringify({ configured: true, recipients });
  const alertsApi = (u: URL) => u.origin === API && u.pathname.startsWith('/settings/alerts');
  await page.route(alertsApi, (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    if (path === '/settings/alerts/remove' && req.method() === 'POST') {
      const { email } = req.postDataJSON();
      recipients = recipients.filter((r) => r.email !== email);
      return route.fulfill({ status: 204 });
    }
    if (path !== '/settings/alerts') return route.fulfill({ status: 404, body: 'not found' });
    if (req.method() === 'POST') {
      recipients = [...recipients, { email: req.postDataJSON().email, status: 'pending' }];
    }
    return route.fulfill({ contentType: 'application/json', body: body() });
  });
  // No address may reach a request URL (access logs keep URLs): it travels in the body.
  const urls: string[] = [];
  page.on('request', (r) => urls.push(r.url()));
  await page.goto('/settings');
  const list = page.locator('ul.recipients');
  await expect(list.getByText('ops@lab.example.org')).toBeVisible();

  await page.getByRole('textbox', { name: 'Recipient email' }).fill('new-oncall@lab.example.org');
  await page.getByRole('button', { name: 'Add', exact: true }).click();
  const added = list.locator('li', { hasText: 'new-oncall@lab.example.org' });
  await expect(added).toContainText('awaiting confirmation');
  await expect(page.getByRole('textbox', { name: 'Recipient email' })).toHaveValue('');

  const ops = list.locator('li', { hasText: 'ops@lab.example.org' });
  await ops.getByRole('button', { name: 'Remove' }).click();
  await expect(ops).toHaveCount(0);
  expect(urls.some((u) => u.includes('/settings/alerts/remove'))).toBe(true);
  const leaked = urls.filter((u) =>
    /ops(@|%40)lab\.example\.org|new-oncall/i.test(decodeURIComponent(u)),
  );
  expect(leaked).toEqual([]);
});

test('an alert recipient status the page does not know shows as itself', async ({ page }) => {
  await page.route(`${API}/settings/alerts`, (route) =>
    route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        configured: true,
        recipients: [{ email: 'gone@lab.example.org', status: 'lapsed' }],
      }),
    }),
  );
  await page.goto('/settings');
  const row = page.locator('ul.recipients li', { hasText: 'gone@lab.example.org' });
  await expect(row.locator('.status')).toHaveText('lapsed');
  await expect(row).not.toContainText('confirmed');
  await expect(row.getByRole('button', { name: 'Remove' })).toHaveCount(0);
  await expect(row).not.toContainText('lapses after 3 days');
});

for (const theme of THEMES) {
  test(`settings (${theme}) has no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto('/settings');
    await expect(page.getByRole('textbox', { name: 'Title' })).toBeVisible();
    await expect(page.locator('ul.recipients')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}

test.describe('Settings: support contact', () => {
  test('saves and clears the site’s support contact, showing the server’s refusal', async ({
    page,
  }) => {
    // This page's own settings store, so the shared mock's are never changed.
    const stored: Record<string, unknown> = { title: 'biffix (mock)', logo_data_url: null };
    const sent: unknown[] = [];
    await page.route(`${API}/settings`, async (route) => {
      if (await preflight(route)) return;
      if (route.request().method() === 'PUT') {
        const update = route.request().postDataJSON() as { support?: { email: string } | null };
        sent.push(update);
        if (update.support && !update.support.email.includes('@'))
          return route.fulfill({
            status: 400,
            headers: { ...CORS, 'content-type': 'text/plain' },
            body: 'support.email: `nope` is not a valid email address (one `@`, a dotted domain, no spaces, and none of `? & # % , < > =` (unsafe in a mail link))',
          });
        if (update.support === null) delete stored.support;
        else Object.assign(stored, update);
      }
      await route.fulfill({ headers: CORS, json: stored });
    });
    await page.goto('/settings');
    const name = page.getByRole('textbox', { name: 'Name', exact: true });
    const email = page.getByRole('textbox', { name: 'Email', exact: true });
    await name.fill('Sam Okafor');
    await email.fill('nope');
    await page.getByRole('button', { name: 'Set contact' }).click();
    await expect(page.getByText('is not a valid email address')).toBeVisible();

    await email.fill('sam@lab.example.org');
    await page.getByRole('button', { name: 'Set contact' }).click();
    await expect(page.getByText('Saved ✓')).toBeVisible();
    expect(sent.at(-1)).toEqual({
      support: { name: 'Sam Okafor', email: 'sam@lab.example.org', url: null },
    });

    await page.getByRole('button', { name: 'Clear' }).click();
    await expect(page.getByText('Cleared ✓')).toBeVisible();
    expect(sent.at(-1)).toEqual({ support: null });
    await expect(name).toHaveValue('');
  });

  test('saving one section keeps unsaved edits in the other', async ({ page }) => {
    const stored: Record<string, unknown> = { title: 'biffix (mock)', logo_data_url: null };
    await page.route(`${API}/settings`, async (route) => {
      if (await preflight(route)) return;
      if (route.request().method() === 'PUT') Object.assign(stored, route.request().postDataJSON());
      await route.fulfill({ headers: CORS, json: stored });
    });
    await page.goto('/settings');
    const title = page.getByRole('textbox', { name: 'Title', exact: true });
    const name = page.getByRole('textbox', { name: 'Name', exact: true });
    const email = page.getByRole('textbox', { name: 'Email', exact: true });
    await title.fill('Genomics core');
    await name.fill('Sam Okafor');
    await email.fill('sam@lab.example.org');
    await page.getByRole('button', { name: 'Set contact' }).click();
    await expect(page.getByText('Saved ✓')).toBeVisible();
    // The title was not saved, and is still as typed.
    expect(stored.title).toBe('biffix (mock)');
    await expect(title).toHaveValue('Genomics core');

    await name.fill('Ana Ruiz');
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => stored.title).toBe('Genomics core');
    // The reply is applied (the site form says so; typing in the contact cleared its own) before
    // the other form's draft is checked.
    await expect(page.getByText('Saved ✓')).toBeVisible();
    await expect(name).toHaveValue('Ana Ruiz');
  });

  test('shows the server’s refusal when only an Admin may set the contact', async ({ page }) => {
    await page.route(`${API}/settings`, async (route) => {
      if (await preflight(route)) return;
      if (route.request().method() === 'PUT')
        return route.fulfill({
          status: 403,
          headers: { ...CORS, 'content-type': 'text/plain' },
          body: 'only an admin may set the support contact',
        });
      await route.fulfill({ headers: CORS, json: { title: 'biffix (mock)', logo_data_url: null } });
    });
    await page.goto('/settings');
    await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Sam Okafor');
    await page.getByRole('textbox', { name: 'Email', exact: true }).fill('sam@lab.example.org');
    await page.getByRole('button', { name: 'Set contact' }).click();
    await expect(
      page.getByRole('alert').filter({ hasText: 'only an admin may set the support contact' }),
    ).toBeVisible();
  });
});
