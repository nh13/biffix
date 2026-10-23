import { describe, expect, it, vi } from 'vitest';
import { storageWith } from '$lib/api/testing';
import type { FormField, FormSchema } from '$lib/workflows/schema';
import {
  defaultOutdir,
  draftValues,
  loadDraft,
  firstProblems,
  formHasInput,
  helpOf,
  humanize,
  isChanged,
  launchParts,
  launchRequest,
  type LaunchRequestInputs,
  launchProblems,
  labelOf,
  layout,
  paramsFrom,
  paramsCommand,
  paramsYaml,
  parseParamsYaml,
  pinnedSpelling,
  prefillFromRun,
  prefillValues,
  presetMeaning,
  problemOrder,
  problemTitle,
  runNameFrom,
  secretsToReenter,
  sectionsOf,
  uploadingProblems,
  validateForm,
  valuesFromExample,
} from './form';
import { NO_CHOICE, REFERENCE_FIELD } from './referenceChoice';

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
  sections: [
    { id: 'samples', title: 'Samples' },
    { id: 'options', title: 'Options' },
    { id: 'trimming', title: 'Trimming', advanced: true },
  ],
  fields: [
    field('input', { widget: 'samplesheet', required: true, section: 'samples' }),
    field('outdir', { widget: 's3_dir', required: true, is_output: true, section: 'samples' }),
    field('with_umi', {
      widget: 'enum',
      enum_values: ['true', 'false'],
      default: false,
      section: 'options',
    }),
    field('umi_pattern', {
      required: true,
      section: 'options',
      show_if: [{ field: 'with_umi', values: [true] }],
    }),
    field('min_reads', {
      widget: 'number',
      min: 1,
      step: 1,
      default: 10,
      section: 'trimming',
    }),
    field('email', { hidden: true, section: 'trimming' }),
  ],
};

describe('labels', () => {
  it('humanizes names and prefers the author label', () => {
    expect(humanize('min_trimmed_reads')).toBe('Min trimmed reads');
    expect(labelOf(field('x', { label: 'Aligner' }))).toBe('Aligner');
    expect(labelOf(field('umi-pattern'))).toBe('Umi pattern');
  });
  it('offers help only when it adds to the description', () => {
    expect(helpOf(field('a', { description: 'Same', help: 'Same' }))).toBeNull();
    expect(helpOf(field('a', { description: 'Short', help: 'Longer' }))).toBe('Longer');
  });
});

describe('layout', () => {
  it('puts visible sections in cards and advanced ones together', () => {
    const l = layout(schema, 'nextflow', {}, false);
    expect(l.cards.map((c) => c.section.id)).toEqual(['samples', 'options']);
    expect(l.advanced.map((c) => c.section.id)).toEqual(['trimming']);
    // outdir moves to Run details; the gated UMI pattern is hidden; `email` is hidden by the author.
    expect(l.cards[0].fields.map((f) => f.name)).toEqual(['input']);
    expect(l.cards[1].fields.map((f) => f.name)).toEqual(['with_umi']);
    expect(l.advanced[0].fields.map((f) => f.name)).toEqual(['min_reads']);
    expect(l.outdir?.name).toBe('outdir');
  });
  it('shows gated fields when the gate opens, and hidden ones under show-all', () => {
    const l = layout(schema, 'nextflow', { with_umi: true }, true);
    expect(l.cards[1].fields.map((f) => f.name)).toEqual(['with_umi', 'umi_pattern']);
    expect(l.advanced[0].fields.map((f) => f.name)).toEqual(['min_reads', 'email']);
  });
  it('shows a hidden field it is asked to reveal', () => {
    const l = layout(schema, 'nextflow', {}, false, { reveal: new Set(['email']) });
    expect(l.advanced[0].fields.map((f) => f.name)).toEqual(['min_reads', 'email']);
  });
  it('marks a group whose values differ from their defaults', () => {
    expect(layout(schema, 'nextflow', { min_reads: 10 }, false).advanced[0].changed).toBe(false);
    expect(layout(schema, 'nextflow', { min_reads: '20' }, false).advanced[0].changed).toBe(true);
    expect(isChanged(schema.fields[4], {}, {})).toBe(false);
  });
  it('judges a change by what is sent against what a fresh form sends', () => {
    const reads = { ...schema.fields[4], default: 10000, prefill: 5000 };
    const start = { min_reads: 5000 };
    expect(isChanged(reads, { min_reads: 5000 }, start)).toBe(false);
    expect(isChanged(reads, { min_reads: 10000 }, start)).toBe(true);
    // A cleared prefill runs with the engine's default: a change.
    expect(isChanged(reads, {}, start)).toBe(true);
    // Without a prefill, the default typed in is no change.
    expect(isChanged({ ...reads, prefill: undefined }, { min_reads: 10000 }, {})).toBe(false);
  });
  it('keeps outdir in its section for Snakemake', () => {
    const l = layout(schema, 'snakemake', {}, false);
    expect(l.outdir).toBeNull();
    expect(l.cards[0].fields.map((f) => f.name)).toEqual(['input', 'outdir']);
  });
  it('gives a form without sections Inputs and Other options', () => {
    const old: FormSchema = {
      warnings: [],
      fields: [field('reads', { required: true }), field('threads')],
    };
    expect(sectionsOf(old).map((s) => [s.title, !!s.advanced])).toEqual([
      ['Inputs', false],
      ['Other options', true],
    ]);
    const l = layout(old, 'snakemake', {}, false);
    expect(l.cards[0].fields.map((f) => f.name)).toEqual(['reads']);
    expect(l.advanced[0].fields.map((f) => f.name)).toEqual(['threads']);
  });
  it('shows a field naming no declared section under Other options', () => {
    const stray: FormSchema = {
      warnings: [],
      sections: [{ id: 'main', title: 'Main' }],
      fields: [
        field('reads', { section: 'main' }),
        field('lost', { section: 'gone' }),
        field('bare'),
      ],
    };
    const l = layout(stray, 'snakemake', {}, false);
    expect(l.cards.map((c) => c.section.id)).toEqual(['main']);
    expect(l.advanced.map((c) => [c.section.title, c.fields.map((f) => f.name)])).toEqual([
      ['Other options', ['lost', 'bare']],
    ]);
  });
});

describe('params', () => {
  it('reads number text as the server does, keeping what a number cannot hold exactly', () => {
    const numbers: FormSchema = {
      warnings: [],
      fields: ['a', 'b', 'c', 'd', 'e', 'f'].map((n) => field(n, { widget: 'number' })),
    };
    expect(
      paramsFrom(numbers, {
        a: '25',
        b: ' 0.5 ',
        c: '0x10',
        d: '1e400',
        e: '12345678901234567890',
        f: '1e5',
      }),
    ).toEqual({ a: 25, b: 0.5, c: '0x10', d: '1e400', e: '12345678901234567890', f: 100000 });
  });
  it('drops blanks and hidden fields, trims text and reads numbers', () => {
    expect(
      paramsFrom(schema, {
        input: ' s3://b/sheet.csv ',
        min_reads: '25',
        umi_pattern: 'NNNN',
        email: '',
      }),
    ).toEqual({ input: 's3://b/sheet.csv', min_reads: 25 });
  });
  it('validates like the server, with Nextflow outdir defaulted', () => {
    expect(validateForm(schema, 'nextflow', {})).toEqual([{ field: 'input', message: 'required' }]);
    expect(validateForm(schema, 'snakemake', { input: 's3://b/x' })).toEqual([
      { field: 'outdir', message: 'required' },
    ]);
    expect(
      validateForm(schema, 'nextflow', { input: 's3://b/x', with_umi: true, min_reads: '0.5' }),
    ).toEqual([
      { field: 'umi_pattern', message: 'required' },
      { field: 'min_reads', message: 'must be a whole number' },
    ]);
  });
});

describe('run names', () => {
  const now = new Date('2026-09-24T12:00:00Z');
  it('fills the template, or names the run after the workflow and date', () => {
    expect(runNameFrom('rnaseq · {date}', 'nf-core/rnaseq', now)).toBe('rnaseq · 2026-09-24');
    expect(runNameFrom(null, 'nf-core/rnaseq', now)).toBe('rnaseq · 2026-09-24');
  });
});

describe('params YAML', () => {
  it('round-trips through the file the CLI accepts', () => {
    const text = paramsYaml('abc:def', { input: 's3://b/x', min_reads: 25, with_umi: true });
    expect(text.startsWith('# biffix runs submit abc:def --params params.yaml\n')).toBe(true);
    const back = parseParamsYaml(text, schema);
    expect(back.values).toEqual({ input: 's3://b/x', min_reads: 25, with_umi: true });
    expect(back.unknown).toEqual([]);
  });
  it('heads the file with the whole command, and names sheets it cannot carry', () => {
    const text = paramsYaml(
      'abc:def',
      { a: 1 },
      { reference: 'grch38', retention: 'delete_on_success', rowSheets: ['input'] },
    );
    expect(text.split('\n').slice(0, 2)).toEqual([
      '# biffix runs submit abc:def --params params.yaml --reference grch38 --retention delete-on-success',
      '# Not included: the samplesheet `input` built in the form; give its file as `input`.',
    ]);
    // A secret's value is never written to the file.
    expect(paramsYaml('abc:def', { a: 1, token: 's3cret' }, { secrets: ['token'] })).toBe(
      '# biffix runs submit abc:def --params params.yaml\n' +
        '# Not included: the secret `token`; add --param token=… to the command.\n' +
        'a: 1\n',
    );
    // The run's name, experiment and images too: what the launch would send.
    expect(
      paramsCommand('abc:def', {
        name: "Tumor's run",
        experiment: 'exp_1',
        images: ['ubuntu:24.04@sha256:aa'],
      }),
    ).toBe(
      "biffix runs submit abc:def --params params.yaml --name 'Tumor'\\''s run' --experiment exp_1 --image ubuntu:24.04@sha256:aa",
    );
  });
  it('names keys that are not params, and rejects what is not a mapping', () => {
    expect(parseParamsYaml('input: x\nbogus: 1\n', schema).unknown).toEqual(['bogus']);
    expect(() => parseParamsYaml('- a\n- b\n', schema)).toThrow(/mapping/);
    expect(() => parseParamsYaml('a: [', schema)).toThrow(/Not valid YAML/);
    expect(parseParamsYaml('', schema).values).toEqual({});
  });
});

describe('prefill', () => {
  const secretSchema: FormSchema = {
    ...schema,
    fields: [...schema.fields, field('token', { is_secret: true })],
  };
  const run = {
    run_id: 'run-1',
    engine: 'nextflow',
    output_prefix: 's3://b/biffix/runs/run-1/',
    run_name: 'batch-12',
    params: {
      input: 's3://b/x',
      outdir: 's3://b/biffix/runs/run-1/results',
      token: '[REDACTED]',
    },
  };
  it('re-runs with the recorded params, minus secrets and the defaulted outdir', () => {
    const p = prefillFromRun(run, secretSchema, false);
    expect(p.values).toEqual({ input: 's3://b/x' });
    expect(p.secrets).toEqual(['token']);
    expect(p.runName).toBe('batch-12 (re-run)');
  });
  it('leaves the re-run or resume of an unnamed run unnamed, never named by its id', () => {
    const unnamed = { ...run, run_name: null };
    expect(prefillFromRun(unnamed, secretSchema, false).runName).toBe('');
    expect(prefillFromRun({ ...run, run_name: '  ' }, secretSchema, true).runName).toBe('');
  });
  it('tries a run again in the experiment it is in', () => {
    expect(prefillFromRun(run, secretSchema, false).experiment).toBeNull();
    expect(prefillFromRun({ ...run, experiment_id: 'exp_1' }, secretSchema, true).experiment).toBe(
      'exp_1',
    );
  });
  it('resumes keeping the outdir the runs share', () => {
    const p = prefillFromRun(run, secretSchema, true);
    expect(p.values.outdir).toBe('s3://b/biffix/runs/run-1/results');
    expect(p.runName).toBe('batch-12 (resumed)');
  });
  it('drops the defaulted outdir of a prefix given without a trailing slash', () => {
    expect(defaultOutdir('s3://b/proj/x')).toBe('s3://b/proj/x/results');
    const p = prefillFromRun(
      {
        ...run,
        output_prefix: 's3://b/proj/x',
        params: { input: 's3://b/x', outdir: 's3://b/proj/x/results' },
      },
      secretSchema,
      false,
    );
    expect(p.values).toEqual({ input: 's3://b/x' });
  });
  it('keeps an outdir the user chose, and any outdir of a Snakemake run', () => {
    const custom = { ...run, params: { input: 's3://b/x', outdir: 's3://b/mine/' } };
    expect(prefillFromRun(custom, secretSchema, false).values.outdir).toBe('s3://b/mine/');
    const snakemake = { ...run, engine: 'snakemake' };
    expect(prefillFromRun(snakemake, secretSchema, false).values.outdir).toBe(
      's3://b/biffix/runs/run-1/results',
    );
  });
  it('asks again for a secret field, and for any value recorded as the placeholder', () => {
    const p = prefillFromRun(
      { run_id: 'run-1', params: { token: 'not-a-placeholder', old_key: '[REDACTED]', a: 1 } },
      secretSchema,
      false,
    );
    expect(p.secrets.sort()).toEqual(['old_key', 'token']);
    expect(p.values).toEqual({ a: 1 });
  });
  it('turns a sheet biffix wrote from rows back into rows, and keeps the retention choice', () => {
    const rows = [{ sample: 'P01', fastq_1: 's3://b/P01_R1.fq.gz' }];
    const p = prefillFromRun(
      {
        run_id: 'run-1',
        params: { input: 's3://b/biffix/runs/run-1/inputs/input.csv' },
        input_tables: { input: { uri: 's3://b/biffix/runs/run-1/inputs/input.csv', rows } },
        retention: 'keep',
      },
      schema,
      false,
    );
    expect(p.tables).toEqual({ input: rows });
    expect(p.values.input).toBeUndefined();
    expect(p.retention).toBe('keep');
  });
  it("resumes and re-runs with the prior run's submit-time images, as spelled at the digest ran", () => {
    const withImages = {
      ...run,
      submit_images: [
        { declared_ref: 'ubuntu:24.04', source_ref: 'docker.io/library/ubuntu:24.04@sha256:a' },
        { declared_ref: 'ghcr.io/x/y:dev@sha256:b', source_ref: 'ghcr.io/x/y:dev@sha256:b' },
      ],
    };
    const expected = ['ubuntu:24.04@sha256:a', 'ghcr.io/x/y:dev@sha256:b'];
    expect(prefillFromRun(withImages, secretSchema, true).images).toEqual(expected);
    expect(prefillFromRun(withImages, secretSchema, false).images).toEqual(expected);
    expect(prefillFromRun(run, secretSchema, true).images).toEqual([]);
    expect(prefillFromRun({ ...run, submit_images: null }, secretSchema, false).images).toEqual([]);
  });

  it('gives an image again as its spelling at the digest it ran', () => {
    expect(
      pinnedSpelling({ declared_ref: 'quay.io/b/z', source_ref: 'quay.io/b/z:latest@sha256:f' }),
    ).toBe('quay.io/b/z@sha256:f');
    expect(
      pinnedSpelling({ declared_ref: 'x@sha256:e', source_ref: 'docker.io/library/x@sha256:e' }),
    ).toBe('x@sha256:e');
    // No digest to pin to (never so for a pinned image): the source as recorded.
    expect(pinnedSpelling({ declared_ref: 'x:1', source_ref: 'docker.io/library/x:1' })).toBe(
      'docker.io/library/x:1',
    );
  });
});

describe('launch', () => {
  const inputs: LaunchRequestInputs = {
    workflowId: 'abc:def:0',
    engine: 'nextflow',
    schema,
    parts: { params: { min_reads: 5 }, rowFields: [field('input'), field('other')] },
    runName: '  batch-12 ',
    tables: { input: [{ sample: 'P01' }], other: [] },
    reference: NO_CHOICE,
    from: null,
    retention: 'default',
    experiment: null,
  };
  const request = (over: Partial<LaunchRequestInputs> = {}) =>
    launchRequest({ ...inputs, ...over });
  const resume = { runId: 'run-1', resume: true, images: [] };
  it('builds the request, leaving out empty sheets and default choices', () => {
    expect(request()).toEqual({
      workflow_version_id: 'abc:def:0',
      params: { min_reads: 5 },
      run_name: 'batch-12',
      tables: { input: [{ sample: 'P01' }] },
    });
    // Only the row-built sheets of shown fields are sent.
    expect(request({ parts: { ...inputs.parts, rowFields: [] } })).not.toHaveProperty('tables');
  });
  it('sends the bundle, the run it resumes, and a Nextflow retention choice', () => {
    const bundled: FormSchema = {
      ...schema,
      references: { fill: {}, required: [], label: 'Reference genome', custom: true },
    };
    expect(
      request({
        schema: bundled,
        runName: ' ',
        tables: {},
        reference: { bundle: 'grch38', custom: false },
        from: resume,
        retention: 'keep',
      }),
    ).toEqual({
      workflow_version_id: 'abc:def:0',
      params: { min_reads: 5 },
      run_name: undefined,
      reference: 'grch38',
      resume_from: 'run-1',
      retention: 'keep',
    });
    // A custom reference, or a version without bundles, sends no bundle.
    const custom = { bundle: 'grch38', custom: true };
    expect(request({ schema: bundled, reference: custom })).not.toHaveProperty('reference');
    expect(request({ reference: { bundle: 'grch38', custom: false } })).not.toHaveProperty(
      'reference',
    );
    expect(request({ engine: 'snakemake', retention: 'keep' }).retention).toBe(undefined);
  });
  it('names the run a re-run tries again, and a resumed run only as resume_from', () => {
    expect(request({ from: { ...resume, resume: false } }).retry_of).toBe('run-1');
    const resumed = request({ from: resume });
    expect(resumed.resume_from).toBe('run-1');
    expect(resumed).not.toHaveProperty('retry_of');
    expect(request()).not.toHaveProperty('retry_of');
  });
  it('launches into an experiment only when one is chosen', () => {
    expect(request({ experiment: 'exp_1' }).experiment).toBe('exp_1');
    expect(request({ experiment: '' })).not.toHaveProperty('experiment');
  });
  it('sends the prior run’s images only when there are some', () => {
    const images = ['ghcr.io/x/y:dev@sha256:a'];
    expect(request({ from: { ...resume, images } }).images).toEqual(images);
    expect(request({ from: resume })).not.toHaveProperty('images');
  });
  it('orders problems as the form does, a field error winning over its other notes', () => {
    const order = new Map([
      ['input', 0],
      ['token', 1],
      ['min_reads', 2],
    ]);
    const problems = launchProblems(
      {
        errors: [{ field: 'min_reads', message: 'must be a number' }],
        sheets: [{ field: 'input', message: 'row 1, sample: required' }],
        uploading: [{ field: 'min_reads', message: 'still uploading' }],
        reference: null,
        secretsMissing: ['token'],
      },
      order,
    );
    expect(problems.map((p) => [p.field, p.message])).toEqual([
      ['input', 'row 1, sample: required'],
      ['token', 'secret — enter it again'],
      ['min_reads', 'must be a number'],
    ]);
    // A secret's own error wins over "enter it again", and the reference's problem is listed.
    const withSecretError = launchProblems(
      {
        errors: [{ field: 'token', message: 'required' }],
        sheets: [],
        uploading: [],
        reference: { field: REFERENCE_FIELD, message: 'choose one' },
        secretsMissing: ['token'],
      },
      new Map([
        [REFERENCE_FIELD, 0],
        ['token', 1],
      ]),
    );
    expect(withSecretError.map((p) => [p.field, p.message])).toEqual([
      [REFERENCE_FIELD, 'choose one'],
      ['token', 'required'],
    ]);
    const first = firstProblems([
      { field: 'a', message: 'first' },
      { field: 'a', message: 'second' },
    ]);
    expect(first.get('a')?.message).toBe('first');
  });
});

describe('drafts', () => {
  it('never keep a secret', () => {
    const secretSchema: FormSchema = { ...schema, fields: [field('token', { is_secret: true })] };
    expect(draftValues(secretSchema, { token: 'hunter2', a: 1 })).toEqual({ a: 1 });
  });
});

describe('run names', () => {
  it('fill {date} with the local date', () => {
    const lateEvening = new Date(2026, 8, 27, 23, 30);
    expect(runNameFrom('{date}', 'wf', lateEvening)).toBe('2026-09-27');
  });
});

describe('reference bundles', () => {
  const refSchema: FormSchema = {
    warnings: [],
    sections: [
      { id: 'samples', title: 'Samples' },
      { id: 'reference', title: 'Reference genome' },
    ],
    fields: [
      field('input', { required: true, section: 'samples' }),
      field('fasta', { widget: 's3_file', required: true, section: 'reference' }),
      field('gtf', { widget: 's3_file', section: 'reference' }),
    ],
    references: {
      fill: { fasta: 'fasta', gtf: 'gtf' },
      required: ['fasta'],
      label: 'Reference genome',
      section: 'reference',
      custom: true,
    },
  };
  it('hides the fields the caller judged show_if to hide', () => {
    const lay = layout(refSchema, 'nextflow', {}, false, { inactive: new Set(['input']) });
    expect(lay.cards.flatMap((c) => c.fields.map((f) => f.name))).toEqual(['fasta', 'gtf']);
  });
  it('lays out the dropdown card without the params a bundle fills', () => {
    const lay = layout(refSchema, 'nextflow', {}, false, {
      omit: new Set(['fasta', 'gtf']),
      keep: 'reference',
    });
    expect(lay.cards.map((c) => [c.section.id, c.fields.length])).toEqual([
      ['samples', 1],
      ['reference', 0],
    ]);
  });
  it('re-runs with the same bundle, dropping the params it filled', () => {
    const p = prefillFromRun(
      {
        run_id: 'run-1',
        params: { input: 's3://b/in.csv', fasta: 's3://refs/hg38.fa' },
        reference_bundle: { id: 'grch38' },
      },
      refSchema,
      false,
    );
    expect(p.values).toEqual({ input: 's3://b/in.csv' });
    expect(p.reference).toEqual({ bundle: 'grch38', custom: false });
  });
  it("keeps a param the run's bundle had no key for: the user gave it", () => {
    const p = prefillFromRun(
      {
        run_id: 'run-1',
        params: { input: 's3://b/in.csv', fasta: 's3://refs/hg38.fa', gtf: 's3://mine.gtf' },
        reference_bundle: { id: 'grch38', uris: { fasta: 's3://refs/hg38.fa' } },
      },
      refSchema,
      false,
    );
    expect(p.values).toEqual({ input: 's3://b/in.csv', gtf: 's3://mine.gtf' });
    expect(p.reference).toEqual({ bundle: 'grch38', custom: false });
  });
  it('re-runs a run with no bundle as own files only when the form allows them', () => {
    const noCustom = { ...refSchema, references: { ...refSchema.references!, custom: false } };
    const p = prefillFromRun({ run_id: 'run-1', params: {} }, noCustom, false);
    expect(p.reference).toEqual({ bundle: null, custom: false });
  });
  it('re-runs a run launched with its own files as own files', () => {
    const p = prefillFromRun(
      { run_id: 'run-1', params: { fasta: 's3://mine.fa' } },
      refSchema,
      false,
    );
    expect(p.values).toEqual({ fasta: 's3://mine.fa' });
    expect(p.reference).toEqual({ bundle: null, custom: true });
  });
});

describe('examples', () => {
  it('fills params and the samplesheet', () => {
    expect(
      valuesFromExample(schema, {
        name: 'Smoke',
        params: { min_reads: 5 },
        samplesheet_uri: 's3://b/smoke.csv',
      }),
    ).toEqual({ min_reads: 5, input: 's3://b/smoke.csv' });
  });
});

describe('prefill', () => {
  it('starts a fresh form with the author prefills, not the engine defaults', () => {
    const s: FormSchema = {
      warnings: [],
      fields: [
        field('with_umi', { default: false, prefill: true }),
        field('aligner', { default: 'star' }),
        field('note', { prefill: null }),
      ],
    };
    const values = prefillValues(s);
    expect(values).toEqual({ with_umi: true });
    // Entered like a typed value, so it is submitted and the gate the engine sees agrees.
    expect(paramsFrom(s, values)).toEqual({ with_umi: true });
    // An example fills over the prefills; what it names wins.
    expect(valuesFromExample(s, { name: 'Smoke', params: { aligner: 'bwa' } })).toEqual({
      with_umi: true,
      aligner: 'bwa',
    });
    expect(valuesFromExample(s, { name: 'Off', params: { with_umi: false } })).toEqual({
      with_umi: false,
    });
  });
});

describe('example overwrite protection', () => {
  const s: FormSchema = {
    warnings: [],
    fields: [
      field('input', {
        widget: 'samplesheet',
        samplesheet: { columns: [{ name: 'sample', role: 'sample_id' }] },
      }),
      field('with_umi', { default: false, prefill: true }),
      field('aligner', { default: 'star' }),
    ],
  };
  const none = { bundle: null, custom: false };

  it('a fresh form, or one with only blank rows and cleared text, has nothing to lose', () => {
    expect(formHasInput(s, { values: prefillValues(s), tables: {}, reference: none })).toBe(false);
    expect(
      formHasInput(s, {
        values: { ...prefillValues(s), aligner: '  ' },
        tables: { input: [{ sample: ' ' }] },
        reference: { bundle: null, custom: true },
      }),
    ).toBe(false);
  });

  it('an entered value, a cleared prefill, a filled row or a chosen bundle would be lost', () => {
    const fresh = prefillValues(s);
    expect(
      formHasInput(s, { values: { ...fresh, aligner: 'bwa' }, tables: {}, reference: none }),
    ).toBe(true);
    expect(formHasInput(s, { values: {}, tables: {}, reference: none })).toBe(true);
    expect(
      formHasInput(s, { values: fresh, tables: { input: [{ sample: 'A' }] }, reference: none }),
    ).toBe(true);
    expect(
      formHasInput(s, {
        values: fresh,
        tables: {},
        reference: { bundle: 'grch38', custom: false },
      }),
    ).toBe(true);
  });

  it('a typed run name, a retention or an experiment other than a fresh form’s would be lost', () => {
    const fresh = { values: prefillValues(s), tables: {}, reference: none };
    const base = {
      runName: 'rnaseq · Oct 1',
      defaultRunName: 'rnaseq · Oct 1',
      retention: 'default',
      experiment: 'exp_1',
      defaultExperiment: 'exp_1',
    };
    expect(formHasInput(s, { ...fresh, extras: base })).toBe(false);
    expect(formHasInput(s, { ...fresh, extras: { ...base, runName: 'Liver' } })).toBe(true);
    expect(formHasInput(s, { ...fresh, extras: { ...base, retention: 'keep' } })).toBe(true);
    expect(formHasInput(s, { ...fresh, extras: { ...base, experiment: null } })).toBe(true);
  });
});

describe('problems that point at a sample', () => {
  const s: FormSchema = {
    warnings: [],
    fields: [
      field('input', {
        widget: 'samplesheet',
        required: true,
        samplesheet: {
          columns: [
            { name: 'sample', role: 'sample_id', required: true },
            { name: 'fastq_1', role: 'read1', required: true, file: true },
            { name: 'fastq_2', role: 'read2', file: true },
          ],
          paired_only: true,
        },
      }),
    ],
  };

  it('names the row by its sample, with the problem in words', () => {
    const parts = launchParts({
      schema: s,
      engine: 'nextflow',
      values: {},
      tables: {
        input: [
          { sample: 'A', fastq_1: 's3://b/a1.fq.gz', fastq_2: 's3://b/a2.fq.gz' },
          { sample: 'B', fastq_1: 's3://b/b1.fq.gz', fastq_2: '' },
          { sample: '', fastq_1: 's3://b/c1.fq.gz', fastq_2: 's3://b/c2.fq.gz' },
        ],
      },
      sheetModes: {},
      reference: { bundle: null, custom: false },
      bundle: null,
    });
    expect(parts.sheetProblems).toEqual([
      {
        field: 'input',
        message: 'read 2: this workflow needs paired-end reads',
        row: 1,
        column: 'fastq_2',
        who: 'Sample B',
      },
      { field: 'input', message: 'sample name missing', row: 2, column: 'sample', who: 'Row 3' },
    ]);
    expect(problemTitle(parts.sheetProblems[0], 'Samplesheet')).toBe(
      'Sample B · read 2: this workflow needs paired-end reads',
    );
    expect(problemTitle({ field: 'input', message: 'required' }, 'Samplesheet')).toBe(
      'Samplesheet: required',
    );
  });
});

describe('what blocks a launch', () => {
  it('orders problems as the fields are, the reference dropdown at its first filled param', () => {
    const order = problemOrder({
      ...schema,
      references: { label: 'Genome', fill: { min_reads: 'x', umi_pattern: 'y' }, required: [] },
    });
    expect(order.get('input')).toBe(0);
    expect(order.get('email')).toBe(5);
    expect(order.get(REFERENCE_FIELD)).toBe(2.5);
    // Without a reference dropdown its problem (there is none) sorts last.
    expect(problemOrder(schema).get(REFERENCE_FIELD)).toBe(Infinity);
  });

  it('notes each field with a file still uploading once, in a param or a sheet cell', () => {
    const uploading = (uri: string) => (uri.endsWith('.part') ? `${uri} still uploading` : null);
    expect(
      uploadingProblems(
        { reads: ' s3://b/a.part ', other: 's3://b/done.fq', n: 3 },
        [
          {
            name: 'input',
            rows: [
              { sample: 'A', fastq_1: 's3://b/a1.part' },
              { sample: 'B', fastq_1: 's3://b/b1.part' },
            ],
          },
        ],
        uploading,
      ),
    ).toEqual([
      { field: 'reads', message: 's3://b/a.part still uploading' },
      { field: 'input', message: 's3://b/a1.part still uploading' },
    ]);
    // A `multiple` path param's list: each of its paths.
    expect(
      uploadingProblems({ reads: ['s3://b/done.fq', 's3://b/c.part'] }, [], uploading),
    ).toEqual([{ field: 'reads', message: 's3://b/c.part still uploading' }]);
  });

  it("asks again only for a re-run's secrets this form shows and has no value for", () => {
    const fields = new Map(schema.fields.map((f) => [f.name, f]));
    expect(
      secretsToReenter(
        ['email', 'umi_pattern', 'with_umi', 'gone'],
        fields,
        new Set(['umi_pattern']),
        { with_umi: true },
      ),
    ).toEqual(['email']);
  });
});

describe('preset meaning', () => {
  it("says a preset's value and unit unless its label already does", () => {
    const f = field('window', { widget: 'number', unit: 'bp' });
    expect(presetMeaning(f, { label: 'Cas9', value: -3 })).toBe('= -3 bp');
    expect(presetMeaning(f, { label: 'Default (10,000)', value: 10000 })).toBeNull();
    expect(presetMeaning(field('x'), { label: 'Strict', value: 'high' })).toBe('= high');
    expect(presetMeaning(f, { label: 'None', value: null })).toBeNull();
    // Only a whole token states the value: never a part of another word or number.
    expect(presetMeaning(f, { label: 'Cas9', value: 9 })).toBe('= 9 bp');
    expect(presetMeaning(f, { label: 'Top 10', value: 1 })).toBe('= 1 bp');
    expect(presetMeaning(f, { label: 'Fast (15 min)', value: 5 })).toBe('= 5 bp');
    expect(presetMeaning(f, { label: 'Fast (1.5)', value: 5 })).toBe('= 5 bp');
    expect(presetMeaning(field('x'), { label: 'Highest', value: 'high' })).toBe('= high');
    expect(presetMeaning(f, { label: 'Loose (-3)', value: -3 })).toBeNull();
    expect(presetMeaning(f, { label: 'Strict (100,000)', value: 100000 })).toBeNull();
    expect(presetMeaning(field('x'), { label: 'High', value: 'high' })).toBeNull();
  });
});

describe('a saved draft', () => {
  it('is read only when it is a draft: a mapping of values and a run name', () => {
    const saved = storageWith({});
    vi.stubGlobal('sessionStorage', saved);
    try {
      const put = (d: unknown) => saved.setItem('biffix:launch-draft:v1', JSON.stringify(d));
      put({ values: { a: 1 }, runName: '' });
      expect(loadDraft('v1')?.values).toEqual({ a: 1 });
      for (const values of [null, [1], 'x']) {
        put({ values, runName: '' });
        expect(loadDraft('v1'), JSON.stringify(values)).toBeNull();
      }
      put(null);
      expect(loadDraft('v1')).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
