<script lang="ts">
  import { page } from '$app/state';
  import { getUsage, type UsageReport } from '$lib/api';
  import { saveBlob } from '$lib/util/download';
  import { errorMessage, fmtUsd, plural } from '$lib/util/format';
  import { replaceUrl } from '$lib/util/replaceUrl';
  import {
    GROUP_LABELS,
    PERIOD_LABELS,
    lastDay,
    parseUsageState,
    rangeFor,
    usageCsv,
    usageParams,
    type PeriodPreset,
    type UsageGroupBy,
    type UsageState,
  } from '$lib/usage/usage';

  // Spend over a period, grouped: estimates from EC2 prices, a bar per group,
  // and a CSV of what is shown. The period and grouping live in the URL so a view can be shared.
  const view = $derived(parseUsageState(page.url.searchParams));
  const range = $derived(rangeFor(view, new Date()));

  function update(next: Partial<UsageState>) {
    const url = new URL(page.url);
    url.search = usageParams({ ...view, ...next }).toString();
    replaceUrl(url);
  }
  function choosePeriod(period: PeriodPreset) {
    if (period === 'custom' && view.period !== 'custom') {
      // Start a custom range from what was shown.
      update({ period, from: range?.from ?? '', to: range ? lastDay(range.to) : '' });
    } else update({ period });
  }

  let usage = $state<UsageReport | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  $effect(() => {
    const r = range;
    const groupBy = view.groupBy;
    error = null;
    if (!r) {
      usage = null;
      loading = false;
      return;
    }
    loading = true;
    let cancelled = false;
    getUsage(r.from, r.to, groupBy)
      .then((u) => {
        if (!cancelled) usage = u;
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

  const max = $derived(Math.max(0, ...(usage?.rows.map((r) => r.total_usd) ?? [])));
  const share = (usd: number) => (max > 0 ? Math.max(0.5, (usd / max) * 100) : 0);

  // While a new period or grouping loads, the previous response stays on screen, so everything
  // describing the rows (the header, the note, the CSV and its name) comes from the response itself
  // (its own grouping and period), never from the controls. The grouping is an open string: one
  // this page doesn't know is shown as itself.
  const shownGroup = $derived((usage?.group_by ?? view.groupBy) as UsageGroupBy);
  const shownGroupLabel = $derived(GROUP_LABELS[shownGroup] ?? shownGroup);

  function downloadCsv() {
    if (!usage || loading || error) return;
    const blob = new Blob([usageCsv(shownGroup, usage.rows, usage.total)], { type: 'text/csv' });
    saveBlob(blob, `biffix-usage-${usage.from}-to-${lastDay(usage.to)}-by-${shownGroup}.csv`);
  }
</script>

<h1>Usage</h1>
<p class="muted lede">
  What runs cost, estimated from EC2 prices (compute and the storage attached to it); a run counts
  in the period it was submitted in.
</p>

<div class="controls">
  <label>
    <span>Period</span>
    <select
      value={view.period}
      onchange={(e) => choosePeriod(e.currentTarget.value as PeriodPreset)}
    >
      {#each Object.entries(PERIOD_LABELS) as [value, label] (value)}
        <option {value}>{label}</option>
      {/each}
    </select>
  </label>
  {#if view.period === 'custom'}
    <label>
      <span>From</span>
      <input
        type="date"
        value={view.from}
        onchange={(e) => update({ from: e.currentTarget.value })}
      />
    </label>
    <label>
      <span>To</span>
      <input type="date" value={view.to} onchange={(e) => update({ to: e.currentTarget.value })} />
    </label>
  {/if}
  <label>
    <span>Group by</span>
    <select
      value={view.groupBy}
      onchange={(e) => update({ groupBy: e.currentTarget.value as UsageGroupBy })}
    >
      {#each Object.entries(GROUP_LABELS) as [value, label] (value)}
        <option {value}>{label}</option>
      {/each}
    </select>
  </label>
  <!-- Only what the controls describe is exported: not while the new view loads or failed. -->
  <button
    class="btn btn-compact"
    onclick={downloadCsv}
    disabled={!usage || usage.rows.length === 0 || loading || !!error}>Download CSV</button
  >
</div>

{#if !range}
  <p class="muted">Choose both dates.</p>
{:else if error}
  <p class="error" role="alert">Couldn't load usage: {error}</p>
{:else if loading && !usage}
  <p class="muted">Loading…</p>
{:else if usage}
  <p class="summary">
    <strong>{fmtUsd(usage.total.total_usd)}</strong>
    <span class="muted"
      >estimated · {plural(usage.total.runs, 'run')} · {usage.from} to {lastDay(usage.to)}</span
    >
  </p>
  {#if usage.rows.length === 0}
    <p class="muted">No run in this period has a cost estimate yet.</p>
  {:else}
    <table class="usage" class:stale={loading} aria-busy={loading}>
      <thead>
        <tr>
          <th scope="col">{shownGroupLabel}</th>
          <th scope="col" class="bar-col"><span class="sr-only">Share</span></th>
          <th scope="col" class="num">Runs</th>
          <th scope="col" class="num">Compute</th>
          <th scope="col" class="num">Storage</th>
          <th scope="col" class="num">Total</th>
        </tr>
      </thead>
      <tbody>
        {#each usage.rows as r (r.key)}
          <tr>
            <th scope="row" class="key">{r.key}</th>
            <td class="bar-col" aria-hidden="true">
              <span class="bar" style:width={`${share(r.total_usd)}%`}></span>
            </td>
            <td class="num">{r.runs}</td>
            <td class="num">{fmtUsd(r.compute_usd)}</td>
            <td class="num">{fmtUsd(r.ebs_usd)}</td>
            <td class="num total">{fmtUsd(r.total_usd)}</td>
          </tr>
        {/each}
      </tbody>
      <tfoot>
        <tr>
          <th scope="row">Total</th>
          <td class="bar-col"></td>
          <td class="num">{usage.total.runs}</td>
          <td class="num">{fmtUsd(usage.total.compute_usd)}</td>
          <td class="num">{fmtUsd(usage.total.ebs_usd)}</td>
          <td class="num total">{fmtUsd(usage.total.total_usd)}</td>
        </tr>
      </tfoot>
    </table>
    {#if shownGroup === 'instance_type'}
      <p class="muted note">A run that used several instance types counts under each.</p>
    {/if}
  {/if}
{/if}

<style>
  .lede {
    margin: -0.25rem 0 1rem;
    max-width: 48rem;
    font-size: 0.9rem;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .controls label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .controls select,
  .controls input {
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
    font-size: 0.85rem;
  }
  .summary {
    margin: 0 0 0.75rem;
    font-size: 1rem;
  }
  .summary strong {
    font-size: 1.4rem;
    margin-right: 0.35rem;
  }
  table.usage.stale {
    opacity: 0.6;
  }
  table.usage {
    width: 100%;
    max-width: 64rem;
    font-size: 0.85rem;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  th.key {
    text-align: left;
    font-weight: 500;
    word-break: break-word;
  }
  .bar-col {
    width: 35%;
  }
  .bar {
    display: block;
    height: 0.6rem;
    border-radius: 0.3rem;
    background: var(--accent);
  }
  .total {
    font-weight: 600;
  }
  tfoot th,
  tfoot td {
    font-weight: 600;
    border-top: 2px solid var(--border);
  }
  .note {
    font-size: 0.8rem;
  }
  @media (max-width: 640px) {
    .bar-col {
      display: none;
    }
  }
</style>
