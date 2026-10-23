// *Try again* on the Lab analysis page: which way a failed or cancelled analysis is
// tried again, what the button says, and the `POST /runs` it sends. The request is the launch
// form's own for `?from=` (the same pre-fill, the same pruning, the same `launchRequest`), so a
// Try again and a Re-run through the form launch the same thing. Pure, so it is unit-tested.

import type { ReferenceBundle, RunSubmitRequest } from '$lib/api';
import { parseSchema } from '$lib/workflows/schema';
import { workflowTabHref } from '$lib/workflows/workflowNav';
import {
  launchParts,
  launchRequest,
  pathSheetModes,
  prefillFromRun,
  type LaunchTarget,
  type RunFacts,
} from '$lib/launch/form';
import { chosenBundle, referenceProblem } from '$lib/launch/referenceChoice';

/**
 * `resume` picks up where the run stopped; `rerun` starts again with the same inputs; the two
 * `fix_*` modes open the launch form first, at the samples or at the options, because retrying
 * unchanged would fail the same way.
 */
export type TryAgainMode = 'resume' | 'rerun' | 'fix_samples' | 'fix_options';

const MODES: ReadonlySet<string> = new Set(['resume', 'rerun', 'fix_samples', 'fix_options']);

/**
 * One failure's mode, as the server sent it (it already sends `rerun` for a run it would not
 * resume); an unknown or absent one is a re-run.
 */
function modeOf(item: { try_again?: string | null }): TryAgainMode {
  const asked = item.try_again ?? '';
  return (MODES.has(asked) ? asked : 'rerun') as TryAgainMode;
}

/**
 * The mode for a run with these failures. A fix wins over trying unchanged, since a failure that
 * needs fixing fails the same way again: any `fix_samples`, else any `fix_options`, else the first
 * failure's mode. No failures (a cancelled run) is a re-run.
 */
export function tryAgainModeOf(
  items: readonly { try_again?: string | null }[] | null | undefined,
): TryAgainMode {
  const modes = (items ?? []).map(modeOf);
  if (modes.includes('fix_samples')) return 'fix_samples';
  if (modes.includes('fix_options')) return 'fix_options';
  return modes[0] ?? 'rerun';
}

/** Whether `mode` opens the launch form to change something first. */
export const isFixMode = (mode: TryAgainMode): boolean =>
  mode === 'fix_samples' || mode === 'fix_options';

/** The button's words. */
export function tryAgainLabel(mode: TryAgainMode): string {
  return isFixMode(mode) ? 'Fix and try again' : 'Try again';
}

/** What the button will do, in a line (under the button, and in its confirm dialog). */
export function tryAgainHint(mode: TryAgainMode): string {
  switch (mode) {
    case 'resume':
      return 'Picks up where it left off — finished samples are kept';
    case 'rerun':
      return 'Starts the analysis again with the same samples and settings';
    case 'fix_samples':
      return 'Opens the launch form with these samples, so you can fix them first';
    case 'fix_options':
      return 'Opens the launch form at the options, so you can change them first';
  }
}

/**
 * The launch form pre-filled from `runId`: at `step` of the wizard, resuming when `resume`. The
 * form is the fallback for every mode (a secret to type again, a refused launch).
 */
export function launchFormHref(
  versionId: string,
  runId: string,
  { resume = false, step = null }: { resume?: boolean; step?: 2 | 3 | null } = {},
): string {
  let href = `${workflowTabHref(versionId, 'run')}?from=${encodeURIComponent(runId)}`;
  if (resume) href += '&resume=1';
  if (step) href += `&step=${step}`;
  return href;
}

/**
 * Where a fix mode goes: the wizard at Add samples (2), naming the failing `sample` so its row is
 * highlighted, or at Reference & options (3).
 */
export function fixHref(
  mode: TryAgainMode,
  versionId: string,
  runId: string,
  sample: string | null = null,
): string {
  if (mode !== 'fix_samples') return launchFormHref(versionId, runId, { step: 3 });
  const href = launchFormHref(versionId, runId, { step: 2 });
  return sample ? `${href}&sample=${encodeURIComponent(sample)}` : href;
}

/** The sample a fix-samples Try again highlights: the first failure that needs fixing and names
 * one. */
export function sampleToFix(
  items: readonly { try_again?: string | null; sample?: string | null }[] | null | undefined,
): string | null {
  return (items ?? []).find((i) => i.try_again === 'fix_samples' && i.sample)?.sample ?? null;
}

/**
 * The `POST /runs` that tries `run` again, exactly as the launch form would submit it once
 * pre-filled from the run (`?from=`, plus `&resume=1` when `resume`) and launched untouched (the
 * same pre-fill and the same [`launchParts`]); `null` whenever that form would not launch
 * untouched, so the caller opens it instead: a secret param to type again (its recorded value is
 * redacted), a reference to choose (or a bundle no longer offered), or a param or samplesheet row
 * it reports.
 *
 * `offered` is the bundles the form's reference dropdown offers (`null` when not loaded): with it,
 * the chosen bundle fills exactly its own params, as in the form once its bundles load; without it,
 * every param a bundle may fill counts as filled (the form's own view while it loads).
 *
 * `openExperiments` is the ids of the experiments open in the version's project (`null` when not
 * known). A run in an experiment that is not among them (archived since, or not loaded) is `null`
 * too: the form drops that experiment and says so, where the server would refuse the launch.
 */
export function tryAgainRequest(
  run: RunFacts,
  version: LaunchTarget,
  resume: boolean,
  offered: ReferenceBundle[] | null = null,
  openExperiments: readonly string[] | null = null,
): RunSubmitRequest | null {
  const schema = parseSchema(version.form_schema);
  const prefill = prefillFromRun(run, schema, resume);
  if (prefill.secrets.length > 0) return null;
  if (prefill.experiment && !openExperiments?.includes(prefill.experiment)) return null;
  if (referenceProblem(schema, prefill.reference, offered) !== null) return null;
  const parts = launchParts({
    schema,
    engine: version.engine,
    values: prefill.values,
    tables: prefill.tables,
    // The form shows a sheet given as a path in path mode; one biffix wrote comes back as rows.
    sheetModes: pathSheetModes(schema, Object.keys(prefill.values)),
    reference: prefill.reference,
    bundle: chosenBundle(prefill.reference, offered),
  });
  if (parts.errors.length > 0 || parts.sheetProblems.length > 0) return null;
  return launchRequest({
    workflowId: version.id,
    engine: version.engine,
    schema,
    parts,
    runName: prefill.runName,
    tables: prefill.tables,
    reference: prefill.reference,
    from: { runId: run.run_id, resume, images: prefill.images },
    retention: prefill.retention,
    experiment: prefill.experiment,
  });
}
