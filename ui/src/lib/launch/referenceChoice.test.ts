import { describe, expect, it } from 'vitest';
import type { ReferenceBundle } from '#lib/api/index.js';
import type { FormField, FormSchema } from '#lib/workflows/schema.js';
import { validateForm } from './form';
import {
  bundleParams,
  choiceFor,
  chosenBundle,
  fillsOf,
  NO_CHOICE,
  REFERENCE_FIELD,
  referenceErrors,
  referenceProblem,
  valuesForSubmit,
  valuesForValidation,
} from './referenceChoice';

// The fill map is param → bundle key, and deliberately not the identity, so a mix-up between the
// params (what the form hides, strips and validates) and the keys (what a bundle holds) shows.
const schema: FormSchema = {
  fields: [],
  warnings: [],
  references: {
    kind: 'assembly',
    fill: { genome_fasta: 'fasta', annotation: 'gtf', aligner_index: 'star_index' },
    required: ['fasta', 'gtf'],
    label: 'Reference genome',
    custom: true,
  },
};

const bundle = (id: string, kind: string, keys: string[]): ReferenceBundle => ({
  id,
  kind,
  display_name: id,
  uris: Object.fromEntries(keys.map((k) => [k, `s3://refs/${id}/${k}`])),
  created_at_micros: 0,
  updated_at_micros: 0,
});

const grch38 = bundle('grch38', 'assembly', ['fasta', 'gtf', 'star_index']);
const noIndex = bundle('no-index', 'assembly', ['fasta', 'gtf']);
const pick = (b: ReferenceBundle) => ({ bundle: b.id, custom: false });

describe('reference dropdown', () => {
  it('says what a bundle fills and which optional keys it lacks, by bundle key', () => {
    const { fills, lacks } = fillsOf(noIndex, schema.references!);
    expect(fills.map((f) => f.key)).toEqual(['fasta', 'gtf']);
    expect(fills[0].uri).toBe('s3://refs/no-index/fasta');
    expect(lacks).toEqual(['star_index']);
  });

  it('finds the chosen bundle once it is offered', () => {
    expect(chosenBundle(pick(grch38), [noIndex, grch38])).toBe(grch38);
    expect(chosenBundle(pick(grch38), null)).toBeNull();
    expect(chosenBundle(pick(grch38), [noIndex])).toBeNull();
    expect(chosenBundle(NO_CHOICE, [grch38])).toBeNull();
  });

  it('hides and fills the params, by param name, while a bundle provides them', () => {
    const choice = pick(grch38);
    // Before the bundle loads, every param a bundle may fill.
    expect([...bundleParams(schema, choice)]).toEqual([
      'genome_fasta',
      'annotation',
      'aligner_index',
    ]);
    expect([...bundleParams(schema, choice, grch38)]).toEqual([
      'genome_fasta',
      'annotation',
      'aligner_index',
    ]);
    expect(bundleParams(schema, { bundle: null, custom: true }).size).toBe(0);
    const values = { genome_fasta: 's3://mine.fa', reads: 's3://r.fq' };
    expect(valuesForSubmit(schema, values, choice, grch38)).toEqual({ reads: 's3://r.fq' });
    // Validation sees the bundle's own URIs, as the server fills them before it validates.
    expect(valuesForValidation(schema, values, choice, grch38)).toEqual({
      genome_fasta: 's3://refs/grch38/fasta',
      annotation: 's3://refs/grch38/gtf',
      aligner_index: 's3://refs/grch38/star_index',
      reads: 's3://r.fq',
    });
    // Before a bundle is chosen, its params still count as given (the dropdown asks instead).
    expect(valuesForValidation(schema, {}, NO_CHOICE).genome_fasta).toBeTruthy();
    // Own files: submitted and validated as entered.
    const own = { bundle: null, custom: true };
    expect(valuesForSubmit(schema, values, own)).toEqual(values);
    expect(valuesForValidation(schema, values, own)).toEqual(values);
  });

  it('leaves a param whose key the chosen bundle lacks on the form, as given', () => {
    const choice = pick(noIndex);
    expect([...bundleParams(schema, choice, noIndex)]).toEqual(['genome_fasta', 'annotation']);
    const values = { genome_fasta: 's3://mine.fa', aligner_index: 's3://my/star/' };
    expect(valuesForSubmit(schema, values, choice, noIndex)).toEqual({
      aligner_index: 's3://my/star/',
    });
    const v = valuesForValidation(schema, values, choice, noIndex);
    expect(v.aligner_index).toBe('s3://my/star/');
    expect(v.genome_fasta).toBe('s3://refs/no-index/fasta');
  });

  it("checks a filled param's rules against the bundle's URI, never a stand-in", () => {
    const field = (name: string, pattern: string): FormField => ({
      name,
      widget: 's3_file',
      required: true,
      is_path_input: true,
      is_output: false,
      is_secret: false,
      rules: [{ pattern, message: `must match ${pattern}` }],
    });
    const ruled: FormSchema = {
      ...schema,
      fields: [
        field('genome_fasta', '^\\S+/fasta$'),
        field('annotation', '\\.gtf$'),
        {
          name: 'aligner_index',
          widget: 's3_dir',
          required: true,
          is_path_input: true,
          is_output: false,
          is_secret: false,
        },
      ],
    };
    const errorsFor = (choice: typeof NO_CHOICE, b: ReferenceBundle | null) =>
      referenceErrors(
        ruled,
        choice,
        b,
        validateForm(ruled, 'nextflow', valuesForValidation(ruled, {}, choice, b)),
      );
    // None chosen, or the chosen one still loading: nothing about the params it would fill.
    expect(errorsFor(NO_CHOICE, null)).toEqual([]);
    expect(errorsFor(pick(grch38), null)).toEqual([]);
    // Loaded: the server's verdict on its URIs, reported on the dropdown (the param is hidden).
    expect(errorsFor(pick(grch38), grch38)).toEqual([
      { field: REFERENCE_FIELD, message: 'annotation must match \\.gtf$' },
    ]);
    // A lacked key's param is the user's to give, and required as usual.
    const got = errorsFor(pick(noIndex), noIndex);
    expect(got).toContainEqual({ field: 'aligner_index', message: 'required' });
    // Own files: every error on its own field.
    expect(errorsFor({ bundle: null, custom: true }, null).map((e) => e.field)).toEqual([
      'genome_fasta',
      'annotation',
      'aligner_index',
    ]);
  });

  it('asks for a bundle until one is chosen, and flags one no longer offered', () => {
    const offered = [bundle('grch38', 'assembly', ['fasta', 'gtf'])];
    expect(referenceProblem(schema, NO_CHOICE, offered)).toBe('choose one');
    expect(referenceProblem(schema, { bundle: 'grch38', custom: false }, offered)).toBeNull();
    expect(referenceProblem(schema, { bundle: 'gone', custom: false }, offered)).toMatch(
      /not available/,
    );
    expect(referenceProblem(schema, { bundle: null, custom: true }, offered)).toBeNull();
    // Still loading: no verdict on the id yet.
    expect(referenceProblem(schema, { bundle: 'gone', custom: false }, null)).toBeNull();
    expect(referenceProblem({ ...schema, references: null }, NO_CHOICE, offered)).toBeNull();
  });

  it('says the bundles failed to load, not that the chosen one is gone or none is chosen', () => {
    for (const choice of [NO_CHOICE, { bundle: 'grch38', custom: false }])
      expect(referenceProblem(schema, choice, [], 'boom')).toBe("couldn't load the references");
    // Own files need no bundle, loaded or not.
    expect(referenceProblem(schema, { bundle: null, custom: true }, [], 'boom')).toBeNull();
  });

  it('loads an example or import with its bundle, or as own files when it sets a filled param', () => {
    expect(choiceFor(schema, 'grch38', {}, NO_CHOICE)).toEqual({ bundle: 'grch38', custom: false });
    expect(choiceFor(schema, null, { genome_fasta: 's3://mine.fa' }, NO_CHOICE)).toEqual({
      bundle: null,
      custom: true,
    });
    const current = { bundle: 'grch38', custom: false };
    // A bundle key is not a param: setting `fasta` sets nothing a bundle fills.
    expect(choiceFor(schema, null, { fasta: 's3://mine.fa' }, current)).toBe(current);
    expect(choiceFor(schema, null, { reads: 'x' }, current)).toBe(current);
    // A null is unset, as validation and the server read it.
    expect(choiceFor(schema, null, { genome_fasta: null }, current)).toBe(current);
  });

  it('never counts an Object.prototype name as a key a bundle holds', () => {
    const odd = { ...schema, references: { ...schema.references!, fill: { p: 'constructor' } } };
    expect(bundleParams(odd, pick(grch38), grch38).size).toBe(0);
    expect(fillsOf(grch38, odd.references!)).toEqual({ fills: [], lacks: ['constructor'] });
  });

  it('ignores Custom when the author does not offer it', () => {
    const noCustom = { ...schema, references: { ...schema.references!, custom: false } };
    expect(bundleParams(noCustom, { bundle: null, custom: true }).size).toBe(3);
  });
});
