import {
  test,
  expect,
  expectNoSeriousA11yViolations,
  THEMES,
  useTheme,
  RUN_TAB,
  API,
  signIn,
  useView,
  keepNotificationReads,
  CORS,
  browseGenomeFasta,
  holdParts,
} from '../fixtures';
import type { Page } from '@playwright/test';
import { writeFileSync } from 'node:fs';

// Browser uploads: from a path field's file picker into the project bucket, with
// the tray's pause, resume, cancel and resume-after-reload. The mock takes 1 MiB parts, slowly;
// a test that must see an upload in flight holds its parts (`holdParts`) rather than race them.
//
// A finished upload stays in the shared mock and an upload never replaces an object, so a test that
// finishes one names its file for the attempt (a retry or repeat would otherwise find it taken).

const MIB = 1024 * 1024;
const fasta = (mib: number, name = 'genome.fa') => ({
  name,
  mimeType: 'text/plain',
  buffer: Buffer.alloc(mib * MIB, 'A'),
});

const re = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

async function openUpload(page: Page) {
  await browseGenomeFasta(page);
  const picker = page.getByRole('dialog');
  await picker.getByRole('button', { name: 'Upload files…' }).click();
  return picker;
}

test('an uploaded file fills the field, and launch waits for it', async ({ page }, info) => {
  const name = `genome-${info.repeatEachIndex}-${info.retry}.fa`;
  const release = await holdParts(page);
  const picker = await openUpload(page);
  await expect(picker.getByLabel('Upload folder')).toHaveValue(/^uploads\/\d{4}-\d{2}-\d{2}\/$/);
  await picker.getByLabel('Files to upload').setInputFiles(fasta(3, name));
  await expect(picker).toBeHidden();
  const field = page.getByRole('textbox', { name: /^Genome FASTA/ });
  await expect(field).toHaveValue(new RegExp(`^s3://genomics-project/uploads/.+/${re(name)}$`));
  await expect(page.getByText(new RegExp(`Uploading ${re(name)}… \\d+%`))).toBeVisible();
  const footer = page.locator('footer.launch-bar');
  await page.getByRole('button', { name: 'Launch run →' }).click();
  await expect(footer).toContainText(`${name} is still uploading`);

  release();
  const tray = page.getByRole('region', { name: 'Uploads' });
  await expect(tray.getByText('Uploaded')).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText(new RegExp(`Uploading ${re(name)}`))).toHaveCount(0);
  await expect(footer).not.toContainText('still uploading');
  // The file is now in the project bucket.
  await page.getByRole('button', { name: 'Browse for Genome FASTA' }).click();
  await expect(page.getByRole('dialog').getByText(name, { exact: true })).toBeVisible();
});

test('an upload pauses, resumes and cancels', async ({ page }) => {
  const release = await holdParts(page);
  const picker = await openUpload(page);
  await picker.getByLabel('Files to upload').setInputFiles(fasta(8, 'pause.fa'));
  const tray = page.getByRole('region', { name: 'Uploads' });
  await tray.getByRole('button', { name: 'Pause pause.fa' }).click();
  await expect(tray.getByText(/Paused at \d+%/)).toBeVisible();
  // Screen readers hear the change.
  await expect(page.locator('p[aria-live="polite"]')).toContainText('pause.fa: Paused at');
  await tray.getByRole('button', { name: 'Resume pause.fa' }).click();
  await expect(tray.getByText(/\d+% · /)).toBeVisible();
  await tray.getByRole('button', { name: 'Cancel pause.fa' }).click();
  await expect(tray.getByText('Cancelled')).toBeVisible();
  await tray.getByRole('button', { name: 'Clear finished' }).click();
  await expect(tray).toHaveCount(0);
  release();
});

test('a reload interrupts an upload, and choosing the file again finishes it', async ({
  page,
}, info) => {
  // A file on disk, so choosing it again gives the same file (name, size and modification time).
  // Its own name: tests share the mock, and an upload never replaces a finished one's object.
  const name = `reload-${info.repeatEachIndex}-${info.retry}.fa`;
  const path = info.outputPath(name);
  writeFileSync(path, fasta(24).buffer);
  // Some parts go, then the rest wait, so the reload always lands mid-upload.
  const release = await holdParts(page, 4);
  // No upload request names the file in its URL (the key rides in the POST body).
  const urls: string[] = [];
  // The parts sent (upload id and part number), before the reload and after it.
  const parts = { before: [] as [string, number][], after: [] as [string, number][] };
  let reloaded = false;
  page.on('request', (r) => {
    if (r.url().includes('/data/uploads')) urls.push(`${r.method()} ${r.url()}`);
    const part = /\/mock-s3\/([^/]+)\/(\d+)$/.exec(new URL(r.url()).pathname);
    if (part && r.method() === 'PUT')
      (reloaded ? parts.after : parts.before).push([part[1], Number(part[2])]);
  });
  const picker = await openUpload(page);
  await picker.getByLabel('Files to upload').setInputFiles(path);
  const tray = page.getByRole('region', { name: 'Uploads' });
  await expect(tray.getByText(/[1-9]\d*% · /)).toBeVisible();
  await page.reload();
  reloaded = true;
  // What S3 has of the upload, as the resumed page asks for it.
  const status = page.waitForResponse((r) =>
    /^\/data\/uploads\/[^/]+\/status$/.test(new URL(r.url()).pathname),
  );

  await expect(tray.getByText('Interrupted by a reload.')).toBeVisible();
  // The form (restored from its draft) waits for it.
  await expect(page.getByText(`Waiting for ${name} to be chosen again`)).toBeVisible();
  // Another file is refused.
  await tray
    .getByLabel(`Choose ${name} again to resume`)
    .setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.alloc(10, 'A') });
  await expect(tray.getByRole('alert')).toContainText('is not the file this upload started from');
  // The same file: only the missing parts go, and it finishes.
  release();
  await tray.getByLabel(`Choose ${name} again to resume`).setInputFiles(path);
  await expect(tray.getByText('Uploaded')).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText(`Waiting for ${name}`)).toHaveCount(0);
  expect(urls.some((u) => u.includes('/status'))).toBe(true);
  // The same multipart upload carried on, sending exactly the parts S3 did not have: none again.
  const done = ((await (await status).json()) as { parts: { part_number: number }[] }).parts.map(
    (p) => p.part_number,
  );
  expect(done.length).toBeGreaterThan(0);
  const [uploadId] = parts.before[0];
  expect(parts.after.every(([id]) => id === uploadId)).toBe(true);
  const sent = parts.after.map(([, n]) => n);
  expect(sent.filter((n) => done.includes(n))).toEqual([]);
  expect([...done, ...sent].sort((a, b) => a - b)).toEqual(
    Array.from({ length: 24 }, (_, i) => i + 1),
  );
  expect(urls.filter((u) => /reload|key=/.test(u) || !u.startsWith('POST '))).toEqual([]);
});

test("another tab's upload is that tab's: shown, but not offered to resume or discard", async ({
  page,
  context,
}) => {
  const release = await holdParts(page);
  const picker = await openUpload(page);
  await picker.getByLabel('Files to upload').setInputFiles(fasta(24, 'twotabs.fa'));
  const tray = page.getByRole('region', { name: 'Uploads' });
  await expect(tray.getByText(/\d+% · /)).toBeVisible();

  const other = await context.newPage();
  // Signed in there too, and kept off the shared mock's prefs and reads, as the fixture keeps
  // `page` (it does so for `page` only).
  await signIn(other);
  await useView(other, 'expert');
  await keepNotificationReads(other);
  await other.goto(RUN_TAB);
  const otherTray = other.getByRole('region', { name: 'Uploads' });
  await expect(otherTray.getByText('Uploading in another tab.')).toBeVisible();
  await expect(otherTray.getByText('Interrupted by a reload.')).toHaveCount(0);
  await expect(otherTray.getByRole('button', { name: /Discard/ })).toHaveCount(0);
  // Once the first tab's upload ends (here, cancelled), the other tab stops showing it.
  await tray.getByRole('button', { name: 'Cancel twotabs.fa' }).click();
  await expect(otherTray).toHaveCount(0);
  release();
});

test('an upload never replaces a file: a taken name is offered another', async ({ page }) => {
  // Held, and cancelled once it has started: finishing it would take the offered name in the shared
  // mock, and a retry would then be offered it again and refused.
  const release = await holdParts(page);
  const picker = await openUpload(page);
  await picker.getByLabel('Upload folder').fill('uploads/shared/');
  await picker.getByLabel('Files to upload').setInputFiles(fasta(1));
  const taken = picker.getByRole('group', { name: 'Name taken' });
  await expect(taken.getByRole('alert')).toContainText(
    'an object already exists at uploads/shared/genome.fa',
  );
  await expect(taken.getByLabel('New file name')).toHaveValue('genome-2.fa');
  await taken.getByRole('button', { name: 'Upload under this name' }).click();
  await expect(picker).toBeHidden();
  await expect(page.getByRole('textbox', { name: /^Genome FASTA/ })).toHaveValue(
    's3://genomics-project/uploads/shared/genome-2.fa',
  );
  const tray = page.getByRole('region', { name: 'Uploads' });
  await tray.getByRole('button', { name: /^Cancel genome/ }).click();
  await expect(tray.getByText('Cancelled')).toBeVisible();
  release();
});

test('a taken name offered again is offered the next number', async ({ page }) => {
  await page.route(`${API}/data/uploads`, async (route) => {
    const body = route.request().postDataJSON() as { key?: string } | null;
    if (route.request().method() === 'POST' && body?.key === 'uploads/shared/genome-2.fa')
      return route.fulfill({
        status: 409,
        headers: CORS,
        body: 'an object already exists at uploads/shared/genome-2.fa',
      });
    await route.fallback();
  });
  const picker = await openUpload(page);
  await picker.getByLabel('Upload folder').fill('uploads/shared/');
  await picker.getByLabel('Files to upload').setInputFiles(fasta(1));
  const taken = picker.getByRole('group', { name: 'Name taken' });
  await expect(taken.getByLabel('New file name')).toHaveValue('genome-2.fa');
  await taken.getByRole('button', { name: 'Upload under this name' }).click();
  await expect(taken.getByRole('alert')).toContainText('uploads/shared/genome-2.fa');
  await expect(taken.getByLabel('New file name')).toHaveValue('genome-3.fa');
});

test('read-only sources offer no upload', async ({ page }) => {
  await browseGenomeFasta(page);
  const picker = page.getByRole('dialog');
  await expect(picker.getByRole('button', { name: 'Upload files…' })).toBeVisible();
  await picker.getByRole('button', { name: /Reference genomes/ }).click();
  await expect(picker.getByRole('button', { name: 'Upload files…' })).toHaveCount(0);
});

for (const theme of THEMES) {
  test(`uploads (${theme}) have no serious accessibility violations`, async ({ page }) => {
    await useTheme(page, theme);
    const release = await holdParts(page);
    const picker = await openUpload(page);
    await expectNoSeriousA11yViolations(page);
    await picker.getByLabel('Files to upload').setInputFiles(fasta(4, `axe-${theme}.fa`));
    await page
      .getByRole('region', { name: 'Uploads' })
      .getByRole('button', { name: /Pause/ })
      .click();
    await expect(
      page.getByRole('region', { name: 'Uploads' }).getByText(/Paused at/),
    ).toBeVisible();
    await expectNoSeriousA11yViolations(page);
    release();
  });
}
