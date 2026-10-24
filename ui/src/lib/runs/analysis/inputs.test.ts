import { describe, expect, it } from 'vitest';
import type { FormField, FormSchema } from '#lib/workflows/schema.js';
import {
  sheetFile,
  allSettings,
  s3Uris,
  settingCount,
  usedFiles,
  usedOptions,
  usedSheets,
  sharesOriginFolder,
} from './inputs';

const field = (name: string, over: Partial<FormField> = {}): FormField =>
  ({
    name,
    widget: 'text',
    required: false,
    is_output: false,
    is_secret: false,
    is_path_input: false,
    ...over,
  }) as FormField;

const schema = {
  fields: [
    field('input', {
      widget: 'samplesheet',
      samplesheet: { columns: [{ name: 'sample', role: 'sample_id', required: true }] },
    }),
    field('outdir', { widget: 's3_dir', is_output: true }),
    field('fasta', { widget: 's3_file' }),
    field('aligner', { widget: 'enum', enum_values: ['star', 'hisat2'], default: 'star' }),
    field('min_reads', { widget: 'number', default: 10 }),
    field('token', { is_secret: true }),
  ],
  warnings: [],
  references: { kind: 'assembly', fill: { fasta: 'fasta' }, required: ['fasta'] },
} as unknown as FormSchema;

describe('usedSheets', () => {
  it('lists the sheets biffix wrote, with the samples they name', () => {
    const rows = ['A', 'B', 'B', 'C', 'D'].map((sample) => ({ sample }));
    const sheets = usedSheets(
      { input: { uri: 's3://x/input.csv', rows }, other: { rows } },
      schema,
    );
    expect(sheets).toHaveLength(1);
    expect(sheets[0]).toMatchObject({ param: 'input', count: 4, first: ['A', 'B', 'C'], more: 1 });
    expect(usedSheets(null, schema)).toEqual([]);
    expect(usedSheets({ input: { rows: [] } }, schema)).toEqual([]);
  });
});

describe('sheetFile', () => {
  it('names a downloaded samplesheet by its param, with its registered media type', () => {
    expect(sheetFile('input', 'tsv')).toEqual({
      name: 'input.tsv',
      type: 'text/tab-separated-values',
    });
    expect(sheetFile('input', 'csv')).toEqual({ name: 'input.csv', type: 'text/csv' });
    expect(sheetFile('input', undefined)).toEqual({ name: 'input.csv', type: 'text/csv' });
  });
});

describe('usedOptions', () => {
  it('shows only what differs from the standard settings, a secret as set', () => {
    const params = {
      input: 's3://x/input.csv',
      outdir: 's3://x/results',
      fasta: 's3://refs/genome.fa',
      aligner: 'hisat2',
      min_reads: 10,
      token: '[REDACTED]',
    };
    expect(usedOptions(params, schema, 'nextflow', true)).toEqual([
      { name: 'aligner', label: 'Aligner', value: 'hisat2' },
      { name: 'token', label: 'Token', value: 'Set' },
    ]);
    // Without a bundle, the reference file is the user's own choice.
    expect(usedOptions(params, schema, 'nextflow', false).map((o) => o.name)).toEqual([
      'fasta',
      'aligner',
      'token',
    ]);
    expect(usedOptions({}, schema, 'nextflow', false)).toEqual([]);
  });
});

describe('s3Uris', () => {
  it('reads an S3 URI or a list of them, and nothing else', () => {
    expect(s3Uris(' s3://b/k ')).toEqual(['s3://b/k']);
    expect(s3Uris(['s3://b/a', 's3://b/b'])).toEqual(['s3://b/a', 's3://b/b']);
    expect(s3Uris(['s3://b/a', 'local'])).toEqual([]);
    expect(s3Uris('s3://bucket-only')).toEqual([]);
    expect(s3Uris(3)).toEqual([]);
  });

  it('lists a location named twice once (the page keys its rows by location)', () => {
    expect(s3Uris(['s3://b/a', 's3://b/b', ' s3://b/a'])).toEqual(['s3://b/a', 's3://b/b']);
  });
});

describe('usedFiles', () => {
  // Every field a path input, so it is the output, outdir and bundle checks that leave some out.
  const files = schema.fields
    .map((f) => (f.name === 'outdir' || f.name === 'fasta' ? { ...f, is_path_input: true } : f))
    .concat([
      field('reads', { widget: 's3_file', is_path_input: true, label: 'Reads' }),
      field('bed', { widget: 's3_file', is_path_input: true }),
    ]);
  const withFiles = { ...schema, fields: files } as FormSchema;
  const params = {
    input: 's3://x/input.csv',
    outdir: 's3://x/results',
    fasta: 's3://refs/genome.fa',
    reads: ['s3://core/a.fq.gz', 's3://core/b.fq.gz'],
  };
  it('lists the files given, not the samplesheet written from rows, outputs or bundle files', () => {
    const sheetParams = new Set(['input']);
    expect(usedFiles(params, withFiles, 'nextflow', { usedBundle: true, sheetParams })).toEqual([
      { name: 'reads', label: 'Reads', uris: ['s3://core/a.fq.gz', 's3://core/b.fq.gz'] },
    ]);
  });
  it('lists a reference file the user chose, when no bundle filled it', () => {
    const opts = { usedBundle: false, sheetParams: new Set(['input']) };
    expect(usedFiles(params, withFiles, 'nextflow', opts).map((f) => f.name)).toEqual([
      'fasta',
      'reads',
    ]);
  });
  it('lists a samplesheet given as a file', () => {
    const sheetField = field('input', { widget: 'samplesheet', is_path_input: true });
    const s = { ...schema, fields: [sheetField] } as FormSchema;
    const opts = { usedBundle: false, sheetParams: new Set<string>() };
    expect(usedFiles({ input: 's3://x/mine.csv' }, s, 'nextflow', opts)).toEqual([
      { name: 'input', label: 'Input', uris: ['s3://x/mine.csv'] },
    ]);
  });
});

describe('allSettings', () => {
  const params = {
    input: 's3://x/input.csv',
    outdir: 's3://x/results',
    fasta: 's3://refs/genome.fa',
    aligner: 'hisat2',
    token: '[REDACTED]',
    extra_flag: { a: 1 },
  };
  const opts = { usedBundle: true, sheetParams: new Set(['input']) };

  it('lists every setting by section: defaults, changes, secrets and reference files', () => {
    const sections = allSettings(params, schema, opts);
    expect(sections.map((s) => s.title)).toEqual(['Other options', 'Other settings']);
    const rows = Object.fromEntries(sections.flatMap((s) => s.rows).map((r) => [r.name, r]));
    expect(rows.input).toBeUndefined();
    expect(rows.aligner).toMatchObject({ value: 'hisat2', changed: true });
    expect(rows.min_reads).toMatchObject({ value: 'Default (10)', changed: false });
    expect(rows.token).toMatchObject({ value: 'Set', uris: [] });
    expect(rows.fasta).toMatchObject({ uris: ['s3://refs/genome.fa'], fromReference: true });
    expect(rows.outdir.uris).toEqual(['s3://x/results']);
    expect(rows.extra_flag).toMatchObject({ value: '{"a":1}', changed: true });
    expect(settingCount(sections)).toBe(6);
  });

  it('lists a hidden field only when the run sent it, and a secret not sent as not set', () => {
    const hidden = {
      ...schema,
      fields: [field('mode', { hidden: true }), field('pw', { is_secret: true, default: 'x' })],
    } as FormSchema;
    const values = (p: Record<string, unknown>) =>
      allSettings(p, hidden, opts).flatMap((s) => s.rows.map((r) => [r.name, r.value]));
    expect(values({})).toEqual([['pw', 'Not set']]);
    expect(values({ mode: 'on' })).toEqual([
      ['mode', 'on'],
      ['pw', 'Not set'],
    ]);
  });
});

describe('sharesOriginFolder', () => {
  const run = { resumed_from: 'r0', output_prefix: 's3://b/biffix/runs/r0/' };
  it('says so only once the origin is known, ignoring a trailing slash', () => {
    expect(sharesOriginFolder(run, { output_prefix: 's3://b/biffix/runs/r0' })).toBe(true);
    expect(sharesOriginFolder(run, { output_prefix: 's3://b/elsewhere/' })).toBe(false);
    // Unknown (not loaded, or not readable): no claim.
    expect(sharesOriginFolder(run, null)).toBe(false);
    expect(sharesOriginFolder({ ...run, resumed_from: null }, run)).toBe(false);
    expect(sharesOriginFolder({ ...run, output_prefix: null }, run)).toBe(false);
  });
});
