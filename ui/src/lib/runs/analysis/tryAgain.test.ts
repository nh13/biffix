import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import type { ReferenceBundle } from '$lib/api';
import {
  launchParts,
  launchRequest,
  pathSheetModes,
  prefillFromRun,
  type LaunchTarget,
  type RunFacts,
} from '$lib/launch/form';
import { chosenBundle } from '$lib/launch/referenceChoice';
import { parseSchema, type FormField, type FormSchema } from '$lib/workflows/schema';
import {
  fixHref,
  isFixMode,
  launchFormHref,
  sampleToFix,
  tryAgainHint,
  tryAgainLabel,
  tryAgainModeOf,
  tryAgainRequest,
  type TryAgainMode,
} from './tryAgain';

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

const schema: FormSchema = {
  fields: [
    field('input', {
      widget: 'samplesheet',
      samplesheet: {
        columns: [
          { name: 'sample', role: 'sample_id', required: true },
          { name: 'fastq_1', role: 'read1', required: true, file: true },
        ],
      },
    }),
    field('outdir', { widget: 's3_dir', is_output: true }),
    field('fasta', { widget: 's3_file' }),
    field('gtf', { widget: 's3_file' }),
    field('aligner', { widget: 'enum', enum_values: ['star', 'hisat2'], default: 'star' }),
    field('with_umi', { widget: 'enum', enum_values: ['true', 'false'], default: false }),
    field('umi_pattern', { show_if: [{ field: 'with_umi', values: [true] }] }),
  ],
  warnings: [],
  references: {
    kind: 'assembly',
    fill: { fasta: 'fasta', gtf: 'gtf' },
    required: ['fasta', 'gtf'],
    custom: true,
  },
} as unknown as FormSchema;

const version = { id: 'abc:v1', name: 'rnaseq', engine: 'nextflow', form_schema: schema };

const rows = [
  { sample: 'Tumor_1', fastq_1: 's3://b/T1_R1.fq.gz' },
  { sample: 'Tumor_4', fastq_1: 's3://b/T4_R1.fq.gz' },
];
const run = {
  run_id: 'run_1',
  run_name: 'batch 11',
  engine: 'nextflow',
  output_prefix: 's3://b/biffix/runs/run_1/',
  params: {
    input: 's3://b/biffix/runs/run_1/inputs/input.csv',
    outdir: 's3://b/biffix/runs/run_1/results',
    fasta: 's3://refs/genome.fa',
    gtf: 's3://refs/genes.gtf',
    aligner: 'hisat2',
    with_umi: false,
    umi_pattern: 'NNNN',
  },
  input_tables: { input: { uri: 's3://b/…/input.csv', rows } },
  reference_bundle: { id: 'grcm39' },
  retention: null,
  submit_images: [
    { declared_ref: 'ghcr.io/lab/x:dev', source_ref: 'ghcr.io/lab/x:dev@sha256:abc' },
  ],
};

describe('tryAgainModeOf', () => {
  const of = (...modes: (string | null)[]) => tryAgainModeOf(modes.map((m) => ({ try_again: m })));

  it("follows the server's mode for one failure", () => {
    expect(of('resume')).toBe('resume');
    expect(of('fix_samples')).toBe('fix_samples');
    expect(of('fix_options')).toBe('fix_options');
    expect(of('rerun')).toBe('rerun');
  });

  it('lets a fix win over trying unchanged: samples, then options, then the first', () => {
    // An out-of-memory failure (resume) beside a damaged input (fix the samples).
    expect(of('resume', 'fix_samples')).toBe('fix_samples');
    expect(of('fix_options', 'resume', 'fix_samples')).toBe('fix_samples');
    expect(of('rerun', 'fix_options')).toBe('fix_options');
    expect(of('rerun', 'resume')).toBe('rerun');
    expect(of('resume', 'rerun')).toBe('resume');
  });

  it('re-runs for an unknown mode or no failures (a cancelled run)', () => {
    expect(of('teleport')).toBe('rerun');
    expect(of(null)).toBe('rerun');
    expect(tryAgainModeOf([])).toBe('rerun');
    expect(tryAgainModeOf(null)).toBe('rerun');
    expect(tryAgainModeOf(undefined)).toBe('rerun');
  });

  it('names the first failing sample to fix', () => {
    expect(
      sampleToFix([
        { try_again: 'resume', sample: 'Tumor_4' },
        { try_again: 'fix_samples', sample: null },
        { try_again: 'fix_samples', sample: 'Normal_2' },
      ]),
    ).toBe('Normal_2');
    expect(sampleToFix([{ try_again: 'resume', sample: 'Tumor_4' }])).toBeNull();
    expect(sampleToFix(undefined)).toBeNull();
  });

  it('labels and explains each mode', () => {
    expect(tryAgainLabel('resume')).toBe('Try again');
    expect(tryAgainLabel('rerun')).toBe('Try again');
    expect(tryAgainLabel('fix_samples')).toBe('Fix and try again');
    expect(isFixMode('fix_options')).toBe(true);
    expect(isFixMode('resume')).toBe(false);
    expect(tryAgainHint('resume')).toMatch(/finished samples are kept/);
    expect(tryAgainHint('rerun')).toMatch(/same samples and settings/);
  });
});

describe('launch form links', () => {
  it('pre-fill the form from the run, at a step, resuming when asked', () => {
    expect(launchFormHref('abc:v1', 'run 1')).toBe('/workflows/abc%3Av1/run?from=run%201');
    expect(launchFormHref('abc:v1', 'run_1', { resume: true })).toBe(
      '/workflows/abc%3Av1/run?from=run_1&resume=1',
    );
    expect(fixHref('fix_samples', 'abc:v1', 'run_1')).toBe(
      '/workflows/abc%3Av1/run?from=run_1&step=2',
    );
    expect(fixHref('fix_samples', 'abc:v1', 'run_1', 'Tumor 4')).toBe(
      '/workflows/abc%3Av1/run?from=run_1&step=2&sample=Tumor%204',
    );
    expect(fixHref('fix_options', 'abc:v1', 'run_1', 'Tumor_4')).toBe(
      '/workflows/abc%3Av1/run?from=run_1&step=3',
    );
    expect(fixHref('fix_options', 'abc:v1', 'run_1')).toBe(
      '/workflows/abc%3Av1/run?from=run_1&step=3',
    );
  });
});

describe('tryAgainRequest', () => {
  it('re-runs with the same samples, bundle, options and images, as the form submits them', () => {
    expect(tryAgainRequest(run, version, false)).toEqual({
      workflow_version_id: 'abc:v1',
      run_name: 'batch 11 (re-run)',
      // No sheet param (its rows go as a table), no bundle params, no defaulted outdir, and no
      // param a `show_if` hides.
      params: { aligner: 'hisat2', with_umi: false },
      tables: { input: rows },
      reference: 'grcm39',
      images: ['ghcr.io/lab/x:dev@sha256:abc'],
      // So the server marks run_1's notifications read (it leaves Needs attention).
      retry_of: 'run_1',
    });
  });

  it('resumes into the same run folder', () => {
    const request = tryAgainRequest(run, version, true)!;
    expect(request.resume_from).toBe('run_1');
    // A resume names the run once, in `resume_from`.
    expect(request).not.toHaveProperty('retry_of');
    expect(request.run_name).toBe('batch 11 (resumed)');
    // A resume keeps the outdir the server defaulted into the shared folder.
    expect(request.params).toMatchObject({ outdir: 's3://b/biffix/runs/run_1/results' });
  });

  it('keeps a samplesheet given as a path', () => {
    const request = tryAgainRequest({ ...run, input_tables: null }, version, false)!;
    expect(request.tables).toBeUndefined();
    expect(request.params).toMatchObject({ input: 's3://b/biffix/runs/run_1/inputs/input.csv' });
  });

  it('sends nothing when the form would ask for a reference, or report a value', () => {
    // A run of a version whose reference must come from a bundle, launched without one.
    const bundleOnly = {
      ...schema,
      references: { ...schema.references!, custom: false },
    } as FormSchema;
    expect(
      tryAgainRequest(
        { ...run, reference_bundle: null },
        { ...version, form_schema: bundleOnly },
        false,
      ),
    ).toBeNull();
    // The bundle it used is no longer offered.
    expect(tryAgainRequest(run, version, false, [])).toBeNull();
    // A value the form reports (no longer one of the choices).
    expect(
      tryAgainRequest({ ...run, params: { ...run.params, aligner: 'bowtie' } }, version, false),
    ).toBeNull();
    // A samplesheet row the form reports (no read 1).
    expect(
      tryAgainRequest(
        { ...run, input_tables: { input: { uri: 's3://b/x.csv', rows: [{ sample: 'S1' }] } } },
        version,
        false,
      ),
    ).toBeNull();
  });

  it('keeps the run in its experiment only while that experiment is open', () => {
    const inExp = { ...run, experiment_id: 'exp_a' };
    expect(tryAgainRequest(inExp, version, false, null, ['exp_b', 'exp_a'])!.experiment).toBe(
      'exp_a',
    );
    // Archived since (not among the project's open ones), or not known: the form drops it.
    expect(tryAgainRequest(inExp, version, false, null, ['exp_b'])).toBeNull();
    expect(tryAgainRequest(inExp, version, true, null, null)).toBeNull();
    // A run in no experiment needs none of them.
    expect(tryAgainRequest(run, version, false, null, null)).not.toBeNull();
  });

  it('sends nothing when a secret must be typed again', () => {
    const secret: FormSchema = {
      ...schema,
      fields: [...schema.fields, field('token', { is_secret: true })],
    };
    const withSecret = { ...run, params: { ...run.params, token: '[REDACTED]' } };
    expect(tryAgainRequest(withSecret, { ...version, form_schema: secret }, false)).toBeNull();
  });
});

// The launch form's own steps for a `?from=` pre-fill launched untouched: `prefillFromRun`, the
// sheets it shows as paths (`pathModeFor`), the chosen bundle once its bundles load, then
// `launchParts` and `launchRequest`, which `LaunchForm.launch()` sends through.
function formRequest(
  run: RunFacts,
  target: LaunchTarget,
  resume: boolean,
  offered: ReferenceBundle[] | null,
  openExperiments: readonly string[] = [],
) {
  const s = parseSchema(target.form_schema);
  const p = prefillFromRun(run, s, resume);
  const parts = launchParts({
    schema: s,
    engine: target.engine,
    values: p.values,
    tables: p.tables,
    sheetModes: pathSheetModes(s, Object.keys(p.values)),
    reference: p.reference,
    bundle: chosenBundle(p.reference, offered),
  });
  expect(parts.errors).toEqual([]);
  expect(parts.sheetProblems).toEqual([]);
  return launchRequest({
    workflowId: target.id,
    engine: target.engine,
    schema: s,
    parts,
    runName: p.runName,
    tables: p.tables,
    reference: p.reference,
    from: { runId: run.run_id, resume, images: p.images },
    retention: p.retention,
    // As the form: a pre-filled experiment its project does not offer open is dropped.
    experiment: p.experiment && openExperiments.includes(p.experiment) ? p.experiment : null,
  });
}

describe('tryAgainRequest matches the launch form', () => {
  // A form with a bundle-filled param that gates another (`star_index` shows `star_opts` only when
  // the bundle's index is the known one), a row-built samplesheet, and a second samplesheet given
  // as a path.
  const gated: FormSchema = {
    ...schema,
    fields: [
      ...schema.fields,
      field('star_index', { widget: 's3_dir' }),
      field('star_opts', {
        show_if: [{ field: 'star_index', values: ['s3://refs/star/'] }],
      }),
      field('panel', {
        widget: 'samplesheet',
        samplesheet: { columns: [{ name: 'sample', role: 'sample_id', required: true }] },
      }),
    ],
    references: {
      ...schema.references!,
      fill: { ...schema.references!.fill, star_index: 'star_index' },
    },
  } as unknown as FormSchema;
  const gatedVersion = { ...version, form_schema: gated };
  const gatedRun = {
    ...run,
    params: {
      ...run.params,
      star_index: 's3://refs/star/',
      star_opts: '--twopass',
      panel: 's3://b/panel.csv',
    },
  };
  const bundles: ReferenceBundle[] = [
    {
      id: 'grcm39',
      kind: 'assembly',
      uris: {
        fasta: 's3://refs/genome.fa',
        gtf: 's3://refs/genes.gtf',
        star_index: 's3://refs/star/',
      },
    } as unknown as ReferenceBundle,
  ];

  for (const resume of [false, true])
    for (const offered of [null, bundles])
      it(`launches what the form launches (${resume ? 'resume' : 're-run'}, bundles ${offered ? 'loaded' : 'loading'})`, () => {
        const expected = formRequest(gatedRun, gatedVersion, resume, offered);
        expect(tryAgainRequest(gatedRun, gatedVersion, resume, offered)).toEqual(expected);
        // The row-built sheet goes as rows, the path-given one as its path; no bundle param.
        expect(expected.tables).toEqual({ input: rows });
        expect(expected.params).toMatchObject({ panel: 's3://b/panel.csv' });
        expect(expected.params).not.toHaveProperty('star_index');
        expect(expected.params).not.toHaveProperty('input');
      });

  it('launches into the run’s experiment as the form does, and leaves a closed one to the form', () => {
    const inExp = { ...gatedRun, experiment_id: 'exp_a' };
    const open = formRequest(inExp, gatedVersion, false, bundles, ['exp_a']);
    expect(open.experiment).toBe('exp_a');
    expect(tryAgainRequest(inExp, gatedVersion, false, bundles, ['exp_a'])).toEqual(open);
    // Archived: the form would launch without it (and say so), so Try again opens the form
    // rather than launch a different thing untold.
    const closed = formRequest(inExp, gatedVersion, false, bundles, []);
    expect(closed).not.toHaveProperty('experiment');
    expect(tryAgainRequest(inExp, gatedVersion, false, bundles, [])).toBeNull();
  });

  it('judges a show_if on a bundle param by the loaded bundle, as the form does', () => {
    expect(tryAgainRequest(gatedRun, gatedVersion, false, bundles)!.params).toMatchObject({
      star_opts: '--twopass',
    });
    // While loading, the stand-in does not match the gate, so the form drops it too.
    expect(tryAgainRequest(gatedRun, gatedVersion, false, null)!.params).not.toHaveProperty(
      'star_opts',
    );
  });
});

describe('every try-again mode the server sends', () => {
  // Written by `emit_schemas` from `TryAgain::ALL` (crates/biffix-core).
  const contract = JSON.parse(
    readFileSync(new URL('../../../../../schemas/failure-causes.json', import.meta.url), 'utf8'),
  ) as { try_again: string[] };

  it('has a button, a hint and its own mode', () => {
    expect(contract.try_again.length).toBeGreaterThan(0);
    for (const mode of contract.try_again) {
      expect(tryAgainModeOf([{ try_again: mode }])).toBe(mode);
      expect(tryAgainHint(mode as TryAgainMode)).toBeTruthy();
      expect(tryAgainLabel(mode as TryAgainMode)).toBeTruthy();
    }
  });
});
