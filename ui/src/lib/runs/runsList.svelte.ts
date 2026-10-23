// The runs list's state, shared by its two presentations (the Lab view's analyses and the Expert
// view's runs table): the filter and scopes (kept in the URL), whose runs, the search, keyset
// paging, the live poll, Needs attention's dismissals and the client-side sort. Switching view
// keeps it all, since the page owns one list and hands it to whichever presentation is shown.
//
// Construct it while a component initialises: its effects live as long as that component.

import { untrack } from 'svelte';
import { page } from '$app/state';
import { replaceQueryParam } from '$lib/util/replaceUrl';
import { validParam } from '$lib/util/urlParam';
import { listExperiments, listRuns, type RunSummary, type RunFilter } from '$lib/api';
import { PollNudge, startPoll } from '$lib/util/poll';
import { delayed } from '$lib/util/debounce';
import type { SortDir } from '$lib/util/sort';
import { nextPage, pageRange, pastLastPage, prevPage, type PageRequest } from './runsPager';
import { view } from '$lib/state/userPrefs.svelte';
import { t } from '$lib/view/vocab';
import { runName } from './runName';
import { afterDismissal, sortRuns, type SortKey } from './runsSort';
import { saveNavList } from './analysis/analysisNav';
import { Dismissals, withoutDismissed } from './analysis/dismiss.svelte';

export const RUN_FILTERS: RunFilter[] = [
  'all',
  'active',
  'submitted',
  'succeeded',
  'failed',
  'completed',
];
// `attention`: the caller's failed runs not dismissed (My work's *Needs attention*; the server
// implies `mine`). A Lab tab; an Expert link with it still filters.
export type ListFilter = RunFilter | 'attention';
const URL_FILTERS: ListFilter[] = [...RUN_FILTERS, 'attention'];
// Lab view: fewer filters, in words. (`?filter=submitted`/`completed` from an Expert link still
// filter; they just have no tab there.)
export const LAB_FILTERS: { f: ListFilter; label: string }[] = [
  { f: 'all', label: 'All' },
  { f: 'active', label: 'In progress' },
  { f: 'succeeded', label: 'Finished' },
  { f: 'attention', label: 'Needs attention' },
  { f: 'failed', label: 'Failed' },
];

const LIMIT = 50;
// Refresh live so a run submitted (or advancing) elsewhere shows up without a reload.
const POLL_MS = 5000;

export class RunsList {
  readonly lab = $derived(view.mode === 'lab');
  // The state filter lives in the URL (`?filter=`, e.g. from a homepage card), so the tab shown
  // is always the URL's and Back/Forward move between them. It can't name a sample, so it may stay
  // in the query. The search text can (a run name), so it stays out of the URL altogether.
  readonly filter = $derived(validParam(page.url.searchParams.get('filter'), URL_FILTERS, 'all'));
  // Needs attention is always the caller's own (the server implies `mine`): no Everyone's there,
  // and no column saying who started them. The viewer's Mine/Everyone's choice is kept for the
  // other tabs.
  readonly ownOnly = $derived(this.filter === 'attention');
  // Optional experiment scope (`?experiment=<id>`, from an experiment's page): its runs, whoever
  // started them, under a removable chip that names it.
  readonly experimentScope = $derived(page.url.searchParams.get('experiment') ?? '');
  experimentName = $state<string | null>(null);
  // Lab view lists the caller's own analyses unless they ask for everyone's.
  everyone = $state(false);
  readonly showEveryone = $derived(this.everyone && !this.ownOnly);
  readonly mine = $derived(this.lab && !this.showEveryone && !this.experimentScope);
  // Who started each one shows wherever the list is not just the caller's own.
  readonly showWho = $derived(this.showEveryone || !!this.experimentScope);
  // Optional exact workflow-version scope (from a version page's "Runs" link, ?workflow=<id>).
  readonly workflowScope = $derived(page.url.searchParams.get('workflow') ?? '');

  search = $state('');
  // Debounced, so we don't fire a request per keystroke.
  debounced = $state('');
  // Keyset paging (the server orders by submitted_at, newest first, unsubmitted runs first of all):
  // `req` is the cursor request for the CURRENT page (empty = first page), so live polling
  // re-fetches the same page; `firstCursor` / `lastCursor` bound the page for Prev/Next;
  // `pageIndex` is client-side, only for the "X–Y of Z" range display.
  pageIndex = $state(0);
  req = $state<PageRequest>({});
  firstCursor = $state<string | undefined>(undefined);
  lastCursor = $state<string | undefined>(undefined);

  runs = $state<RunSummary[]>([]);
  total = $state(0);
  loading = $state(true);
  error = $state<string | null>(null);
  // Needs attention's rows can be dismissed (one, or every one on this page): hidden at once, shown
  // again if the server refuses. Opening a row never dismisses it.
  readonly dismissals = new Dismissals();
  readonly listed = $derived(
    this.ownOnly
      ? withoutDismissed({ runs: this.runs, total: this.total }, this.dismissals.hidden)
      : { runs: this.runs, total: this.total },
  );
  // Once the server has a dismissal the list is read again at once (not at the next poll), so the
  // next rows, the pager and an empty state are right.
  readonly #nudge = new PollNudge();

  sortKey = $state<SortKey | null>(null);
  sortDir = $state<SortDir>('asc');

  constructor() {
    $effect(() => {
      const id = this.experimentScope;
      this.experimentName = null;
      if (!id) return;
      let cancelled = false;
      // The list (archived too) names it; the experiment's detail would read all its runs.
      listExperiments({ archived: true })
        .then((l) => {
          if (!cancelled)
            this.experimentName = l.experiments.find((e) => e.id === id)?.name ?? null;
        })
        .catch(() => {
          /* the chip names its id instead */
        });
      return () => {
        cancelled = true;
      };
    });

    $effect(() => {
      const s = this.search;
      return delayed(() => (this.debounced = s), 300);
    });

    // Back to the first page whenever the filter, search, or a scope changes. (Untracked: the
    // paging state is written here, not read as a dependency, and an unchanged first-page request
    // is left alone so the poll isn't restarted a second time.)
    $effect(() => {
      void this.filter;
      void this.debounced;
      void this.workflowScope;
      void this.experimentScope;
      void this.mine;
      untrack(() => {
        if (this.req.after || this.req.before) this.req = {};
        this.pageIndex = 0;
      });
    });

    $effect(() => {
      const f = this.filter;
      const q = this.debounced.trim();
      const cursor = this.req;
      const wf = this.workflowScope;
      const exp = this.experimentScope;
      const onlyMine = this.mine;
      this.loading = true;
      this.error = null;
      return startPoll({
        fetch: () =>
          listRuns(f === 'attention' ? 'all' : f, {
            attention: f === 'attention' || undefined,
            q: q || undefined,
            workflow: wf || undefined,
            experiment: exp || undefined,
            mine: onlyMine || undefined,
            limit: LIMIT,
            after: cursor.after,
            before: cursor.before,
          }),
        onData: (r) => {
          this.runs = r.runs;
          this.total = r.total;
          this.firstCursor = r.first_cursor ?? undefined;
          this.lastCursor = r.last_cursor ?? undefined;
          this.error = null;
          // Live polling can shrink the total under us (e.g. an 'active' filter as runs complete).
          // If that leaves us past the last page, snap back to the first page. Resetting `req`
          // re-fetches page one at once.
          if (pastLastPage(this.pageIndex, LIMIT, this.total)) {
            this.pageIndex = 0;
            this.req = {};
          }
        },
        onError: (m) => (this.error = m),
        onSettled: () => (this.loading = false),
        intervalMs: POLL_MS,
        nudge: this.#nudge,
      });
    });

    // The rows as shown, remembered for the analysis page's previous / next (Lab view).
    $effect(() => {
      if (!this.lab || this.loading || this.error) return;
      saveNavList({
        items: this.sorted.map((r) => ({ id: r.run_id, name: this.nameOf(r) })),
        href: `${page.url.pathname}${page.url.search}`,
        more: this.listed.total > this.listed.runs.length,
      });
    });
  }

  /** A run's name, as this view says it. */
  nameOf = (r: RunSummary): string => (this.lab ? runName(r) : (r.run_name ?? r.run_id));

  setFilter(f: ListFilter): void {
    void replaceQueryParam('filter', f === 'all' ? null : f);
  }
  clearExperimentScope(): void {
    void replaceQueryParam('experiment', null);
  }
  clearWorkflowScope(): void {
    void replaceQueryParam('workflow', null);
  }

  toggleSort = (key: SortKey): void => {
    if (this.sortKey === key) this.sortDir = this.sortDir === 'asc' ? 'desc' : 'asc';
    else {
      this.sortKey = key;
      this.sortDir = 'asc';
    }
  };

  #goTo(to: { index: number; req: PageRequest }): void {
    this.pageIndex = to.index;
    this.req = to.req;
  }
  prev(): void {
    this.#goTo(prevPage(this.pageIndex, this.firstCursor));
  }
  next(): void {
    const to = nextPage(this.pageIndex, this.lastCursor);
    if (to) this.#goTo(to);
  }

  #focusAttentionHead(): void {
    document.getElementById('attention-head')?.focus();
  }
  // `left` is how many still need attention: a page emptied while more remain goes back to the
  // first page.
  #afterDismissal = (left: number | null): void => {
    const at = { index: this.pageIndex, req: this.req };
    if (afterDismissal(this.listed.runs.length, left, at) === 'first-page') {
      this.pageIndex = 0;
      this.req = {};
    } else this.#nudge.now();
  };
  dismissOne(r: RunSummary): void {
    this.#focusAttentionHead();
    void this.dismissals.dismiss([r.run_id], this.nameOf(r), this.#afterDismissal);
  }
  /** Dismiss exactly the rows on this page (`Dismiss these 50`, or `Dismiss all 3`). */
  dismissPage(): void {
    const rows = this.listed.runs;
    this.#focusAttentionHead();
    const what =
      rows.length === 1 ? this.nameOf(rows[0]) : `${rows.length} ${t('runsLower', view.mode)}`;
    void this.dismissals.dismiss(
      rows.map((r) => r.run_id),
      what,
      this.#afterDismissal,
    );
  }

  readonly sorted = $derived.by(() =>
    this.sortKey
      ? sortRuns(this.listed.runs, this.sortKey, this.sortDir, {
          nameOf: this.nameOf,
          lab: this.lab,
        })
      : this.listed.runs,
  );

  // Clamped so a mid-poll shrink never renders a reversed/negative range for the one frame before
  // onData's page reset takes effect.
  readonly range = $derived(pageRange(this.pageIndex, LIMIT, this.listed.total));
  // Column sort is client-side over the current page only, while paging is server-side keyset — so
  // when the result set spans more than one page, a sort reorders just the visible rows. Surface
  // that rather than present a table-wide sort that silently isn't one.
  readonly sortIsPageLocal = $derived(
    this.sortKey !== null && this.listed.total > this.listed.runs.length,
  );
}
