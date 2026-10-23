// One folder of one data source at a time, as the file picker and the Data page browse it: the
// first page and more, a "starts with" filter (re-listed, debounced), and a search under the folder.
// A quick second request never lets an earlier response land last, and a search cancels a pending
// filter re-list.

import {
  findDataFiles,
  listDataFolder,
  type DataFile,
  type DataFolder,
  type DataSource,
} from '#lib/api/index.js';
import { debounce } from '#lib/util/debounce.js';
import { errorMessage } from '#lib/util/format.js';

export class FolderListing {
  /** The project whose sources these are; `null` for the deploy's default project. */
  project = $state<string | null>(null);
  source = $state<DataSource | null>(null);
  path = $state('');
  folders = $state<DataFolder[]>([]);
  files = $state<DataFile[]>([]);
  cursor = $state<string | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);
  startsWith = $state('');
  /** Search results under the folder, in place of its listing; `null` when not searching. */
  searchResults = $state<DataFile[] | null>(null);
  searchTruncated = $state(false);

  #request = 0;
  #relist = debounce(() => this.load(), 250);
  /** The last request made, which `retry()` repeats (a failed search leaves no results to go by). */
  #last: () => void = () => this.load();

  /** Show `path` (a folder, `''` or ending in `/`) of `source` (of `project`, the default's when
   * absent: source ids repeat across projects). */
  open(source: DataSource, path: string, project: string | null = null) {
    this.#relist.cancel();
    this.project = project;
    this.source = source;
    this.path = path;
    this.startsWith = '';
    this.searchResults = null;
    this.cursor = null;
    this.folders = [];
    this.files = [];
    this.load();
  }

  /** List the folder (filtered by `startsWith`), or its next page with `more`. */
  load(more = false) {
    const source = this.source;
    // No next page to ask for: the first, appended again, would repeat every row.
    if (!source || (more && !this.cursor)) return;
    const id = ++this.#request;
    this.#last = () => this.load(more);
    this.loading = true;
    this.error = null;
    listDataFolder(source.id, this.path, {
      startsWith: this.startsWith || undefined,
      cursor: more ? (this.cursor ?? undefined) : undefined,
      project: this.project,
    })
      .then((page) => {
        if (id !== this.#request) return;
        this.folders = more ? [...this.folders, ...page.folders] : page.folders;
        this.files = more ? [...this.files, ...page.files] : page.files;
        this.cursor = page.next_cursor ?? null;
      })
      .catch((e) => {
        if (id === this.#request) this.error = errorMessage(e);
      })
      .finally(() => {
        if (id === this.#request) this.loading = false;
      });
  }

  /** The filter text changed: leave any search and re-list by name prefix, debounced. */
  filterChanged() {
    this.searchResults = null;
    // A search still in flight is for the old text: drop its results when they come.
    ++this.#request;
    this.#relist();
  }

  /** Files under the folder (any depth) whose name contains the filter text. */
  async search() {
    const source = this.source;
    const q = this.startsWith.trim();
    if (!source || !q) return;
    // A pending filter re-list would supersede this search and drop its results.
    this.#relist.cancel();
    const id = ++this.#request;
    this.#last = () => this.search();
    this.loading = true;
    this.error = null;
    try {
      const found = await findDataFiles(source.id, this.path, q, this.project);
      if (id !== this.#request) return;
      this.searchResults = found.files;
      this.searchTruncated = found.truncated;
    } catch (e) {
      if (id === this.#request) this.error = errorMessage(e);
    } finally {
      if (id === this.#request) this.loading = false;
    }
  }

  /** Retry what failed: the search, or the listing. */
  retry() {
    this.#last();
  }
}
