// The Settings → References editor's form model, and the request it describes (or what is missing
// from it). Pure, so each client-side check is unit-tested.

import type { ReferenceBundle, ReferenceBundleRequest } from '$lib/api';

/** The kinds of bundle the form offers (a newer server's other kind stays selectable as itself). */
export const KINDS = [
  { value: 'assembly', label: 'Genome assembly' },
  { value: 'assay_kit', label: 'Assay kit' },
];
/** Keys workflows commonly ask for; a key is lowercase letters, digits and `_` (`fasta_index`). */
export const COMMON_KEYS = [
  'fasta',
  'fasta_index',
  'sequence_dict',
  'gtf',
  'bwa_index',
  'bwa_mem2_index',
  'star_index',
  'salmon_index',
  'targets_bed',
  'baits_bed',
];

/**
 * How the file picker opens for a row: on folders for an index (given by its folder, ending in
 * `/`), whether the row already holds one or is a blank `…_index` row; else on files.
 */
export function pickerMode(row: { key: string; uri: string }): 'file' | 'folder' {
  const uri = row.uri.trim();
  if (uri) return uri.endsWith('/') ? 'folder' : 'file';
  return row.key.trim().endsWith('_index') ? 'folder' : 'file';
}

/** A kind's words, else the kind itself. */
export function kindLabel(kind: string): string {
  return KINDS.find((x) => x.value === kind)?.label ?? kind;
}

export interface Row {
  key: string;
  uri: string;
}

export interface Editing {
  /** The bundle being edited; `null` for a new one. */
  original: string | null;
  id: string;
  kind: string;
  displayName: string;
  notes: string;
  /** A genome the alignment viewer hosts (`hg38`, `mm39`) to use instead of the bundle's FASTA. */
  igvGenome: string;
  rows: Row[];
}

/** The form for a new bundle: an assembly, with its FASTA and index to fill in. */
export function newBundleForm(): Editing {
  return {
    original: null,
    id: '',
    kind: 'assembly',
    displayName: '',
    notes: '',
    igvGenome: '',
    rows: [
      { key: 'fasta', uri: '' },
      { key: 'fasta_index', uri: '' },
    ],
  };
}

/** The form for editing `b`, one row per file. */
export function bundleForm(b: ReferenceBundle): Editing {
  return {
    original: b.id,
    id: b.id,
    kind: b.kind,
    displayName: b.display_name,
    notes: b.notes ?? '',
    igvGenome: b.igv_genome ?? '',
    rows: Object.entries(b.uris).map(([key, uri]) => ({ key, uri })),
  };
}

/** What the server accepts as an `igv_genome` (`ReferenceBundle.igv_genome`). */
const IGV_GENOME = /^[A-Za-z0-9._-]{1,32}$/;

/** The request the form describes, or a sentence saying what is missing from it. */
export function requestOf(e: Editing): ReferenceBundleRequest | string {
  // Every field is checked and sent trimmed. A row left entirely blank is ignored; one with only a
  // key or only a path is an error below.
  const rows = e.rows
    .map((r) => ({ key: r.key.trim(), uri: r.uri.trim() }))
    .filter((r) => r.key || r.uri);
  const id = e.id.trim();
  const displayName = e.displayName.trim();
  if (!id) return 'Give the reference an id.';
  if (!displayName) return 'Give the reference a name.';
  if (rows.length === 0) return 'Add at least one file.';
  if (rows.some((r) => !r.key || !r.uri)) return 'Every file needs both a key and a path.';
  // Two rows under one key would silently lose a path in the map.
  const keys = rows.map((r) => r.key);
  const dup = keys.find((k, i) => keys.indexOf(k) !== i);
  if (dup) return `The key ${dup} is used twice.`;
  const igvGenome = e.igvGenome.trim();
  if (igvGenome && !IGV_GENOME.test(igvGenome))
    return 'A hosted genome is a short id such as hg38 or mm39 (letters, digits, ., _ or -).';
  return {
    id,
    kind: e.kind,
    display_name: displayName,
    notes: e.notes.trim() || null,
    // Left out when blank: the server reads an absent one as none (and a PUT replaces the bundle).
    ...(igvGenome ? { igv_genome: igvGenome } : {}),
    uris: Object.fromEntries(rows.map((r) => [r.key, r.uri])),
  };
}
