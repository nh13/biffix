import type { Locator, Page, Route } from '@playwright/test';
import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  onApi,
  runTabs,
  useTheme,
  CORS,
  THEMES,
} from '../fixtures';

// QC previews on the Lab analysis page: the pipeline's own FastQC as data,
// and alignments in the real igv.js, reading the mock's tiny BAM, BAI, FASTA and FAI (the backend's
// test fixtures, crates/biffix-backend/tests/fixtures/igv) with HTTP Range requests as it would
// read presigned S3 URLs.

const DONE = '/runs/run_01HGWDdone';
const NOREF = '/runs/run_01HGWHnoref';

const results = (page: Page) => page.getByRole('region', { name: /^Results/ });
const qc = (page: Page) => page.getByRole('region', { name: /QC at a glance/ });
const card = (page: Page, name: string) => results(page).getByRole('article', { name });

/** Open one sample's alignments from QC at a glance. */
async function viewAlignments(page: Page, sample: string): Promise<Locator> {
  const section = qc(page);
  await section.getByRole('combobox', { name: 'Alignments' }).selectOption({ label: sample });
  await section.getByRole('button', { name: /^View alignments/ }).click();
  return page.getByRole('dialog');
}

/** igv's locus box, once igv has started (and the viewer keeps its links renewed). */
async function igvStarted(dialog: Locator): Promise<Locator> {
  const locus = await igvLocus(dialog);
  await expect(dialog.getByText('Starting the genome browser…')).toHaveCount(0);
  return locus;
}

/** Counts the alignment-view requests that only renew the links (`locus=false`). */
function countRenewals(page: Page): { count: number } {
  const seen = { count: 0 };
  page.on('request', (r) => {
    if (/\/alignment-view\?(.*&)?locus=false/.test(r.url())) seen.count += 1;
  });
  return seen;
}

/** igv's locus box, once igv has drawn the alignment track. */
async function igvLocus(dialog: Locator): Promise<Locator> {
  const igv = dialog.getByTestId('igv');
  const locus = igv.locator('input.igv-search-input');
  await expect(locus).toBeVisible({ timeout: 20_000 });
  return locus;
}

test.describe('QC previews', () => {
  test.use({ view: 'lab' });

  test('says when the full FastQC report cannot be found, rather than leaving it out', async ({
    page,
  }) => {
    await onApi(page, 'GET', /^\/runs\/run_01HGWDdone\/outputs\/[^/]+$/, (route) =>
      route.fulfill({ status: 503, headers: CORS, body: 'Service Unavailable' }),
    );
    // A FastQC zip opened from All files: its HTML report is looked up by id.
    await page.goto(`${DONE}/files`);
    const zip = page.getByTestId('lab-file').filter({ hasText: 'Tumor_1_fastqc.zip' });
    await zip.getByRole('button', { name: /^Open/ }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toContainText('1 failed · 2 with a warning · 7 passed');
    await expect(dialog.getByRole('alert')).toContainText("Couldn't find the full FastQC report");
  });

  for (const theme of THEMES) {
    test(`a sample's FastQC opens as data, with the full report a click away (${theme})`, async ({
      page,
    }) => {
      await useTheme(page, theme);
      await page.goto(DONE);
      const perSample = card(page, 'Per-sample QC');
      await expect(perSample).toContainText('FastQC · Normal_1');
      await perSample.getByRole('combobox', { name: 'Sample' }).selectOption('Tumor_1');
      await perSample.getByRole('button', { name: /^Open/ }).click();

      const dialog = page.getByRole('dialog', { name: 'Per-sample QC · Tumor_1' });
      await expect(dialog).toContainText("From the pipeline's FastQC (v0.12.1)");
      await expect(dialog).toContainText('1 failed · 2 with a warning · 7 passed');
      const modules = dialog.getByRole('navigation', { name: 'FastQC modules' });
      // Each status is a word, not only a colour; the first module that did not pass is shown.
      await expect(
        modules.getByRole('button', { name: /^Per base sequence quality\s*: Fail$/ }),
      ).toHaveAttribute('aria-current', 'true');
      await expect(
        modules.getByRole('button', { name: /^Per sequence quality scores\s*: Warning$/ }),
      ).toBeVisible();
      await expect(
        dialog.getByRole('img', { name: /Per base sequence quality: quality/ }),
      ).toBeVisible();
      await expectNoSeriousA11yViolations(page);

      // The numbers behind the plot, and a module that is a table.
      await dialog.getByRole('button', { name: 'Show the numbers' }).click();
      await expect(dialog.getByRole('columnheader', { name: 'Lower Quartile' })).toBeVisible();
      await modules.getByRole('button', { name: /Basic Statistics/ }).click();
      await expect(dialog.getByRole('row', { name: /Total Sequences\s*2000/ })).toBeVisible();
      await modules.getByRole('button', { name: /Sequence Duplication Levels/ }).click();
      await expect(dialog).toContainText(/Total Deduplicated Percentage\s*91\.8/);
      await expect(dialog.getByRole('img', { name: /Sequence Duplication Levels/ })).toBeVisible();

      // FastQC's own HTML report, in the sandboxed viewer; closing it comes back to the modules.
      await dialog.getByRole('button', { name: 'Full FastQC report' }).click();
      const frame = page.frameLocator('iframe[title="Per-sample QC · Tumor_1"]');
      await expect(frame.getByRole('heading', { name: 'FastQC: Tumor_1' })).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(page.getByRole('navigation', { name: 'FastQC modules' })).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(page.getByRole('dialog')).toHaveCount(0);
      // Focus is back on the button that opened the viewer, after the swap to the full report and
      // back.
      await expect(perSample.getByRole('button', { name: /^Open/ })).toBeFocused();
    });
  }

  test("a FastQC zip that can't be read opens the HTML report beside it", async ({ page }) => {
    await onApi(page, 'GET', /^\/runs\/run_01HGWDdone\/outputs\/[^/]+\/fastqc$/, (route) =>
      route.fulfill({ status: 422, headers: CORS, body: 'S1_fastqc.zip is too large' }),
    );
    await page.goto(DONE);
    const perSample = card(page, 'Per-sample QC');
    await perSample.getByRole('combobox', { name: 'Sample' }).selectOption('Tumor_1');
    await perSample.getByRole('button', { name: /^Open/ }).click();
    const frame = page.frameLocator('iframe[title="Per-sample QC · Tumor_1"]');
    await expect(frame.getByRole('heading', { name: 'FastQC: Tumor_1' })).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
  });

  test("a paired-end sample's files are one sample in one card, each file named", async ({
    page,
  }) => {
    await page.goto(NOREF);
    const perSample = card(page, 'Per-sample QC');
    await expect(results(page).getByRole('article', { name: 'Per-sample QC' })).toHaveCount(1);
    await expect(perSample).toContainText('2 samples');
    const sample = perSample.getByRole('combobox', { name: 'Sample' });
    await expect(sample.locator('option')).toHaveText([
      'Liver_1 · Liver_1_1_fastqc.zip',
      'Liver_1 · Liver_1_2_fastqc.zip',
      'Liver_2 · Liver_2_1_fastqc.zip',
      'Liver_2 · Liver_2_2_fastqc.zip',
    ]);
    await sample.selectOption({ label: 'Liver_2 · Liver_2_2_fastqc.zip' });
    await perSample.getByRole('button', { name: /^Open/ }).click();
    const dialog = page.getByRole('dialog', { name: 'Per-sample QC · Liver_2' });
    await expect(dialog).toContainText("From the pipeline's FastQC (v0.12.1)");
    // QC at a glance names each file too.
    await page.keyboard.press('Escape');
    await expect(
      qc(page).getByRole('combobox', { name: 'FastQC per sample' }).locator('option'),
    ).toHaveCount(4);
  });

  test("without FastQC's data, its HTML report opens instead", async ({ page }) => {
    await page.goto(DONE);
    const section = qc(page);
    await section.getByRole('combobox', { name: 'FastQC per sample' }).selectOption('Normal_2');
    await section.getByRole('button', { name: /^Open FastQC/ }).click();
    const frame = page.frameLocator('iframe[title="Per-sample QC · Normal_2"]');
    await expect(frame.getByRole('heading', { name: 'FastQC: Normal_2' })).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'FastQC modules' })).toHaveCount(0);
  });

  test("alignments open in the genome browser at the reads, against the run's reference", async ({
    page,
  }) => {
    await page.goto(DONE);
    // The run's QC at a glance offers both previews beside the General Statistics.
    await expect(qc(page).getByRole('button', { name: /^Open FastQC/ })).toBeVisible();
    const dialog = await viewAlignments(page, 'Tumor_1');
    await expect(dialog.getByRole('heading', { name: 'Alignments · Tumor_1' })).toBeVisible();
    await expect(dialog).toContainText(
      'Tumor_1.markdup.sorted.bam · against Mouse GRCm39 · Ensembl 110',
    );
    // What was left out is said, not silently dropped.
    await expect(dialog.getByRole('list', { name: 'Notes' })).toContainText('genes.gtf');

    // Real igv: it opened at the first contig with reads, and drew the alignment track.
    const locus = await igvLocus(dialog);
    await expect(locus).toHaveValue(/^chrB:9,001-11,000$/);
    const igv = dialog.getByTestId('igv');
    await expect(
      igv.locator('.igv-track-label', { hasText: 'Tumor_1.markdup.sorted.bam' }),
    ).toHaveCount(1);
    await expect(igv.locator('canvas').first()).toBeVisible();

    // Move, then reload: the viewer reopens where the person was (this tab's session).
    await locus.fill('chrB:9,200-9,400');
    await locus.press('Enter');
    await expect(locus).toHaveValue('chrB:9,200-9,400');
    await page.reload();
    const again = await viewAlignments(page, 'Tumor_1');
    await expect(await igvLocus(again)).toHaveValue('chrB:9,200-9,400');
    // Another sample's file has its own place.
    await page.keyboard.press('Escape');
    const other = await viewAlignments(page, 'Tumor_2');
    await expect(await igvLocus(other)).toHaveValue('chrB:9,001-11,000');
  });

  test('an analysis without a reference asks which one to show the reads against', async ({
    page,
  }) => {
    await page.goto(NOREF);
    // No General Statistics here (checked once the QC has rendered), but the previews still make a
    // QC at a glance.
    await expect(qc(page).getByRole('button', { name: /^Open FastQC/ })).toBeVisible();
    await expect(qc(page)).not.toContainText('from the QC report');
    const dialog = await viewAlignments(page, 'Liver_2');
    await expect(dialog).toContainText('no reference genome');
    const choose = dialog.getByRole('combobox', { name: 'Reference' });
    // Assemblies with a FASTA only (not the assay kit).
    await expect(choose.locator('option')).toContainText([
      'Choose a reference',
      'Human GRCh38 · Ensembl 110',
      'Mouse GRCm39 · Ensembl 110',
    ]);
    await expect(choose.locator('option', { hasText: 'Twist' })).toHaveCount(0);
    await expectNoSeriousA11yViolations(page);
    await choose.selectOption('grcm39');
    await dialog.getByRole('button', { name: 'Show' }).click();
    await expect(dialog).toContainText('against Mouse GRCm39 · Ensembl 110');
    await expect(await igvLocus(dialog)).toHaveValue('chrB:9,001-11,000');
  });

  test('Escape in the locus box leaves the box, not the genome browser', async ({ page }) => {
    await page.goto(DONE);
    const dialog = await viewAlignments(page, 'Tumor_1');
    const locus = await igvLocus(dialog);
    await locus.focus();
    await page.keyboard.press('Escape');
    await expect(dialog).toBeVisible();
    await expect(locus).not.toBeFocused();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });

  test('new links before they expire keep the genome browser where the person is', async ({
    page,
  }) => {
    await page.clock.install();
    const renewals = countRenewals(page);
    await page.goto(DONE);
    const dialog = await viewAlignments(page, 'Tumor_1');
    const locus = await igvStarted(dialog);
    await locus.fill('chrB:9,200-9,400');
    await locus.press('Enter');
    await expect(locus).toHaveValue('chrB:9,200-9,400');
    // The links last an hour: two minutes before, the viewer asks for new ones (without the
    // locus) and reloads igv's tracks from them, where it was.
    await page.clock.fastForward('58:30');
    await expect.poll(() => renewals.count).toBe(1);
    await expect(locus).toHaveValue('chrB:9,200-9,400');
    const igv = dialog.getByTestId('igv');
    await expect(
      igv.locator('.igv-track-label', { hasText: 'Tumor_1.markdup.sorted.bam' }),
    ).toHaveCount(1);
    await expect(dialog.getByRole('status')).toHaveText('');
    // One renewal, not two: the tracks have reloaded, so a second would have been asked for.
    expect(renewals.count).toBe(1);
  });

  test("igv's refused reads get new links once, then say so without closing it", async ({
    page,
  }) => {
    await page.clock.install();
    const renewals = countRenewals(page);
    await page.goto(DONE);
    const dialog = await viewAlignments(page, 'Tumor_1');
    const locus = await igvStarted(dialog);
    // From now on the storage refuses the BAM (links that died before `expires_at`).
    const refuse = async (route: Route) =>
      route.fulfill({ status: 403, headers: CORS, body: 'AccessDenied' });
    await page.route('**/mock-files/igv/tiny.bam', refuse);
    // igv reads the BAM again when its tracks reload (the scheduled renewal): refused, so the viewer
    // asks for new links once more, and when those are refused too it says so, once.
    await page.clock.fastForward('58:30');
    await expect(dialog.getByRole('status')).toContainText('refused the links to this file again');
    expect(renewals.count).toBe(2);
    await expect(dialog.getByTestId('igv')).toBeVisible();
    await expect(locus).toBeVisible();
    // igv says so too, in its own alert over the page: dismissed as a person would.
    const igvAlert = dialog
      .getByTestId('igv')
      .locator('.igv-ui-alert-dialog-container:visible')
      .first();
    await expect(igvAlert).toBeVisible();
    while (await igvAlert.isVisible()) await igvAlert.getByText('OK', { exact: true }).click();
    // The storage serves the file again: Try again renews, and the message goes.
    await page.unroute('**/mock-files/igv/tiny.bam', refuse);
    await dialog.getByRole('button', { name: 'Try again' }).click();
    await expect.poll(() => renewals.count).toBe(3);
    await expect(dialog.getByRole('status')).toHaveText('');
    await expect(locus).toBeVisible();
    // Once: a second renewal would have been asked for by now.
    await page.evaluate(() => 0);
    expect(renewals.count).toBe(3);
  });

  test('links refused before igv starts are renewed once, without telling anyone to close it', async ({
    page,
  }) => {
    // The first check of the BAM is refused; the links asked for again (held until the test lets
    // them go) work.
    let probes = 0;
    await page.route('**/mock-files/igv/tiny.bam', (route) =>
      probes++ === 0
        ? route.fulfill({ status: 403, headers: CORS, body: 'AccessDenied' })
        : route.fallback(),
    );
    let views = 0;
    let release = () => {};
    const held = new Promise<void>((r) => (release = r));
    await page.route(/\/alignment-view(\?|$)/, async (route) => {
      if (route.request().method() !== 'OPTIONS' && views++ === 1) await held;
      return route.fallback();
    });
    await page.goto(DONE);
    const dialog = await viewAlignments(page, 'Tumor_1');
    await expect.poll(() => views).toBe(2);
    await expect(dialog.getByText('Starting the genome browser…')).toBeVisible();
    await expect(dialog.getByText(/links to this file were refused/)).toHaveCount(0);
    release();
    await igvStarted(dialog);
    await expect(dialog.getByRole('alert')).toHaveCount(0);
  });

  test('a failed renewal keeps the genome browser open and says it will try again', async ({
    page,
  }) => {
    await page.clock.install();
    await page.goto(DONE);
    const dialog = await viewAlignments(page, 'Tumor_1');
    const locus = await igvStarted(dialog);
    await page.route(/\/alignment-view\?(.*&)?locus=false/, (route) =>
      route.fulfill({ status: 503, headers: CORS, body: 'Service Unavailable' }),
    );
    await page.clock.fastForward('58:30');
    await expect(dialog.getByRole('status')).toContainText(
      "Couldn't renew the links to this file: Service Unavailable. Trying again shortly",
    );
    await expect(dialog.getByRole('alert')).toHaveCount(0);
    await expect(locus).toHaveValue('chrB:9,001-11,000');
  });

  test("storage without the viewer's CORS rule says so instead of failing silently", async ({
    page,
  }) => {
    await page.goto(DONE);
    const dialog = await viewAlignments(page, 'Normal_2');
    await expect(dialog.getByRole('alert')).toHaveText(
      "This storage isn't set up for the viewer yet; ask your admin to apply the CORS rule the deploy prints.",
    );
    await expect(dialog.getByTestId('igv')).toHaveCount(0);
  });
});

test.describe('FastQC from MultiQC', () => {
  test.use({ view: 'lab' });

  const MQC_ONLY = '/runs/run_01HGWJmqconly';

  for (const theme of THEMES) {
    test(`a run with only MultiQC's output shows each FastQC result's statuses (${theme})`, async ({
      page,
    }) => {
      await useTheme(page, theme);
      await page.goto(MQC_ONLY);
      const section = qc(page);
      const view = section.getByTestId('fastqc-from-multiqc');
      await expect(view).toContainText('FastQC per sample (from MultiQC)');
      // Said plainly: statuses only, the plots are in the MultiQC report.
      await expect(view).toContainText('Status only');
      await expect(view).toContainText('no plots here');
      // No FastQC of its own to open.
      await expect(section.getByRole('button', { name: /^Open FastQC/ })).toHaveCount(0);

      const pick = view.getByRole('combobox', { name: 'FastQC per sample (from MultiQC)' });
      await expect(pick.locator('option')).toHaveText(['S1 · S1_R1', 'S1 · S1_R2', 'S10']);
      await expect(view).toContainText('S1_R1.fastq.gz');
      await pick.selectOption({ label: 'S10' });
      await expect(view).toContainText('2 failed · 8 passed');
      const modules = view.getByRole('list', { name: 'FastQC modules · S10' });
      await expect(modules.getByRole('listitem')).toHaveCount(10);
      // Each status is a word, not only a colour.
      await expect(
        modules.getByRole('listitem').filter({ hasText: 'Per base sequence quality' }),
      ).toHaveText(/Per base sequence quality\s*: Fail/);
      await expect(modules.getByRole('listitem').filter({ hasText: 'Adapter Content' })).toHaveText(
        /Adapter Content\s*: Pass/,
      );
      const stats = view.getByRole('definition');
      await expect(view.getByRole('term')).toContainText(['Total sequences', 'GC']);
      await expect(stats).toContainText(['1,000', '30%']);
      await expectNoSeriousA11yViolations(page);

      // The full MultiQC report, in the existing viewer; closing it returns to the button.
      await view.getByRole('button', { name: 'Open the MultiQC report' }).click();
      const frame = page.frameLocator('iframe[title="QC report"]');
      await expect(
        frame.getByRole('heading', { name: 'MultiQC (mock, lab-rnaseq)' }),
      ).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(page.getByRole('dialog')).toHaveCount(0);
      await expect(view.getByRole('button', { name: 'Open the MultiQC report' })).toBeFocused();
    });
  }

  test("a run with FastQC's own results never asks MultiQC for them", async ({ page }) => {
    let asked = 0;
    await onApi(page, 'GET', /^\/runs\/[^/]+\/qc\/fastqc-status$/, (route) => {
      asked += 1;
      return route.fulfill({
        status: 200,
        headers: { ...CORS, 'content-type': 'application/json' },
        body: JSON.stringify({
          source: 'multiqc',
          report_output_id: 'x',
          samples: [{ sample: 'Normal_1', fastqc_name: 'Normal_1', modules: [], stats: {} }],
        }),
      });
    });
    // The General Statistics are asked on the same change that would ask for the statuses: once
    // they are answered, a request for the statuses would have been routed too.
    const generalStats = page.waitForResponse(/\/runs\/[^/]+\/qc\/general-stats$/);
    await page.goto(DONE);
    await generalStats;
    await expect(qc(page).getByRole('button', { name: /^Open FastQC/ })).toBeVisible();
    await expect(page.getByTestId('fastqc-from-multiqc')).toHaveCount(0);
    expect(asked).toBe(0);
  });

  test('a run whose MultiQC data has no FastQC table shows nothing for it', async ({ page }) => {
    await onApi(page, 'GET', /^\/runs\/run_01HGWJmqconly\/qc\/fastqc-status$/, (route) =>
      route.fulfill({ status: 404, headers: CORS, body: 'no FastQC results' }),
    );
    const answered = page.waitForResponse(/\/qc\/fastqc-status$/);
    await page.goto(MQC_ONLY);
    await answered;
    // The General Statistics still make a QC at a glance; no FastQC view, no error.
    await expect(qc(page)).toContainText('General statistics');
    await expect(page.getByTestId('fastqc-from-multiqc')).toHaveCount(0);
    await expect(page.getByTestId('fastqc-from-multiqc-error')).toHaveCount(0);
    await expect(page.getByRole('alert')).toHaveCount(0);
  });

  test('a FastQC table too large to show says so, and the rest of QC stays', async ({ page }) => {
    await onApi(page, 'GET', /^\/runs\/run_01HGWJmqconly\/qc\/fastqc-status$/, (route) =>
      route.fulfill({
        status: 422,
        headers: CORS,
        body: "MultiQC's FastQC table is too large to show",
      }),
    );
    await page.goto(MQC_ONLY);
    await expect(qc(page)).toContainText('General statistics');
    await expect(page.getByTestId('fastqc-from-multiqc-error')).toHaveText(
      "FastQC per sample (from MultiQC) could not be shown: MultiQC's FastQC table is too large to show",
    );
    await expect(page.getByTestId('fastqc-from-multiqc')).toHaveCount(0);
  });

  test('raw and trimmed reads are two sections, never confused', async ({ page }) => {
    await page.goto('/runs/run_01HGWKmqctrim');
    const view = qc(page).getByTestId('fastqc-from-multiqc');
    const section = view.getByRole('combobox', { name: 'FastQC section' });
    await expect(section.locator('option')).toHaveText(['fastqc_raw', 'fastqc_trimmed']);
    await expect(view).toContainText('section fastqc_raw');
    const pick = view.getByRole('combobox', { name: 'FastQC per sample (from MultiQC)' });
    await expect(pick.locator('option')).toHaveText(['S1 · S1_R1', 'S1 · S1_R2', 'S10']);
    await expect(
      view.getByRole('list', { name: 'FastQC modules · S1 · S1_R1 · fastqc_raw' }),
    ).toBeVisible();
    await expect(view).toContainText('1 with a warning · 9 passed');
    await expect(view.getByRole('definition')).toContainText(['100 bp']);
    // The same FASTQ's trimmed reads: the sample stays picked, the section says which.
    await section.selectOption({ label: 'fastqc_trimmed' });
    await expect(
      view.getByRole('list', { name: 'FastQC modules · S1 · S1_R1 · fastqc_trimmed' }),
    ).toBeVisible();
    await expect(view).toContainText('section fastqc_trimmed');
    await expect(view).toContainText('10 passed');
    await expect(view.getByRole('definition')).toContainText(['80 bp']);
    await expectNoSeriousA11yViolations(page);
  });
});

test.describe('the Expert view', () => {
  test('is unchanged: reports open in place, no QC previews', async ({ page }) => {
    await page.goto(DONE);
    // Once the run's reports are in (its Reports tab counts them), the overview offers no preview.
    const reportsTab = runTabs(page).getByRole('link', { name: /Reports/ });
    await expect(reportsTab.locator('.count')).toBeVisible();
    await expect(page.getByRole('button', { name: /View alignments|Open FastQC/ })).toHaveCount(0);
    await page.goto(`${DONE}/reports#report=out5`);
    const frame = page.frameLocator('iframe');
    await expect(frame.getByRole('heading', { name: 'FastQC: Tumor_1' })).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'FastQC modules' })).toHaveCount(0);
  });

  test('lists a per-sample report once, with a sample picker; a FastQC zip opens as data', async ({
    page,
  }) => {
    await page.goto(`${NOREF}/reports`);
    const picker = page.getByRole('group', { name: 'Reports' });
    await expect(picker.getByRole('button', { name: 'Per-sample QC' })).toHaveCount(1);
    await picker.getByRole('button', { name: 'Per-sample QC' }).click();
    const sample = page.getByRole('combobox', { name: 'Sample' });
    await expect(sample.locator('option')).toHaveCount(4);
    await sample.selectOption({ label: 'Liver_2 · Liver_2_1_fastqc.zip' });
    await expect(page).toHaveURL(/#report=/);
    await page.getByRole('button', { name: 'Open FastQC' }).click();
    const dialog = page.getByRole('dialog', { name: 'Per-sample QC · Liver_2' });
    await expect(dialog).toContainText("From the pipeline's FastQC (v0.12.1)");
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Open FastQC' })).toBeFocused();
  });
});
