// One project's data sources: the Data page's (asked afresh on each visit), and the run page's
// Data links' (`DataLinks`, from the page's shared cache). Each answer is kept with the project it
// is for, and only the latest request's answer lands: a slow response for a project the user
// already left never replaces the current one's (which would leave the page with no sources to
// show, or links into the wrong project).

import { listDataSources, type DataSource } from '$lib/api';
import { errorMessage } from '$lib/util/format';

interface Loaded {
  project: string | null;
  sources: DataSource[] | null;
  error: string | null;
}

export class ProjectSources {
  #loaded = $state<Loaded | null>(null);
  #request = 0;
  readonly #fetch: (project: string | null) => Promise<DataSource[]>;

  /** Over `fetch` (default: ask the server each time). */
  constructor(fetch: (project: string | null) => Promise<DataSource[]> = listDataSources) {
    this.#fetch = fetch;
  }

  /** List `project`'s sources (`null`: the default project's), superseding any request in flight. */
  load(project: string | null) {
    const id = ++this.#request;
    this.#fetch(project)
      .then((all) => {
        if (id === this.#request) this.#loaded = { project, sources: all, error: null };
      })
      .catch((e) => {
        if (id === this.#request) this.#loaded = { project, sources: null, error: errorMessage(e) };
      });
  }

  /** `project`'s sources, or `null` until they load (or when they failed to). */
  sources(project: string | null): DataSource[] | null {
    return this.#for(project)?.sources ?? null;
  }

  /** Why `project`'s sources failed to load, or `null`. */
  error(project: string | null): string | null {
    return this.#for(project)?.error ?? null;
  }

  #for(project: string | null): Loaded | null {
    const loaded = this.#loaded;
    return loaded && loaded.project === project ? loaded : null;
  }
}
