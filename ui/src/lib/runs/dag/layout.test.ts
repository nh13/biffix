import fc from 'fast-check';
import { describe, expect, it } from 'vitest';

import { layout } from './layout';

const layerOf = (l: ReturnType<typeof layout>, id: string) =>
  l.nodes.find((n) => n.id === id)?.layer;

describe('layout', () => {
  it('layers a linear chain top to bottom', () => {
    const l = layout(
      ['a', 'b', 'c'],
      [
        ['a', 'b'],
        ['b', 'c'],
      ],
    );
    expect(layerOf(l, 'a')).toBe(0);
    expect(layerOf(l, 'b')).toBe(1);
    expect(layerOf(l, 'c')).toBe(2);
    // y increases with layer; every node is placed once.
    expect(l.nodes).toHaveLength(3);
    const y = (id: string) => l.nodes.find((n) => n.id === id)!.y;
    expect(y('a')).toBeLessThan(y('b'));
    expect(y('b')).toBeLessThan(y('c'));
    expect(l.height).toBe(y('c') + l.nodeHeight);
  });

  it('reports the node size it laid out at', () => {
    expect(layout(['a'], [])).toMatchObject({ nodeWidth: 150, nodeHeight: 46 });
    expect(layout(['a'], [], { nodeWidth: 80, nodeHeight: 20 })).toMatchObject({
      nodeWidth: 80,
      nodeHeight: 20,
      width: 80,
      height: 20,
    });
  });

  it('places a diamond join on the longest path', () => {
    // a→b→d and a→c→d: d must sit below b/c (longest path = 2), not at layer 1.
    const l = layout(
      ['a', 'b', 'c', 'd'],
      [
        ['a', 'b'],
        ['a', 'c'],
        ['b', 'd'],
        ['c', 'd'],
      ],
    );
    expect(layerOf(l, 'a')).toBe(0);
    expect(layerOf(l, 'b')).toBe(1);
    expect(layerOf(l, 'c')).toBe(1);
    expect(layerOf(l, 'd')).toBe(2);
  });

  it('drops edges to unknown nodes and self-loops', () => {
    const l = layout(
      ['a', 'b'],
      [
        ['a', 'b'],
        ['a', 'ghost'], // unknown target
        ['b', 'b'], // self-loop
      ],
    );
    expect(l.edges).toEqual([{ from: 'a', to: 'b' }]);
  });

  it('terminates on a cycle instead of hanging', () => {
    // A buggy emitter could produce a cycle; the iteration cap must still return a finite layout.
    const l = layout(
      ['a', 'b', 'c'],
      [
        ['a', 'b'],
        ['b', 'c'],
        ['c', 'a'],
      ],
    );
    expect(l.nodes).toHaveLength(3);
    expect(Number.isFinite(l.width)).toBe(true);
    expect(Number.isFinite(l.height)).toBe(true);
    // No empty rows: three nodes take at most three rows, from the top.
    const rows = [...new Set(l.nodes.map((n) => n.layer))].sort();
    expect(rows).toEqual(rows.map((_, i) => i));
    expect(l.height).toBeLessThanOrEqual(3 * 46 + 2 * 44);
  });

  it('deduplicates node ids and handles the empty graph', () => {
    const dup = layout(['a', 'a', 'b'], [['a', 'b']]);
    expect(dup.nodes).toHaveLength(2);

    const empty = layout([], []);
    expect(empty.nodes).toEqual([]);
    // Falls back to at least one node's dimensions rather than a zero/negative canvas.
    expect(empty.width).toBeGreaterThan(0);
    expect(empty.height).toBeGreaterThan(0);
  });
});

// The function's own doc calls it "pure ... so it can be property-tested"; these assert the
// invariants over arbitrary graphs, complementing the hand-picked cases above.
describe('layout invariants (property-based)', () => {
  // Arbitrary graph, edges may reference unknown ids, self-loop, or form cycles.
  const anyGraph = fc.record({
    ids: fc.array(fc.string(), { maxLength: 20 }),
    edges: fc.array(fc.tuple(fc.string(), fc.string()), { maxLength: 30 }),
  });

  // Acyclic graph: every generated edge runs from an earlier to a later node (self-loops, which map
  // to from===to, are dropped by layout), so the downward-layering invariant must hold.
  const dagGraph = fc.array(fc.string(), { maxLength: 20 }).chain((raw) => {
    const ids = [...new Set(raw)];
    const edge =
      ids.length >= 2
        ? fc
            .tuple(fc.nat(ids.length - 1), fc.nat(ids.length - 1))
            .map(([a, b]) => [ids[Math.min(a, b)], ids[Math.max(a, b)]] as [string, string])
        : fc.constant(['x', 'y'] as [string, string]);
    return fc.record({ ids: fc.constant(ids), edges: fc.array(edge, { maxLength: 30 }) });
  });

  it('places each unique input id exactly once', () => {
    fc.assert(
      fc.property(anyGraph, ({ ids, edges }) => {
        const l = layout(ids, edges);
        const unique = new Set(ids).size;
        expect(l.nodes).toHaveLength(unique);
        expect(new Set(l.nodes.map((n) => n.id)).size).toBe(unique);
      }),
    );
  });

  it('always returns a positive, finite canvas (even on cycles / unknown edges)', () => {
    fc.assert(
      fc.property(anyGraph, ({ ids, edges }) => {
        const l = layout(ids, edges);
        expect(Number.isFinite(l.width) && l.width > 0).toBe(true);
        expect(Number.isFinite(l.height) && l.height > 0).toBe(true);
      }),
    );
  });

  it('lays every surviving edge downward: child layer > parent layer', () => {
    fc.assert(
      fc.property(dagGraph, ({ ids, edges }) => {
        const l = layout(ids, edges);
        const layerOf = new Map(l.nodes.map((n) => [n.id, n.layer]));
        for (const e of l.edges) {
          expect(layerOf.get(e.to)!).toBeGreaterThan(layerOf.get(e.from)!);
        }
      }),
    );
  });

  it('places boxes in rows by layer, inside the canvas, never overlapping', () => {
    const opts = { nodeWidth: 150, nodeHeight: 46, gapX: 28, gapY: 44 };
    fc.assert(
      fc.property(dagGraph, ({ ids, edges }) => {
        const l = layout(ids, edges, opts);
        for (const n of l.nodes) {
          // Each layer is one row.
          expect(n.y).toBe(n.layer * (opts.nodeHeight + opts.gapY));
          // Every box lies within the canvas.
          expect(n.x).toBeGreaterThanOrEqual(0);
          expect(n.y).toBeGreaterThanOrEqual(0);
          expect(n.x + opts.nodeWidth).toBeLessThanOrEqual(l.width + 1e-9);
          expect(n.y + opts.nodeHeight).toBeLessThanOrEqual(l.height);
        }
        // Within a row, neighbouring boxes sit at least a box plus a gap apart.
        const rows = new Map<number, number[]>();
        for (const n of l.nodes) rows.set(n.layer, [...(rows.get(n.layer) ?? []), n.x]);
        for (const xs of rows.values()) {
          xs.sort((a, b) => a - b);
          for (let i = 1; i < xs.length; i++) {
            expect(xs[i] - xs[i - 1]).toBeGreaterThanOrEqual(opts.nodeWidth + opts.gapX - 1e-9);
          }
        }
      }),
    );
  });
});
