// What is entered in the launch form, as plain data: the form a Re-run pre-filled, the snapshot an
// example's *Undo* puts back, and how a restored draft and an imported params file land in it.
// Kept apart from `LaunchForm`, which holds this state reactively, so it is unit-tested.

import type { FormSchema } from '#lib/workflows/schema.js';
import { formHasInput, parseParamsYaml, type Draft, type FormValues } from '../form';
import { choiceFor, NO_CHOICE, type RefChoice } from '../referenceChoice';
import type { Retention } from '../retention';
import type { SheetRow } from '../samplesheet/sheet';

export type SheetModes = Record<string, 'rows' | 'path'>;

/** Pre-filled from an earlier run: its id, whether this launch resumes it, and what it carried. */
export interface FromRun {
  runId: string;
  resume: boolean;
  secrets: string[];
  images: string[];
}

/** The form as it was before an example replaced it, for *Undo*. */
export interface FormSnapshot {
  values: FormValues;
  tables: Record<string, SheetRow[]>;
  sheetModes: SheetModes;
  refChoice: RefChoice;
  retention: Retention;
  experiment: string | null;
  runName: string;
  from: FromRun | null;
  notice: string | null;
  touched: Record<string, boolean>;
}

/** What an edit since an example went in changes: the entries *Undo* would lose. */
export function fingerprint(s: FormSnapshot): string {
  return JSON.stringify({
    values: s.values,
    tables: s.tables,
    refChoice: s.refChoice,
    retention: s.retention,
    experiment: s.experiment,
    runName: s.runName,
  });
}

/** The banner after *Undo*: the one before the example, but a draft's is not said again. */
export function undoNotice(before: string | null): string | null {
  return before === 'draft' ? null : before;
}

/** What a draft restores, after a fresh form's run name and experiment. */
export interface DraftEntry {
  values: FormValues;
  tables: Record<string, SheetRow[]>;
  sheetModes: SheetModes;
  refChoice: RefChoice;
  retention: Retention;
  runName: string;
  experiment: string | null;
}

/**
 * The draft to restore, or `null` when it holds nothing the user entered (a value, a row, a
 * reference, a name, a retention or an experiment), whether or not the author declared prefills.
 * `runName` is the fresh form's (kept when the draft's is empty), and `experimentParam` the one
 * `?experiment=` names, which wins over the draft's.
 */
export function draftEntry(
  schema: FormSchema,
  draft: Draft | null,
  fresh: { runName: string; defaultRunName: string; experimentParam: string | null },
): DraftEntry | null {
  if (
    !draft ||
    !formHasInput(schema, {
      values: draft.values,
      tables: draft.tables ?? {},
      reference: draft.reference ?? NO_CHOICE,
      extras: {
        runName: draft.runName,
        defaultRunName: fresh.defaultRunName,
        retention: draft.retention ?? 'default',
        experiment: draft.experiment ?? null,
        defaultExperiment: fresh.experimentParam,
      },
    })
  )
    return null;
  return {
    values: draft.values,
    tables: draft.tables ?? {},
    sheetModes: draft.sheetModes ?? {},
    refChoice: draft.reference ?? NO_CHOICE,
    retention: draft.retention ?? 'default',
    runName: draft.runName || fresh.runName,
    experiment: fresh.experimentParam ?? draft.experiment ?? null,
  };
}

/** What a params import adds to the form, and the banner that says so. */
export interface ImportEntry {
  /** The values imported (merged over the form's). */
  values: FormValues;
  /** The reference choice those values imply. */
  refChoice: RefChoice;
  notice: string;
}

/**
 * Parse an imported params file into what it adds to the form. A key that is no parameter of this
 * version is kept only with *Show all parameters* on. Throws on a file that is not a params mapping.
 */
export function importEntry(
  schema: FormSchema,
  text: string,
  current: { showAll: boolean; refChoice: RefChoice },
): ImportEntry {
  const imported = parseParamsYaml(text, schema);
  const values = Object.fromEntries(
    Object.entries(imported.values).filter(
      ([k]) => current.showAll || !imported.unknown.includes(k),
    ),
  );
  return {
    values,
    refChoice: choiceFor(schema, null, values, current.refChoice),
    notice: imported.unknown.length
      ? `imported-unknown:${imported.unknown.join(', ')}`
      : 'imported',
  };
}
