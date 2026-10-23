// The alignment viewer's igv.js configuration, from what the API returns for one
// BAM/CRAM output (`GET …/alignment-view`). Pure, so the mapping is unit-tested; the component
// hands the result to `igv.createBrowser` (and `loadSessionObject` when the links are re-minted).

import type { AlignmentReference, AlignmentLinks } from '#lib/api/index.js';

/** An igv track, as this viewer configures them (a subset of igv.js's track options). */
export interface IgvTrack {
  type: 'alignment' | 'annotation';
  format: string;
  name: string;
  url: string;
  /** Always explicit: a presigned URL's index cannot be derived from it by adding `.bai`. */
  indexURL?: string;
  /** `false` for a small annotation loaded whole (no index). */
  indexed?: false;
}

/** The options for `igv.createBrowser` (and a session for `loadSessionObject`). */
export interface IgvOptions {
  /** The bundle's own FASTA. */
  reference?: {
    id: string;
    name: string;
    fastaURL: string;
    indexURL: string;
    compressedIndexURL?: string;
  };
  /** A genome igv hosts (`hg38`), from the bundle's `igv_genome`. */
  genome?: string;
  /**
   * Whether igv fetches its list of hosted genomes from igv.org. Only a hosted genome needs it; a
   * bundle's FASTA never sends the browser there.
   */
  loadDefaultGenomes: boolean;
  locus?: string;
  tracks: IgvTrack[];
  showSVGButton: boolean;
}

/** How the header names the reference: the bundle's name, or the hosted genome's id. */
export function referenceLabel(ref: AlignmentReference): string {
  return ref.source === 'bundle'
    ? ref.display_name
    : ref.display_name
      ? `${ref.display_name} (hosted ${ref.genome})`
      : `hosted ${ref.genome}`;
}

/**
 * igv's options for `view`: the alignment (and its index) as a track, the bundle's FASTA (with its
 * `.fai`, and `.gzi` when bgzipped) or the hosted genome as the reference, the bundle's annotation
 * as a gene track, and where to open — `savedLocus` (where the person last was) wins over the
 * server's first contig with reads. `null` without a reference: the person chooses one first.
 */
export function igvOptions(view: AlignmentLinks, savedLocus: string | null): IgvOptions | null {
  const ref = view.reference ?? null;
  if (!ref) return null;
  const tracks: IgvTrack[] = [];
  if (ref.source === 'bundle' && ref.annotation) {
    const a = ref.annotation;
    tracks.push({
      type: 'annotation',
      format: a.format,
      name: 'Genes',
      url: a.url,
      ...(a.index_url ? { indexURL: a.index_url } : { indexed: false }),
    });
  }
  tracks.push({
    type: 'alignment',
    format: view.alignment.format,
    name: view.alignment.name,
    url: view.alignment.url,
    indexURL: view.alignment.index_url,
  });
  const locus = savedLocus || view.locus || undefined;
  const common = { tracks, showSVGButton: false, ...(locus ? { locus } : {}) };
  if (ref.source === 'igv_genome')
    return { genome: ref.genome, loadDefaultGenomes: true, ...common };
  return {
    reference: {
      id: ref.bundle_id,
      name: ref.display_name,
      fastaURL: ref.fasta_url,
      indexURL: ref.fai_url,
      ...(ref.gzi_url ? { compressedIndexURL: ref.gzi_url } : {}),
    },
    loadDefaultGenomes: false,
    ...common,
  };
}

/**
 * The `sessionStorage` key of where the person last was in one output against one reference (per
 * run, per file, per reference): another reference may name its contigs differently, so a place in
 * one is not kept for the other.
 */
export function lastLocusKey(
  runId: string,
  outputId: string,
  reference: AlignmentReference | null,
): string {
  const ref = !reference
    ? ''
    : reference.source === 'bundle'
      ? `bundle:${reference.bundle_id}`
      : `genome:${reference.genome}`;
  return `biffix-igv-locus:${runId}:${outputId}:${ref}`;
}

/** Every link igv reads in `view`: the alignment and its index, and the reference's files. */
export function viewLinks(view: AlignmentLinks): string[] {
  const ref = view.reference ?? null;
  const links = [view.alignment.url, view.alignment.index_url];
  if (ref?.source === 'bundle') {
    links.push(ref.fasta_url, ref.fai_url);
    if (ref.gzi_url) links.push(ref.gzi_url);
    if (ref.annotation) {
      links.push(ref.annotation.url);
      if (ref.annotation.index_url) links.push(ref.annotation.index_url);
    }
  }
  return links;
}

/** The urls the viewer checks it can read before starting igv: the alignment, and the FASTA. */
export function probeUrls(view: AlignmentLinks): string[] {
  const ref = view.reference ?? null;
  return [view.alignment.url, ...(ref?.source === 'bundle' ? [ref.fasta_url] : [])];
}

/**
 * What a probe's outcome means: `ok` (a 2xx), `denied` (403: the link expired, or the storage
 * refused it), `cors` (the request failed before any answer — the bucket has no CORS rule for this
 * site), or `error` (any other status).
 */
export type ProbeResult = 'ok' | 'denied' | 'cors' | 'error';

export function probeResultOf(outcome: { status: number } | { thrown: unknown }): ProbeResult {
  if ('thrown' in outcome) return outcome.thrown instanceof TypeError ? 'cors' : 'error';
  if (outcome.status >= 200 && outcome.status < 300) return 'ok';
  return outcome.status === 403 ? 'denied' : 'error';
}

/**
 * What the probes of several files say together: the first of `cors`, `denied` and `error` any of
 * them gave (a missing CORS rule explains the rest), else `ok`.
 */
export function probeSummary(results: readonly ProbeResult[]): ProbeResult {
  return (['cors', 'denied', 'error'] as const).find((r) => results.includes(r)) ?? 'ok';
}
