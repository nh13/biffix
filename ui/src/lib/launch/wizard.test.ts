import { describe, expect, it } from 'vitest';
import type { ReferenceBundle } from '#lib/api/index.js';
import type { FormField, FormSchema } from '#lib/workflows/schema.js';
import { NO_CHOICE, REFERENCE_FIELD } from './referenceChoice';
import {
  canReach,
  changedOptions,
  describeValue,
  firstBlockedStep,
  parseStep,
  placeFields,
  reviewDetailsSummary,
  reviewOptions,
  reviewReference,
  reviewSampleLines,
  sampleFields,
  sampleNames,
  stepCards,
  stepOfField,
  stepProblems,
} from './wizard';

const field = (name: string, extra: Partial<FormField> = {}): FormField => ({
  name,
  widget: 'text',
  required: false,
  is_output: false,
  is_secret: false,
  is_path_input: false,
  ...extra,
});

const schemaOf = (fields: FormField[], extra: Partial<FormSchema> = {}): FormSchema => ({
  warnings: [],
  fields,
  ...extra,
});

const spec = {
  columns: [
    { name: 'sample', role: 'sample_id' as const, required: true },
    { name: 'fastq_1', role: 'read1' as const, required: true, file: true },
  ],
};

describe('parseStep', () => {
  it('reads steps 1 to 4 and defaults to Add samples', () => {
    expect(parseStep('1')).toBe(1);
    expect(parseStep('3')).toBe(3);
    expect(parseStep('4')).toBe(4);
    for (const raw of [null, '', '0', '5', '2.5', 'review']) expect(parseStep(raw)).toBe(2);
  });
});

describe('sampleFields', () => {
  it('is the samplesheet params when there are any', () => {
    const s = schemaOf([
      field('input', { widget: 'samplesheet', samplesheet: spec, is_path_input: true }),
      field('fasta', { widget: 's3_file', is_path_input: true }),
    ]);
    expect([...sampleFields(s)]).toEqual(['input']);
  });

  it('never counts a hidden samplesheet', () => {
    const s = schemaOf([
      field('input', { widget: 'samplesheet', samplesheet: spec, hidden: true }),
      field('reads', { widget: 's3_file', is_path_input: true }),
    ]);
    expect([...sampleFields(s)]).toEqual(['reads']);
  });

  it('leaves input files in an advanced section to the options step', () => {
    const s = schemaOf(
      [
        field('reads', { widget: 's3_file', is_path_input: true, section: 'main' }),
        field('index', { widget: 's3_dir', is_path_input: true, section: 'extra' }),
      ],
      {
        sections: [
          { id: 'main', title: 'Main' },
          { id: 'extra', title: 'Extra', advanced: true },
        ],
      },
    );
    expect([...sampleFields(s)]).toEqual(['reads']);
  });

  it('is the input files otherwise, leaving out outputs, hidden and reference-filled params', () => {
    const s = schemaOf(
      [
        field('reads', { widget: 's3_file', is_path_input: true }),
        field('bams', { widget: 's3_dir', is_path_input: true }),
        field('out_dir', { widget: 's3_dir', is_path_input: true, is_output: true }),
        field('secret_file', { widget: 's3_file', is_path_input: true, hidden: true }),
        field('fasta', { widget: 's3_file', is_path_input: true }),
        field('threads', { widget: 'number' }),
      ],
      {
        references: {
          kind: 'assembly',
          fill: { fasta: 'fasta' },
          required: ['fasta'],
          label: 'Reference genome',
        },
      },
    );
    expect([...sampleFields(s)]).toEqual(['reads', 'bams']);
  });
});

describe('stepCards', () => {
  const card = (id: string, fields: FormField[]) => ({
    section: { id, title: id },
    fields,
    changed: false,
  });
  const input = field('input', { widget: 'samplesheet', samplesheet: spec });
  const fasta = field('fasta');
  const reads = field('reads');
  const trim = field('trim');
  const lay = {
    cards: [card('inputs', [input]), card('reference', [fasta]), card('options', [trim])],
    advanced: [card('extra', [reads]), card('more', [field('x')])],
  };
  const at = { samples: new Set(['input', 'reads']), review: new Set<string>() };

  it("puts Add samples' fields on step 2 in the layout's order, from Advanced too", () => {
    expect(stepCards(lay, at, null).samples.map((f) => f.name)).toEqual(['input', 'reads']);
  });

  it("leaves step 3 the rest, dropping emptied cards except the reference dropdown's", () => {
    const s = stepCards(lay, at, 'inputs');
    expect(s.cards.map((c) => [c.section.id, c.fields.map((f) => f.name)])).toEqual([
      ['inputs', []],
      ['reference', ['fasta']],
      ['options', ['trim']],
    ]);
    expect(s.advanced.map((c) => c.section.id)).toEqual(['more']);
    expect(stepCards(lay, at, null).cards.map((c) => c.section.id)).toEqual([
      'reference',
      'options',
    ]);
  });
});

describe('step problems', () => {
  const samples = { samples: new Set(['input']), review: new Set(['outdir']) };
  const problems = [
    { field: 'input', message: 'required' },
    { field: REFERENCE_FIELD, message: 'choose one' },
    { field: 'umi_pattern', message: 'required' },
  ];

  it('puts each problem on the step its field is on', () => {
    expect(stepOfField('input', samples)).toBe(2);
    expect(stepOfField(REFERENCE_FIELD, samples)).toBe(3);
    expect(stepOfField('aligner', samples)).toBe(3);
    // Nextflow's output folder is set on the Review step.
    expect(stepOfField('outdir', samples)).toBe(4);
    expect(stepProblems(problems, samples, 2).map((p) => p.field)).toEqual(['input']);
    expect(stepProblems(problems, samples, 3).map((p) => p.field)).toEqual([
      REFERENCE_FIELD,
      'umi_pattern',
    ]);
    // The Review is blocked by every problem.
    expect(stepProblems(problems, samples, 4)).toHaveLength(3);
  });

  it('reaches a step only once every step before it is complete', () => {
    expect(canReach(2, problems, samples)).toBe(true);
    expect(canReach(3, problems, samples)).toBe(false);
    const step3Only = problems.slice(1);
    expect(canReach(3, step3Only, samples)).toBe(true);
    expect(canReach(4, step3Only, samples)).toBe(false);
    expect(canReach(4, [], samples)).toBe(true);
    // Every earlier step counts, not just the one before.
    expect(canReach(4, problems.slice(0, 1), samples)).toBe(false);
    // A problem on the Review's own fields blocks nothing before it.
    expect(canReach(4, [{ field: 'outdir', message: 'must be an s3:// path' }], samples)).toBe(
      true,
    );
  });

  it('names the first step a problem blocks', () => {
    expect(firstBlockedStep(problems, samples)).toBe(2);
    expect(firstBlockedStep(problems.slice(1), samples)).toBe(3);
    expect(firstBlockedStep([], samples)).toBeNull();
  });

  it("places Nextflow's output folder on the Review step", () => {
    const s = schemaOf([field('outdir', { widget: 's3_dir', is_output: true })]);
    expect([...placeFields(s, 'nextflow').review]).toEqual(['outdir']);
    expect([...placeFields(s, 'snakemake').review]).toEqual([]);
  });
});

describe('changed options', () => {
  const aligner = field('aligner', {
    widget: 'enum',
    enum_values: ['star_salmon', 'hisat2'],
    default: 'star_salmon',
    presets: [
      { label: 'STAR + Salmon', value: 'star_salmon' },
      { label: 'HISAT2', value: 'hisat2' },
    ],
  });
  const umi = field('with_umi', { widget: 'enum', enum_values: ['true', 'false'], default: false });
  const reads = field('min_reads', { widget: 'number', default: 10000, prefill: 5000 });
  const note = field('note');

  it('describes a value by its preset, as Yes/No for a switch, else as entered', () => {
    expect(describeValue(aligner, 'hisat2')).toBe('HISAT2');
    expect(describeValue(umi, true)).toBe('Yes');
    expect(describeValue(umi, 'false')).toBe('No');
    expect(describeValue(note, ' hi ')).toBe('hi');
    expect(describeValue(reads, 7)).toBe('7');
  });

  it('lists only the changed ones, in field order, keyed by name', () => {
    expect(
      changedOptions(
        [aligner, umi, reads, note],
        { aligner: 'star_salmon', with_umi: true, min_reads: 5000, note: 'hi' },
        { min_reads: 5000 },
      ),
    ).toEqual([
      { name: 'with_umi', label: 'With umi', value: 'Yes' },
      { name: 'note', label: 'Note', value: 'hi' },
    ]);
  });

  it('compares what is sent: a number typed another way is no change, a cleared prefill is', () => {
    expect(changedOptions([reads], { min_reads: 5000.0 }, { min_reads: 5000 })).toEqual([]);
    expect(changedOptions([reads], {}, { min_reads: 5000 })).toEqual([
      { name: 'min_reads', label: 'Min reads', value: 'Default (10000)' },
    ]);
    expect(changedOptions([note], {}, { note: 'x' })).toEqual([
      { name: 'note', label: 'Note', value: 'Not set' },
    ]);
  });

  it('never shows a secret', () => {
    const token = field('token', { is_secret: true });
    expect(changedOptions([token], { token: 'hunter2' }, {})).toEqual([
      { name: 'token', label: 'Token', value: 'Set' },
    ]);
  });
});

describe('sampleNames', () => {
  it('counts samples as the server does and names the first few', () => {
    const rows = ['A', 'B', 'B', 'C', 'D', 'E', 'F', ''].map((s, i) => ({
      sample: s,
      fastq_1: s ? `s3://b/${s}_${i}.fq.gz` : '',
    }));
    expect(sampleNames(spec, rows, 3)).toEqual({ count: 6, first: ['A', 'B', 'C'], more: 3 });
    expect(sampleNames(spec, [])).toEqual({ count: 0, first: [], more: 0 });
  });
});

describe('the Review step', () => {
  const sheet = field('input', { widget: 'samplesheet', samplesheet: spec });
  const reads = field('reads_dir', { is_path_input: true });
  const fasta = field('genome_fasta', { is_path_input: true });
  const gtf = field('annotation', { is_path_input: true });
  const depth = field('depth', { widget: 'number', default: 10 });
  const outdir = field('outdir', { is_output: true, is_path_input: true });
  const refs = {
    kind: 'assembly',
    fill: { genome_fasta: 'fasta', annotation: 'gtf' },
    required: ['fasta'],
    label: 'Reference genome',
    custom: true,
  };
  const withRefs = schemaOf([sheet, fasta, gtf, depth, outdir], { references: refs });
  const grch38: ReferenceBundle = {
    id: 'grch38',
    kind: 'assembly',
    display_name: 'GRCh38',
    uris: { fasta: 's3://refs/grch38/fasta' },
    created_at_micros: 0,
    updated_at_micros: 0,
  };
  const custom = { bundle: null, custom: true };

  const byName = (s: FormSchema) => new Map(s.fields.map((f) => [f.name, f]));
  const reviewRef = (
    s: FormSchema,
    choice: { bundle: string | null; custom: boolean },
    bundle: ReferenceBundle | null,
    params: Record<string, unknown> = {},
    inactive: ReadonlySet<string> = new Set(),
  ) => reviewReference(s, { byName: byName(s), choice, bundle, inactive, params });

  it('summarizes a sheet built as rows, a sheet file and a plain input', () => {
    const rows = [{ sample: 'A', fastq_1: 's3://b/a.fq' }];
    expect(
      reviewSampleLines([sheet], { sheetModes: {}, tables: { input: rows }, params: {} }),
    ).toEqual([{ kind: 'rows', name: 'input', label: 'Input', count: 1, first: ['A'], more: 0 }]);
    expect(
      reviewSampleLines([sheet], {
        sheetModes: { input: 'path' },
        tables: {},
        params: { input: 's3://b/s.csv' },
      }),
    ).toEqual([
      { kind: 'value', name: 'input', label: 'Input', value: 's3://b/s.csv', sheetFile: true },
    ]);
    expect(reviewSampleLines([reads], { sheetModes: {}, tables: {}, params: {} })).toEqual([
      { kind: 'value', name: 'reads_dir', label: 'Reads dir', value: null, sheetFile: false },
    ]);
    // A list of paths, one after another.
    expect(
      reviewSampleLines([reads], {
        sheetModes: {},
        tables: {},
        params: { reads_dir: ['s3://b/a/', 's3://b/b/'] },
      })[0],
    ).toMatchObject({ value: 's3://b/a/, s3://b/b/' });
  });

  it('shows a samplesheet param without a sheet spec as a sheet file, even in rows mode', () => {
    const bare = field('sheet', { widget: 'samplesheet' });
    const rows = [{ sample: 'A', fastq_1: 's3://b/a.fq' }];
    expect(
      reviewSampleLines([bare], {
        sheetModes: {},
        tables: { sheet: rows },
        params: { sheet: 's3://b/s.csv' },
      }),
    ).toEqual([
      { kind: 'value', name: 'sheet', label: 'Sheet', value: 's3://b/s.csv', sheetFile: true },
    ]);
  });

  it('has no reference card without a reference dropdown', () => {
    expect(reviewRef(schemaOf([depth]), NO_CHOICE, null)).toBeNull();
  });

  it('names the chosen bundle and what it provides, or that none is chosen yet', () => {
    const label = 'Reference genome';
    const chooseGrch38 = { bundle: 'grch38', custom: false };
    expect(reviewRef(withRefs, chooseGrch38, grch38)).toEqual({
      label,
      name: 'GRCh38',
      detail: 'Provides fasta',
      files: [],
    });
    expect(reviewRef(withRefs, chooseGrch38, null)).toEqual({
      label,
      name: 'grch38',
      detail: '',
      files: [],
    });
    expect(reviewRef(withRefs, NO_CHOICE, null)).toEqual({
      label,
      name: 'Not chosen yet',
      detail: '',
      files: [],
    });
  });

  it("lists a bundle's keys in the form's fill order", () => {
    const full = {
      ...grch38,
      uris: { gtf: 's3://refs/grch38/gtf', fasta: 's3://refs/grch38/fasta' },
    };
    expect(reviewRef(withRefs, { bundle: 'grch38', custom: false }, full)?.detail).toBe(
      'Provides fasta, gtf',
    );
  });

  it("lists the user's own files, in fill order, without those show_if hides", () => {
    // The fill order (annotation, then genome_fasta) is not the fields' order.
    const reordered = schemaOf([sheet, fasta, gtf, depth, outdir], {
      references: { ...refs, fill: { annotation: 'gtf', genome_fasta: 'fasta' } },
    });
    expect(reviewRef(reordered, custom, null, { genome_fasta: 's3://b/g.fa' })).toEqual({
      label: 'Reference genome',
      name: 'Your own files',
      detail: '',
      files: [
        { name: 'annotation', label: 'Annotation', value: null },
        { name: 'genome_fasta', label: 'Genome fasta', value: 's3://b/g.fa' },
      ],
    });
    expect(
      reviewRef(withRefs, custom, null, { genome_fasta: 's3://b/g.fa' }, new Set(['annotation']))
        ?.files,
    ).toEqual([{ name: 'genome_fasta', label: 'Genome fasta', value: 's3://b/g.fa' }]);
  });

  it('lists changed options other than the samples, reference params and output folder', () => {
    const at = placeFields(withRefs, 'nextflow');
    const sent = { input: 'x', genome_fasta: 'y', annotation: 'z', depth: 20, outdir: 's3://o' };
    const base = { at, choice: custom, bundle: null, inactive: new Set<string>(), params: sent };
    expect(reviewOptions(withRefs, { ...base, start: {} })).toEqual([
      { name: 'depth', label: 'Depth', value: '20' },
    ]);
    // A bundle's params are the reference card's; one the bundle lacks is an option.
    expect(
      reviewOptions(withRefs, {
        ...base,
        choice: { bundle: 'grch38', custom: false },
        bundle: grch38,
        start: {},
      }),
    ).toEqual([
      { name: 'annotation', label: 'Annotation', value: 'z' },
      { name: 'depth', label: 'Depth', value: '20' },
    ]);
    // A field show_if hides is left out.
    expect(reviewOptions(withRefs, { ...base, inactive: new Set(['depth']), start: {} })).toEqual(
      [],
    );
  });

  it("compares with a fresh form's params and lists an author-hidden field", () => {
    const at = placeFields(withRefs, 'nextflow');
    const base = { at, choice: custom, bundle: null, inactive: new Set<string>() };
    // What a fresh form sends is no change.
    expect(
      reviewOptions(withRefs, { ...base, params: { depth: 20 }, start: { depth: 20 } }),
    ).toEqual([]);
    const hiddenDepth = schemaOf([sheet, fasta, gtf, { ...depth, hidden: true }, outdir], {
      references: refs,
    });
    expect(reviewOptions(hiddenDepth, { ...base, params: { depth: 30 }, start: {} })).toEqual([
      { name: 'depth', label: 'Depth', value: '30' },
    ]);
  });
});

describe('reviewDetailsSummary', () => {
  it('says where results go, how long intermediate files stay, and that the bell tells', () => {
    expect(
      reviewDetailsSummary({
        outdir: null,
        resume: false,
        retention: 'Keep for 30 days (default)',
      }),
    ).toBe(
      'Results folder: this analysis’s own · Intermediate files: keep for 30 days (default) · Notify: in the app',
    );
    expect(
      reviewDetailsSummary({ outdir: ' s3://lab/out/ ', resume: false, retention: null }),
    ).toBe('Results folder: s3://lab/out/ · Notify: in the app');
    expect(reviewDetailsSummary({ outdir: '  ', resume: false, retention: null })).toBe(
      'Results folder: this analysis’s own · Notify: in the app',
    );
    expect(reviewDetailsSummary({ outdir: 's3://x/', resume: true, retention: null })).toBe(
      'Results folder: the resumed analysis’s · Notify: in the app',
    );
  });
});
