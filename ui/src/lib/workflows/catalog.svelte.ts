// The registered workflows as the pages that offer them read them: the Lab view's pipeline catalog,
// the Expert view's Workflows table and New run's workflow list; My work's and Settings' pinned
// pipelines and the Experiments page's project filter read it too, unfiltered. One list, grouped
// into pipelines (project + name, newest version first), narrowed by the page, then by the
// Developer filter: only pipelines with a released version unless the viewer turns Developer on.
//
// A component owns one catalog and loads it from an `$effect`:
//
//   const catalog = new WorkflowCatalog((g) => !g.latest.hidden);
//   $effect(() => catalog.load());

import { listWorkflows, type WorkflowSummary } from '#lib/api/index.js';
import { prefs } from '#lib/state/localPrefs.svelte.js';
import { errorMessage } from '#lib/util/format.js';
import { releaseFilter } from './release';
import { groupVersions, type VersionGroup } from './version';

export type WorkflowGroup = VersionGroup<WorkflowSummary>;

export class WorkflowCatalog {
  workflows = $state<WorkflowSummary[]>([]);
  loading = $state(true);
  error = $state<string | null>(null);
  readonly #keep: (g: WorkflowGroup) => boolean;

  /** `keep`: the pipelines this page offers at all (it may read reactive state). */
  constructor(keep: (g: WorkflowGroup) => boolean = () => true) {
    this.#keep = keep;
  }

  /** The pipelines the page offers, before the Developer filter. */
  readonly groups = $derived(groupVersions(this.workflows).filter((g) => this.#keep(g)));
  /** What the Developer filter shows of them, and how many it left out. */
  readonly filtered = $derived(releaseFilter(this.groups, prefs.showDeveloper));

  /**
   * What the list says of a pin (a `workflowKey`): `listed`, a pipeline it offers and does not hide;
   * `gone`, none (or a hidden one); `unknown` while it loads, or when it could not be read. Every
   * page that lists pins judges them by this one rule.
   */
  pinStatus(key: string): 'listed' | 'gone' | 'unknown' {
    if (this.loading || this.error) return 'unknown';
    const g = this.groups.find((g) => g.key === key);
    return g && !g.latest.hidden ? 'listed' : 'gone';
  }

  /** Read the list. */
  load(): void {
    this.loading = true;
    this.error = null;
    listWorkflows()
      .then((w) => (this.workflows = w))
      .catch((e) => (this.error = errorMessage(e)))
      .finally(() => (this.loading = false));
  }
}
