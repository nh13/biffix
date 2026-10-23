import { test as signedOut, expect, type Page } from '@playwright/test';
import { API, test } from '../fixtures';

// The app shell's sign-in gate and its service-health icon. The gate tests start signed out (the
// base test, not the token-seeding fixture); the mock's IdP is an outside host, so its pages are
// stood in for here and each visit recorded.

const IDP = /^https:\/\/mock\.auth\.us-east-1\.amazoncognito\.com\//;

async function watchIdp(page: Page): Promise<string[]> {
  const visits: string[] = [];
  await page.route(IDP, (route) => {
    visits.push(route.request().url());
    return route.fulfill({ contentType: 'text/html', body: '<h1>Mock IdP</h1>' });
  });
  return visits;
}

signedOut('/signed-out stays signed out: no redirect to the IdP', async ({ page }) => {
  const visits = await watchIdp(page);
  await page.goto('/signed-out');
  await expect(page.getByRole('heading', { name: "You're signed out" })).toBeVisible();
  await page.waitForLoadState('networkidle');
  expect(visits).toEqual([]);
  await expect(page).toHaveURL(/\/signed-out$/);
  // No app chrome either.
  await expect(page.getByRole('link', { name: 'Runs' })).toHaveCount(0);
});

signedOut('signing in again says why it could not start', async ({ page }) => {
  await page.route(`${API}/auth/config`, (route) => route.abort());
  await page.goto('/signed-out');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toHaveText(/^Sign-in couldn't start: \S/);
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeEnabled();
});

signedOut('an app page with no session goes to the IdP to sign in', async ({ page }) => {
  const visits = await watchIdp(page);
  await page.goto('/runs');
  await expect(page.getByRole('heading', { name: 'Mock IdP' })).toBeVisible();
  expect(visits[0]).toMatch(/\/oauth2\/authorize\?/);
});

signedOut(
  "a failed sign-in's Try again signs in afresh, even inside the re-auth cooldown",
  async ({ page }) => {
    const visits = await watchIdp(page);
    // A forced re-auth was just claimed: the loop guard now refuses another for a minute, so only
    // the shell's own sign-in check can reach the IdP.
    await page.addInitScript(() => sessionStorage.setItem('biffix-reauth-at', String(Date.now())));
    await page.goto(
      '/auth/callback?error=access_denied&error_description=Token%20exchange%20failed',
    );
    await expect(page.getByRole('heading', { name: 'Sign-in failed' })).toBeVisible();
    await expect(page.getByText('Token exchange failed')).toBeVisible();
    await page.getByRole('link', { name: 'Try again' }).click();
    await expect(page.getByRole('heading', { name: 'Mock IdP' })).toBeVisible();
    expect(visits).toHaveLength(1);
  },
);

test('a refused session whose sign-in the loop guard held back offers Sign in', async ({
  page,
}) => {
  const visits = await watchIdp(page);
  // A forced re-auth was just claimed: the automatic one is held back for a minute.
  await page.addInitScript(() => sessionStorage.setItem('biffix-reauth-at', String(Date.now())));
  await page.route(
    (url) => url.origin === API && url.pathname === '/runs/run_01HGWDdone',
    (route) =>
      route.request().method() === 'OPTIONS'
        ? route.fallback()
        : route.fulfill({ status: 401, headers: { 'access-control-allow-origin': '*' } }),
  );
  await page.goto('/runs/run_01HGWDdone');
  await expect(page.getByRole('heading', { name: 'Sign in again' })).toBeVisible();
  await expect(page.getByText('Your session has ended. Sign in again to carry on.')).toBeVisible();
  expect(visits).toEqual([]);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('heading', { name: 'Mock IdP' })).toBeVisible();
});

signedOut('a sign-in that cannot start says so, with a Retry', async ({ page }) => {
  await page.route(/\/auth\/config$/, (route) => route.fulfill({ status: 503, body: 'down' }));
  await page.goto('/runs');
  const alert = page.getByRole('alert');
  await expect(alert.getByRole('heading', { name: "Can't reach biffix" })).toBeVisible();
  await expect(alert).toContainText('OAuth configuration unavailable (503)');
  await expect(alert).not.toContainText('Error:');
  await expect(alert.getByRole('button', { name: 'Retry' })).toBeVisible();
});

for (const [status, color] of [
  ['ok', 'rgb(48, 164, 108)'],
  ['degraded', 'rgb(245, 165, 36)'],
  ['down', 'rgb(229, 72, 77)'],
] as const) {
  test(`the Status icon shows a ${status} service`, async ({ page }) => {
    await page.route(/\/status$/, (route) =>
      route.fulfill({ json: { status, components: [], checked_at_micros: 0 } }),
    );
    await page.goto('/');
    const icon = page.getByRole('link', { name: 'Status' }).locator('svg');
    await expect(icon).toHaveCSS('color', color);
  });
}

test('the theme follows the OS until the viewer picks one', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto('/');
  const html = page.locator('html');
  await expect(html).toHaveClass(/\bdark\b/);
  // Showing the OS's theme is not a choice: nothing is saved, and an OS change is followed.
  expect(await page.evaluate(() => localStorage.getItem('biffix-theme'))).toBeNull();
  await page.emulateMedia({ colorScheme: 'light' });
  await expect(html).not.toHaveClass(/\bdark\b/);

  await page.getByRole('button', { name: 'Toggle theme' }).click();
  await expect(html).toHaveClass(/\bdark\b/);
  expect(await page.evaluate(() => localStorage.getItem('biffix-theme'))).toBe('dark');
  // A choice wins over the OS from then on.
  await page.emulateMedia({ colorScheme: 'light' });
  await page.reload();
  await expect(html).toHaveClass(/\bdark\b/);
});

test('the Status icon turns red when /status cannot be reached', async ({ page }) => {
  await page.route(/\/status$/, (route) => route.fulfill({ status: 502, body: 'bad gateway' }));
  await page.goto('/');
  await expect(page.getByRole('link', { name: 'Status' }).locator('svg')).toHaveCSS(
    'color',
    'rgb(229, 72, 77)',
  );
});
