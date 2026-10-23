// A tiny layered ("Sugiyama-lite") DAG layout — no dependency. Rule/process graphs are small
// (typically 5–50 nodes), so longest-path layering + a couple of barycenter ordering sweeps give a
// readable top-to-bottom layout. Pure function of (nodes, edges) so it can be property-tested.

export interface Positioned {
  id: string;
  /** Top-left of the node box. */
  x: number;
  y: number;
  layer: number;
}

export interface Layout {
  nodes: Positioned[];
  edges: { from: string; to: string }[];
  width: number;
  height: number;
  /** The size every node box was laid out at, for drawing it. */
  nodeWidth: number;
  nodeHeight: number;
}

/** Overrides of the default node size and gaps (the tests use them; the graph uses the defaults). */
export interface LayoutOpts {
  nodeWidth?: number;
  nodeHeight?: number;
  gapX?: number;
  gapY?: number;
}

const DEFAULTS = { nodeWidth: 150, nodeHeight: 46, gapX: 28, gapY: 44 };

/**
 * Lay a directed graph out in layers (top → bottom). Cycles (which a well-formed DAG won't have,
 * but a buggy emitter might) are broken defensively by an iteration cap, so this always terminates.
 */
export function layout(
  nodeIds: string[],
  edges: [string, string][],
  opts: LayoutOpts = {},
): Layout {
  const o = { ...DEFAULTS, ...opts };
  const ids = Array.from(new Set(nodeIds));
  const idset = new Set(ids);
  // Keep only edges between known nodes.
  const es = edges.filter(([f, t]) => idset.has(f) && idset.has(t) && f !== t);

  const parents = new Map<string, string[]>();
  const children = new Map<string, string[]>();
  for (const id of ids) {
    parents.set(id, []);
    children.set(id, []);
  }
  for (const [f, t] of es) {
    children.get(f)!.push(t);
    parents.get(t)!.push(f);
  }

  // Longest-path layering: layer(n) = 1 + max(layer(parent)); roots at 0. Iterate to a fixpoint with
  // a cap (breaks any accidental cycle).
  const layer = new Map<string, number>(ids.map((id) => [id, 0]));
  for (let iter = 0; iter < ids.length + 1; iter++) {
    let changed = false;
    for (const id of ids) {
      const ps = parents.get(id)!;
      if (ps.length === 0) continue;
      const want = Math.max(...ps.map((p) => layer.get(p)! + 1));
      if (want > (layer.get(id) ?? 0)) {
        layer.set(id, want);
        changed = true;
      }
    }
    if (!changed) break;
  }
  // A cycle the cap broke has been pushed down on every pass, leaving rows above it empty: number
  // the occupied layers densely (a DAG's layers are dense already).
  const used = [...new Set(layer.values())].sort((a, b) => a - b);
  const dense = new Map(used.map((l, i) => [l, i]));
  for (const id of ids) layer.set(id, dense.get(layer.get(id)!)!);

  // Bucket nodes by layer, initial order = insertion order.
  const maxLayer = Math.max(0, ...ids.map((id) => layer.get(id)!));
  const layers: string[][] = Array.from({ length: maxLayer + 1 }, () => []);
  for (const id of ids) layers[layer.get(id)!].push(id);

  // Barycenter ordering: a few sweeps, ordering each layer by the mean position of its parents.
  const orderIndex = new Map<string, number>();
  layers.forEach((l) => l.forEach((id, i) => orderIndex.set(id, i)));
  for (let sweep = 0; sweep < 4; sweep++) {
    for (let li = 1; li <= maxLayer; li++) {
      const bary = (id: string) => {
        const ps = parents.get(id)!;
        if (ps.length === 0) return orderIndex.get(id)!;
        return ps.reduce((s, p) => s + orderIndex.get(p)!, 0) / ps.length;
      };
      layers[li] = [...layers[li]].sort((a, b) => bary(a) - bary(b));
      layers[li].forEach((id, i) => orderIndex.set(id, i));
    }
  }

  // Assign coordinates. Each layer is a row; nodes are spread across it and centered.
  const rowStride = o.nodeWidth + o.gapX;
  const widest = Math.max(1, ...layers.map((l) => l.length));
  const width = widest * rowStride - o.gapX;
  const positioned: Positioned[] = [];
  layers.forEach((l, li) => {
    const rowWidth = l.length * rowStride - o.gapX;
    const offset = (width - rowWidth) / 2;
    l.forEach((id, i) => {
      positioned.push({
        id,
        x: offset + i * rowStride,
        y: li * (o.nodeHeight + o.gapY),
        layer: li,
      });
    });
  });
  const height = (maxLayer + 1) * (o.nodeHeight + o.gapY) - o.gapY;
  return {
    nodes: positioned,
    edges: es.map(([from, to]) => ({ from, to })),
    width: Math.max(width, o.nodeWidth),
    height: Math.max(height, o.nodeHeight),
    nodeWidth: o.nodeWidth,
    nodeHeight: o.nodeHeight,
  };
}
