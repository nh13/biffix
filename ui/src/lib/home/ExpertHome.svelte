<script lang="ts">
  // The Expert view's home: run counts by state, each a link to the runs list, and quick links.
  // The Lab view's home is `LabHome`.
  import { listRuns } from '#lib/api/index.js';
  import { errorMessage } from '#lib/util/format.js';

  // Counts come from each filter's server-side `total`, not a client count of a capped page.
  let active = $state<number | null>(null);
  let succeeded = $state<number | null>(null);
  let failed = $state<number | null>(null);
  let total = $state<number | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  $effect(() => {
    loading = true;
    error = null;
    const one = { limit: 1 };
    Promise.all([
      listRuns('all', one),
      listRuns('active', one),
      listRuns('succeeded', one),
      listRuns('failed', one),
    ])
      .then(([a, ac, s, f]) => {
        total = a.total;
        active = ac.total;
        succeeded = s.total;
        failed = f.total;
      })
      .catch((e) => (error = errorMessage(e)))
      .finally(() => (loading = false));
  });

  const show = (n: number | null) => (loading || n == null ? '–' : n);
</script>

<section class="hero">
  <div>
    <h1>biffix</h1>
    <p class="lede">Launch and follow your Snakemake &amp; Nextflow runs on AWS Batch.</p>
  </div>
  <a class="new-run" href="/runs/new">New run</a>
</section>

{#if error}
  <p class="error" role="alert">Couldn't reach the control plane: {error}</p>
{:else}
  <div class="cards">
    <a class="card" href="/runs?filter=active">
      <span class="num">{show(active)}</span>
      <span class="cap">Active</span>
    </a>
    <a class="card" href="/runs?filter=succeeded">
      <span class="num">{show(succeeded)}</span>
      <span class="cap">Succeeded</span>
    </a>
    <a class="card" href="/runs?filter=failed">
      <span class="num ok-bad" class:bad={(failed ?? 0) > 0}>{show(failed)}</span>
      <span class="cap">Failed</span>
    </a>
    <a class="card" href="/runs">
      <span class="num">{show(total)}</span>
      <span class="cap">Total runs</span>
    </a>
  </div>

  <div class="quick">
    <h2>Quick links</h2>
    <div class="links">
      <a href="/runs">Browse all runs →</a>
      <a href="/status">Service status →</a>
      <a href="/workflows">Registered workflows →</a>
    </div>
  </div>
{/if}

<style>
  .hero {
    margin-bottom: 1.5rem;
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .new-run {
    padding: 0.45rem 1rem;
    border-radius: 0.45rem;
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    text-decoration: none;
    font-size: 0.9rem;
    white-space: nowrap;
  }
  .new-run:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .lede {
    color: var(--text-muted);
    margin: 0.25rem 0 0;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 0.75rem;
    margin-bottom: 0.5rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 1rem 1.1rem;
    border: 1px solid var(--border);
    text-decoration: none;
    color: var(--text);
  }
  .card:hover {
    background: var(--th-bg);
    text-decoration: none;
  }
  .num {
    font-size: 1.6rem;
    font-weight: 600;
    line-height: 1;
  }
  .num.bad {
    color: var(--fg-fail);
  }
  .cap {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--text-muted);
  }
  .quick {
    display: flex;
    align-items: center;
    gap: 1.25rem;
    flex-wrap: wrap;
    margin-top: 1rem;
  }
  .quick h2 {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.9rem;
    font-weight: 600;
  }
  .links {
    display: flex;
    flex-direction: row;
    flex-wrap: wrap;
    gap: 1rem;
  }
</style>
