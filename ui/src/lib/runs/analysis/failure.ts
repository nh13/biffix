// A failed analysis in plain words: each failure's one line, the likely cause
// behind it, and which failures to show. Pure, so it is unit-tested.

import type { FailureSummary } from '$lib/api';
import { stepLabel } from './stepLabel';

export type FailureItem = FailureSummary['items'][number];

/**
 * The likely cause, in the words the *Why?* panel uses, by the server's `cause`: exactly the ones
 * `FailureCause::ALL` has, which `schemas/failure-causes.json` lists (a test pins the two together,
 * both ways).
 */
const CAUSES: Record<string, string> = {
  could_not_start: 'Its software could not be downloaded, so it never started.',
  out_of_memory: 'It needed more memory than it was given, and was stopped.',
  spot_reclaimed: 'AWS took back the machine it ran on. This is not about your inputs.',
  host_lost: 'The machine it ran on stopped unexpectedly. This is not about your inputs.',
  timed_out: 'It ran past its time limit and was stopped.',
  missing_input: 'A file it needed was not found.',
  bad_input: 'One of its input files looks damaged, or is not in the format the pipeline expects.',
  reference_mismatch:
    "The reference genome does not match the samples (or the pipeline's own index).",
  no_space: 'It ran out of disk space. This is not about your inputs.',
  access_denied: 'It was not allowed to read or write one of its files.',
  missing_output: 'It finished without writing the files it should have: usually a pipeline bug.',
  step_error: 'The step stopped with an error; its last lines below usually say why.',
  engine_error: 'The pipeline stopped before any step ran: one of its settings could not be used.',
  launch_failed: 'biffix could not start the analysis.',
  unknown: 'biffix could not tell why from what it recorded.',
};

/** The causes `causeWords` words (for the test that pins them to the contract). */
export const wordedCauses = (): string[] => Object.keys(CAUSES);

/** Where a pre-launch failure stopped: preparing the pipeline's software (else launching it). */
const IMAGE_PREPARATION = 'image_preparation';

/**
 * The likely cause of `cause`, in plain words; an unknown cause reads as `unknown`. A failure
 * before launch (`launch_failed`) is worded by the `stage` it stopped at.
 */
export function causeWords(cause: string, stage?: string | null): string {
  if (cause === 'launch_failed' && stage === IMAGE_PREPARATION)
    return "biffix could not prepare the pipeline's software.";
  // Own keys only: an open-enum cause named like an Object member is still unknown.
  return Object.hasOwn(CAUSES, cause) ? CAUSES[cause] : CAUSES.unknown;
}

/** "tried 2 times", or nothing for a single try. */
export function triesText(attempts: number): string {
  return attempts > 1 ? `tried ${attempts} times` : '';
}

/**
 * A failure's one line: *Tumor_4 · STAR align · it ran out of memory*; without the sample when the
 * failed task names none. The step reads as a person would name it (the author's `labels`, else
 * a readable form of the engine's name).
 */
export function failureLine(item: FailureItem, labels?: Record<string, string> | null): string {
  const step = item.step?.trim() ? stepLabel(item.step, { labels }) : '';
  return [item.sample, step, item.message].filter((p) => p && p.trim()).join(' · ');
}

/**
 * The failures to show, grouped by cause: at most `max` of them, earliest first, each cause's
 * together (in the order its first failure came).
 */
export function groupByCause(
  items: readonly FailureItem[],
  max = 3,
): { cause: string; items: FailureItem[] }[] {
  const groups: { cause: string; items: FailureItem[] }[] = [];
  for (const item of items.slice(0, max)) {
    const group = groups.find((g) => g.cause === item.cause);
    if (group) group.items.push(item);
    else groups.push({ cause: item.cause, items: [item] });
  }
  return groups;
}
