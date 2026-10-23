import { deflateSync } from 'node:zlib';
import { test, expect, startFromExample } from '../fixtures';

// A form preview: `biffix workflows form --open` puts an unregistered form in the
// URL fragment; the page renders it with Launch disabled and never saves a draft.

const form = {
  curated: true,
  sections: [{ id: 'input', title: 'Input' }],
  fields: [
    {
      name: 'greeting',
      widget: 'text',
      required: true,
      label: 'Greeting',
      section: 'input',
      is_output: false,
      is_secret: false,
      is_path_input: false,
    },
  ],
  examples: [{ name: 'Say hi', params: { greeting: 'hi' } }],
  warnings: [],
};

function previewUrl(value: unknown): string {
  const payload = deflateSync(Buffer.from(JSON.stringify(value))).toString('base64url');
  return `/workflows/preview#${payload}`;
}

test('a preview link renders the form with Launch disabled', async ({ page }) => {
  await page.goto(previewUrl({ name: 'hello-preview', engine: 'nextflow', form_schema: form }));
  await expect(page.getByRole('heading', { name: 'hello-preview', level: 1 })).toBeVisible();
  await expect(page.getByRole('status').first()).toContainText('Preview — not registered.');
  const launch = page.getByRole('button', { name: 'Launch run →' });
  await expect(launch).toBeDisabled();

  // Examples still fill the form; nothing is kept as a draft.
  await startFromExample(page, /Say hi/);
  await expect(page.getByRole('textbox', { name: /^Greeting/ })).toHaveValue('hi');
  await expect(page.locator('footer.launch-bar')).toContainText('Ready to launch');
  await expect(launch).toBeDisabled();
  const keys = await page.evaluate(() => Object.keys(sessionStorage));
  expect(keys.filter((k) => k.startsWith('biffix:launch-draft:'))).toEqual([]);
});

test("a preview's CLI hint names no made-up version id", async ({ page }) => {
  await page.goto(previewUrl({ name: 'hello-preview', engine: 'nextflow', form_schema: form }));
  await expect(page.getByText('biffix runs submit <version-id> --params')).toBeVisible();
  await expect(page.getByText('biffix runs submit preview')).toHaveCount(0);
});

test('quick link changes show the last link, not whichever decoded last', async ({ page }) => {
  await page.goto(previewUrl({ name: 'first', engine: 'nextflow', form_schema: form }));
  await expect(page.getByRole('heading', { name: 'first', level: 1 })).toBeVisible();
  const second = previewUrl({ name: 'second', engine: 'nextflow', form_schema: form });
  const third = previewUrl({ name: 'third', engine: 'snakemake', form_schema: form });
  await page.evaluate(
    ([a, b]) => {
      location.hash = a.split('#')[1];
      location.hash = b.split('#')[1];
    },
    [second, third],
  );
  await expect(page.getByRole('heading', { name: 'third', level: 1 })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'second', level: 1 })).toHaveCount(0);
});

test('a broken preview link says so', async ({ page }) => {
  await page.goto('/workflows/preview#not-a-form');
  await expect(page.getByText(/Couldn't read the form in this link/)).toBeVisible();
  await page.goto('/workflows/preview');
  await expect(page.getByText(/This link has no form in it/)).toBeVisible();
});
