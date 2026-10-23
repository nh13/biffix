<script lang="ts">
  // A running analysis's progress under its status: the bar, how long it has
  // run against how long its pipeline usually takes, the cost so far, and (with no samplesheet to
  // count against) a strip with each step's state in pipeline order. The arithmetic is
  // `liveProgress`; the bar never claims more than the run's facts say.
  import type { RunDetail, WorkflowEstimate } from '$lib/api';
  import SampleStateChip from './SampleStateChip.svelte';
  import { liveTimeFacts, stepChips, type StepState } from './liveProgress';
  import { stepLabel } from './stepLabel';

  interface Props {
    run: RunDetail;
    /** How far along the bar is (0..1); no bar when null. */
    fraction: number | null;
    /** The bar's total is still growing (the pipeline is finding more steps). */
    growing?: boolean;
    /** Show the steps strip (when there is no samples grid to show progress instead). */
    showSteps?: boolean;
    /** The pipeline's typical run, from its past runs; none, and only the time so far is said. */
    typical?: WorkflowEstimate['typical'] | null;
    /** The author's step labels. */
    labels?: Record<string, string> | null;
    /** "≈ $3.84 so far", when a job is priced. */
    costSoFar?: string | null;
  }
  let {
    run,
    fraction,
    growing = false,
    showSteps = false,
    typical = null,
    labels = null,
    costSoFar = null,
  }: Props = $props();

  // The clock the time so far reads: the layout's poll replaces `run` every few seconds, and this
  // ticks too, so the line moves on even between polls.
  let now = $state(Math.floor(Date.now() / 1000));
  $effect(() => {
    const timer = setInterval(() => (now = Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(timer);
  });
  $effect(() => {
    void run;
    now = Math.floor(Date.now() / 1000);
  });

  const time = $derived(liveTimeFacts(run, now, typical));
  const chips = $derived(showSteps ? stepChips(run.dag) : []);
  const pct = $derived(fraction == null ? null : Math.round(fraction * 100));

  const STATE_WORDS: Record<StepState, string> = {
    done: 'done',
    running: 'running',
    waiting: 'waiting',
    failed: 'failed',
    none: 'not started',
  };
</script>

{#if pct !== null}
  <div
    class="bar"
    class:growing
    role="progressbar"
    aria-label="Progress"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={pct}
    aria-valuetext={growing ? `${pct}% of the steps known so far; more may come` : `${pct}% done`}
  >
    <span style:width={`${pct}%`}></span>
  </div>
{/if}
{#if time}
  <p class="time" data-testid="analysis-time">
    {time.elapsed}{#if time.usual}{' · '}{time.usual}{/if}{#if costSoFar}{' · '}{costSoFar}{/if}
  </p>
{/if}
{#if chips.length}
  <ol class="steps" aria-label="Steps" data-testid="steps-strip">
    {#each chips as chip (chip.group)}
      <li title={chip.group}>
        <span class="name">{stepLabel(chip.group, { labels })}</span>
        <SampleStateChip
          value={chip.state}
          label={chip.total > 0 && chip.state !== 'done'
            ? `${STATE_WORDS[chip.state]} · ${chip.done}/${chip.total}`
            : STATE_WORDS[chip.state]}
        />
      </li>
    {/each}
  </ol>
{/if}

<style>
  .bar {
    height: 0.45rem;
    margin-top: 0.5rem;
    border-radius: 0.3rem;
    background: var(--hover);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  /* A total still growing: the unfilled rest is striped, so it never reads as a fixed end. */
  .bar.growing {
    background: repeating-linear-gradient(-45deg, var(--hover) 0 6px, var(--border) 6px 12px);
  }
  .time {
    margin: 0.45rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem 0.9rem;
    margin: 0.6rem 0 0;
    padding: 0;
    list-style: none;
  }
  .steps li {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.8rem;
  }
  .steps li:not(:last-child)::after {
    content: '→' / '';
    margin-left: 0.4rem;
    color: var(--text-muted);
  }
</style>
