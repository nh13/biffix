import { describe, it, expect } from 'vitest';
import { parse as parseYaml } from 'yaml';
import {
  parseSchema,
  humanizeWidget,
  groupFields,
  summarize,
  scaffoldYaml,
  type FormField,
  type WidgetHint,
} from './schema';

const field = (over: Partial<FormField>): FormField => ({
  name: 'x',
  widget: 'text',
  required: false,
  is_output: false,
  is_secret: false,
  is_path_input: false,
  ...over,
});

describe('parseSchema', () => {
  it('defaults missing fields/warnings to empty arrays', () => {
    expect(parseSchema(null)).toMatchObject({
      fields: [],
      warnings: [],
      sections: [],
      examples: [],
    });
    expect(
      parseSchema({ fields: [], sections: [{ id: 's', title: 'S' }], run_name: 'x · {date}' }),
    ).toMatchObject({ sections: [{ id: 's', title: 'S' }], run_name: 'x · {date}' });
    expect(parseSchema({ fields: [field({})] }).fields).toHaveLength(1);
  });
});

describe('humanizeWidget', () => {
  it('maps known widgets and falls back to the raw value', () => {
    expect(humanizeWidget(field({ widget: 's3_file' }))).toBe('S3 file');
    expect(humanizeWidget(field({ widget: 'reference_bundle' }))).toBe('Reference bundle');
    expect(humanizeWidget(field({ widget: 'enum' }))).toBe('Choice');
    // An out-of-enum widget can still arrive in the opaque runtime JSON, so the label falls back to
    // the raw value; the cast reflects that this value is deliberately outside the typed union.
    expect(humanizeWidget(field({ widget: 'whatever' as WidgetHint }))).toBe('whatever');
  });
});

describe('groupFields', () => {
  it('splits into required / optional / outputs, outputs winning', () => {
    const g = groupFields({
      warnings: [],
      fields: [
        field({ name: 'a', required: true }),
        field({ name: 'b', required: false }),
        field({ name: 'o', required: true, is_output: true }),
      ],
    });
    expect(g.requiredInputs.map((f) => f.name)).toEqual(['a']);
    expect(g.optionalInputs.map((f) => f.name)).toEqual(['b']);
    expect(g.outputs.map((f) => f.name)).toEqual(['o']);
  });

  it('counts a required field with a default as optional, as the validator does', () => {
    const s = {
      warnings: [],
      fields: [field({ name: 'genome', required: true, default: 'GRCh38' })],
    };
    expect(groupFields(s).optionalInputs.map((f) => f.name)).toEqual(['genome']);
    expect(summarize(s)).toBe('1 input (0 required)');
    expect(scaffoldYaml(s)).toContain('# genome: null  # Text · default: "GRCh38"');
  });
});

describe('summarize', () => {
  it('reports the empty case', () => {
    expect(summarize({ fields: [], warnings: [] })).toBe('No parameters declared');
  });
  it('uses the singular wording for exactly one input', () => {
    const s = summarize({ warnings: [], fields: [field({ name: 'r1', required: true })] });
    expect(s).toBe('1 input (1 required)');
  });
  it('counts inputs, outputs, and special needs', () => {
    const s = summarize({
      warnings: [],
      fields: [
        field({ name: 'r1', required: true, widget: 's3_file' }),
        field({ name: 'ss', required: true, widget: 'samplesheet' }),
        field({ name: 'ref', required: false, widget: 'reference_bundle' }),
        field({ name: 'out', is_output: true }),
      ],
    });
    expect(s).toBe('3 inputs (2 required) · 1 output · needs a samplesheet and a reference bundle');
  });
});

describe('scaffoldYaml', () => {
  it('emits required as null and optional as commented', () => {
    const y = scaffoldYaml({
      warnings: [],
      fields: [
        field({ name: 'read1', required: true, widget: 's3_file', description: 'FASTQ' }),
        field({ name: 'threads', required: false, widget: 'number' }),
        field({ name: 'token', required: true, is_secret: true }),
        field({ name: 'out', is_output: true, widget: 's3_dir' }),
      ],
    });
    expect(y).toContain('read1: null  # REQUIRED · S3 file · FASTQ');
    expect(y).toContain('# threads: null  # Number');
    // Assert the whole secret line, not just the marker, so the annotation must be attached to the
    // `token` field (a stray marker on the wrong field would still contain the bare substring).
    expect(y).toContain('token: null  # REQUIRED · Text · SECRET, do not commit');
    expect(y).toContain('# out  # S3 directory');
  });
  it('keeps a multi-line description or choice inside its comment', () => {
    // nf-schema help_text (the description fallback) is often multi-paragraph; a line break must not
    // end the comment and turn the rest of the text into live YAML keys.
    const y = scaffoldYaml({
      warnings: [],
      fields: [
        field({
          name: 'input',
          required: true,
          widget: 's3_file',
          description: 'Samplesheet.\n\nIt has to be a CSV: sample,fastq',
        }),
        field({ name: 'genome', description: 'iGenomes key.\r\nkey: value' }),
        field({ name: 'mode', widget: 'enum', enum_values: ['a\nb: c', 'd'] }),
        field({ name: 'out', is_output: true, description: 'Results.\nx: y' }),
      ],
    });
    expect(y).toContain(
      'input: null  # REQUIRED · S3 file · Samplesheet. It has to be a CSV: sample,fastq',
    );
    for (const line of y.split('\n')) {
      if (line.trim() === '') continue;
      expect(line.startsWith('#') || line.startsWith('input: null  #')).toBe(true);
    }
    // Only the declared required field is a live key once parsed.
    expect(parseYaml(y)).toEqual({ input: null });
  });
  it('handles the empty schema', () => {
    expect(scaffoldYaml({ fields: [], warnings: [] })).toContain('declares no parameters');
  });
});
