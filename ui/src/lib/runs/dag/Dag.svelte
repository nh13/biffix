<script lang="ts">
  import type { components } from '$lib/generated/api';
  import { layout } from './layout';
  import { COUNT_CHARS, fitText, nodeLabel } from './labels';

  type Node = components['schemas']['DagNode'];
  interface Props {
    nodes: Node[];
    edges: [string, string][];
    /** Called with a node's id when it is clicked. */
    onselect?: (id: string) => void;
  }
  let { nodes, edges, onselect }: Props = $props();

  const PAD = 8;
  // Unique per instance, so two graphs on one page don't share (and fight over) one marker.
  const uid = $props.id();
  const arrowId = `dag-arrow-${uid}`;

  // The layout depends only on the node ids and the edges. A polling parent hands over new arrays
  // (new counts) on every refresh, so they are keyed by content first: a derived string that did
  // not change stops there, and the layout is not recomputed for the same graph.
  const shape = $derived(JSON.stringify([nodes.map((n) => n.id), edges]));
  const laid = $derived.by(() => {
    const [ids, pairs] = JSON.parse(shape) as [string[], [string, string][]];
    return layout(ids, pairs);
  });
  const NODE_W = $derived(laid.nodeWidth);
  const NODE_H = $derived(laid.nodeHeight);
  const byId = $derived(new Map(nodes.map((n) => [n.id, n])));
  const posById = $derived(new Map(laid.nodes.map((p) => [p.id, p])));

  // Dominant tier for a node's fill/outline: failed wins, then running, then (all done) succeeded,
  // else pending.
  function tier(n: Node): 'failed' | 'running' | 'succeeded' | 'pending' {
    if (n.failed > 0) return 'failed';
    if (n.running > 0) return 'running';
    if (total(n) > 0 && n.succeeded === total(n)) return 'succeeded';
    return 'pending';
  }
  function total(n: Node): number {
    return n.pending + n.running + n.succeeded + n.failed;
  }
  function centerX(id: string): number {
    return (posById.get(id)?.x ?? 0) + NODE_W / 2 + PAD;
  }
  function topY(id: string): number {
    return (posById.get(id)?.y ?? 0) + PAD;
  }
  function bottomY(id: string): number {
    return (posById.get(id)?.y ?? 0) + NODE_H + PAD;
  }
  function counts(n: Node): string {
    return `${n.succeeded}/${total(n)} done${n.running ? ` · ${n.running} run` : ''}${
      n.failed ? ` · ${n.failed} fail` : ''
    }${n.cached ? ` · ${n.cached} cached` : ''}`;
  }
  function spoken(id: string, n: Node): string {
    return `${id}: ${n.succeeded} of ${total(n)} done${n.running ? `, ${n.running} running` : ''}${
      n.failed ? `, ${n.failed} failed` : ''
    }${n.cached ? `, ${n.cached} cached` : ''}`;
  }
</script>

<div class="dag-scroll">
  <svg
    viewBox={`0 0 ${laid.width + PAD * 2} ${laid.height + PAD * 2}`}
    width={laid.width + PAD * 2}
    height={laid.height + PAD * 2}
    role="group"
    aria-label="Workflow dependency graph"
  >
    <defs>
      <marker
        id={arrowId}
        viewBox="0 0 10 10"
        refX="9"
        refY="5"
        markerWidth="6"
        markerHeight="6"
        orient="auto-start-reverse"
      >
        <path d="M0,0 L10,5 L0,10 z" fill="var(--text-muted)" />
      </marker>
    </defs>
    {#each laid.edges as e}
      <line
        x1={centerX(e.from)}
        y1={bottomY(e.from)}
        x2={centerX(e.to)}
        y2={topY(e.to)}
        stroke="var(--text-muted)"
        stroke-width="1.2"
        opacity="0.5"
        marker-end={`url(#${arrowId})`}
      />
    {/each}
    {#each laid.nodes as p}
      {@const n = byId.get(p.id)}
      {#if n}
        {@const t = tier(n)}
        <g
          class={`node ${t}`}
          transform={`translate(${p.x + PAD}, ${p.y + PAD})`}
          onclick={() => onselect?.(p.id)}
          onkeydown={(ev) => {
            if (ev.key !== 'Enter' && ev.key !== ' ') return;
            // Without this, Space would also scroll the page.
            ev.preventDefault();
            onselect?.(p.id);
          }}
          role="button"
          tabindex="0"
          aria-label={spoken(p.id, n)}
        >
          <!-- A box holds a shortened name: the whole id is its tooltip. -->
          <title>{p.id}</title>
          <rect width={NODE_W} height={NODE_H} rx="6" />
          <text class="label" x={NODE_W / 2} y={NODE_H / 2 - 3}>{nodeLabel(p.id)}</text>
          <text class="count" x={NODE_W / 2} y={NODE_H / 2 + 12}>
            {fitText(counts(n), COUNT_CHARS)}
          </text>
        </g>
      {/if}
    {/each}
  </svg>
</div>

<style>
  .dag-scroll {
    overflow-x: auto;
    padding-bottom: 0.5rem;
  }
  /* Drawn at its own size: a wide graph scrolls rather than shrinking its text. */
  svg {
    display: block;
  }
  .node rect {
    fill: var(--bg-content);
    stroke: var(--border);
    stroke-width: 1.5;
    cursor: pointer;
  }
  .node.succeeded rect {
    stroke: #30a46c;
  }
  .node.running rect {
    stroke: #4a7cf5;
  }
  .node.failed rect {
    stroke: #e5484d;
    stroke-width: 2;
  }
  .node.pending rect {
    stroke: var(--border);
  }
  .label {
    text-anchor: middle;
    font-size: 0.74rem;
    font-weight: 500;
    fill: var(--text);
    pointer-events: none;
  }
  .count {
    text-anchor: middle;
    font-size: 0.62rem;
    fill: var(--text-muted);
    pointer-events: none;
  }
</style>
