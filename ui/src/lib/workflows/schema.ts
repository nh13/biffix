// Pure logic for the workflow parameter schema (the normalized FormSchema).
// Kept separate from rendering so the read-only Parameters view, the future launch form, and a
// later version-to-version schema diff can all share one normalized field model, keyed by name.

// The field model is generated from `schemas/formschema.schema.json` (itself emitted from the Rust
// `biffix_workflow::formschema::FormSchema`), so it can never drift from the source of truth.
// Re-export it here so call sites import the model and the pure helpers from one place.
import type {
  FormField as GeneratedFormField,
  FormSchema as GeneratedFormSchema,
  FormExample,
  FormReferences,
  FormSection,
  Preset,
  ShowIf,
  WidgetHint,
} from '#lib/generated/formschema.js';
import { plural } from '#lib/util/format.js';

/**
 * A form field. `default` and `prefill` may be any JSON value; the JSON Schema says so (they are
 * unconstrained), but the generator renders an unconstrained property as an object, so they are
 * widened here.
 */
export type FormField = Omit<GeneratedFormField, 'default' | 'prefill'> & {
  default?: unknown;
  prefill?: unknown;
};
export type FormSchema = Omit<GeneratedFormSchema, 'fields'> & { fields: FormField[] };

export type { FormExample, FormReferences, FormSection, Preset, ShowIf, WidgetHint };

/** Parse the opaque `form_schema` JSON into a normalized, always-defined FormSchema. */
export function parseSchema(raw: unknown): FormSchema {
  const s = (raw ?? {}) as Record<string, unknown>;
  const fields = Array.isArray(s.fields) ? (s.fields as FormField[]) : [];
  const warnings = Array.isArray(s.warnings) ? (s.warnings as string[]) : [];
  const list = <T>(v: unknown): T[] => (Array.isArray(v) ? (v as T[]) : []);
  const text = (v: unknown): string | null => (typeof v === 'string' ? v : null);
  return {
    fields,
    warnings,
    title: text(s.title),
    description: text(s.description),
    sections: list<FormSection>(s.sections),
    examples: list<FormExample>(s.examples),
    run_name: text(s.run_name),
    curated: s.curated === true,
    references:
      s.references && typeof s.references === 'object' && !Array.isArray(s.references)
        ? (s.references as FormReferences)
        : null,
  };
}

// Exhaustive over the generated WidgetHint: adding a widget to the Rust enum makes this a compile
// error until a label is supplied, so the label map can never silently fall behind the schema.
const WIDGET_LABELS: Record<WidgetHint, string> = {
  text: 'Text',
  number: 'Number',
  enum: 'Choice',
  s3_file: 'S3 file',
  s3_dir: 'S3 directory',
  reference_bundle: 'Reference bundle',
  samplesheet: 'Samplesheet',
};

/** Human label for a field's widget, e.g. `s3_file` → "S3 file". Falls back to the raw value. */
export function humanizeWidget(field: FormField): string {
  return WIDGET_LABELS[field.widget] ?? field.widget;
}

export interface GroupedFields {
  requiredInputs: FormField[];
  optionalInputs: FormField[];
  outputs: FormField[];
}

/**
 * Whether `f` must be given at launch: a required input with no default (the default satisfies
 * `required`, as the server's validator reads it, `validate_params`; an output is never asked for).
 */
export const isRequiredInput = (f: FormField): boolean =>
  !f.is_output && !!f.required && f.default == null;

/** Split fields into required inputs ([`isRequiredInput`]) / optional inputs / outputs. */
export function groupFields(schema: FormSchema): GroupedFields {
  const requiredInputs: FormField[] = [];
  const optionalInputs: FormField[] = [];
  const outputs: FormField[] = [];
  for (const f of schema.fields) {
    if (f.is_output) outputs.push(f);
    else if (isRequiredInput(f)) requiredInputs.push(f);
    else optionalInputs.push(f);
  }
  return { requiredInputs, optionalInputs, outputs };
}

/** A one-line, human summary of what it takes to launch: "6 inputs (4 required) · 2 outputs ·
 * …". */
export function summarize(schema: FormSchema): string {
  const g = groupFields(schema);
  const inputs = g.requiredInputs.length + g.optionalInputs.length;
  if (inputs === 0 && g.outputs.length === 0) return 'No parameters declared';
  const parts = [`${plural(inputs, 'input')} (${g.requiredInputs.length} required)`];
  if (g.outputs.length > 0) parts.push(plural(g.outputs.length, 'output'));
  const needs: string[] = [];
  if (schema.fields.some((f) => f.widget === 'samplesheet')) needs.push('a samplesheet');
  if (schema.fields.some((f) => f.widget === 'reference_bundle')) needs.push('a reference bundle');
  if (needs.length > 0) parts.push(`needs ${needs.join(' and ')}`);
  return parts.join(' · ');
}

/** Text for a one-line YAML `#` comment: any line break would end the comment and turn the rest
 * of the text into live YAML, so collapse every run of whitespace to one space. */
const commentText = (text: string): string => text.replace(/\s+/g, ' ').trim();

/**
 * Generate a YAML params scaffold the user edits and passes to
 * `biffix runs submit <version> --params <file>` (see `paramsCommand`). Required fields are present
 * but `null` (so the file is deliberately invalid until filled); optional fields are
 * present-but-commented; each line carries the widget, enum choices, and description as a one-line
 * comment. Outputs go in their own commented section. Pure function of the schema.
 */
export function scaffoldYaml(schema: FormSchema): string {
  const g = groupFields(schema);
  const lines: string[] = [];

  const annot = (f: FormField): string => {
    const bits = [humanizeWidget(f)];
    if (f.is_secret) bits.push('SECRET, do not commit');
    if (f.enum_values && f.enum_values.length) bits.push(`choices: ${f.enum_values.join(', ')}`);
    if (f.default != null) bits.push(`default: ${JSON.stringify(f.default)}`);
    if (f.description) bits.push(f.description);
    return commentText(bits.join(' · '));
  };

  if (g.requiredInputs.length) {
    lines.push(
      '# Required inputs — fill these in (left null so the file is invalid until edited).',
    );
    for (const f of g.requiredInputs) lines.push(`${f.name}: null  # REQUIRED · ${annot(f)}`);
    lines.push('');
  }
  if (g.optionalInputs.length) {
    lines.push('# Optional inputs — uncomment and set to override the default.');
    for (const f of g.optionalInputs) lines.push(`# ${f.name}: null  # ${annot(f)}`);
    lines.push('');
  }
  if (g.outputs.length) {
    lines.push('# Outputs (produced by the workflow):');
    for (const f of g.outputs) lines.push(`# ${f.name}  # ${annot(f)}`);
    lines.push('');
  }
  if (lines.length === 0) lines.push('# This workflow version declares no parameters.');
  return lines.join('\n').trimEnd() + '\n';
}
