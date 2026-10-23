import { describe, expect, it } from 'vitest';
import type { AlignmentReference, AlignmentLinks } from '#lib/api/index.js';
import {
  igvOptions,
  lastLocusKey,
  probeResultOf,
  probeSummary,
  probeUrls,
  referenceLabel,
  viewLinks,
} from './igvConfig';

const bundleRef = (over: Partial<Extract<AlignmentReference, { source: 'bundle' }>> = {}) =>
  ({
    source: 'bundle',
    bundle_id: 'grcm39',
    display_name: 'Mouse GRCm39',
    fasta_url: 'https://s3/genome.fa?sig=1',
    fai_url: 'https://s3/genome.fa.fai?sig=2',
    ...over,
  }) as AlignmentReference;

const viewWith = (reference: AlignmentReference | null, over: Partial<AlignmentLinks> = {}) =>
  ({
    alignment: {
      name: 'S1.bam',
      format: 'bam',
      url: 'https://s3/S1.bam?sig=3',
      index_url: 'https://s3/S1.bam.bai?sig=4',
    },
    expires_at: 1_000_000,
    bucket_cors: 'configured',
    locus: 'chrB:9001-11000',
    ...(reference ? { reference } : {}),
    ...over,
  }) as AlignmentLinks;

describe('igvOptions', () => {
  it("shows the alignment against the bundle's FASTA, every index given explicitly", () => {
    const options = igvOptions(viewWith(bundleRef()), null)!;
    expect(options.reference).toEqual({
      id: 'grcm39',
      name: 'Mouse GRCm39',
      fastaURL: 'https://s3/genome.fa?sig=1',
      indexURL: 'https://s3/genome.fa.fai?sig=2',
    });
    expect(options.genome).toBeUndefined();
    // A bundle never sends the browser to igv.org for its genome list.
    expect(options.loadDefaultGenomes).toBe(false);
    expect(options.tracks).toEqual([
      {
        type: 'alignment',
        format: 'bam',
        name: 'S1.bam',
        url: 'https://s3/S1.bam?sig=3',
        indexURL: 'https://s3/S1.bam.bai?sig=4',
      },
    ]);
    expect(options.locus).toBe('chrB:9001-11000');
  });

  it("adds a bgzipped FASTA's .gzi and the annotation as a gene track", () => {
    const indexed = igvOptions(
      viewWith(
        bundleRef({
          gzi_url: 'https://s3/genome.fa.gz.gzi',
          annotation: {
            url: 'https://s3/genes.gtf.gz',
            index_url: 'https://s3/genes.gtf.gz.tbi',
            format: 'gtf',
          },
        }),
      ),
      null,
    )!;
    expect(indexed.reference?.compressedIndexURL).toBe('https://s3/genome.fa.gz.gzi');
    expect(indexed.tracks[0]).toEqual({
      type: 'annotation',
      format: 'gtf',
      name: 'Genes',
      url: 'https://s3/genes.gtf.gz',
      indexURL: 'https://s3/genes.gtf.gz.tbi',
    });
    expect(indexed.tracks[1].type).toBe('alignment');
    // A small annotation without an index is loaded whole.
    const whole = igvOptions(
      viewWith(bundleRef({ annotation: { url: 'https://s3/genes.gff3', format: 'gff3' } })),
      null,
    )!;
    expect(whole.tracks[0]).toMatchObject({ format: 'gff3', indexed: false });
    expect(whole.tracks[0]).not.toHaveProperty('indexURL');
  });

  it('uses a hosted genome by its id, which loads igv.org genome list', () => {
    const ref = { source: 'igv_genome', genome: 'mm39', bundle_id: 'grcm39' } as AlignmentReference;
    const options = igvOptions(viewWith(ref), null)!;
    expect(options.genome).toBe('mm39');
    expect(options.reference).toBeUndefined();
    expect(options.loadDefaultGenomes).toBe(true);
    expect(options.tracks.map((t) => t.type)).toEqual(['alignment']);
  });

  it('opens where the person last was, else at the reads, else wherever igv likes', () => {
    expect(igvOptions(viewWith(bundleRef()), 'chrA:1-100')!.locus).toBe('chrA:1-100');
    expect(igvOptions(viewWith(bundleRef(), { locus: null }), null)).not.toHaveProperty('locus');
  });

  it('has nothing to show without a reference (the person chooses one)', () => {
    expect(igvOptions(viewWith(null), 'chrB:1-10')).toBeNull();
  });
});

describe('the rest of the viewer’s rules', () => {
  const hosted = { source: 'igv_genome', genome: 'hg38' } as AlignmentReference;

  it('names the reference by its bundle, or as a hosted genome', () => {
    expect(referenceLabel(bundleRef())).toBe('Mouse GRCm39');
    expect(referenceLabel(hosted)).toBe('hosted hg38');
    expect(referenceLabel({ ...hosted, display_name: 'Human' } as AlignmentReference)).toBe(
      'Human (hosted hg38)',
    );
  });

  it('keeps the last locus per run, file and reference', () => {
    const grcm39 = bundleRef();
    expect(lastLocusKey('run_1', 'out7', grcm39)).toBe('biffix-igv-locus:run_1:out7:bundle:grcm39');
    expect(lastLocusKey('run_1', 'out8', grcm39)).not.toBe(lastLocusKey('run_1', 'out7', grcm39));
    // Another reference may name its contigs differently: its place is its own.
    const other = bundleRef({ bundle_id: 'grch38' });
    expect(lastLocusKey('run_1', 'out7', other)).not.toBe(lastLocusKey('run_1', 'out7', grcm39));
    expect(lastLocusKey('run_1', 'out7', hosted)).toBe('biffix-igv-locus:run_1:out7:genome:hg38');
  });

  it("lists every link igv reads, so a refused one is known to be the viewer's", () => {
    const ref = bundleRef({
      gzi_url: 'https://s3/genome.fa.gz.gzi?sig=5',
      annotation: {
        format: 'gtf',
        url: 'https://s3/genes.gtf.gz?sig=6',
        index_url: 'https://s3/genes.gtf.gz.tbi?sig=7',
      },
    } as Partial<Extract<AlignmentReference, { source: 'bundle' }>>);
    expect(viewLinks(viewWith(ref))).toEqual([
      'https://s3/S1.bam?sig=3',
      'https://s3/S1.bam.bai?sig=4',
      'https://s3/genome.fa?sig=1',
      'https://s3/genome.fa.fai?sig=2',
      'https://s3/genome.fa.gz.gzi?sig=5',
      'https://s3/genes.gtf.gz?sig=6',
      'https://s3/genes.gtf.gz.tbi?sig=7',
    ]);
    expect(viewLinks(viewWith(hosted))).toEqual([
      'https://s3/S1.bam?sig=3',
      'https://s3/S1.bam.bai?sig=4',
    ]);
  });

  it('probes the alignment and the FASTA (a hosted genome is not ours to probe)', () => {
    expect(probeUrls(viewWith(bundleRef()))).toEqual([
      'https://s3/S1.bam?sig=3',
      'https://s3/genome.fa?sig=1',
    ]);
    expect(probeUrls(viewWith(hosted))).toEqual(['https://s3/S1.bam?sig=3']);
  });

  it('tells a missing CORS rule (a network error) from expired links (403)', () => {
    expect(probeResultOf({ status: 206 })).toBe('ok');
    expect(probeResultOf({ status: 200 })).toBe('ok');
    expect(probeResultOf({ status: 403 })).toBe('denied');
    expect(probeResultOf({ status: 500 })).toBe('error');
    expect(probeResultOf({ thrown: new TypeError('Failed to fetch') })).toBe('cors');
    expect(probeResultOf({ thrown: new Error('aborted') })).toBe('error');
  });

  it("sums up several files' probes: a missing CORS rule, then refused links, then any error", () => {
    expect(probeSummary(['ok', 'ok'])).toBe('ok');
    expect(probeSummary([])).toBe('ok');
    expect(probeSummary(['ok', 'error'])).toBe('error');
    expect(probeSummary(['error', 'denied'])).toBe('denied');
    expect(probeSummary(['denied', 'cors', 'error'])).toBe('cors');
  });
});
