<script lang="ts">
  // A run's samples × steps: each sample's row with a cell per per-sample step, then one row for the
  // whole-study steps (MultiQC, joint calling…). Polled while the run is live.
  import { getRunSamples, type SampleGrid } from '#lib/api/index.js';
  import { startPoll } from '#lib/util/poll.js';
  import SampleStateChip from './SampleStateChip.svelte';
  import { stepLabel } from './stepLabel';
  import { plural } from '#lib/util/format.js';

  type Cell = SampleGrid['study'][string];

  let {
    runId,
    live,
    id,
  }: { runId: string; live: boolean; /** The section's anchor id. */ id?: string } = $props();

  let grid = $state<SampleGrid | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    const id = runId;
    const polling = live;
    return startPoll({
      fetch: () => getRunSamples(id),
      onData: (g) => {
        grid = g;
        error = null;
      },
      onError: (m) => (error = m),
      until: () => !polling,
      intervalMs: 10_000,
    });
  });

  /** A step's label: the author's, else a readable form of the engine's name. */
  const labelOf = (s: { group: string; label: string; authored?: boolean }) =>
    stepLabel(s.group, { label: s.label, authored: s.authored });
  const perSample = $derived(grid?.steps.filter((s) => s.scope === 'sample') ?? []);
  const studySteps = $derived(grid?.steps.filter((s) => s.scope === 'study') ?? []);
  const summary = $derived.by(() => {
    if (!grid) return '';
    const n = (s: string) => grid!.samples.filter((r) => r.state === s).length;
    const parts = [`${n('done')} done`];
    if (n('running')) parts.push(`${n('running')} running`);
    if (n('failed')) parts.push(`${n('failed')} failed`);
    if (n('waiting')) parts.push(`${n('waiting')} waiting`);
    if (n('stopped')) parts.push(`${n('stopped')} stopped when the run ended`);
    return parts.join(' · ');
  });

  /** A sample row's progress: where it stopped, or its steps finished of those it has so far. */
  function rowProgress(row: SampleGrid['samples'][number]): string {
    const failedAt = perSample.find((s) => row.cells[s.group]?.state === 'failed');
    if (failedAt) return `failed at ${labelOf(failedAt)}`;
    if (row.state === 'waiting') return 'waiting';
    if (row.state === 'stopped' && Object.keys(row.cells).length === 0) return 'not run';
    const cells = Object.values(row.cells);
    const finished = cells.filter((c) => c.state === 'done' || c.state === 'reused').length;
    return `${finished}/${cells.length}`;
  }

  /** How many times the cell's tasks were retried (attempts beyond one per task). */
  const retries = (c: Cell) => c.attempts - c.tasks;
  const retriesWord = (n: number) => plural(n, 'retry', 'retries');
</script>

{#if grid && grid.samples.length > 0}
  <section class="grid-card" {id} aria-labelledby="samples-h">
    <h2 id="samples-h">
      Samples <span class="count"
        >{grid.samples.length}{grid.declared ? '' : ' seen so far'} · {summary}</span
      >
    </h2>
    <div class="scroll">
      <table>
        <thead>
          <tr>
            <th scope="col">Sample</th>
            {#each perSample as step (step.group)}
              <th scope="col" title={step.group}>{labelOf(step)}</th>
            {/each}
            <th scope="col" class="num">Progress</th>
          </tr>
        </thead>
        <tbody>
          {#each grid.samples as row (row.id)}
            <tr class:failed-row={row.state === 'failed'}>
              <th scope="row">{row.id}</th>
              {#each perSample as step (step.group)}
                {@const cell = row.cells[step.group]}
                {@const retried = cell ? retries(cell) : 0}
                <td>
                  <SampleStateChip value={cell?.state ?? null} missing="not started"
                    >{#if retried > 0}<span class="retry" title={retriesWord(retried)}
                        ><span aria-hidden="true">{' ↻'}{retried}</span><span class="sr-only"
                          >{`, ${retriesWord(retried)}`}</span
                        ></span
                      >{/if}</SampleStateChip
                  >
                </td>
              {/each}
              <td class="num" class:bad={row.state === 'failed'}
                >{rowProgress(
                  row,
                )}{#if row.state === 'stopped' && Object.keys(row.cells).length > 0}<span
                    class="muted"
                  >
                    · run ended</span
                  >{/if}</td
              >
            </tr>
          {/each}
          {#if studySteps.length}
            <tr class="study">
              <th scope="row">Whole study</th>
              <td colspan={perSample.length + 1}>
                {#each studySteps as step (step.group)}
                  {@const cell = grid.study[step.group]}
                  <span class="study-step" title={step.group}
                    >{labelOf(step)}
                    <SampleStateChip
                      value={cell?.state ?? 'none'}
                      label={cell ? null : 'not started'}
                    /></span
                  >
                {/each}
              </td>
            </tr>
          {/if}
        </tbody>
      </table>
    </div>
    {#if grid.unattributed > 0}
      <p class="note">
        {plural(grid.unattributed, 'step')} biffix could not match to a sample.
      </p>
    {/if}
    <p class="legend">
      <span class="sr-only">Legend:</span>
      <SampleStateChip value="done" />
      <SampleStateChip value="running" />
      <SampleStateChip value="waiting" />
      <SampleStateChip value="reused" label="reused from an earlier analysis" />
      <SampleStateChip value="failed" />
      <span class="retry-key">↻1 = retried once</span>
    </p>
  </section>
{:else if error}
  <p class="error">Couldn't load samples: {error}</p>
{/if}

<style>
  .grid-card {
    margin: 0 0 1.25rem;
  }

  h2 {
    font-size: 1rem;
    margin: 0 0 0.5rem;
  }
  .count {
    font-weight: 400;
    color: var(--text-muted);
    font-size: 0.82rem;
  }
  .scroll {
    overflow-x: auto;
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-size: 0.8rem;
  }
  th,
  td {
    text-align: left;
    padding: 0.35rem 0.6rem;
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
  }
  thead th {
    background: var(--th-bg);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--text-muted);
    font-weight: 600;
  }
  /* Row headers read as the sample's name, not as a column header (the app's global th style). */
  tbody th {
    font-weight: 600;
    text-transform: none;
    letter-spacing: normal;
    font-size: 0.8rem;
    background: none;
    color: var(--text);
  }
  .num {
    text-align: right;
  }
  .failed-row th {
    color: var(--fg-fail);
  }
  .bad {
    color: var(--fg-fail);
    font-weight: 600;
  }
  .study th {
    color: var(--text-muted);
  }
  .study-step {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    margin-right: 1rem;
    color: var(--text-muted);
  }
  .retry {
    font-size: 0.7rem;
  }
  .note,
  .legend {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin: 0.4rem 0 0;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    align-items: center;
  }
  .retry-key {
    margin-left: 0.3rem;
  }
</style>
