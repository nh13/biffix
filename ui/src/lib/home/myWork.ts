// My work's rows: Needs attention, In progress and Recent
// results, then Experiments. Each is an accordion with the server's true count, its five newest and
// a "See all" into the list filtered the same way (the Analyses list for the three analysis rows,
// the caller's Experiments for the last); whether it is open is remembered per row in this browser
// (Needs attention opens again when more need a look than when it was closed). Pure, so it is
// unit-tested.

import { readPref, writePref } from '#lib/util/storage.js';

/** The rows of analyses. */
export type WorkRow = 'attention' | 'running' | 'finished';

/** Every accordion row of My work: the analyses' rows and the caller's experiments. */
export type AccordionRow = WorkRow | 'experiments';

export interface WorkRowSpec {
  key: WorkRow;
  title: string;
  /** The Analyses list's `?filter=` for "See all". */
  filter: 'attention' | 'active' | 'succeeded';
  /** What an empty row says. */
  empty: string;
}

export const WORK_ROWS: readonly WorkRowSpec[] = [
  {
    key: 'attention',
    title: 'Needs attention',
    filter: 'attention',
    empty: 'Nothing needs a look.',
  },
  { key: 'running', title: 'In progress', filter: 'active', empty: 'Nothing running.' },
  {
    key: 'finished',
    title: 'Recent results',
    filter: 'succeeded',
    empty: 'Your finished analyses appear here.',
  },
];

/** How many analyses (or experiments) each row lists. */
export const ROW_LIMIT = 5;

/** The Experiments row's "See all": the Experiments list, the caller's alone. The page's flags are
 * `=true`, as the API's are (`GET /experiments?mine=true`). */
export const EXPERIMENTS_SEE_ALL = '/experiments?mine=true';

/** The caller's experiments, archived ones shown: where an Experiments row with only archived ones
 * points. */
export const EXPERIMENTS_ARCHIVED = '/experiments?mine=true&archived=true';

/** "See all N →"'s link: the Analyses list, filtered as `row` is. */
export function seeAllHref(row: WorkRowSpec): string {
  return `/runs?filter=${row.filter}`;
}

/** How the viewer left a row: open or closed, and the count it showed when they did. */
export interface RowChoice {
  open: boolean;
  /** The row's count when they chose (lowered as it falls): more than this is news. */
  seen: number;
}

/**
 * Whether `row` is open: as the viewer last left it, else Needs attention when it has something
 * in it, and the others closed.
 */
export function rowOpen(row: AccordionRow, choice: RowChoice | null, total: number): boolean {
  if (choice !== null) return choice.open;
  return row === 'attention' && total > 0;
}

/**
 * The viewer's choice for `row` now that it counts `total`. Needs attention closed with fewer
 * than it now has (something new failed since) forgets the choice, so the row opens again; a
 * count that falls lowers what was seen, so the next rise counts as news. Otherwise unchanged
 * (the same object, so a caller can tell).
 */
export function choiceAfter(
  row: AccordionRow,
  choice: RowChoice | null,
  total: number,
): RowChoice | null {
  if (row !== 'attention' || choice === null) return choice;
  if (!choice.open && total > choice.seen) return null;
  if (total < choice.seen) return { open: choice.open, seen: total };
  return choice;
}

const storageKey = (row: AccordionRow) => `biffix-mywork-${row}`;

/** How the viewer left `row` in this browser, or `null` when they have not chosen. */
export function rememberedChoice(row: AccordionRow): RowChoice | null {
  const v = readPref(storageKey(row));
  const m = v?.match(/^(open|closed)(?::(\d+))?$/);
  if (!m) return null;
  // A choice stored before the count was (`open`/`closed` alone): nothing counts as news.
  return { open: m[1] === 'open', seen: m[2] === undefined ? Infinity : Number(m[2]) };
}

/** Remember how the viewer left `row` (`null`: forget, back to the default). */
export function rememberChoice(row: AccordionRow, choice: RowChoice | null): void {
  writePref(
    storageKey(row),
    choice === null ? '' : `${choice.open ? 'open' : 'closed'}:${choice.seen}`,
  );
}

/** The line under My work's title: `2 running, 1 needs a look.`; empty when nothing is either. */
export function workSummary(running: number, attention: number): string {
  const parts: string[] = [];
  if (running) parts.push(`${running} running`);
  if (attention) parts.push(`${attention} ${attention === 1 ? 'needs' : 'need'} a look`);
  return parts.length ? `${parts.join(', ')}.` : '';
}
