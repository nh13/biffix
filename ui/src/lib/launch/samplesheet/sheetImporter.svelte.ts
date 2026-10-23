// Importing a CSV/TSV into the samplesheet builder: read the file, match its columns to the sheet's
// (asking first when its headers are not the sheet's own), replace the rows, and say what was left
// out. The match the user confirms is remembered per pipeline param (`memoryKey`).

import type { SheetSpec } from '#lib/generated/samples.js';
import { errorMessage, plural } from '#lib/util/format.js';
import {
  autoMapColumns,
  importNotes,
  loadRememberedMapping,
  mappingToRemember,
  needsMapping,
  readSheetTable,
  remapColumn,
  rowsFromMapping,
  saveRememberedMapping,
  type ColumnMapping,
  type SheetTable,
} from './sheetImport';
import type { SheetState } from './sheetState.svelte';

/** What the importer is given: read through getters, so they stay the current props. */
export interface ImporterProps {
  readonly spec: SheetSpec;
  /** Where a CSV import's column mapping is remembered (the pipeline's param); none: not kept. */
  readonly memoryKey: string | null;
}

/** The column-matching step: the file, its table, and each sheet column's file column. */
export interface Matching {
  file: string;
  table: SheetTable;
  mapping: ColumnMapping;
}

export class SheetImporter {
  readonly #sheet: SheetState;
  readonly #p: ImporterProps;
  /** What the last import did (or why it did nothing). */
  note = $state<string | null>(null);
  /** The file last read: its name and table, so its columns can be matched again after an import. */
  last = $state<{ file: string; table: SheetTable } | null>(null);
  /** The column-matching step, open while a file whose headers are not the sheet's is imported. */
  matching = $state<Matching | null>(null);
  readonly matchPreview = $derived(
    this.matching
      ? rowsFromMapping(this.#spec, this.matching.table.records.slice(0, 3), this.matching.mapping)
      : [],
  );

  constructor(sheet: SheetState, props: ImporterProps) {
    this.#sheet = sheet;
    this.#p = props;
  }

  get #spec(): SheetSpec {
    return this.#p.spec;
  }

  #remembered(): Record<string, string> {
    const key = this.#p.memoryKey;
    return key ? loadRememberedMapping(key) : {};
  }

  async read(file: File) {
    const spec = this.#spec;
    let table: SheetTable;
    try {
      table = readSheetTable(await file.text(), spec);
    } catch (e) {
      this.note = `${file.name} could not be read (${errorMessage(e)}); nothing was imported.`;
      this.last = null;
      return;
    }
    if (table.records.length === 0) {
      this.note = `${file.name} has no rows; nothing was imported.`;
      // Nothing of it to match again: *Change how its columns match* must not reopen an earlier file.
      this.last = null;
      return;
    }
    const remembered = this.#remembered();
    const auto = autoMapColumns(spec, table.headers, remembered);
    // Headers that are not the sheet's own (a guessed match, a required column with no match, or a
    // remembered choice that would overrule what the file's headers say): show the match before
    // importing anything.
    if (needsMapping(spec, table.headers, auto, remembered)) {
      this.note = null;
      this.matching = { file: file.name, table, mapping: auto.mapping };
      return;
    }
    // Only an import that happened is the one *Change how its columns match* reopens.
    if (this.#importRows(file.name, table, auto.mapping)) this.last = { file: file.name, table };
  }

  /** Reopen the matching step for the file last imported. */
  rematch() {
    const last = this.last;
    if (!last) return;
    this.matching = {
      ...last,
      mapping: autoMapColumns(this.#spec, last.table.headers, this.#remembered()).mapping,
    };
  }

  setMapping(column: string, value: string) {
    if (!this.matching) return;
    this.matching = {
      ...this.matching,
      mapping: remapColumn(this.matching.mapping, column, value),
    };
  }

  cancel() {
    this.matching = null;
  }

  confirm() {
    if (!this.matching) return;
    const { file, table, mapping } = this.matching;
    if (this.#importRows(file, table, mapping)) {
      this.last = { file, table };
      const key = this.#p.memoryKey;
      if (key)
        saveRememberedMapping(key, {
          ...loadRememberedMapping(key),
          ...mappingToRemember(table.headers, mapping),
        });
      this.matching = null;
    }
  }

  /** Replace the table with `table`'s rows under `mapping`; false when the user keeps the rows. */
  #importRows(file: string, table: SheetTable, mapping: ColumnMapping): boolean {
    const spec = this.#spec;
    const imported = rowsFromMapping(spec, table.records, mapping);
    // An import replaces the table, unlike the other ways to add samples: ask before losing rows.
    const n = this.#sheet.rows.length;
    if (
      n > 0 &&
      !confirm(
        `Replace the ${plural(n, 'row')} in the table with the ${imported.length} in ${file}?`,
      )
    )
      return false;
    this.#sheet.replace(imported);
    this.note = importNotes(spec, file, table.headers, mapping, imported.length);
    return true;
  }
}
