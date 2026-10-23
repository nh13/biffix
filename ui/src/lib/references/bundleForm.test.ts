import { describe, expect, it } from 'vitest';

import type { ReferenceBundle } from '#lib/api/index.js';
import {
  bundleForm,
  kindLabel,
  newBundleForm,
  pickerMode,
  requestOf,
  type Editing,
} from './bundleForm';

describe('the form for a bundle', () => {
  it('starts a new one as an assembly with its FASTA and index', () => {
    const f = newBundleForm();
    expect(f.original).toBeNull();
    expect(f.kind).toBe('assembly');
    expect(f.rows.map((r) => r.key)).toEqual(['fasta', 'fasta_index']);
  });

  it('edits a stored one with a row per file, blanks for what it lacks', () => {
    const b = {
      id: 'grch38',
      kind: 'assembly',
      display_name: 'Human GRCh38',
      notes: null,
      igv_genome: null,
      uris: { fasta: 's3://r/g.fa', gtf: 's3://r/g.gtf' },
    } as unknown as ReferenceBundle;
    expect(bundleForm(b)).toEqual({
      original: 'grch38',
      id: 'grch38',
      kind: 'assembly',
      displayName: 'Human GRCh38',
      notes: '',
      igvGenome: '',
      rows: [
        { key: 'fasta', uri: 's3://r/g.fa' },
        { key: 'gtf', uri: 's3://r/g.gtf' },
      ],
    });
  });
});

describe('pickerMode', () => {
  it('picks a folder for an index row, blank or holding a folder, else a file', () => {
    expect(pickerMode({ key: 'star_index', uri: '' })).toBe('folder');
    expect(pickerMode({ key: 'fasta', uri: '' })).toBe('file');
    expect(pickerMode({ key: 'fasta', uri: 's3://refs/hg38/' })).toBe('folder');
    expect(pickerMode({ key: 'star_index', uri: 's3://refs/hg38.tar' })).toBe('file');
  });
});

describe('kindLabel', () => {
  it('names the known kinds and leaves another as itself', () => {
    expect(kindLabel('assembly')).toBe('Genome assembly');
    expect(kindLabel('assay_kit')).toBe('Assay kit');
    expect(kindLabel('panel')).toBe('panel');
  });
});

const form = (over: Partial<Editing> = {}): Editing => ({
  original: null,
  id: ' grch38 ',
  kind: 'assembly',
  displayName: ' Human GRCh38 ',
  notes: '  ',
  igvGenome: '',
  rows: [
    { key: ' fasta ', uri: ' s3://refs/genome.fa ' },
    { key: 'fasta_index', uri: 's3://refs/genome.fa.fai' },
  ],
  ...over,
});

describe('requestOf', () => {
  it('builds the trimmed request, notes cleared when blank', () => {
    expect(requestOf(form())).toEqual({
      id: 'grch38',
      kind: 'assembly',
      display_name: 'Human GRCh38',
      notes: null,
      uris: { fasta: 's3://refs/genome.fa', fasta_index: 's3://refs/genome.fa.fai' },
    });
  });

  it('keeps notes that have text', () => {
    expect(requestOf(form({ notes: ' no ALTs ' }))).toMatchObject({ notes: 'no ALTs' });
  });

  it('asks for an id and a name', () => {
    expect(requestOf(form({ id: '  ' }))).toBe('Give the reference an id.');
    expect(requestOf(form({ displayName: '' }))).toBe('Give the reference a name.');
  });

  it('asks for a file, ignoring rows left entirely blank', () => {
    expect(requestOf(form({ rows: [] }))).toBe('Add at least one file.');
    expect(requestOf(form({ rows: [{ key: ' ', uri: '' }] }))).toBe('Add at least one file.');
  });

  it('drops blank rows from the request', () => {
    const req = requestOf(
      form({
        rows: [
          { key: 'gtf', uri: 's3://r/a.gtf' },
          { key: '', uri: '' },
        ],
      }),
    );
    expect(req).toMatchObject({ uris: { gtf: 's3://r/a.gtf' } });
  });

  it('needs both a key and a path on every row', () => {
    const want = 'Every file needs both a key and a path.';
    expect(requestOf(form({ rows: [{ key: 'fasta', uri: ' ' }] }))).toBe(want);
    expect(requestOf(form({ rows: [{ key: '', uri: 's3://r/a.fa' }] }))).toBe(want);
  });

  it('refuses a key used twice, compared after trimming', () => {
    const rows = [
      { key: 'fasta', uri: 's3://r/a.fa' },
      { key: ' fasta ', uri: 's3://r/b.fa' },
    ];
    expect(requestOf(form({ rows }))).toBe('The key fasta is used twice.');
  });

  it('sends a hosted genome id, trimmed, and leaves a blank one out', () => {
    expect(requestOf(form({ igvGenome: ' mm39 ' }))).toMatchObject({ igv_genome: 'mm39' });
    expect(requestOf(form({ igvGenome: '  ' }))).not.toHaveProperty('igv_genome');
  });

  it('refuses a hosted genome id the server would not accept', () => {
    const want = 'A hosted genome is a short id such as hg38 or mm39 (letters, digits, ., _ or -).';
    expect(requestOf(form({ igvGenome: 'hg 38' }))).toBe(want);
    expect(requestOf(form({ igvGenome: 'x'.repeat(33) }))).toBe(want);
    expect(requestOf(form({ igvGenome: 'GRCh38.p14_v-2' }))).toMatchObject({
      igv_genome: 'GRCh38.p14_v-2',
    });
  });
});
