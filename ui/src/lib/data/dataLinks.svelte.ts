// Links into the Data page for one project's S3 URIs: the project's sources come from the page's
// shared cache through `ProjectSources` (so only the latest project's answer lands, and an answer
// for another project never links this one's URIs). Until they have loaded (or when they cannot)
// no URI links, so a link never opens onto an error.

import { cachedDataSources, dataBrowserHref } from './browse';
import { ProjectSources } from './projectSources.svelte';

export class DataLinks {
  #sources = new ProjectSources(cachedDataSources);
  #project = $state<string | null>(null);

  /** Load `project`'s sources (`null`/absent: the default project's). */
  load(project: string | null | undefined) {
    const key = project ?? null;
    this.#project = key;
    // Every call supersedes the one in flight (a cached answer costs nothing).
    this.#sources.load(key);
  }

  /** The Data page's URL for `uri`, or null when it cannot show it (or the sources have not loaded). */
  href(uri: string | null | undefined): string | null {
    const sources = this.#sources.sources(this.#project);
    return sources ? dataBrowserHref(uri, sources, this.#project) : null;
  }
}
