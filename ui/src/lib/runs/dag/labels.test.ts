import { describe, expect, it } from 'vitest';
import { fitText, nodeLabel } from './labels';

describe('fitText', () => {
  it('keeps text that fits and ends a longer one with an ellipsis, at the budget', () => {
    expect(fitText('align', 10)).toBe('align');
    expect(fitText('abcdefghij', 10)).toBe('abcdefghij');
    expect(fitText('abcdefghijk', 10)).toBe('abcdefghi…');
  });
});

describe('nodeLabel', () => {
  it("names a Nextflow process by its own name, shortened to the box's budget", () => {
    const id = 'NFCORE_RNASEQ:RNASEQ:FASTQ_QC_TRIM_FILTER_SETSTRANDEDNESS:FASTQC';
    expect(nodeLabel(id)).toBe('FASTQC');
    const long = nodeLabel('WF:QUANTIFY_PSEUDO_ALIGNMENT_WITH_SALMON_AND_TXIMPORT');
    expect(long.length).toBeLessThanOrEqual(20);
    expect(long.endsWith('…')).toBe(true);
    expect(nodeLabel('align')).toBe('align');
  });
});
