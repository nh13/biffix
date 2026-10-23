import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  CORS,
  nextFrame,
  onApi,
  THEMES,
  useTheme,
} from '../fixtures';
import type { Page } from '@playwright/test';

// One Data browser, two views: the Lab and Expert views browse the same folder the same way
// (the same listing requests, files, viewer and URL), differ only in words and Lab additions, and
// a switch of view keeps the folder, its listing and an upload still going.

const GRCH38 = '/data/refs#path=GRCh38%2F';
/** An analysis's results folder, in the project bucket. */
const RUN_FOLDER = '/data/project#path=biffix%2Fruns%2Frun_01HGWDdone%2Fstar_salmon%2F';

/** The bodies of the folder listings the page asks for. */
function listings(page: Page): unknown[] {
  const asked: unknown[] = [];
  page.on('request', (r) => {
    if (/\/data\/list$/.test(r.url()) && r.method() === 'POST') asked.push(r.postDataJSON());
  });
  return asked;
}

/** The names in the folder's first column (folders and files), in order. */
const listed = (page: Page) =>
  page.getByRole('region', { name: 'Folder contents' }).locator('tbody td:first-child');

const switchTo = (page: Page, view: 'Lab' | 'Expert') =>
  page.getByRole('group', { name: 'View' }).first().getByRole('button', { name: view }).click();

test('the same folder lists and previews alike in both views, and a switch keeps it', async ({
  page,
}) => {
  const asked = listings(page);
  await page.goto(GRCH38);
  await expect(page.getByRole('cell', { name: 'genome.fa', exact: true })).toBeVisible();
  const expertNames = await listed(page).allTextContents();
  expect(expertNames.length).toBeGreaterThan(0);
  expect(asked).toEqual([expect.objectContaining({ source: 'refs', path: 'GRCh38/' })]);

  // The Lab view: the same folder (URL and trail), the same files, and no second listing.
  await switchTo(page, 'Lab');
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F$/);
  await expect(page.getByRole('button', { name: 'Search inside folders' })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Folder path' })).toContainText('GRCh38');
  expect(await listed(page).allTextContents()).toEqual(expertNames);
  expect(asked).toHaveLength(1);

  // The viewer opens a file the same way in both: the same URL and the same dialog.
  await page.getByRole('button', { name: 'stats.tsv', exact: true }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F&file=stats\.tsv$/);
  await expect(
    page.getByRole('dialog').getByRole('cell', { name: 'chr1', exact: true }),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Close' }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F$/);
  await switchTo(page, 'Expert');
  await expect(page.getByRole('button', { name: 'Search subfolders' })).toBeVisible();
  await page.getByRole('button', { name: 'stats.tsv', exact: true }).click();
  await expect(page).toHaveURL(/\/data\/refs#path=GRCh38%2F&file=stats\.tsv$/);
  await expect(
    page.getByRole('dialog').getByRole('cell', { name: 'chr1', exact: true }),
  ).toBeVisible();
  expect(asked).toHaveLength(1);
});

test.describe('uploads', () => {
  test.use({ view: 'lab' });

  test('a file uploaded in the Lab view lists alike in both views', async ({ page }, info) => {
    const folder = `uploads/parity-${info.retry}-${Date.now()}/`;
    await page.goto('/data');
    await page.getByRole('button', { name: 'Upload files' }).click();
    const panel = page.getByRole('group', { name: 'Upload files' });
    await panel.getByLabel('Upload folder').fill(folder);
    await panel.getByLabel('Files to upload').setInputFiles({
      name: 'parity.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('same in both'),
    });
    const status = page.getByTestId('upload-status');
    await expect(status).toContainText('Uploaded to', { timeout: 20_000 });
    await status.getByRole('link', { name: folder }).click();
    const file = page.getByRole('cell', { name: 'parity.txt', exact: true });
    await expect(file).toBeVisible();
    const labNames = await listed(page).allTextContents();

    // The Expert view lists the same folder alike, and takes no uploads (a Lab addition).
    await switchTo(page, 'Expert');
    await expect(page.getByRole('button', { name: 'Search subfolders' })).toBeVisible();
    await expect(file).toBeVisible();
    expect(await listed(page).allTextContents()).toEqual(labNames);
    await expect(page.getByRole('button', { name: 'Upload files' })).toHaveCount(0);
    await expect(page.getByTestId('upload-status')).toHaveCount(0);
    // Back in the Lab view, the upload form is offered again, and the batch's status with it.
    await switchTo(page, 'Lab');
    await expect(page.getByRole('button', { name: 'Upload files' })).toBeVisible();
    await expect(page.getByTestId('upload-status')).toContainText('Uploaded to');
  });

  test('an upload still going when the view switches lists its folder once done', async ({
    page,
  }, info) => {
    // Hold the upload's last step until the view has switched.
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    await onApi(page, 'POST', /^\/data\/uploads\/[^/]+\/complete$/, async (route) => {
      await gate;
      await route.fallback();
    });
    const asked = listings(page);
    const folder = `uploads/switch-${info.retry}-${Date.now()}/`;
    await page.goto(`/data/project#path=${encodeURIComponent(folder)}`);
    await expect(page.getByRole('button', { name: 'Upload files' })).toBeVisible();
    await page.getByRole('button', { name: 'Upload files' }).click();
    const panel = page.getByRole('group', { name: 'Upload files' });
    await panel.getByLabel('Upload folder').fill(folder);
    await panel.getByLabel('Files to upload').setInputFiles({
      name: 'late.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('arrives in the Expert view'),
    });
    await expect(page.getByTestId('upload-status')).toContainText('Uploading 1 file');
    await switchTo(page, 'Expert');
    await expect(page.getByTestId('upload-status')).toHaveCount(0);
    const before = asked.length;
    release();
    // The folder is listed again once the upload is done, in the Expert view.
    await expect(page.getByRole('cell', { name: 'late.txt', exact: true })).toBeVisible({
      timeout: 20_000,
    });
    expect(asked.length).toBe(before + 1);
  });
});

test.describe('the words', () => {
  const DATA_SOURCES = '/data-sources';
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
  const words = {
    lab: {
      lede: 'Your sequencing data, reference genomes and analysis results. Open a file to look at it, or download it.',
      noSources:
        "Nothing is connected yet. Ask your admin to connect your sequencing core's folder.",
      noDataFolders:
        "Only your project's own files are here. Ask your admin to connect your sequencing core's folder.",
      unknownSource:
        "This folder doesn't exist, or it is in a project you can't open. Pick one of the places listed here.",
      details: 0,
    },
    expert: {
      lede: "The project's bucket and the data sources your admins registered. Open a table, text file, image, PDF or HTML report in place, or download it.",
      noSources:
        'No data sources are connected, not even the project bucket — an admin sets them in the deploy config.',
      noDataFolders: 'No data folders are connected — an admin adds them in the deploy config.',
      unknownSource:
        "This folder doesn't exist, or it is in a project you don't have access to. Pick one of the folders listed here.",
      details: 1,
    },
  } as const;
  for (const view of ['lab', 'expert'] as const) {
    test.describe(view, () => {
      test.use({ view });
      const w = words[view];

      test('the lede and an unknown source', async ({ page }) => {
        await page.goto('/data/nope');
        await expect(page.getByText(w.lede, { exact: true })).toBeVisible();
        const missing = page.getByTestId('unknown-source');
        await expect(missing.locator('p')).toHaveText(w.unknownSource);
        await expect(missing.getByText('Details')).toHaveCount(w.details);
      });

      test('no sources at all', async ({ page }) => {
        await onApi(page, 'GET', DATA_SOURCES, (route) =>
          route.fulfill({ json: [], headers: CORS }),
        );
        await page.goto('/data');
        await expect(page.getByText(w.noSources, { exact: true })).toBeVisible();
      });

      test('no data folders, only the project storage', async ({ page }) => {
        await onApi(page, 'GET', DATA_SOURCES, (route) =>
          route.fulfill({ json: PROJECT_ONLY, headers: CORS }),
        );
        await page.goto('/data');
        await expect(page.getByText(w.noDataFolders, { exact: true })).toBeVisible();
      });
    });
  }
});

test("an analysis's results folder: the Lab additions, and none of them in the Expert view", async ({
  page,
}) => {
  await page.goto(RUN_FOLDER);
  const trail = page.getByRole('navigation', { name: 'Folder path' });
  await expect(
    page.getByRole('cell', { name: /Normal_1\.markdup\.sorted\.bam/ }).first(),
  ).toBeVisible();
  const expertNames = await listed(page).allTextContents();
  // The Expert view: the bucket's own trail (biffix named, not linked), a folder Copy path, no
  // analysis named and nothing opened as a result.
  await expect(trail).toHaveText(/Project data\s*\/\s*biffix\s*\/\s*runs\s*\/\s*run_01HGWDdone/);
  await expect(trail.getByRole('link', { name: 'biffix' })).toHaveCount(0);
  await expect(trail.getByRole('button', { name: 'Copy path' })).toBeVisible();
  await expect(page.getByTestId('data-provenance')).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^View alignments/ })).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^Open FastQC/ })).toHaveCount(0);
  await expect(page.getByText('Result of', { exact: false })).toHaveCount(0);

  // The Lab view: the analysis named, its BAM opened as an alignment, the same files.
  await switchTo(page, 'Lab');
  await expect(page.getByTestId('data-provenance')).toContainText('Result of rnaseq · batch-10');
  await expect(trail).toHaveText(/Project data\s*\/\s*Analysis results\s*\/\s*rnaseq · batch-10/);
  await expect(trail.getByRole('button', { name: 'Copy path' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^View alignments/ }).first()).toBeVisible();
  expect(await listed(page).allTextContents()).toEqual(expertNames);
});

test.describe('a late answer', () => {
  test.use({ view: 'lab' });

  /** Hold every lookup of an analysis's outputs until `release`. */
  async function holdOutputs(page: Page) {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    await onApi(page, 'POST', /^\/runs\/[^/]+\/outputs\/query$/, async (route) => {
      await gate;
      await route.fallback();
    });
    return () => {
      const answered = page.waitForResponse((r) => /\/runs\/[^/]+\/outputs\/query/.test(r.url()));
      release();
      return answered;
    };
  }
  const viewAlignments = (page: Page) =>
    page
      .getByRole('row', { name: /Normal_1\.markdup\.sorted\.bam\b(?!\.bai)/ })
      .first()
      .getByRole('button', { name: /^View alignments/ });

  test('for a folder left behind is dropped, and its button is free again', async ({ page }) => {
    const release = await holdOutputs(page);
    await page.goto(RUN_FOLDER);
    await viewAlignments(page).click();
    await expect(viewAlignments(page)).toBeDisabled();
    await page
      .getByRole('navigation', { name: 'Folder path' })
      .getByRole('link', { name: 'Analysis results' })
      .click();
    await expect(page).toHaveURL(/#path=biffix%2Fruns%2F$/);
    await release();
    // The answer has landed, and a frame later the page has handled it: it must open nothing and
    // explain nothing here.
    await nextFrame(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page.getByRole('alert')).toHaveCount(0);
    await page.goBack();
    await expect(viewAlignments(page)).toBeEnabled();
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });

  test('for the view left behind is dropped', async ({ page }) => {
    const release = await holdOutputs(page);
    await page.goto(RUN_FOLDER);
    await viewAlignments(page).click();
    await expect(viewAlignments(page)).toBeDisabled();
    await switchTo(page, 'Expert');
    await expect(page.getByRole('button', { name: 'Search subfolders' })).toBeVisible();
    await release();
    await nextFrame(page);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page.getByRole('alert')).toHaveCount(0);
    await expect(page).not.toHaveURL(/file=/);
    await switchTo(page, 'Lab');
    await expect(viewAlignments(page)).toBeEnabled();
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });
});

for (const view of ['lab', 'expert'] as const) {
  for (const theme of THEMES) {
    test.describe(`the Data browser (${view}, ${theme})`, () => {
      test.use({ view });
      for (const path of [GRCH38, '/data/project#path=uploads%2F']) {
        test(`${path} has no serious accessibility violations`, async ({ page }) => {
          await useTheme(page, theme);
          await page.goto(path);
          await expect(listed(page).first()).toBeVisible();
          await expectNoSeriousA11yViolations(page);
        });
      }
    });
  }
}
