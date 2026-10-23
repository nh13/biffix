// Run outputs: the tree the Outputs section draws from a run's registered objects.

import type { RunOutput } from '$lib/api';

/** A directory of the output tree, with rollups over everything beneath it. */
export interface OutputDir {
  name: string;
  /** The directory's path from the root, with a trailing `/` (`''` for the root). */
  path: string;
  dirs: OutputDir[];
  files: RunOutput[];
  /** Files anywhere beneath this directory. */
  fileCount: number;
  /** Sum of the known sizes beneath this directory. */
  knownBytes: number;
}

/**
 * Build a directory tree from the outputs' `path`s (a trie on `/`-separated segments), with file
 * counts and size rollups. Directories and files are sorted by name. O(n) in the number of paths'
 * segments.
 */
export function buildOutputTree(outputs: RunOutput[]): OutputDir {
  const root = emptyDir('', '');
  // Each directory's children by name, so a wide directory (one per sample) is not scanned per file.
  const children = new Map<OutputDir, Map<string, OutputDir>>();
  for (const output of outputs) {
    const segments = output.path.split('/').filter((s) => s.length > 0);
    // A path that is only slashes still names a file; show it at the root under its own name.
    const dirSegments = segments.slice(0, -1);
    let dir = root;
    for (const segment of dirSegments) {
      let byName = children.get(dir);
      if (!byName) children.set(dir, (byName = new Map()));
      let child = byName.get(segment);
      if (!child) {
        child = emptyDir(segment, `${dir.path}${segment}/`);
        dir.dirs.push(child);
        byName.set(segment, child);
      }
      dir = child;
    }
    dir.files.push(output);
  }
  rollup(root);
  return root;
}

function emptyDir(name: string, path: string): OutputDir {
  return { name, path, dirs: [], files: [], fileCount: 0, knownBytes: 0 };
}

function rollup(dir: OutputDir) {
  dir.dirs.sort((a, b) => a.name.localeCompare(b.name));
  dir.files.sort((a, b) => a.name.localeCompare(b.name));
  dir.fileCount = dir.files.length;
  dir.knownBytes = 0;
  for (const f of dir.files) dir.knownBytes += f.size ?? 0;
  for (const child of dir.dirs) {
    rollup(child);
    dir.fileCount += child.fileCount;
    dir.knownBytes += child.knownBytes;
  }
}

/** One visible row of the tree: a directory (expandable) or a file, at a nesting depth. */
export type OutputRow =
  | { kind: 'dir'; dir: OutputDir; depth: number; open: boolean }
  | { kind: 'file'; file: RunOutput; depth: number };

/**
 * The rows to render: a depth-first walk that descends only into directories in `open` (keyed by
 * `OutputDir.path`), so a collapsed directory with thousands of files costs one row.
 */
export function visibleRows(root: OutputDir, open: Set<string>): OutputRow[] {
  const rows: OutputRow[] = [];
  const walk = (dir: OutputDir, depth: number) => {
    for (const child of dir.dirs) {
      const isOpen = open.has(child.path);
      rows.push({ kind: 'dir', dir: child, depth, open: isOpen });
      if (isOpen) walk(child, depth + 1);
    }
    for (const file of dir.files) rows.push({ kind: 'file', file, depth });
  };
  walk(root, 0);
  return rows;
}

/**
 * The directories to open initially: the top `levels` levels, so a small result set is fully
 * visible while a large one does not render every file.
 */
export function initiallyOpen(root: OutputDir, levels: number): Set<string> {
  const open = new Set<string>();
  const walk = (dir: OutputDir, depth: number) => {
    if (depth >= levels) return;
    for (const child of dir.dirs) {
      open.add(child.path);
      walk(child, depth + 1);
    }
  };
  walk(root, 0);
  return open;
}

/** Whether an output matches a free-text search (its path or task, case-insensitive). */
export function matchesSearch(output: RunOutput, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return output.path.toLowerCase().includes(q) || (output.task_key ?? '').toLowerCase().includes(q);
}

/**
 * Whether two listings are the same rows with the same fields, so a poll that brings back an
 * unchanged listing can keep the one shown (and skip re-filtering and rebuilding the tree). Every
 * field of an output is a scalar, so a field-by-field comparison sees any change (a size arriving,
 * a file gone missing, one reused).
 */
export function sameOutputs(a: readonly RunOutput[], b: readonly RunOutput[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  return a.every((x, i) => {
    const y = b[i] as Record<string, unknown>;
    const keys = Object.keys(x);
    return (
      keys.length === Object.keys(y).length &&
      keys.every((k) => (x as Record<string, unknown>)[k] === y[k])
    );
  });
}

/** A human label for an output role: work-dir intermediates read "work"; other roles as named. */
export const roleLabel = (role: string): string => (role === 'intermediate' ? 'work' : role);

/**
 * How many of `outputs` a resumed run reused from the runs it resumed (`reused_from`: the engine
 * skipped those tasks, so the files are an earlier run's), and from which: "2 reused from run_x", or
 * "3 reused from 2 earlier runs". Empty when none are.
 */
export function reusedFromSummary(outputs: readonly RunOutput[]): string {
  const from = outputs.flatMap((o) => (o.reused_from ? [o.reused_from] : []));
  if (from.length === 0) return '';
  const runs = new Set(from);
  return `${from.length} reused from ${runs.size === 1 ? from[0] : `${runs.size} earlier runs`}`;
}
