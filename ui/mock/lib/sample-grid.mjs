// A run's samples: the grid `GET /runs/{run_id}/samples` answers, and the counts summaries carry.
import { TERMINAL } from './run-states.mjs';

// Per-sample progress, after `biffix_core::sample_progress`, with the samples left open: a task's
// sample is its `sample` wildcard or its Nextflow tag. Not mirrored: a run's sample sheet (the grid
// is never `declared`, though a Lab run carries one), the cells a resume reuses from the run it
// resumed, and the hiding of a Snakemake target step (the mock's runs have none).
// A tag that looks like a file name (reference prep: `GUNZIP (genome.fa.gz)`) names no sample.
const FILE_TAG = /\.(fa|fasta|fna|gtf|gff|gff3|bed|gz|bam|cram|vcf|txt|tsv|csv|json|dict|fai)$/i;
/** attribution.rs `nextflow_key`: `undefined` for a key that is not Nextflow's, else its tag (or
 * `null`): `process (tag) #index`, or `process (inner)` whose inner is the tag unless all digits
 * (an untagged task's index). A Snakemake key with a ` (` in a wildcard value has an `=` before. */
function nextflowTag(key) {
  const open = key.indexOf(' (');
  if (open < 0 || key.slice(0, open).includes('=')) return undefined;
  const rest = key.slice(open + 2);
  const close = rest.lastIndexOf(') #');
  if (close >= 0 && /^\d+$/.test(rest.slice(close + 3))) return rest.slice(0, close);
  if (!rest.endsWith(')')) return undefined;
  const inner = rest.slice(0, -1);
  return /^\d*$/.test(inner) ? null : inner;
}
function sampleOf(key) {
  const tag = nextflowTag(key);
  if (tag !== undefined) return tag && !FILE_TAG.test(tag) ? tag : null;
  // A Snakemake key is `rule:wildcards`; a wildcard value may hold a `:` itself.
  const colon = key.indexOf(':');
  if (colon < 0) return null;
  const wildcards = new Map(
    key
      .slice(colon + 1)
      .split(',')
      .filter((kv) => kv.includes('='))
      .map((kv) => [kv.slice(0, kv.indexOf('=')), kv.slice(kv.indexOf('=') + 1)]),
  );
  for (const w of ['sample', 'sample_id', 'sample_name', 'sm', 'id'])
    if (wildcards.has(w)) return wildcards.get(w);
  return null;
}
const CELL_RANK = { failed: 4, running: 3, waiting: 2, done: 1, reused: 0 };
const cellOf = (t) =>
  t.state === 'succeeded'
    ? t.cached
      ? 'reused'
      : 'done'
    : t.state === 'failed'
      ? 'failed'
      : t.state === 'running'
        ? 'running'
        : 'waiting';
export function sampleGrid(run) {
  const tasks = (run.tasks ?? []).map((t) => ({
    ...t,
    group: t.group ?? t.task_key.split(/[: ]/)[0],
    sample: sampleOf(t.task_key),
  }));
  const perSample = new Set(tasks.filter((t) => t.sample).map((t) => t.group));
  const groups = [...new Set(tasks.map((t) => t.group))];
  const cells = new Map();
  const study = {};
  let unattributed = 0;
  const merge = (into, t) => {
    const state = cellOf(t);
    if (!into.state || CELL_RANK[state] > CELL_RANK[into.state]) into.state = state;
    into.attempts = (into.attempts ?? 0) + (t.attempts ?? 0);
    into.tasks = (into.tasks ?? 0) + 1;
  };
  for (const t of tasks) {
    if (perSample.has(t.group) && !t.sample) unattributed += 1;
    else if (perSample.has(t.group)) {
      const row = cells.get(t.sample) ?? {};
      cells.set(t.sample, row);
      merge((row[t.group] ??= {}), t);
    } else merge((study[t.group] ??= {}), t);
  }
  // As `RunEnd`: only a succeeded run settles every sample; a failed or cancelled one stops the rest.
  const succeeded = run.state === 'succeeded';
  const ended = TERMINAL.has(run.state);
  const nodes = run.dag_nodes ?? [];
  // No DAG yet (Nextflow sends it at completion): no step can be known to have started.
  const allStarted = nodes.length > 0 && nodes.every((n) => groups.includes(n));
  const samples = [...cells.keys()].sort().map((id) => {
    const row = cells.get(id);
    const states = Object.values(row).map((c) => c.state);
    const finished = states.every((st) => st === 'done' || st === 'reused');
    const state = states.includes('failed')
      ? 'failed'
      : finished && (succeeded || (allStarted && [...perSample].every((g) => row[g])))
        ? 'done'
        : ended
          ? 'stopped'
          : 'running';
    return { id, state, cells: row };
  });
  // Pipeline order, as `step_order`: a topological order of the DAG, ties by name, then the rest.
  const edges = run.dag_edges ?? [];
  const indegree = new Map(nodes.map((n) => [n, 0]));
  // Only an edge from a node counts: one from outside the graph is never taken off again.
  for (const [from, to] of edges)
    if (indegree.has(from) && indegree.has(to)) indegree.set(to, indegree.get(to) + 1);
  const ready = [...indegree].filter(([, d]) => d === 0).map(([n]) => n);
  const topo = [];
  while (ready.length) {
    ready.sort();
    const n = ready.shift();
    topo.push(n);
    for (const [from, to] of edges) {
      if (from === n && indegree.has(to)) {
        indegree.set(to, indegree.get(to) - 1);
        if (indegree.get(to) === 0) ready.push(to);
      }
    }
  }
  for (const n of [...nodes].sort()) if (!topo.includes(n)) topo.push(n);
  const order = topo.filter((n) => groups.includes(n));
  for (const g of groups.sort()) if (!order.includes(g)) order.push(g);
  const label = (g) => g.split(':').pop().replaceAll('_', ' ');
  const steps = order.map((g) => ({
    group: g,
    label: label(g),
    scope: perSample.has(g) ? 'sample' : 'study',
  }));
  return { steps, samples, study, unattributed, declared: false };
}
export function sampleCounts(grid) {
  if (!grid.samples.length) return null;
  const n = (s) => grid.samples.filter((r) => r.state === s).length;
  return { total: grid.samples.length, done: n('done'), failed: n('failed'), declared: false };
}
