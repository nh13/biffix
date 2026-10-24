// The samplesheet builder's rows as it edits them: their problems and statuses, which rows were
// edited by hand (they show their problems before a launch attempt), the *Only rows with problems*
// filter, and the merges detection suggested. The rows themselves belong to the form: every change
// goes out through `onchange`, and through `commit`, so the flags follow their rows.
//
// Construct it while a component initialises: its effects live as long as that component.

import { untrack } from 'svelte';
import type { DetectedSamples } from '#lib/api/index.js';
import type { SheetSpec } from '#lib/generated/samples.js';
import { standingSuggestions } from './detect';
import {
  carryTouched,
  emptyRow,
  mergeSamples,
  problemRows,
  problemsByRow,
  roleColumn,
  rowStatuses,
  shownRowIndices,
  validateSheet,
  type SheetProblem,
  type SheetRow,
} from './sheet';

/** What the builder is given: read through getters, so they stay the current props. */
export interface SheetProps {
  readonly spec: SheetSpec;
  readonly rows: SheetRow[];
  /** Show every problem (after a launch attempt), not only those in rows already edited. */
  readonly showErrors: boolean;
  onchange(rows: SheetRow[]): void;
}

/** Two detected samples that look like one pair, and the id merging them gives. */
export interface Suggestion {
  ids: [string, string];
  into: string;
}

export class SheetState {
  readonly #p: SheetProps;

  readonly idCol = $derived(roleColumn(this.spec, 'sample_id'));
  readonly problems = $derived(validateSheet(this.spec, this.rows));
  readonly #byRow = $derived(problemsByRow(this.problems));
  // Each row's status (ready, problems, empty) for the status column, which shows as soon as there
  // are rows: with 24 samples the question is which row, not whether there is a problem.
  readonly statuses = $derived(rowStatuses(this.rows, this.problems));
  readonly badRows = $derived(problemRows(this.statuses));

  onlyProblems = $state(false);
  // Rows kept in view under the filter: those with a problem when it was switched on, and any edited
  // or added since. A row being fixed never vanishes under the cursor (cells commit on each
  // keystroke, so it would turn valid mid-word); it shows ✓ until the filter is switched again.
  pinned = $state<Set<number>>(new Set());
  readonly shownRows = $derived(shownRowIndices(this.statuses, this.onlyProblems, this.pinned));
  // Rows edited by hand, which show their problems before a launch attempt.
  touchedRows = $state<Set<number>>(new Set());

  suggestions = $state<Suggestion[]>([]);
  readonly liveSuggestions = $derived(
    standingSuggestions(this.suggestions, this.rows, this.idCol?.name ?? null),
  );

  constructor(props: SheetProps) {
    this.#p = props;
    // The filter turns itself off once no row has a problem, so no row stays hidden for nothing.
    $effect(() => {
      if (this.badRows.length === 0 && untrack(() => this.onlyProblems))
        this.setOnlyProblems(false);
    });
  }

  get spec(): SheetSpec {
    return this.#p.spec;
  }

  get rows(): SheetRow[] {
    return this.#p.rows;
  }

  rowProblems(i: number): SheetProblem[] {
    return this.#byRow.get(i) ?? [];
  }

  /** A cell's problem, once its row was edited or a launch was attempted. */
  cellProblem(i: number, column: string): string | null {
    return this.#p.showErrors || this.touchedRows.has(i)
      ? (this.rowProblems(i).find((p) => p.column === column)?.message ?? null)
      : null;
  }

  setOnlyProblems(on: boolean) {
    this.onlyProblems = on;
    this.pinned = on ? new Set(this.badRows) : new Set();
  }

  commit(next: SheetRow[], touched = carryTouched(this.rows, this.touchedRows, next)) {
    this.touchedRows = touched;
    if (this.onlyProblems) this.pinned = carryTouched(this.rows, this.pinned, next);
    this.#p.onchange(next);
  }

  /** Keep row `i` of the rows just committed in view under the filter. */
  #pin(i: number) {
    if (this.onlyProblems) this.pinned = new Set(this.pinned).add(i);
  }

  setCell(i: number, column: string, value: string) {
    const rows = this.rows;
    const next = rows.map((r, j) => (j === i ? { ...r, [column]: value } : r));
    this.commit(next, carryTouched(rows, this.touchedRows, next).add(i));
    this.#pin(i);
  }

  removeRow(i: number) {
    this.commit(this.rows.filter((_, j) => j !== i));
  }

  addEmptyRow() {
    const i = this.rows.length;
    this.commit([...this.rows, emptyRow(this.spec)]);
    // Added under the filter: shown, though an empty row has no problem yet.
    this.#pin(i);
  }

  /** Replace every row (an import, or Clear): the suggestions go with the rows they were about. */
  replace(rows: SheetRow[]) {
    this.suggestions = [];
    this.commit(rows);
  }

  /** Add the samples a detection found, and offer the merges it suggests. */
  addDetected(preview: DetectedSamples) {
    this.commit([...this.rows, ...(preview.rows as SheetRow[])]);
    this.suggestions = (preview.detection.suggestions ?? []).map((s) => ({
      ids: [s.ids[0], s.ids[1]],
      into: s.into,
    }));
  }

  applySuggestion(s: Suggestion) {
    this.commit(mergeSamples(this.spec, this.rows, s.ids, s.into));
    this.dismissSuggestion(s);
  }

  dismissSuggestion(s: Suggestion) {
    this.suggestions = this.suggestions.filter((x) => x !== s);
  }
}
