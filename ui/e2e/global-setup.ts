import { readdirSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { chromium, type FullConfig } from '@playwright/test';
import { signIn } from './fixtures';

// Warms the dev server before any test runs. A cold dev server (a fresh `npm ci`, or a dropped
// `node_modules/.vite`) compiles a route's modules on its first load, and every worker's first test
// lands on one at once: on a loaded machine that compile alone can outlast the test's first
// `expect`. Loading each route once here pays that cost up front, outside any test's timeout.

/** A mock id for each dynamic route segment (`mock/fixtures`); `data`'s rest may be empty. */
const PARAMS: Record<string, string> = {
  'experiments/[id]': 'exp_liver_tumor',
  'runs/[id]': 'run_01HGWDdone',
  'workflows/[id]': 'aabbccddeeff%3Av1',
  'data/[...rest]': '',
};

/** One URL per `+page.svelte` under `src/routes`. */
function routes(dir = 'src/routes'): string[] {
  const urls: string[] = [];
  const walk = (at: string) => {
    for (const entry of readdirSync(at, { withFileTypes: true })) {
      if (entry.isDirectory()) walk(join(at, entry.name));
      else if (entry.name === '+page.svelte') urls.push(url(relative(dir, at).split(sep)));
    }
  };
  walk(dir);
  return urls;
}

function url(segments: string[]): string {
  const out = segments.map((segment, i) => {
    if (!segment.startsWith('[')) return segment;
    const key = `${segments[i - 1]}/${segment}`;
    const value = PARAMS[key];
    if (value === undefined) throw new Error(`global-setup: no mock value for ${key}`);
    return value;
  });
  return `/${out.filter((s) => s !== '').join('/')}`;
}

export default async function globalSetup(config: FullConfig): Promise<void> {
  const { baseURL, launchOptions } = config.projects[0].use;
  const browser = await chromium.launch(launchOptions);
  // Signed in, as every test is, so a route renders rather than sending the page to sign in.
  const open = async (path: string) => {
    const page = await browser.newPage({ baseURL });
    await signIn(page);
    await page.goto(path);
    // Settled once the route's modules have loaded: no request for half a second. Best effort: a
    // route that never settles (one that polls) is compiled by the time this gives up.
    await page.waitForLoadState('networkidle', { timeout: 60_000 }).catch(() => {});
    await page.close();
  };
  try {
    // The shell, which every route shares, first; then the routes, a few at a time.
    const [root, ...rest] = routes().sort();
    await open(root);
    await Promise.all(
      Array.from({ length: 4 }, async () => {
        for (let path = rest.shift(); path !== undefined; path = rest.shift()) await open(path);
      }),
    );
  } finally {
    await browser.close();
  }
}
