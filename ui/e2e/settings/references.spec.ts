import type { Page } from '@playwright/test';
import {
  API,
  RUN_TAB,
  test,
  expect,
  expectNoSeriousA11yViolations,
  THEMES,
  useTheme,
} from '../fixtures';
import { PROJECT_SOURCES } from '../../mock/fixtures/data.mjs';
import { bundleProblem, storedBundle } from '../../mock/lib/references.mjs';

// Reference bundles: the Run tab's reference dropdown and Settings → References,
// against the mock's nf-core/rnaseq form (which fills fasta and gtf from a bundle).

type Bundle = { id: string; kind: string; uris: Record<string, string> } & Record<string, unknown>;

/** The list and each bundle's own path on the mock API. */
const REFERENCES = /^http:\/\/localhost:8080\/references(\/[^?]*)?(\?.*)?$/;

/** The shared mock's reference bundles, asked for outside the page. */
const sharedReferences = (page: Page) =>
  page.request.get(`${API}/references`, { headers: { authorization: 'Bearer mock' } });

/**
 * The reference bundles as a server would store them, per test: a test that adds, edits or deletes
 * one must not change the shared mock's list under the tests running alongside it (the dropdown
 * test asserts its exact options) or under its own retry. Seeded from the mock's list, and
 * answering as the mock does, with its checks (`mock/lib/references.mjs`): 400 for an id change or
 * a bundle the backend refuses, then 409 for a taken id or 404 for a missing one, and a warning
 * for a path with "missing" in it.
 */
async function fakeReferences(page: Page): Promise<void> {
  const seed = await sharedReferences(page);
  expect(seed.ok()).toBe(true);
  let stored: Bundle[] = await seed.json();
  const json = (status: number, body: unknown) => ({
    status,
    contentType: 'application/json',
    body: JSON.stringify(body),
  });
  await page.route(REFERENCES, (route) => {
    const req = route.request();
    const url = new URL(req.url());
    const id = url.pathname.match(/^\/references\/(.+)$/)?.[1];
    const bundleId = id === undefined ? undefined : decodeURIComponent(id);
    const method = req.method();
    if (method === 'GET' && bundleId === undefined) {
      const kind = url.searchParams.get('kind');
      const keys = (url.searchParams.get('keys') ?? '').split(',').filter(Boolean);
      return route.fulfill(
        json(
          200,
          stored.filter((b) => (!kind || b.kind === kind) && keys.every((k) => k in b.uris)),
        ),
      );
    }
    if (method === 'DELETE' && bundleId !== undefined) {
      if (!stored.some((b) => b.id === bundleId))
        return route.fulfill({ status: 404, body: 'not found' });
      stored = stored.filter((b) => b.id !== bundleId);
      return route.fulfill({ status: 204 });
    }
    if (method === 'POST' || method === 'PUT') {
      const r = req.postDataJSON() as Bundle;
      if (method === 'PUT' && r.id !== bundleId)
        return route.fulfill({
          status: 400,
          body: "a bundle's id cannot change; add a new bundle instead",
        });
      const problem = bundleProblem(r, Object.values(PROJECT_SOURCES).flat());
      if (problem) return route.fulfill({ status: 400, body: problem });
      const existing = stored.find((b) => b.id === r.id);
      if (method === 'POST' && existing)
        return route.fulfill({ status: 409, body: `reference bundle ${r.id} already exists` });
      if (method === 'PUT' && !existing) return route.fulfill({ status: 404, body: 'not found' });
      const bundle: Bundle = {
        ...storedBundle(r, Date.now() * 1000, 'alice'),
        ...(existing
          ? { created_at_micros: existing.created_at_micros, created_by: existing.created_by }
          : {}),
      };
      // By id, bytewise, as the store lists them.
      stored = [...stored.filter((b) => b.id !== r.id), bundle].sort((a, b) =>
        a.id < b.id ? -1 : a.id > b.id ? 1 : 0,
      );
      const warnings = Object.entries(r.uris ?? {})
        .filter(([, u]) => u.includes('missing'))
        .map(([k, u]) => `${k}: nothing found at ${u}`);
      return route.fulfill(json(method === 'POST' ? 201 : 200, { bundle, warnings }));
    }
    return route.fallback();
  });
}

test('the dropdown offers only references with the keys the form needs', async ({ page }) => {
  await page.goto(RUN_TAB);
  const select = page.getByRole('combobox', { name: /^Reference genome/ });
  await expect(select.locator('option')).toHaveText([
    'Choose a reference…',
    'Human GRCh38 · Ensembl 110',
    'Mouse GRCm39 · Ensembl 110',
  ]);
  // The params it fills are not asked for while a reference provides them.
  await expect(page.getByRole('textbox', { name: /^Genome FASTA/ })).toHaveCount(0);
  await select.selectOption('grch38-ensembl110');
  await expect(page.getByText(/Fills fasta, gtf/)).toBeVisible();
  await expect(page.getByText('Primary assembly, no ALTs.')).toBeVisible();
});

test('own files show the params, and a registered reference hides them again', async ({ page }) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use my own files…' }).click();
  await expect(page.getByRole('textbox', { name: /^Genome FASTA/ })).toBeVisible();
  await page.getByRole('button', { name: 'Use a registered reference instead' }).click();
  await expect(page.getByRole('textbox', { name: /^Genome FASTA/ })).toHaveCount(0);
});

test('a launch names a missing reference, then launches with one and re-runs with it', async ({
  page,
}) => {
  await page.goto(RUN_TAB);
  await page.getByRole('button', { name: 'Use an existing samplesheet file' }).click();
  await page.getByRole('textbox', { name: /^Samplesheet/ }).fill('s3://b/sheet.csv');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  const footer = page.locator('footer.launch-bar');
  await expect(footer.getByRole('button', { name: 'Reference genome: choose one' })).toBeVisible();
  await expect(page.getByRole('combobox', { name: /^Reference genome/ })).toBeFocused();

  await page.getByRole('combobox', { name: /^Reference genome/ }).selectOption('grcm39');
  await expect(footer).toContainText('Ready to launch');
  const launched = page.waitForRequest((r) => r.url().endsWith('/runs') && r.method() === 'POST');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  const body = (await launched).postDataJSON();
  expect(body.reference).toBe('grcm39');
  expect(body.params.fasta).toBeUndefined();
  await expect(page).toHaveURL(/\/runs\/run_mock\d+$/);

  // Re-run picks the same reference (opened directly: the mock run has not finished).
  const runId = page.url().split('/').pop();
  await page.goto(`${RUN_TAB}?from=${runId}`);
  await expect(page.getByRole('combobox', { name: /^Reference genome/ })).toHaveValue('grcm39');
  await expect(page.getByRole('textbox', { name: /^Genome FASTA/ })).toHaveCount(0);
});

test('admins add, edit and delete references', async ({ page }) => {
  await fakeReferences(page);
  await page.goto('/settings');
  await page.getByRole('link', { name: 'Manage references →' }).click();
  await expect(page).toHaveURL(/\/settings\/references$/);
  await expect(page.getByRole('cell', { name: /Human GRCh38 · Ensembl 110/ })).toBeVisible();

  await page.getByRole('button', { name: 'Add a reference' }).click();
  await page.getByLabel('Id', { exact: true }).fill('chm13');
  await page.getByLabel('Name', { exact: true }).fill('Human T2T CHM13');
  await page.getByLabel('Path 1').fill('s3://elsewhere/chm13.fa');
  await page.getByRole('button', { name: 'Remove file 2' }).click();
  await page.getByRole('button', { name: 'Save' }).click();
  // Outside every readable source: the server's reason, and the form stays open.
  await expect(page.getByRole('alert')).toContainText('not in the project bucket');

  await page.getByLabel('Path 1').fill('s3://shared-refs/CHM13/genome.fa');
  await page.getByRole('button', { name: '+ Add a file' }).click();
  await page.getByLabel('Key 2').fill('gtf');
  await page.getByLabel('Path 2').fill('s3://shared-refs/CHM13/missing.gtf');
  await page.getByRole('button', { name: 'Save' }).click();
  // Saved, with a warning for the file that is not there yet.
  const banner = page.getByRole('status');
  await expect(banner).toContainText('Saved chm13');
  await expect(banner).toContainText('gtf: nothing found');
  await expect(page.getByRole('cell', { name: /Human T2T CHM13/ })).toBeVisible();
  // Saved in this test's own store: the shared mock, which the tests alongside read, never saw it.
  const shared = await sharedReferences(page);
  expect(((await shared.json()) as Bundle[]).map((b) => b.id)).not.toContain('chm13');

  await page.getByRole('button', { name: 'Edit chm13' }).click();
  await expect(page.getByLabel('Id', { exact: true })).toBeDisabled();
  await page.getByLabel('Name', { exact: true }).fill('Human T2T CHM13 v2');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('cell', { name: /CHM13 v2/ })).toBeVisible();

  await page.getByRole('button', { name: 'Delete chm13' }).click();
  await page.getByRole('button', { name: 'Confirm delete chm13' }).click();
  await expect(page.getByRole('cell', { name: /CHM13/ })).toHaveCount(0);
});

for (const theme of THEMES) {
  test(`references (${theme}) have no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    await page.goto(RUN_TAB);
    await page
      .getByRole('combobox', { name: /^Reference genome/ })
      .selectOption('grch38-ensembl110');
    await expect(page.getByText('Primary assembly, no ALTs.')).toBeVisible();
    await expectNoSeriousA11yViolations(page);
    await page.goto('/settings/references');
    await page.getByRole('button', { name: 'Add a reference' }).click();
    await expect(page.getByLabel('Id', { exact: true })).toBeVisible();
    await expectNoSeriousA11yViolations(page);
  });
}

test('a refused delete says why and keeps the list', async ({ page }) => {
  await page.route(`${API}/references/hg38-gatk`, (route) =>
    route.request().method() === 'DELETE'
      ? route.fulfill({ status: 403, body: 'Requires admin access' })
      : route.fallback(),
  );
  await page.goto('/settings/references');
  await page.getByRole('button', { name: 'Delete hg38-gatk' }).click();
  await page.getByRole('button', { name: 'Confirm delete hg38-gatk' }).click();
  await expect(page.getByRole('alert')).toContainText("Couldn't delete hg38-gatk");
  await expect(page.locator('table')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit hg38-gatk' })).toBeVisible();
});

test('a slow refresh landing after a newer one never brings a deleted bundle back', async ({
  page,
}) => {
  // A store of this page's own: each list answers what it held when asked, the second one late.
  const seed = await sharedReferences(page);
  let stored: { id: string }[] = await seed.json();
  let lists = 0;
  let release = () => {};
  const held = new Promise<void>((r) => (release = r));
  let landed = () => {};
  const lateLanded = new Promise<void>((r) => (landed = r));
  await page.route(REFERENCES, async (route) => {
    const req = route.request();
    const id = new URL(req.url()).pathname.match(/^\/references\/(.+)$/)?.[1];
    if (req.method() === 'DELETE' && id) {
      stored = stored.filter((b) => b.id !== decodeURIComponent(id));
      return route.fulfill({ status: 204 });
    }
    if (req.method() !== 'GET' || id) return route.fallback();
    const snapshot = stored;
    const late = ++lists === 2;
    if (late) await held;
    await route.fulfill({ json: snapshot });
    if (late) landed();
  });
  await page.goto('/settings/references');
  await page.getByRole('button', { name: 'Delete hg38-gatk' }).click();
  await page.getByRole('button', { name: 'Confirm delete hg38-gatk' }).click();
  await expect.poll(() => lists).toBe(2);
  // The second delete's refresh lands first, then the first one's, which still lists hg38-igv.
  await page.getByRole('button', { name: 'Delete hg38-igv' }).click();
  await page.getByRole('button', { name: 'Confirm delete hg38-igv' }).click();
  await expect(page.getByRole('button', { name: 'Edit hg38-igv' })).toHaveCount(0);
  release();
  await lateLanded;
  await expect(page.getByRole('button', { name: 'Edit grcm39' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit hg38-igv' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Edit hg38-gatk' })).toHaveCount(0);
});

test('a reference of a kind this page does not know keeps it when edited', async ({ page }) => {
  let seeded: Bundle | undefined;
  await page.route(`${API}/references`, async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    const res = await route.fetch();
    const bundles = await res.json();
    bundles[0].kind = 'pangenome';
    seeded = bundles[0];
    return route.fulfill({ response: res, json: bundles });
  });
  // The save is answered here (the shared mock is never written): the page's job is to send the
  // kind it was given, not to coerce it to one it knows.
  const saves: Bundle[] = [];
  await page.route(/^http:\/\/localhost:8080\/references\/[^?]+$/, async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback();
    const body = route.request().postDataJSON() as Bundle;
    saves.push(body);
    await route.fulfill({ json: { bundle: body, warnings: [] } });
  });
  await page.goto('/settings/references');
  const first = page.locator('tbody tr').first();
  await expect(first).toContainText('pangenome');
  await first.getByRole('button', { name: /^Edit/ }).click();
  await expect(page.getByLabel('Kind')).toHaveValue('pangenome');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toMatchObject({ kind: 'pangenome', uris: seeded!.uris });
});

test('settings lists the engine releases a workflow may pin', async ({ page }) => {
  await page.goto('/settings');
  const table = page.locator('table.engines');
  await expect(table.getByRole('row')).toHaveText([
    /Engine\s*From\s*Up to/,
    /snakemake\s*9\.0\.0\s*10\.0\.0/,
    /nextflow\s*25\.10\.4\s*26\.0\.0/,
  ]);
});
