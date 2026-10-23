<script lang="ts">
  import { getStatusHistory, type StatusHistory, type StatusWindow } from '$lib/api';
  import { startPoll } from '$lib/util/poll';
  import { barTier, bucketTimeline, historyByComponent, uptime } from '$lib/status/history';
  import { liveStatus, watchStatus } from '$lib/status/live.svelte';

  // The live report comes from the status poll the layout's health badge shares (one GET /status
  // per interval serves both). It reflects every probe: once the control plane stops answering,
  // the page says so, keeping the last report below as stale.
  $effect(() => watchStatus());
  const report = $derived(liveStatus.report);
  const error = $derived(liveStatus.error);
  const checkedAt = $derived(liveStatus.checkedAt);
  const loading = $derived(!liveStatus.settled);

  let history = $state<StatusHistory | null>(null);

  // Controls.
  let search = $state('');
  type StatusFilter = 'all' | 'ok' | 'degraded' | 'down';
  let statusFilter = $state<StatusFilter>('all');
  let historyWindow = $state<StatusWindow>('1d');
  let infoOpen = $state<string | null>(null);

  const HISTORY_MS = 60000; // history refresh

  // History poll, re-run whenever the window changes. The previous window's bars go at once, so a
  // slow or failed first fetch never draws them under the new window's chip. Best-effort: no error
  // surfaced (the live banner still renders), so no onError/onSettled.
  $effect(() => {
    const w = historyWindow;
    history = null;
    return startPoll({
      fetch: () => getStatusHistory(w),
      onData: (h) => (history = h),
      intervalMs: HISTORY_MS,
    });
  });

  // Display labels; the internal component ids stay stable (renaming would orphan history samples).
  const COMPONENT_LABELS: Record<string, string> = { api: 'API', reconciler: 'Background Sync' };
  const prettyName = (n: string) =>
    COMPONENT_LABELS[n] ?? n.replace(/_/g, ' ').replace(/\b\w/g, (m) => m.toUpperCase());

  // One-sentence description per component (the ⓘ button).
  const DESCRIPTIONS: Record<string, string> = {
    api: 'The control-plane API that serves this dashboard and all run data.',
    database:
      'The Postgres database holding the event log and the run/task read model everything is served from.',
    event_ingest:
      'Folds engine and AWS events into the run/task read model; degrades if it falls behind the event log.',
    batch_compute:
      'AWS Batch — the job queues and compute environments that run coordinator and task jobs.',
    reconciler:
      'The background job that polls AWS to backfill missed terminal events and record these status samples.',
    alarms:
      "biffix's CloudWatch alarms (API errors, ingest backlog and failures, background sync); they also email the alert subscribers in Settings.",
  };

  const overall = $derived(report?.status ?? 'unknown');
  const overallLabel: Record<string, string> = {
    ok: 'All systems operational',
    degraded: 'Partial degradation',
    down: 'Service disruption',
    unknown: 'Checking…',
  };
  const statusLabel: Record<string, string> = {
    ok: 'Operational',
    degraded: 'Degraded',
    down: 'Down',
  };
  const filters: { key: StatusFilter; label: string }[] = [
    { key: 'all', label: 'All' },
    { key: 'ok', label: 'Operational' },
    { key: 'degraded', label: 'Degraded' },
    { key: 'down', label: 'Down' },
  ];
  const windows: StatusWindow[] = ['1h', '1d', '1w', '90d'];

  const q = $derived(search.trim().toLowerCase());
  const visibleComponents = $derived(
    (report?.components ?? []).filter((c) => {
      const matchesSearch =
        !q || prettyName(c.name).toLowerCase().includes(q) || c.name.toLowerCase().includes(q);
      const matchesStatus = statusFilter === 'all' || c.status === statusFilter;
      return matchesSearch && matchesStatus;
    }),
  );

  // Re-derived when the history changes (or the live report ticks, which moves `now` along).
  const bucketTimelineNow = $derived.by(() => {
    void checkedAt;
    return bucketTimeline(history, Date.now());
  });
  const byComponent = $derived(historyByComponent(history));

  const fmtBucket = (t: number) => new Date(t * 1000).toLocaleString();
</script>

<h1>Service status</h1>

{#if loading}
  <p class="muted">Loading…</p>
{:else if error}
  <div class="banner down" role="alert">
    <span class="dot"></span>
    <span class="banner-label">Couldn't reach the control plane</span>
    <!-- `checkedAt` is the last check that reached it, not the last try. -->
    {#if checkedAt}<span class="checked">last reached {checkedAt.toLocaleTimeString()}</span>{/if}
  </div>
  <p class="muted err-detail">
    {error}{#if report}. The services below are as of the last check.{/if}
  </p>
{:else if report}
  <div class={`banner ${overall}`}>
    <span class="dot"></span>
    <span class="banner-label">{overallLabel[overall]}</span>
    {#if checkedAt}<span class="checked">checked {checkedAt.toLocaleTimeString()}</span>{/if}
  </div>
{/if}

{#if !loading && report}
  <div class="controls">
    <input
      class="search"
      type="search"
      aria-label="Search services"
      placeholder="Search services…"
      bind:value={search}
    />
    <div class="chips" role="group" aria-label="Filter by status">
      {#each filters as f}
        <button
          class:active={statusFilter === f.key}
          aria-pressed={statusFilter === f.key}
          onclick={() => (statusFilter = f.key)}>{f.label}</button
        >
      {/each}
    </div>
    <div class="chips windows" role="group" aria-label="History window">
      {#each windows as w}
        <button
          class:active={historyWindow === w}
          aria-pressed={historyWindow === w}
          onclick={() => (historyWindow = w)}>{w}</button
        >
      {/each}
    </div>
  </div>

  <div class="components">
    {#each visibleComponents as c}
      {@const byT = byComponent.get(c.name)}
      {@const up = uptime(byT)}
      <div class="component">
        <div class="row">
          <span class={`dot ${c.status}`}></span>
          <span class="name">{prettyName(c.name)}</span>
          {#if DESCRIPTIONS[c.name]}
            <button
              class="info"
              aria-label={`About ${prettyName(c.name)}`}
              aria-expanded={infoOpen === c.name}
              title={DESCRIPTIONS[c.name]}
              onclick={() => (infoOpen = infoOpen === c.name ? null : c.name)}>ⓘ</button
            >
          {/if}
          {#if up != null}<span class="uptime">{up.toFixed(2)}% uptime</span>{/if}
          <span class={`status ${c.status}`}>{statusLabel[c.status] ?? c.status}</span>
        </div>
        {#if infoOpen === c.name && DESCRIPTIONS[c.name]}
          <span class="info-text">{DESCRIPTIONS[c.name]}</span>
        {/if}
        {#if c.detail}<span class="detail">{c.detail}</span>{/if}
        {#if history}
          <div class="bars">
            {#each bucketTimelineNow as t, i}
              {@const b = byT?.get(t)}
              {@const isCurrent = i === bucketTimelineNow.length - 1}
              <span
                class={`bar ${barTier(b, history.expected_per_bucket, isCurrent, error ? null : c.status)}`}
                title={`${fmtBucket(t)}${b ? ` — ${b.ok} ok, ${b.degraded} degraded, ${b.down} down` : isCurrent ? (error ? ' — now (unreachable)' : ' — now (live)') : ' — no data'}`}
              ></span>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
    {#if visibleComponents.length === 0}
      <p class="muted">No services match.</p>
    {/if}
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.9rem 1.1rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    margin-bottom: 1rem;
    background: var(--bg-content);
  }
  .banner-label {
    font-weight: 600;
    font-size: 1.05rem;
  }
  .checked {
    margin-left: auto;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .err-detail {
    font-size: 0.8rem;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    margin-bottom: 1rem;
  }
  .search {
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font-size: 0.85rem;
    min-width: 200px;
  }
  .chips {
    display: inline-flex;
    gap: 0.25rem;
  }
  .chips.windows {
    margin-left: auto;
  }
  .chips button {
    padding: 0.28rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text-muted);
    font-size: 0.78rem;
    cursor: pointer;
  }
  .chips button.active {
    color: var(--text);
    border-color: var(--accent, #4a7cf5);
    background: color-mix(in srgb, var(--accent, #4a7cf5) 12%, transparent);
  }
  .components {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .component {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.7rem 0.95rem;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.7rem;
  }
  .name {
    font-weight: 500;
  }
  .info {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.85rem;
    padding: 0;
    line-height: 1;
  }
  .info:hover {
    color: var(--text);
  }
  .info-text {
    font-size: 0.75rem;
    color: var(--text-muted);
    padding-left: 1.4rem;
  }
  .uptime {
    margin-left: auto;
    font-size: 0.72rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .status {
    font-size: 0.78rem;
  }
  .detail {
    font-size: 0.72rem;
    color: var(--text-muted);
    padding-left: 1.4rem;
  }
  .bars {
    display: flex;
    gap: 2px;
    height: 1.6rem;
    align-items: stretch;
  }
  .bar {
    flex: 1 1 0;
    min-width: 1px;
    border-radius: 1px;
    background: var(--border);
  }
  .bar.ok {
    background: #30a46c;
  }
  .bar.degraded {
    background: #f5a524;
  }
  .bar.down {
    background: #e5484d;
  }
  .bar.none {
    background: color-mix(in srgb, var(--border) 60%, transparent);
  }
  /* A completed bucket that was materially under-sampled: a data gap, drawn grey (hatched to
     distinguish it from an all-absent 'none' bar) rather than a misleading green. */
  .bar.partial {
    background: repeating-linear-gradient(
      -45deg,
      var(--border),
      var(--border) 3px,
      transparent 3px,
      transparent 6px
    );
  }
  .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    flex: 0 0 auto;
    background: var(--text-muted);
  }
  .dot.ok,
  .banner.ok .dot {
    background: #30a46c;
  }
  .dot.degraded,
  .banner.degraded .dot {
    background: #f5a524;
  }
  .dot.down,
  .banner.down .dot {
    background: #e5484d;
  }
  .status.ok {
    color: var(--fg-ok);
  }
  .status.degraded {
    color: var(--fg-warn);
  }
  .status.down {
    color: var(--fg-fail);
  }
  .banner.ok {
    border-color: #30a46c55;
  }
  .banner.degraded {
    border-color: #f5a52455;
  }
  .banner.down {
    border-color: #e5484d55;
  }
</style>
