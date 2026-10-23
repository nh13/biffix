<script lang="ts">
  // *What went wrong* on a failed analysis: which sample and step failed and why,
  // in plain words; what to try; *Try again*, *Copy report for support* and *Ask <name>*; and a
  // *Why?* panel (open) with the likely cause, the tries and the last lines of its log. Logs beyond
  // that tail are in the Expert view.
  import type { FailureSummary, RunDetail, WorkflowDetail } from '$lib/api';
  import { CopiedFlag } from '$lib/util/clipboard.svelte';
  import { jobsHref } from '$lib/runs/jobsUrl';
  import { parseSchema } from '$lib/workflows/schema';
  import { causeWords, failureLine, groupByCause, triesText } from './failure';
  import { authorLabels, stepLabel } from './stepLabel';
  import { runName } from '$lib/runs/runName';
  import { askHref, REPORT_LOG_LINES, supportReport } from './supportReport';
  import TryAgain from './TryAgain.svelte';
  import AttemptStory from './AttemptStory.svelte';
  import type { TryAgainMode } from './tryAgain';
  import { plural } from '$lib/util/format';

  interface Props {
    run: RunDetail;
    summary: FailureSummary | null;
    /** Why the summary could not be loaded. */
    error: string | null;
    mode: TryAgainMode;
    version: WorkflowDetail | null;
    /** Whether Try again is offered (the run names its version). */
    canTryAgain: boolean;
    /** The failing sample a fix-samples Try again highlights in the launch form. */
    sample?: string | null;
  }
  let { run, summary, error, mode, version, canTryAgain, sample = null }: Props = $props();

  const groups = $derived(groupByCause(summary?.items ?? []));
  const shown = $derived(groups.flatMap((g) => g.items));
  const more = $derived(summary ? summary.failed_steps - shown.length : 0);
  // One "what to try" per cause (the server words it the same for every failure of a cause).
  const advice = $derived([
    ...new Set(groups.map((g) => g.items[0].advice).filter((a) => a && a.trim())),
  ]);
  const support = $derived(summary?.support ?? null);
  const name = $derived(runName(run));
  const pageUrl = $derived(
    `${typeof window === 'undefined' ? '' : window.location.origin}/runs/${encodeURIComponent(run.run_id)}`,
  );
  // The author's step labels, when the version's form has them.
  const labels = $derived(authorLabels(version?.form_schema));
  const step = (raw: string) => stepLabel(raw, { labels });
  const diagnosis = $derived(shown[0] ? failureLine(shown[0], labels) : null);

  const copied = new CopiedFlag();
  let copyError = $state<string | null>(null);
  async function copyReport() {
    copyError = null;
    const text = supportReport({
      run,
      url: pageUrl,
      items: summary?.items ?? [],
      // A report copied before the failures are in (or when they could not be read) says so.
      failuresUnread: summary ? null : (error ?? 'not loaded yet'),
      fields: version ? parseSchema(version.form_schema).fields : [],
    });
    if (!(await copied.copy(text, 'report')))
      copyError = "Your browser didn't allow copying; try again, or use the Expert view's logs.";
  }
</script>

<section id="what-went-wrong" class="wrong" aria-labelledby="wrong-h">
  <h2 id="wrong-h">What went wrong</h2>
  {#if error}
    <p class="muted">Couldn't work out why: {error}</p>
  {:else if !summary}
    <p class="muted">Working out why…</p>
  {:else if shown.length === 0}
    <p>It stopped, and biffix recorded no reason.</p>
  {/if}

  {#each shown as item, i (i)}
    {@const tries = triesText(item.attempts)}
    <p class="line">
      {#if item.sample}<strong class="sample">{item.sample}</strong>{' · '}{/if}<strong
        title={item.step}>{step(item.step)}</strong
      >{' · '}{item.message}{#if tries}{' '}<span class="muted">({tries})</span>{/if}
    </p>
  {/each}
  {#if more > 0}<p class="muted">…and {plural(more, 'more failed step')}.</p>{/if}

  {#if advice.length}
    <div class="advice">
      <strong>What to try:</strong>
      {#each advice as a}<p>{a}</p>{/each}
    </div>
  {/if}

  <div class="actions">
    {#if canTryAgain}
      <TryAgain {run} {mode} {version} {sample} primary hint />
    {/if}
    <span class="action">
      <button class="btn" onclick={copyReport}
        >{copied.is('report') ? 'Copied' : 'Copy report for support'}</button
      >
      <span class="hint"
        >The analysis link, sample, step, cause and last log lines. No secrets.</span
      >
      {#if copyError}<span class="error" role="alert">{copyError}</span>{/if}
    </span>
    {#if support}
      <span class="action">
        <a class="btn" href={askHref(support, name, pageUrl, diagnosis)}>Ask {support.name}</a>
        {#if support.url}<a class="help" href={support.url} target="_blank" rel="noopener"
            >Help page ↗</a
          >{/if}
      </span>
    {:else}
      <p class="hint no-contact">Ask whoever runs biffix for your lab.</p>
    {/if}
  </div>
  {#if copied.is('report')}<span class="sr-only" role="status">Report copied</span>{/if}

  {#if shown.length}
    <details class="why" open>
      <summary>Why?</summary>
      {#each shown as item, i (i)}
        <dl>
          {#if item.sample}<dt>Which sample</dt>
            <dd>{item.sample}</dd>{/if}
          <dt>Which step</dt>
          <dd title={item.step}>{step(item.step)}</dd>
          <dt>Likely cause</dt>
          <dd>{causeWords(item.cause, item.stage)}</dd>
          <dt>Tries</dt>
          <dd>
            <AttemptStory
              runId={run.run_id}
              taskKey={item.task_key}
              count={item.attempts}
              runEnded={run.terminal !== false}
            />
          </dd>
          {#if item.log_tail.length}
            <dt>Last lines from the step</dt>
            <dd><pre>{item.log_tail.slice(-REPORT_LOG_LINES).join('\n')}</pre></dd>
          {:else if item.reason}
            <dt>What AWS said</dt>
            <dd><code>{item.reason}</code></dd>
          {/if}
          {#if item.task_key}
            <dt class="sr-only">Full logs</dt>
            <dd>
              <a href={jobsHref(run.run_id, { job: item.task_key })}
                >Open in Expert view for full logs →</a
              >
            </dd>
          {/if}
        </dl>
      {/each}
    </details>
  {/if}
</section>

<style>
  .wrong {
    margin: 0 0 1.5rem;
    padding: 0.8rem 1rem 1rem;
    border: 1px solid var(--fg-fail);
    border-left-width: 4px;
    border-radius: 0.55rem;
  }
  h2 {
    margin: 0 0 0.5rem;
  }
  .line {
    margin: 0 0 0.3rem;
    font-size: 0.95rem;
  }
  .sample {
    color: var(--fg-fail);
  }
  .advice {
    margin: 0.5rem 0 0;
    font-size: 0.88rem;
  }
  .advice p {
    display: inline;
    margin: 0 0.3rem 0 0;
  }
  .actions {
    align-items: flex-start;
    gap: 0.8rem 1.4rem;
    margin: 0.9rem 0 0.2rem;
  }
  .action {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25rem;
    max-width: 17rem;
  }
  .no-contact {
    margin: 0.5rem 0 0;
  }
  .help {
    font-size: 0.78rem;
    color: var(--accent);
    text-decoration: underline;
  }
  .why {
    margin-top: 0.9rem;
    padding-top: 0.7rem;
    border-top: 1px solid var(--border);
  }
  .why summary {
    cursor: pointer;
    color: var(--accent);
    font-weight: 600;
  }
  dl {
    display: grid;
    grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr);
    gap: 0.3rem 1rem;
    margin: 0.6rem 0 0;
    font-size: 0.85rem;
  }
  dl + dl {
    padding-top: 0.6rem;
    border-top: 1px dashed var(--border);
  }
  dt {
    color: var(--text-muted);
  }
  /* A phone stacks each label over its value, so a value (a log) keeps the width. */
  @media (max-width: 640px) {
    dl {
      grid-template-columns: minmax(0, 1fr);
      gap: 0.1rem;
    }
    dd + dt {
      margin-top: 0.4rem;
    }
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  dd a {
    color: var(--accent);
    text-decoration: underline;
  }
  pre {
    margin: 0;
    padding: 0.5rem 0.65rem;
    max-height: 16rem;
    overflow: auto;
    background: var(--hover);
    border-radius: 0.35rem;
    font-family: var(--mono);
    font-size: 0.74rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  code {
    font-family: var(--mono);
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }
  .error {
    font-size: 0.78rem;
  }
</style>
