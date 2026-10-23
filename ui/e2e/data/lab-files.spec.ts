import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  onApi,
  THEMES,
  useTheme,
  CORS,
  holdParts,
} from '../fixtures';

// Lab files & data: an analysis's *All files* as a Lab page (grouped, with per-file actions,
// a search and paging), the Data page in plain words (upload at the top of the project, "Result of"
// an analysis, the genome browser on an analysis's BAM, a trail that never links a hidden folder),
// and *Inputs used* showing every input.

const DONE = '/runs/run_01HGWDdone';
const FILES = `${DONE}/files`;
const RUN_FOLDER = '/data/project#path=biffix%2Fruns%2Frun_01HGWDdone%2Fstar_salmon%2F';

test.describe('All files (Lab view)', () => {
  test.use({ view: 'lab' });

  test('the analysis page links its Lab file list, which groups files by what they are', async ({
    page,
  }) => {
    await page.goto(DONE);
    const all = page
      .getByRole('region', { name: 'Results' })
      .getByRole('link', { name: 'All files →' });
    await expect(all).toHaveAttribute('href', FILES);
    await all.click();
    await expect(page).toHaveURL(FILES);
    await expect(page.getByRole('heading', { name: 'All files', level: 1 })).toBeVisible();
    // No Expert run header or tabs.
    await expect(page.getByRole('navigation', { name: 'Run views' })).toHaveCount(0);
    await expect(page.getByRole('note')).toHaveCount(0);
    const results = page.getByRole('region', { name: /^Key results/ });
    const other = page.getByRole('region', { name: /^Other results/ });
    await expect(results.getByTestId('lab-file').first()).toContainText('QC report');
    await expect(results).toContainText('Alignments · Tumor_2');
    await expect(other).toContainText('Normal_1.markdup.sorted.bam.bai');
    // Key results come first.
    const headings = await page.getByRole('heading', { level: 2 }).allTextContents();
    expect(headings[0]).toMatch(/^Key results/);
    await expect(page.getByRole('link', { name: /rnaseq · batch-10/ })).toHaveAttribute(
      'href',
      DONE,
    );
  });

  test('each file opens, shows in Data, downloads and copies', async ({ page }) => {
    await page.goto(FILES);
    const counts = page.getByTestId('lab-file').filter({ hasText: 'Gene counts' });
    await expect(
      counts.getByRole('link', { name: 'Show in Data browser: Gene counts' }),
    ).toHaveAttribute(
      'href',
      '/data/project?project=genomics#path=biffix%2Fruns%2Frun_01HGWDdone%2Fstar_salmon%2F&file=salmon.merged.gene_counts.tsv',
    );
    await counts.getByRole('button', { name: 'Open Gene counts' }).click();
    await expect(page.getByRole('dialog')).toContainText('Gene counts');
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog')).toHaveCount(0);
    // A BAM opens in the genome browser.
    const bam = page.getByTestId('lab-file').filter({ hasText: 'Alignments · Normal_1' });
    await bam.getByRole('button', { name: /^View alignments/ }).click();
    await expect(page.getByRole('dialog', { name: /Alignments · Normal_1/ })).toBeVisible();
    await page.keyboard.press('Escape');
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
    await counts.getByRole('button', { name: 'Copy path of Gene counts' }).click();
    await expect(counts.getByRole('button', { name: /^Copied/ })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
      's3://genomics-project/biffix/runs/run_01HGWDdone/star_salmon/salmon.merged.gene_counts.tsv',
    );
    // A download asks for its link (last: it navigates to it).
    const asked = page.waitForRequest((r) =>
      /\/outputs\/[^/]+\/download$/.test(new URL(r.url()).pathname),
    );
    await counts.getByRole('button', { name: 'Download Gene counts' }).click();
    await asked;
  });

  test('the total size says "at least" until every file is sized, and sizes keep coming', async ({
    page,
  }) => {
    // The first listing of the ended analysis has not sized a BAM index yet; the next, held until the
    // test lets it go, has.
    let reads = 0;
    let release = () => {};
    const held = new Promise<void>((r) => (release = r));
    await onApi(page, 'POST', '/runs/run_01HGWDdone/outputs/query', async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      if (reads++ > 0) {
        await held;
        return route.fulfill({ response, json: body });
      }
      const outputs = body.outputs.map((o: { s3_uri: string; size?: number }) =>
        o.s3_uri.endsWith('Tumor_1.markdup.sorted.bam.bai') ? { ...o, size: undefined } : o,
      );
      return route.fulfill({ response, json: { ...body, outputs, enrichment_pending: 1 } });
    });
    await page.goto(FILES);
    const head = page
      .locator('header')
      .filter({ has: page.getByRole('heading', { name: 'All files', level: 1 }) });
    await expect(head).toContainText(/18 files · at least 11 GiB$/);
    release();
    await expect(head).toContainText(/18 files · 11 GiB$/);
  });

  test('a search that finds nothing in a cut-short list says what it searched', async ({
    page,
  }) => {
    // More files than were read: the search covers only those listed.
    await onApi(page, 'POST', '/runs/run_01HGWDdone/outputs/query', async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      await route.fulfill({ response, json: { ...body, total: body.total + 5 } });
    });
    await page.goto(FILES);
    await page.getByLabel('Find a file').fill('no-such-file');
    await expect(
      page.getByText(/No file matches “no-such-file” in the first 18 files\./),
    ).toBeVisible();
  });

  test('a search narrows the list; a long group shows a page at a time', async ({ page }) => {
    const many = Array.from({ length: 130 }, (_, i) => {
      const name = `chunk_${String(i).padStart(3, '0')}.tsv`;
      return {
        id: `o${i}`,
        name,
        path: `chunks/${name}`,
        role: 'output',
        s3_uri: `s3://genomics-project/biffix/runs/run_01HGWDdone/chunks/${name}`,
        servable: true,
        size: 100,
      };
    });
    await onApi(page, 'POST', `${DONE}/outputs/query`, (route) =>
      route.fulfill({ json: { outputs: many, total: many.length }, headers: CORS }),
    );
    await page.goto(FILES);
    const group = page.getByRole('region', { name: /^Other results/ });
    await expect(group.getByTestId('lab-file')).toHaveCount(50);
    await expect(group).toContainText('Showing 50 of 130.');
    await group.getByRole('button', { name: /^Show 50 more/ }).click();
    await expect(group.getByTestId('lab-file')).toHaveCount(100);
    await group.getByRole('button', { name: /^Show 30 more/ }).click();
    await expect(group.getByTestId('lab-file')).toHaveCount(130);
    await expect(group.getByRole('button', { name: /more/ })).toHaveCount(0);

    await page.getByLabel('Find a file').fill('chunk_12');
    await expect(group.getByTestId('lab-file')).toHaveCount(10);
    await expect(page.getByRole('region', { name: /^Key results/ })).toHaveCount(0);
    await page.getByLabel('Find a file').fill('Tumor_2');
    await expect(page.getByRole('region', { name: /^Key results/ })).toContainText(
      'Alignments · Tumor_2',
    );
  });

  test('at phone width each file keeps its actions on screen', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(FILES);
    const row = page.getByTestId('lab-file').first();
    await expect(row).toBeVisible();
    const width = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(width).toBeLessThanOrEqual(390);
    const box = await row.getByRole('button', { name: /^Download/ }).boundingBox();
    expect(box && box.x + box.width).toBeLessThanOrEqual(390);
  });

  test('the Expert view toggle goes to the Outputs tab, and back', async ({ page }) => {
    await page.goto(FILES);
    await expect(page.getByRole('heading', { name: 'All files', level: 1 })).toBeVisible();
    await page.getByRole('group', { name: 'View' }).getByRole('button', { name: 'Expert' }).click();
    await expect(page).toHaveURL(`${DONE}/outputs`);
    await page.getByRole('group', { name: 'View' }).getByRole('button', { name: 'Lab' }).click();
    await expect(page).toHaveURL(FILES);
  });
});

test.describe('Data (Lab view)', () => {
  test.use({ view: 'lab' });

  test('speaks plainly and uploads at the top of the project, into uploads/', async ({
    page,
  }, info) => {
    await page.goto('/data');
    await expect(
      page.getByText('Your sequencing data, reference genomes and analysis results'),
    ).toBeVisible();
    await expect(page.getByText('admins registered')).toHaveCount(0);
    await page.getByRole('button', { name: 'Upload files' }).click();
    const panel = page.getByRole('group', { name: 'Upload files' });
    await expect(panel.getByLabel('Upload folder')).toHaveValue(/^uploads\/\d{4}-\d{2}-\d{2}\/$/);
    const folder = `uploads/lab-${info.retry}-${Date.now()}/`;
    await panel.getByLabel('Upload folder').fill(folder);
    await panel.getByLabel('Files to upload').setInputFiles({
      name: 'notes.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('hello'),
    });
    const status = page.getByTestId('upload-status');
    await expect(status).toContainText('Uploaded to', { timeout: 20_000 });
    await status.getByRole('link', { name: folder }).click();
    await expect(page.getByRole('cell', { name: 'notes.txt', exact: true })).toBeVisible();
    // Not offered where uploads cannot go (another source).
    await page
      .getByRole('navigation', { name: 'Data sources' })
      .getByRole('link', { name: /Sequencing core/ })
      .click();
    await expect(page.getByRole('button', { name: 'Upload files' })).toHaveCount(0);
  });

  test('a batch that stops part-way says which files started, and goes on', async ({
    page,
  }, info) => {
    // The second file's upload cannot start.
    let starts = 0;
    await onApi(page, 'POST', '/data/uploads', async (route) => {
      if (++starts === 2)
        return route.fulfill({ status: 500, headers: CORS, body: 'the bucket is unavailable' });
      await route.fallback();
    });
    await page.goto('/data');
    await page.getByRole('button', { name: 'Upload files' }).click();
    const panel = page.getByRole('group', { name: 'Upload files' });
    const folder = `uploads/part-${info.retry}-${Date.now()}/`;
    await panel.getByLabel('Upload folder').fill(folder);
    await panel.getByLabel('Files to upload').setInputFiles(
      ['a.txt', 'b.txt'].map((name) => ({
        name,
        mimeType: 'text/plain',
        buffer: Buffer.from(name),
      })),
    );
    const stopped = panel.getByRole('group', { name: 'Upload stopped' });
    await expect(stopped.getByRole('alert')).toContainText(
      'b.txt could not start: the bucket is unavailable. 1 of 2 files started',
    );
    await stopped.getByRole('button', { name: 'Go on without the rest' }).click();
    await expect(page.getByTestId('upload-status')).toContainText('Uploaded to', {
      timeout: 20_000,
    });
  });

  test('a batch with a paused file says so, and is listed once it finishes', async ({
    page,
  }, info) => {
    // Every part is held until the test lets them go.
    const release = await holdParts(page);
    await page.goto('/data');
    await page.getByRole('button', { name: 'Upload files' }).click();
    const panel = page.getByRole('group', { name: 'Upload files' });
    const folder = `uploads/held-${info.retry}-${Date.now()}/`;
    await panel.getByLabel('Upload folder').fill(folder);
    await panel.getByLabel('Files to upload').setInputFiles(
      ['c.txt', 'd.txt'].map((name) => ({
        name,
        mimeType: 'text/plain',
        buffer: Buffer.from(name),
      })),
    );
    const status = page.getByTestId('upload-status');
    await expect(status).toContainText('Uploading 2 files');
    const tray = page.getByRole('region', { name: 'Uploads' });
    await tray.getByRole('button', { name: 'Pause d.txt' }).click();
    release();
    await expect(status).toContainText(
      `Some files for ${folder} are paused or stopped: resume or retry them in Uploads.`,
    );
    await tray.getByRole('button', { name: 'Resume d.txt' }).click();
    await expect(status).toContainText('Uploaded to', { timeout: 20_000 });
  });

  test('withholds Upload inside the project outside uploads/', async ({ page }) => {
    // An analysis's results folder, and the Analysis results folder itself.
    await page.goto(RUN_FOLDER);
    await expect(page.getByRole('navigation', { name: 'Folder path' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Upload files' })).toHaveCount(0);
    await page.goto('/data/project#path=biffix%2Fruns%2F');
    await expect(page.getByRole('link', { name: /rnaseq · batch-10/ })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Upload files' })).toHaveCount(0);
  });

  test("opens an analysis's FastQC report in the FastQC viewer", async ({ page }) => {
    await page.goto('/data/project#path=biffix%2Fruns%2Frun_01HGWDdone%2Ffastqc%2F');
    const row = page.getByRole('row', { name: /Normal_1_fastqc\.html/ });
    await row.getByRole('button', { name: /^Open FastQC/ }).click();
    await expect(page.getByRole('dialog', { name: /Normal_1_fastqc\.html/ })).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
  });

  test("names an analysis's results folder and links the analysis", async ({ page }) => {
    await page.goto('/data');
    await page.getByRole('link', { name: 'Analysis results' }).click();
    await expect(page).toHaveURL(/#path=biffix%2Fruns%2F$/);
    // Run folders by their analysis's name.
    await page.getByRole('link', { name: /rnaseq · batch-10/ }).click();
    const provenance = page.getByTestId('data-provenance');
    await expect(provenance).toContainText('Result of rnaseq · batch-10');
    await expect(provenance.getByRole('link', { name: 'rnaseq · batch-10' })).toHaveAttribute(
      'href',
      DONE,
    );
  });

  test('a trail never links a hidden folder', async ({ page }) => {
    await page.goto(RUN_FOLDER);
    const trail = page.getByRole('navigation', { name: 'Folder path' });
    await expect(trail).toHaveText(
      /Project data\s*\/\s*Analysis results\s*\/\s*rnaseq · batch-10\s*\/\s*star_salmon/,
    );
    await expect(trail).not.toContainText('biffix');
    await expect(trail.getByRole('link')).toHaveText([
      'Project data',
      'Analysis results',
      'rnaseq · batch-10',
    ]);
    await trail.getByRole('link', { name: 'Analysis results' }).click();
    await expect(page.getByRole('alert')).toHaveCount(0);
  });

  test("opens an analysis's BAM in the genome browser", async ({ page }) => {
    await page.goto(RUN_FOLDER);
    const row = page
      .getByRole('row', { name: /Normal_1\.markdup\.sorted\.bam\b(?!\.bai)/ })
      .first();
    await row.getByRole('button', { name: /^View alignments/ }).click();
    const dialog = page.getByRole('dialog', { name: /Normal_1\.markdup\.sorted\.bam/ });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByTestId('igv')).toBeVisible({ timeout: 20_000 });
    // An index is not an alignment.
    await page.keyboard.press('Escape');
    const bai = page.getByRole('row', { name: /Normal_1\.markdup\.sorted\.bam\.bai/ });
    await expect(bai.getByRole('button', { name: /View alignments/ })).toHaveCount(0);
  });
});

test.describe('Inputs used shows everything (Lab view)', () => {
  test.use({ view: 'lab' });

  test('every sample row, every setting and the input files, behind disclosures', async ({
    page,
  }) => {
    await page.goto(DONE);
    const inputs = page.getByRole('region', { name: 'Inputs used' });
    // The summary still names a few samples...
    await expect(inputs).toContainText('and 1 more');
    // ...and the whole samplesheet is one click away, its files linked into Data.
    await inputs.getByText('Show all 4 rows of the samplesheet').click();
    const sheet = inputs.getByRole('region', { name: 'Samplesheet input' });
    await expect(sheet.getByRole('row')).toHaveCount(5);
    await expect(sheet).toContainText('Tumor_2');
    await expect(
      sheet.getByRole('link', {
        name: 'Open s3://seq-core-deliveries/lab-x/batch-11/Tumor_2_R1.fastq.gz in Data',
      }),
    ).toHaveAttribute('href', /\/data\/core/);
    // Every setting, by section: changed ones marked, defaults shown, reference files linked.
    await inputs.getByText(/^Show all \d+ settings$/).click();
    const all = inputs.getByTestId('all-settings');
    await expect(all.getByRole('heading', { name: 'Options' })).toBeVisible();
    await expect(all).toContainText('Enable UMI');
    await expect(all).toContainText('Default (No)');
    await expect(all).toContainText('Trimmer');
    await expect(all.locator('dt', { hasText: 'Aligner' })).toContainText('changed');
    await expect(all.locator('dt', { hasText: 'Genome FASTA' })).toContainText(
      'from the reference',
    );
    await expect(
      all.getByRole('link', { name: 'Open s3://shared-refs/GRCm39/genome.fa in Data' }),
    ).toBeVisible();
  });

  test("says when the pipeline's settings cannot be loaded, rather than loading forever", async ({
    page,
  }) => {
    await onApi(page, 'GET', /^\/workflows\/[^/]+$/, (route) =>
      route.fulfill({ status: 500, headers: CORS, json: { error: 'catalog unavailable' } }),
    );
    await page.goto(DONE);
    const inputs = page.getByRole('region', { name: 'Inputs used' });
    await expect(inputs).toContainText("Couldn't load the pipeline's settings");
    await expect(inputs).not.toContainText('Loading…');
  });

  test('a param the form does not describe is still listed under all settings', async ({
    page,
  }) => {
    await page.goto('/runs/run_01HGWGengine');
    const inputs = page.getByRole('region', { name: 'Inputs used' });
    await expect(inputs).toBeVisible();
    // A param the form does not describe is still shown.
    await inputs.getByText(/^Show all \d+ settings?$/).click();
    await expect(inputs.getByTestId('all-settings')).toContainText('min_qual');
  });
});

for (const theme of THEMES) {
  test.describe(`Lab files and data (${theme})`, () => {
    test.use({ view: 'lab' });
    for (const [path, ready] of [
      [FILES, 'Key results'],
      [RUN_FOLDER, 'Result of'],
      ['/data', 'Analysis results'],
    ] as const) {
      test(`${path} has no serious accessibility violations`, async ({ page }) => {
        await useTheme(page, theme);
        await page.goto(path);
        await expect(page.getByText(ready).first()).toBeVisible();
        await expectNoSeriousA11yViolations(page);
      });
    }
    test(`Inputs used, opened, has no serious accessibility violations`, async ({ page }) => {
      await useTheme(page, theme);
      await page.goto(DONE);
      const inputs = page.getByRole('region', { name: 'Inputs used' });
      await inputs.getByText('Show all 4 rows of the samplesheet').click();
      await inputs.getByText(/^Show all \d+ settings$/).click();
      await expect(inputs.getByTestId('all-settings')).toContainText('Trimmer');
      await expectNoSeriousA11yViolations(page);
    });
  });
}
