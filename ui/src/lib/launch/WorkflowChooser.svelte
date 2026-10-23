<script lang="ts">
  // New run in the Expert view: pick the workflow to launch. One row per workflow (its newest
  // released version, else its newest); the Run tab of that version is the launch form. Older
  // versions are reachable from the workflow's version picker. Workflows with no released version
  // are listed only with Developer on. The Lab view picks from the pipeline catalog instead.
  import type { Snippet } from 'svelte';
  import { fmtRelativeMicros, plural } from '#lib/util/format.js';
  import { workflowTabHref } from '#lib/workflows/workflowNav.js';
  import { WorkflowCatalog } from '#lib/workflows/catalog.svelte.js';
  import { withExperiment } from '#lib/experiments/experiments.js';
  import { launchVersion } from '#lib/workflows/release.js';
  import ReleaseBadge from '#lib/workflows/ReleaseBadge.svelte';
  import DeveloperToggle from '#lib/workflows/DeveloperToggle.svelte';
  import NothingReleased from '#lib/workflows/NothingReleased.svelte';

  interface Props {
    /** The experiment a launch joins (carried on to the workflow's form). */
    experiment: string | null;
    /** That experiment's project, whose workflows alone are offered. */
    experimentProject: string | null;
    /** Shown under the line (which experiment the run joins). */
    children?: Snippet;
  }
  let { experiment, experimentProject, children }: Props = $props();

  let search = $state('');

  // One choice per workflow (project, name): the version a launch takes, and the runs of every
  // version. Every workflow is listed, those hidden from the Lab view's catalog included.
  const catalog = new WorkflowCatalog(
    (g) => !experimentProject || g.latest.project === experimentProject,
  );
  $effect(() => catalog.load());
  const loading = $derived(catalog.loading);
  const error = $derived(catalog.error);
  const candidates = $derived(catalog.groups);
  // Only those with a released version, unless the viewer turns on Developer; by name.
  const choices = $derived(
    [...catalog.filtered.shown]
      .sort((a, b) => a.name.localeCompare(b.name) || a.project.localeCompare(b.project))
      .map((g) => ({ ...g, latest: launchVersion(g) })),
  );
  const q = $derived(search.trim().toLowerCase());
  const visible = $derived(
    q
      ? choices.filter((c) =>
          `${c.latest.name} ${c.latest.project} ${c.latest.engine} ${c.latest.summary ?? ''}`
            .toLowerCase()
            .includes(q),
        )
      : choices,
  );
</script>

<a class="back-link" href="/runs">← Runs</a>
<h1>New run</h1>
<p class="lede">Choose the workflow to run. You'll fill in its inputs next.</p>
{@render children?.()}

<input
  class="search"
  type="search"
  placeholder="Search workflows…"
  aria-label="Search workflows"
  bind:value={search}
/>
<DeveloperToggle />

{#if loading}
  <p class="muted">Loading…</p>
{:else if error}
  <p class="error">Couldn't load workflows: {error}</p>
{:else if candidates.length > 0 && choices.length === 0}
  <NothingReleased count={candidates.length} noun="workflow" />
{:else if choices.length === 0 && experimentProject}
  <p class="muted">No workflows in project {experimentProject} yet.</p>
{:else if choices.length === 0}
  <p class="muted">
    No workflows yet — a developer registers them with
    <code>biffix workflows register</code>.
  </p>
{:else if visible.length === 0}
  <p class="muted">No workflow matches “{search}”.</p>
{:else}
  <ul class="choices">
    {#each visible as c (c.latest.id)}
      <li>
        <a class="choice" href={withExperiment(workflowTabHref(c.latest.id, 'run'), experiment)}>
          <span class="name"
            >{c.latest.name} <ReleaseBadge released={c.latest.release?.released} /></span
          >
          {#if c.latest.summary}<span class="summary">{c.latest.summary}</span>{/if}
          <span class="meta">
            <span class="tag">{c.latest.project}</span>
            <span class="tag engine">{c.latest.engine}</span>
            <span
              >updated {fmtRelativeMicros(c.latest.created_at_micros)} · {plural(
                c.runCount,
                'run',
              )}</span
            >
          </span>
          <span class="go" aria-hidden="true">→</span>
        </a>
      </li>
    {/each}
  </ul>
{/if}

<style>
  h1 {
    margin: 0;
  }
  .lede {
    color: var(--text-muted);
    margin: 0.3rem 0 1rem;
  }
  .search {
    font: inherit;
    font-size: 0.85rem;
    padding: 0.4rem 0.6rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    width: min(100%, 24rem);
    margin-bottom: 0.9rem;
  }
  .choices {
    list-style: none;
    margin: 0;
    padding: 0;
    max-width: 48rem;
    display: grid;
    gap: 0.5rem;
  }
  .choice {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-areas: 'name go' 'summary go' 'meta go';
    gap: 0.2rem 1rem;
    align-items: center;
    padding: 0.7rem 0.9rem;
    border: 1px solid var(--card-border);
    border-radius: 0.55rem;
    color: var(--text);
    text-decoration: none;
  }
  .choice:hover {
    background: var(--hover);
  }
  .choice:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .name {
    grid-area: name;
    font-weight: 600;
  }
  .summary {
    grid-area: summary;
    font-size: 0.85rem;
    color: var(--text-muted);
  }
  .meta {
    grid-area: meta;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .tag {
    font-size: 0.7rem;
    padding: 0.02rem 0.45rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
  }
  .tag.engine {
    color: var(--fg-engine);
  }
  .go {
    grid-area: go;
    color: var(--text-muted);
  }
</style>
