<script lang="ts">
  // The Expert view's Workflows page: every registered workflow, a row per workflow (its newest
  // released version, else its newest) with its older versions folded under it; searchable and
  // sortable, the sort kept in the URL. The Lab view's is the pipeline catalog.
  import { page } from '$app/state';
  import { replaceUrl } from '$lib/util/replaceUrl';
  import { validParam } from '$lib/util/urlParam';
  import type { WorkflowSummary } from '$lib/api';
  import { fmtRelativeMicros, plural } from '$lib/util/format';
  import { engineLabel, versionLabel, registrantLabel as who } from '$lib/workflows/version';
  import { workflowHref } from '$lib/workflows/workflowNav';
  import { WorkflowCatalog, type WorkflowGroup as Group } from '$lib/workflows/catalog.svelte';
  import SortHeader from '$lib/components/SortHeader.svelte';
  import { prefs } from '$lib/state/localPrefs.svelte';
  import { launchVersion } from '$lib/workflows/release';
  import ReleaseBadge from '$lib/workflows/ReleaseBadge.svelte';
  import DeveloperToggle from '$lib/workflows/DeveloperToggle.svelte';
  import NothingReleased from '$lib/workflows/NothingReleased.svelte';

  const catalog = new WorkflowCatalog();
  const loading = $derived(catalog.loading);
  const error = $derived(catalog.error);
  let search = $state('');
  // Groups the user expanded or collapsed by hand (key → expanded). A group without an entry is
  // expanded only when a search matched one of its older versions; a click overrides that.
  let expanded = $state<Map<string, boolean>>(new Map());

  $effect(() => catalog.load());

  // Sort state lives in the URL so a view can be linked/reloaded. Validate the raw params against
  // the known keys/directions: an unrecognized ?sort=/?dir= (typo, stale link) would otherwise fall
  // through the compare() default with no active-column arrow, silently ignoring the requested sort.
  const SORT_KEYS = ['name', 'project', 'engine', 'registered'] as const;
  type SortKey = (typeof SORT_KEYS)[number];
  const sortKey = $derived(validParam(page.url.searchParams.get('sort'), SORT_KEYS, null));
  const sortDir = $derived<'asc' | 'desc'>(
    page.url.searchParams.get('dir') === 'desc' ? 'desc' : 'asc',
  );
  function toggleSort(key: SortKey) {
    const u = new URL(page.url);
    if (sortKey === key) {
      u.searchParams.set('dir', sortDir === 'asc' ? 'desc' : 'asc');
    } else {
      u.searchParams.set('sort', key);
      u.searchParams.set('dir', 'asc');
    }
    replaceUrl(u);
  }

  // A workflow = (project, name); each registration is a version, the first one its latest. Only
  // workflows with a released version, unless the viewer turns on Developer.
  const allGroups = $derived(catalog.groups);
  const filtered = $derived(catalog.filtered);
  const groups = $derived(filtered.shown);
  // The version a row stands for: what its name opens and its Version, engine, queues and sort
  // describe. The newest released version, or (Developer on) the newest of all.
  const rowVersion = (g: Group): WorkflowSummary =>
    prefs.showDeveloper ? g.latest : launchVersion(g);

  function matchesQuery(w: WorkflowSummary, q: string): boolean {
    return `${w.name} ${w.project} ${w.engine} ${w.git_sha} ${w.id}`.toLowerCase().includes(q);
  }

  const q = $derived(search.trim().toLowerCase());

  // Filter groups (not rows): a match on ANY version keeps the group. A match on a non-latest
  // version also auto-expands the group so the hit isn't hidden behind the rolled-up latest row.
  const visible = $derived.by(() => {
    const filtered = q
      ? groups.filter((g) => g.versions.some((v) => matchesQuery(v, q)))
      : groups.slice();

    const compare = (a: Group, b: Group): number => {
      const dir = sortDir === 'asc' ? 1 : -1;
      switch (sortKey) {
        case 'name':
          return a.name.localeCompare(b.name) * dir;
        case 'project':
          return a.project.localeCompare(b.project) * dir;
        case 'engine':
          return rowVersion(a).engine.localeCompare(rowVersion(b).engine) * dir;
        case 'registered':
          return (
            ((rowVersion(a).created_at_micros ?? 0) - (rowVersion(b).created_at_micros ?? 0)) * dir
          );
        default:
          // Default: name within project (a registry is scanned by name).
          return a.project.localeCompare(b.project) || a.name.localeCompare(b.name);
      }
    };
    return filtered.sort(compare);
  });

  function isExpanded(g: Group): boolean {
    const chosen = expanded.get(g.key);
    if (chosen !== undefined) return chosen;
    // Auto-expand when the search matched a version other than the row's own.
    const own = rowVersion(g).id;
    return !!q && g.versions.some((v) => v.id !== own && matchesQuery(v, q));
  }
  function toggleExpand(g: Group) {
    expanded = new Map(expanded).set(g.key, !isExpanded(g));
  }
</script>

<h1>Registered workflows</h1>

<div class="controls">
  <input
    class="search"
    type="search"
    placeholder="Search workflows…"
    aria-label="Search workflows"
    bind:value={search}
  />
  <DeveloperToggle />
</div>

{#if loading}
  <p class="muted">Loading…</p>
{:else if error}
  <p class="error" role="alert">Couldn't load workflows: {error}</p>
{:else if allGroups.length > 0 && groups.length === 0}
  <NothingReleased count={allGroups.length} noun="workflow" />
{:else if visible.length === 0}
  {#if q}
    <p class="muted">No workflows match your search.</p>
  {:else}
    <p class="muted">
      No workflows yet — a developer registers them with <code>biffix workflows register</code>.
    </p>
  {/if}
{:else}
  {#if filtered.hiddenByFilter > 0}
    <p class="dim filter-note" data-testid="developer-note">
      {plural(filtered.hiddenByFilter, 'workflow')} with no released version not shown — turn on Developer
      to see {filtered.hiddenByFilter === 1 ? 'it' : 'them'}.
    </p>
  {/if}
  <table>
    <thead>
      <tr>
        <SortHeader label="Name" key="name" {sortKey} {sortDir} onsort={toggleSort} />
        <SortHeader label="Project" key="project" {sortKey} {sortDir} onsort={toggleSort} />
        <SortHeader label="Engine" key="engine" {sortKey} {sortDir} onsort={toggleSort} />
        <SortHeader label="Version" key="registered" {sortKey} {sortDir} onsort={toggleSort} />
        <th scope="col">Versions</th>
        <th scope="col">Queues</th>
        <th scope="col">Runs</th>
      </tr>
    </thead>
    <tbody>
      {#each visible as g (g.key)}
        {@const shown = rowVersion(g)}
        <tr>
          <!-- The row is one version: the newest released one (Developer on: the newest). -->
          <td><a href={workflowHref(shown.id)}>{g.name}</a></td>
          <td>{g.project}</td>
          <td>{engineLabel(shown)}</td>
          <td class="version" data-testid="row-version">
            <span class="mono">{versionLabel(shown)}</span>
            <ReleaseBadge released={shown.release?.released} />
            <span class="dim"
              >· {fmtRelativeMicros(shown.created_at_micros)}{who(shown)
                ? ` · ${who(shown)}`
                : ''}</span
            >
            {#if shown.id !== g.latest.id}
              <span class="dim newer" data-testid="newer-developer"
                >· newer developer version {versionLabel(g.latest)}</span
              >
            {/if}
          </td>
          <td>
            {#if g.versions.length > 1}
              <button class="chip" aria-expanded={isExpanded(g)} onclick={() => toggleExpand(g)}>
                {g.versions.length} versions {isExpanded(g) ? '▾' : '▸'}
              </button>
            {:else}
              <span class="dim">1</span>
            {/if}
          </td>
          <td class="dim">{(shown.queue_classes ?? []).join(', ') || '--'}</td>
          <td class="dim">{g.runCount}</td>
        </tr>
        {#if isExpanded(g)}
          {#each g.versions.filter((v) => v.id !== shown.id) as v}
            <tr class="sub-row">
              <td class="sub" colspan="3">
                <a href={workflowHref(v.id)} class="mono">{versionLabel(v)}</a>
                <ReleaseBadge released={v.release?.released} />
              </td>
              <td class="version dim">
                {fmtRelativeMicros(v.created_at_micros)}{who(v) ? ` · ${who(v)}` : ''}
              </td>
              <td></td>
              <td></td>
              <td class="dim">{v.run_count ?? 0}</td>
            </tr>
          {/each}
        {/if}
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .filter-note {
    margin: 0 0 0.5rem;
  }
  .search {
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font-size: 0.85rem;
    min-width: 220px;
  }
  .mono {
    font-size: 0.78rem;
  }
  .version {
    white-space: nowrap;
  }
  .dim {
    color: var(--text-muted);
    font-size: 0.8rem;
  }
  /* A tag filter is a button: it needs a background and a pointer. */
  .chip {
    background: var(--bg-content);
    cursor: pointer;
  }
  .sub-row {
    background: var(--bg-content);
  }
  .sub {
    padding-left: 1.5rem;
  }
</style>
