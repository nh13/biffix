// What a step graph's node box can hold: a box is a fixed width, so its text is cut to a
// character budget (the full id stays in the node's tooltip and accessible name). Pure, so it is
// unit-tested.

import { stepIdentifier } from '$lib/runs/analysis/stepLabel';

/** Characters of the 0.74rem name that fit a 150 px box, with a margin. */
export const LABEL_CHARS = 20;
/** Characters of the 0.62rem count line that fit the same box. */
export const COUNT_CHARS = 24;

/** `text`, cut to `max` characters with an ellipsis when it is longer. */
export function fitText(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, max - 1)}…`;
}

/** A node's name in its box: the step's own name (a Nextflow process without its path), fitted. */
export function nodeLabel(id: string, max = LABEL_CHARS): string {
  return fitText(stepIdentifier(id), max);
}
