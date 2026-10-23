import { describe, expect, it } from 'vitest';
import { authorLabels, readableStep, stepIdentifier, stepLabel } from './stepLabel';

describe('stepIdentifier', () => {
  it('strips process paths, tags and hash suffixes', () => {
    expect(stepIdentifier('NFCORE_RNASEQ:RNASEQ:FASTQC')).toBe('FASTQC');
    expect(stepIdentifier('NFCORE_RNASEQ:RNASEQ:STAR_ALIGN (Tumor_1)')).toBe('STAR_ALIGN');
    expect(stepIdentifier('align_reads_a1b2c3d4e5')).toBe('align_reads');
    expect(stepIdentifier('bwa_map')).toBe('bwa_map');
    // An all-digit suffix is a date or a number, not a hash: distinct steps stay distinct.
    expect(stepIdentifier('merge_20240101')).toBe('merge_20240101');
    expect(stepIdentifier('rule_deadbeef')).toBe('rule');
    expect(stepIdentifier('align_1234567890ab')).toBe('align');
    expect(stepIdentifier('  ')).toBe('');
  });
});

describe('readableStep', () => {
  it.each([
    ['FASTQC', 'FastQC'],
    ['STAR_ALIGN', 'STAR align'],
    ['SALMON_QUANT', 'Salmon quant'],
    ['MULTIQC', 'MultiQC'],
    ['TRIMGALORE', 'Trim Galore'],
    ['BWAMEM2_MEM', 'BWA-MEM2 mem'],
    ['NFCORE_SAREK:SAREK:GATK4_MARKDUPLICATES', 'GATK4 markduplicates'],
    ['NFCORE_RNASEQ:RNASEQ:SAMTOOLS_SORT (Tumor_2)', 'SAMtools sort'],
    ['align_reads', 'Align reads'],
    ['bwa_map', 'BWA map'],
    ['call_variants_0123abcd99', 'Call variants'],
    ['featureCounts', 'featureCounts'],
    ['plotQualityProfile', 'Plot quality profile'],
    ['image preparation', 'Image preparation'],
  ])('%s reads %s', (raw, want) => {
    expect(readableStep(raw)).toBe(want);
  });
});

describe('stepLabel', () => {
  it("keeps the author's label as written and words a made one itself", () => {
    // A label the server made from the group name (not `authored`) is worded here.
    expect(stepLabel('WF:STAR_ALIGN', { label: 'STAR ALIGN' })).toBe('STAR align');
    expect(stepLabel('WF:STAR_ALIGN', { label: 'Align (STAR)', authored: true })).toBe(
      'Align (STAR)',
    );
    // An author's label that happens to match the made one is still the author's.
    expect(stepLabel('WF:STAR_ALIGN', { label: 'STAR ALIGN', authored: true })).toBe('STAR ALIGN');
  });

  it('uses the author labels by full name, else by the last part of a process path', () => {
    const labels = { FASTQC: 'Read QC', 'WF:MULTIQC': 'QC report' };
    expect(stepLabel('WF:FASTQC', { labels })).toBe('Read QC');
    expect(stepLabel('WF:MULTIQC', { labels })).toBe('QC report');
    expect(stepLabel('WF:SALMON_QUANT', { labels })).toBe('Salmon quant');
    expect(stepLabel('SALMON_QUANT')).toBe('Salmon quant');
  });

  it("reads only the author's own labels, whatever the step is called", () => {
    const labels = { FASTQC: 'Read QC' };
    expect(stepLabel('toString', { labels })).toBe('To string');
    expect(stepLabel('WF:constructor', { labels })).toBe('Constructor');
    expect(stepLabel('__proto__', { labels })).toBe('Proto');
  });
});

describe('authorLabels', () => {
  it("reads a form's samples.labels, leniently", () => {
    expect(authorLabels({ samples: { labels: { a: 'A', b: 3 } } })).toEqual({ a: 'A' });
    expect(authorLabels({ samples: {} })).toBeNull();
    expect(authorLabels(null)).toBeNull();
    expect(authorLabels('x')).toBeNull();
  });

  it('keeps a label for a step named like an Object member', () => {
    const labels = authorLabels({ samples: { labels: JSON.parse('{"__proto__": "Odd step"}') } });
    expect(stepLabel('__proto__', { labels })).toBe('Odd step');
  });
});
