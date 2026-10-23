<script lang="ts">
  // One FastQC module drawn as FastQC draws it: Per base sequence quality as a box per base over
  // the good / fair / poor bands, any other numeric module as lines. The geometry is `fastqc.ts`'s;
  // this only draws it. The numbers are one click away in the viewer (Show the numbers).
  import type { FastqcModule } from '$lib/api';
  import {
    DEFAULT_FRAME as F,
    lineData,
    linePlot,
    qualityBoxes,
    qualityPlot,
    type ModuleView,
  } from './fastqc';

  interface Props {
    module: FastqcModule;
    view: Exclude<ModuleView, 'table'>;
  }
  let { module, view }: Props = $props();

  const quality = $derived.by(() => {
    const boxes = view === 'quality' ? qualityBoxes(module) : null;
    return boxes ? qualityPlot(boxes) : null;
  });
  const lines = $derived.by(() => {
    const data = view === 'lines' ? lineData(module) : null;
    return data ? { data, plot: linePlot(data) } : null;
  });
  /** The axes of whichever plot this is. */
  const axes = $derived(quality ?? lines?.plot ?? null);
  const xName = $derived(module.columns[0] ?? '');
  const label = $derived(
    quality
      ? `${module.name}: quality (0 to ${quality.yMax}) by ${xName.toLowerCase()} in read, a box per position`
      : lines
        ? `${module.name}: ${lines.data.series.map((s) => s.name).join(', ')} by ${xName.toLowerCase()}`
        : module.name,
  );
  const bottom = F.height - F.bottom;
</script>

<figure>
  <svg viewBox="0 0 {F.width} {F.height}" role="img" aria-label={label}>
    {#if quality}
      {#each quality.bands as b}
        <rect
          class="band {b.tone}"
          x={F.left}
          y={b.y}
          width={F.width - F.left - F.right}
          height={b.height}
        />
      {/each}
    {/if}
    <g class="axis">
      {#each axes?.yTicks ?? [] as t}
        <line x1={F.left} x2={F.width - F.right} y1={t.at} y2={t.at} class="grid" />
        <text x={F.left - 6} y={t.at} text-anchor="end" dominant-baseline="middle">{t.label}</text>
      {/each}
      {#each axes?.xTicks ?? [] as t}
        <text x={t.at} y={bottom + 14} text-anchor="middle">{t.label}</text>
      {/each}
      <line x1={F.left} x2={F.width - F.right} y1={bottom} y2={bottom} class="edge" />
      <line x1={F.left} x2={F.left} y1={F.top} y2={bottom} class="edge" />
      <text x={(F.left + F.width - F.right) / 2} y={F.height - 4} text-anchor="middle" class="name"
        >{xName}</text
      >
    </g>
    {#if quality}
      {#each quality.boxes as b}
        <line class="whisker" x1={b.cx} x2={b.cx} y1={b.yP90} y2={b.yP10} />
        <rect
          class="box"
          x={b.x}
          y={b.yUpper}
          width={b.width}
          height={Math.max(0.5, b.yLower - b.yUpper)}
        />
        <line class="median" x1={b.x} x2={b.x + b.width} y1={b.yMedian} y2={b.yMedian} />
      {/each}
      <path class="mean" d={quality.meanPath} />
    {:else if lines}
      {#each lines.plot.paths as p, i}
        <path class="series s{i % 6}" d={p.d} />
      {/each}
    {/if}
  </svg>
  {#if quality}
    <figcaption>
      <span class="key"><span class="swatch box-key"></span>Middle half of the reads</span>
      <span class="key"><span class="swatch median-key"></span>Median</span>
      <span class="key"><span class="swatch mean-key"></span>Mean</span>
    </figcaption>
  {:else if lines && lines.data.series.length > 1}
    <figcaption>
      {#each lines.data.series as s, i}
        <span class="key"><span class="swatch line-key s{i % 6}"></span>{s.name}</span>
      {/each}
    </figcaption>
  {/if}
</figure>

<style>
  figure {
    margin: 0;
    --s0: #2f5bd0;
    --s1: #b42318;
    --s2: #1a7f37;
    --s3: #8a5a00;
    --s4: #5b45c9;
    --s5: #0e7490;
  }
  :global(html.dark) figure {
    --s0: #8fb0ff;
    --s1: #f2a5a0;
    --s2: #7bd69a;
    --s3: #e6cf7a;
    --s4: #b8a9ff;
    --s5: #67d4e8;
  }
  svg {
    display: block;
    width: 100%;
    max-width: 52rem;
    height: auto;
    font-size: 11px;
  }
  .band.good {
    fill: color-mix(in srgb, var(--fg-ok) 14%, transparent);
  }
  .band.fair {
    fill: color-mix(in srgb, var(--fg-warn) 14%, transparent);
  }
  .band.poor {
    fill: color-mix(in srgb, var(--fg-fail) 12%, transparent);
  }
  .axis text {
    fill: var(--text-muted);
  }
  .axis .name {
    fill: var(--text);
  }
  .grid {
    stroke: var(--border);
  }
  .edge {
    stroke: var(--text-muted);
  }
  .whisker {
    stroke: var(--text);
    stroke-width: 1;
  }
  .box {
    fill: var(--bg-content);
    stroke: var(--text);
    stroke-width: 1;
  }
  .median {
    stroke: var(--fg-fail);
    stroke-width: 1.5;
  }
  .mean {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.5;
  }
  .series {
    fill: none;
    stroke-width: 1.75;
  }
  .s0 {
    stroke: var(--s0);
    background: var(--s0);
  }
  .s1 {
    stroke: var(--s1);
    background: var(--s1);
  }
  .s2 {
    stroke: var(--s2);
    background: var(--s2);
  }
  .s3 {
    stroke: var(--s3);
    background: var(--s3);
  }
  .s4 {
    stroke: var(--s4);
    background: var(--s4);
  }
  .s5 {
    stroke: var(--s5);
    background: var(--s5);
  }
  figcaption {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem 1rem;
    margin-top: 0.3rem;
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
  }
  .swatch {
    display: inline-block;
    width: 0.9rem;
    height: 0.2rem;
  }
  .box-key {
    height: 0.6rem;
    border: 1px solid var(--text);
  }
  .median-key {
    background: var(--fg-fail);
  }
  .mean-key {
    background: var(--accent);
  }
</style>
