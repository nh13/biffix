import { describe, expect, it } from 'vitest';
import type { FormField, FormSchema } from '#lib/workflows/schema.js';
import { NO_CHOICE } from '../referenceChoice';
import { draftEntry, fingerprint, importEntry, undoNotice, type FormSnapshot } from './formEntry';

const field = (name: string, extra: Partial<FormField> = {}): FormField => ({
  name,
  widget: 'text',
  required: false,
  is_output: false,
  is_secret: false,
  is_path_input: false,
  ...extra,
});

const schema: FormSchema = {
  warnings: [],
  fields: [field('input'), field('fasta'), field('min_reads', { widget: 'number' })],
  references: {
    fill: { fasta: 'fasta' },
    required: ['fasta'],
    label: 'Reference genome',
    custom: true,
  },
};

const snapshot = (extra: Partial<FormSnapshot> = {}): FormSnapshot => ({
  values: { input: 's3://b/a.fq' },
  tables: {},
  sheetModes: {},
  refChoice: NO_CHOICE,
  retention: 'default',
  experiment: null,
  runName: 'run 1',
  from: null,
  notice: null,
  touched: {},
  ...extra,
});

describe('fingerprint', () => {
  it('changes with what is entered', () => {
    const at = fingerprint(snapshot());
    expect(fingerprint(snapshot())).toBe(at);
    expect(fingerprint(snapshot({ values: { input: 's3://b/other.fq' } }))).not.toBe(at);
    expect(fingerprint(snapshot({ runName: 'run 2' }))).not.toBe(at);
    expect(fingerprint(snapshot({ retention: 'keep' }))).not.toBe(at);
    expect(fingerprint(snapshot({ experiment: 'e1' }))).not.toBe(at);
    expect(fingerprint(snapshot({ refChoice: { bundle: 'grch38', custom: false } }))).not.toBe(at);
  });

  it('ignores what is not an edit: the banner, the touched fields, the sheet modes', () => {
    const at = fingerprint(snapshot());
    expect(fingerprint(snapshot({ notice: 'example:demo' }))).toBe(at);
    expect(fingerprint(snapshot({ touched: { input: true } }))).toBe(at);
    expect(fingerprint(snapshot({ sheetModes: { input: 'path' } }))).toBe(at);
  });
});

describe('undoNotice', () => {
  it("keeps the banner from before the example, but not a restored draft's", () => {
    expect(undoNotice('draft')).toBeNull();
    expect(undoNotice('imported')).toBe('imported');
    expect(undoNotice(null)).toBeNull();
  });
});

describe('draftEntry', () => {
  const fresh = { runName: 'fresh name', defaultRunName: 'fresh name', experimentParam: null };

  it('restores nothing without a draft, or from one holding nothing entered', () => {
    expect(draftEntry(schema, null, fresh)).toBeNull();
    expect(draftEntry(schema, { values: {}, runName: 'fresh name' }, fresh)).toBeNull();
  });

  it('restores what was entered, filling what an older draft lacks', () => {
    expect(draftEntry(schema, { values: { min_reads: 5 }, runName: '' }, fresh)).toEqual({
      values: { min_reads: 5 },
      tables: {},
      sheetModes: {},
      refChoice: NO_CHOICE,
      retention: 'default',
      // An empty name keeps the fresh form's.
      runName: 'fresh name',
      experiment: null,
    });
  });

  it("restores a name, a retention or an experiment alone, and the link's experiment wins", () => {
    expect(draftEntry(schema, { values: {}, runName: 'mine' }, fresh)?.runName).toBe('mine');
    expect(
      draftEntry(schema, { values: {}, runName: 'fresh name', retention: 'keep' }, fresh)
        ?.retention,
    ).toBe('keep');
    const draft = { values: {}, runName: 'fresh name', experiment: 'e1' };
    expect(draftEntry(schema, draft, fresh)?.experiment).toBe('e1');
    // The link names e2: the draft holds an experiment other than a fresh form's, and e2 is kept.
    expect(draftEntry(schema, draft, { ...fresh, experimentParam: 'e2' })?.experiment).toBe('e2');
    // The draft's experiment is the link's own: nothing was entered.
    expect(draftEntry(schema, draft, { ...fresh, experimentParam: 'e1' })).toBeNull();
  });
});

describe('importEntry', () => {
  const current = { showAll: false, refChoice: NO_CHOICE };

  it("imports a version's params and says so", () => {
    expect(importEntry(schema, 'input: s3://b/a.fq\nmin_reads: 3\n', current)).toEqual({
      values: { input: 's3://b/a.fq', min_reads: 3 },
      refChoice: NO_CHOICE,
      notice: 'imported',
    });
  });

  it('skips the keys that are no params of this version unless all params are shown', () => {
    const text = 'input: s3://b/a.fq\nextra: 1\n';
    expect(importEntry(schema, text, current)).toEqual({
      values: { input: 's3://b/a.fq' },
      refChoice: NO_CHOICE,
      notice: 'imported-unknown:extra',
    });
    expect(importEntry(schema, text, { ...current, showAll: true }).values).toEqual({
      input: 's3://b/a.fq',
      extra: 1,
    });
  });

  it("switches the reference to the user's own files when it fills a bundle's param", () => {
    expect(importEntry(schema, 'fasta: s3://b/my.fa\n', current).refChoice).toEqual({
      bundle: null,
      custom: true,
    });
  });

  it('throws on a file that is not a params mapping', () => {
    expect(() => importEntry(schema, '- a\n- b\n', current)).toThrow(/mapping/);
  });
});
