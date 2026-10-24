<script lang="ts">
  // Experiments: named groups of a project's analyses. A card per experiment (its
  // name, what it is, its project, how many analyses and when one last started), filtered by
  // project, the caller's alone (Mine) or everyone's (All), archived ones on asking, and New
  // experiment. The same page in both views, in each view's words. The project filter, Mine and the
  // archived switch live in the URL, flags spelled as the API's (`?mine=true&archived=true`); My
  // work's "See all" links here with `?mine=true`.
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { listExperiments, type Experiment } from '#lib/api/index.js';
  import { WorkflowCatalog } from '#lib/workflows/catalog.svelte.js';
  import { errorMessage } from '#lib/util/format.js';
  import { replaceQueryParam } from '#lib/util/replaceUrl.js';
  import { flagParam, FLAG_ON } from '#lib/util/urlParam.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { t } from '#lib/view/vocab.js';
  import {
    activityText,
    attentionText,
    experimentHref,
    experimentProjects,
  } from '#lib/experiments/experiments.js';
  import NewExperimentForm from '#lib/experiments/NewExperimentForm.svelte';

  const project = $derived(page.url.searchParams.get('project') ?? '');
  const archived = $derived(flagParam(page.url.searchParams, 'archived'));
  const mine = $derived(flagParam(page.url.searchParams, 'mine'));

  // Every experiment (archived too), read once: the cards are this list filtered here (Mine keeps
  // those the server marks the caller's, `mine`), so a filter change asks nothing, and it also
  // gives the project choices and the new name's check. The server's order (newest activity
  // first) is kept.
  let everything = $state<Experiment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let attempt = $state(0);
  $effect(() => {
    void attempt;
    let cancelled = false;
    loading = true;
    error = null;
    listExperiments({ archived: true })
      .then((l) => {
        if (!cancelled) everything = l.experiments;
      })
      .catch((e) => {
        if (!cancelled) error = errorMessage(e);
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  /** Whose experiments the cards are drawn from, before the project and archived filters. */
  const whose = $derived(mine ? everything.filter((e) => e.mine) : everything);
  const experiments = $derived(
    whose.filter((e) => (!project || e.project === project) && (archived || e.archived_at == null)),
  );
  /** Some of `whose` in the chosen project are archived and the archived switch hides them. */
  const archivedHidden = $derived(
    !archived && whose.some((e) => e.archived_at != null && (!project || e.project === project)),
  );

  // The projects with a pipeline; when they do not load, the filter offers the experiments'
  // projects alone.
  const catalog = new WorkflowCatalog();
  $effect(() => {
    void attempt;
    catalog.load();
  });
  const projects = $derived(
    experimentProjects(
      catalog.workflows.map((v) => v.project),
      everything,
    ),
  );

  const setParam = (name: string, value: string | null) => void replaceQueryParam(name, value);

  let creating = $state(false);
  function created(e: Experiment) {
    creating = false;
    goto(experimentHref(e.id));
  }
  const filtered = $derived(!!project || archived);
  const runsWord = $derived(t('runsLower', view.mode));
  const resultsWord = $derived(view.mode === 'lab' ? 'results' : 'outputs');
</script>

<div class="title-row">
  <h1>Experiments</h1>
  {#if !creating}
    <button class="btn btn-primary" onclick={() => (creating = true)}>New experiment</button>
  {/if}
</div>
<p class="lede">
  Group {runsWord} under a name to read their results together. An experiment lives in one project and
  adds no permissions.
</p>

{#if creating}
  <section class="card create" aria-labelledby="new-exp-h">
    <h2 id="new-exp-h">New experiment</h2>
    <NewExperimentForm
      projects={projects.length ? projects : project ? [project] : []}
      initialProject={project || null}
      existing={everything}
      oncreated={created}
      oncancel={() => (creating = false)}
    />
  </section>
{/if}

<div class="controls">
  <div class="tabs" role="group" aria-label="Whose experiments">
    <button class:active={mine} aria-pressed={mine} onclick={() => setParam('mine', FLAG_ON)}
      >Mine</button
    >
    <button class:active={!mine} aria-pressed={!mine} onclick={() => setParam('mine', null)}
      >All</button
    >
  </div>
  <label class="filter"
    >Project
    <select value={project} onchange={(e) => setParam('project', e.currentTarget.value || null)}>
      <option value="">All projects</option>
      {#each projects as p (p)}<option value={p}>{p}</option>{/each}
      {#if project && !projects.includes(project)}<option value={project}>{project}</option>{/if}
    </select>
  </label>
  <label class="filter check">
    <input
      type="checkbox"
      checked={archived}
      onchange={(e) => setParam('archived', e.currentTarget.checked ? FLAG_ON : null)}
    />
    Show archived
  </label>
</div>

{#if loading && experiments.length === 0}
  <p class="muted">Loading…</p>
{:else if error}
  <p class="error" role="alert">
    Couldn't load experiments: {error}
    <button class="link" onclick={() => attempt++}>Try again</button>
  </p>
{:else if experiments.length === 0}
  {#if mine && whose.length === 0}
    <!-- None at all is the caller's, archived or not, in any project. -->
    <div class="empty" data-testid="experiments-empty-mine">
      <p class="empty-lede">You're not in an experiment yet.</p>
      <p class="muted">
        An experiment is yours once you make one or put one of your {runsWord} in it. Anyone who can start
        {runsWord} can make one.
      </p>
      <div class="empty-actions">
        {#if !creating}
          <button class="btn btn-primary" onclick={() => (creating = true)}>New experiment</button>
        {/if}
        <button class="btn" onclick={() => setParam('mine', null)}>See all experiments</button>
      </div>
    </div>
  {:else if filtered || whose.length > 0}
    <!-- There are some, but the project or the archived switch hides them all. -->
    <p class="muted" data-testid="experiments-empty-filtered">
      No experiments{mine ? ' of yours' : ''}{project
        ? ` in project ${project}`
        : ''}{archivedHidden ? ' (archived ones are hidden)' : ''}.
      {#if archivedHidden}<button class="link" onclick={() => setParam('archived', FLAG_ON)}
          >Show archived</button
        >{/if}
    </p>
  {:else}
    <div class="empty" data-testid="experiments-empty">
      <p class="empty-lede">Group {runsWord} under a name to read their results together.</p>
      <p class="muted">
        An experiment shows its {runsWord}' status, their {resultsWord} side by side, every sample and
        one total cost. Anyone who can start {runsWord} can make one.
      </p>
      {#if !creating}
        <button class="btn btn-primary" onclick={() => (creating = true)}>New experiment</button>
      {/if}
    </div>
  {/if}
{:else}
  <ul class="grid" aria-label="Experiments">
    {#each experiments as e (e.id)}
      <li>
        <article class="exp" class:archived={e.archived_at != null} aria-labelledby={`exp-${e.id}`}>
          <h2 id={`exp-${e.id}`}>
            <a href={experimentHref(e.id)}>{e.name}</a>
          </h2>
          {#if e.description}<p class="desc">{e.description}</p>{/if}
          <p class="meta">
            <span class="tag">{e.project}</span>
            {#if e.archived_at != null}<span class="tag">archived</span>{/if}
            <span>{activityText(e, view.mode)}</span>
          </p>
          {#if attentionText(e)}<p class="flag">{attentionText(e)}</p>{/if}
          {#if view.mode === 'expert'}<p class="id mono">{e.id}</p>{/if}
        </article>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  h1 {
    margin: 0;
  }
  .lede {
    margin: 0.3rem 0 1rem;
    color: var(--text-muted);
    max-width: 48rem;
  }
  .card {
    margin-bottom: 1rem;
    max-width: 34rem;
  }
  .card h2 {
    margin: 0 0 0.3rem;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem 1.2rem;
    margin-bottom: 1rem;
  }
  .filter {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
  }
  select {
    font: inherit;
    font-size: 0.85rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(18rem, 100%), 1fr));
    gap: 0.8rem;
  }
  .exp {
    height: 100%;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.9rem 1rem;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
  }
  .exp.archived {
    border-style: dashed;
  }
  .exp h2 {
    margin: 0;
    font-size: 1rem;
    overflow-wrap: anywhere;
  }
  .exp h2 a {
    color: var(--text);
  }
  .desc {
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-muted);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .meta {
    margin: auto 0 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .tag {
    font-size: 0.7rem;
    padding: 0.02rem 0.45rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
  }
  .flag {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--fg-fail);
  }
  .empty-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem;
  }
  .id {
    margin: 0;
    font-size: 0.72rem;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .empty {
    padding: 1.2rem 1.2rem;
    border: 1px dashed var(--card-border);
    border-radius: 0.6rem;
    max-width: 40rem;
  }
  .empty-lede {
    margin: 0 0 0.3rem;
    font-weight: 600;
  }
  .empty .muted {
    margin: 0 0 0.8rem;
    font-size: 0.85rem;
  }
</style>
