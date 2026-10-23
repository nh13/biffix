<script lang="ts">
  // One pipeline in the Lab catalog: its name, a Verified badge, what it does, its tags, how much it
  // is used, a pin star and Start. The card is the newest released version (else the newest, a
  // developer version), and says which; counts cover every version.
  import type { WorkflowSummary } from '$lib/api';
  import type { VersionGroup } from '$lib/workflows/version';
  import { workflowTabHref } from '$lib/workflows/workflowNav';
  import { fmtRelative, plural } from '$lib/util/format';
  import { togglePin, view } from '$lib/state/userPrefs.svelte';
  import VerifiedBadge from './VerifiedBadge.svelte';
  import ReleaseBadge from '$lib/workflows/ReleaseBadge.svelte';
  import { launchVersion } from '$lib/workflows/release';
  import { withExperiment } from '$lib/experiments/experiments';
  import WhatYouGet from './WhatYouGet.svelte';

  let {
    group,
    compact = false,
    experiment = null,
  }: {
    group: VersionGroup<WorkflowSummary>;
    compact?: boolean;
    /** The experiment Start launches into (carried on to the wizard). */
    experiment?: string | null;
  } = $props();
  // The heading's id: never built from the pipeline's name or project, which may hold spaces.
  const uid = $props.id();

  // The version the card stands for, and Start launches: the newest released one, else the newest.
  const version = $derived(launchVersion(group));
  const catalog = $derived(version.catalog);
  // The friendly title is the Lab's name for it; the registry name is the small print.
  const title = $derived(version.title?.trim() || group.name);
  const repo = $derived(title !== group.name ? group.name : null);
  const pinned = $derived(view.pinned.includes(group.key));
  const lastRun = $derived(
    group.versions.reduce<number | null>(
      (m, v) => (v.last_run_at != null && (m == null || v.last_run_at > m) ? v.last_run_at : m),
      null,
    ),
  );
  const about = $derived(workflowTabHref(version.id, 'about'));
  const runs = (n: number) => plural(n, 'analysis', 'analyses');
</script>

<article class="card" class:compact aria-labelledby={`${uid}-h`}>
  <header>
    <h3 id={`${uid}-h`}><a href={about}>{title}</a></h3>
    {#if catalog?.verified}<VerifiedBadge />{/if}
    <ReleaseBadge released={version.release?.released} />
    <!-- Offered once the saved pins have loaded, so a pin never replaces pins it has not seen. -->
    <button
      class="pin"
      class:on={pinned}
      aria-pressed={pinned}
      aria-label={pinned ? `Unpin ${title}` : `Pin ${title}`}
      title={pinned ? 'Unpin from My work' : 'Pin to My work'}
      disabled={!view.pinsLoaded}
      onclick={() => togglePin(group.key)}>{pinned ? '★' : '☆'}</button
    >
  </header>
  {#if repo}<p class="repo" data-testid="pipeline-repo">{repo}</p>{/if}
  {#if catalog?.description}
    <p class="desc">{catalog.description}</p>
  {:else}
    <p class="desc muted">No description yet.</p>
  {/if}
  {#if !compact && catalog?.tags?.length}
    <ul class="tags" aria-label="Tags">
      {#each catalog.tags as tag, i (i)}<li>{tag}</li>{/each}
    </ul>
  {/if}
  {#if !compact}
    <!-- Its typical run counts runs of every version, listed or not (the list carries it). -->
    <WhatYouGet {catalog} typical={version.typical} compact />
  {/if}
  <p class="meta">
    {runs(group.runCount)}{#if lastRun != null}{' · last run '}{fmtRelative(lastRun)}{/if}
  </p>
  <footer>
    <a class="start" href={withExperiment(workflowTabHref(version.id, 'run'), experiment)}
      >Start<span class="sr-only"> {title}</span></a
    >
    <a class="about" href={about}>About<span class="sr-only"> {title}</span></a>
  </footer>
</article>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    min-width: 0;
  }
  header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  h3 {
    font-size: 0.95rem;
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  h3 a {
    color: var(--text);
    text-decoration: none;
  }
  h3 a:hover {
    text-decoration: underline;
  }
  .pin {
    margin-left: auto;
    border: none;
    background: none;
    font-size: 1.1rem;
    line-height: 1;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0.1rem 0.2rem;
    border-radius: 0.3rem;
  }
  .pin.on {
    color: var(--fg-warn);
  }
  .pin:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .pin:focus-visible,
  .start:focus-visible,
  .about:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .repo {
    margin: -0.3rem 0 0;
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .desc {
    margin: 0;
    font-size: 0.82rem;
    line-height: 1.4;
  }
  .compact .desc {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .tags li {
    font-size: 0.72rem;
    padding: 0 0.5rem;
    border: 1px solid var(--card-border);
    border-radius: 999px;
    color: var(--text-muted);
  }
  .meta {
    margin: 0;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  footer {
    display: flex;
    gap: 0.5rem;
    margin-top: auto;
    padding-top: 0.2rem;
  }
  .start,
  .about {
    font-size: 0.8rem;
    padding: 0.2rem 0.8rem;
    border-radius: 0.4rem;
    text-decoration: none;
  }
  .start {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
  }
  .about {
    border: 1px solid var(--control-border);
    color: var(--text);
  }
</style>
