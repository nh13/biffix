import { describe, expect, it } from 'vitest';

import { headerlessFormatOf, parseTable, sortRows } from './tsv';

describe('parseTable', () => {
  it('parses a plain TSV with a header and CRLF line endings', () => {
    const t = parseTable('name\tcount\r\na\t1\r\nb\t2\r\n', false);
    expect(t).not.toBeNull();
    expect(t!.delimiter).toBe('\t');
    expect(t!.comments).toEqual([]);
    expect(t!.sections).toHaveLength(1);
    expect(t!.sections[0].header).toEqual(['name', 'count']);
    expect(t!.sections[0].rows).toEqual([
      ['a', '1'],
      ['b', '2'],
    ]);
    expect(t!.sections[0].numeric).toEqual([false, true]);
  });

  it('parses CSV with quoted fields, embedded commas and "" escapes', () => {
    const t = parseTable('id,desc,n\n1,"a, b",3\n2,"say ""hi""",4\n3,plain,5\n', false);
    expect(t!.delimiter).toBe(',');
    expect(t!.sections[0].rows).toEqual([
      ['1', 'a, b', '3'],
      ['2', 'say "hi"', '4'],
      ['3', 'plain', '5'],
    ]);
    expect(t!.sections[0].numeric).toEqual([true, false, true]);
  });

  it('prefers tab when both tab and comma are consistent', () => {
    const t = parseTable('a,x\tb,y\n1,2\t3,4\n5,6\t7,8\n', false);
    expect(t!.delimiter).toBe('\t');
    expect(t!.sections[0].header).toEqual(['a,x', 'b,y']);
  });

  it('splits a Picard/fgbio metrics file into METRICS and HISTOGRAM sections', () => {
    const text = [
      '## htsjdk.samtools.metrics.StringHeader',
      '# CollectInsertSizeMetrics INPUT=x.bam',
      '',
      '## METRICS CLASS\tpicard.analysis.InsertSizeMetrics',
      'MEDIAN_INSERT_SIZE\tMODE_INSERT_SIZE\tMEAN_INSERT_SIZE\tSAMPLE',
      '250\t248\t251.3\t',
      '',
      '## HISTOGRAM\tjava.lang.Integer',
      'insert_size\tAll_Reads.fr_count',
      '100\t5',
      '101\t7',
      '',
    ].join('\n');
    const t = parseTable(text, false)!;
    expect(t.comments).toEqual([
      'htsjdk.samtools.metrics.StringHeader',
      'CollectInsertSizeMetrics INPUT=x.bam',
      'METRICS CLASS\tpicard.analysis.InsertSizeMetrics',
    ]);
    expect(t.sections).toHaveLength(2);
    const [metrics, histogram] = t.sections;
    expect(metrics.header).toEqual([
      'MEDIAN_INSERT_SIZE',
      'MODE_INSERT_SIZE',
      'MEAN_INSERT_SIZE',
      'SAMPLE',
    ]);
    expect(metrics.rows).toEqual([['250', '248', '251.3', '']]);
    expect(metrics.numeric).toEqual([true, true, true, false]);
    expect(histogram.header).toEqual(['insert_size', 'All_Reads.fr_count']);
    expect(histogram.rows).toEqual([
      ['100', '5'],
      ['101', '7'],
    ]);
    // The comment that opens a later section is its title; the first's is a leading comment.
    expect(metrics.title).toBeUndefined();
    expect(histogram.title).toBe('HISTOGRAM\tjava.lang.Integer');
  });

  it('treats a comment after the first data line as a section separator', () => {
    const t = parseTable('a\tb\n1\t2\n# next\nc\td\n3\t4\n', false)!;
    expect(t.comments).toEqual([]);
    expect(t.sections.map((s) => s.header)).toEqual([
      ['a', 'b'],
      ['c', 'd'],
    ]);
  });

  it('parses a VCF: ## lines are comments, #CHROM is the header', () => {
    const text = [
      '##fileformat=VCFv4.2',
      '##contig=<ID=chr1>',
      '#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO',
      'chr1\t100\t.\tA\tG\t50\tPASS\tDP=10',
      'chr1\t200\trs1\tC\tT\t.\tPASS\tDP=3',
    ].join('\n');
    const t = parseTable(text, false)!;
    expect(t.comments).toEqual(['fileformat=VCFv4.2', 'contig=<ID=chr1>']);
    expect(t.sections).toHaveLength(1);
    const s = t.sections[0];
    expect(s.header[0]).toBe('CHROM');
    expect(s.rows).toHaveLength(2);
    // ID is all '.'/text, QUAL has a '.' treated as empty.
    expect(s.numeric).toEqual([false, true, false, false, false, true, false, false]);
  });

  it('drops the final, possibly partial, line when truncated', () => {
    const text = 'a\tb\n1\t2\n3\t4\n5\t';
    expect(parseTable(text, false)!.sections[0].rows).toHaveLength(3);
    expect(parseTable(text, true)!.sections[0].rows).toEqual([
      ['1', '2'],
      ['3', '4'],
    ]);
  });

  it('returns null for prose', () => {
    const text = [
      'This is a log file, not a table.',
      'It has sentences with commas, here, and there.',
      'Some lines have none',
      'Others have, just one.',
    ].join('\n');
    expect(parseTable(text, false)).toBeNull();
  });

  it('returns null for fewer than two data lines', () => {
    expect(parseTable('a\tb\tc\n', false)).toBeNull();
    expect(parseTable('## comment\na\tb\n', false)).toBeNull();
    expect(parseTable('', false)).toBeNull();
  });

  it('pads short rows and extends the header for long rows', () => {
    const rows = ['a\tb\tc', '1\t2\t3', '4\t5', '6\t7\t8', '9\t10\t11\t12'];
    // Only 3 of 5 lines agree on two tabs — too ragged to sniff as a table.
    expect(parseTable(rows.join('\n'), false)).toBeNull();
    const many = [...rows, ...Array.from({ length: 20 }, (_, i) => `${i}\t${i}\t${i}`)];
    const s = parseTable(many.join('\n'), false)!.sections[0];
    expect(s.header).toEqual(['a', 'b', 'c', '']);
    expect(s.rows[1]).toEqual(['4', '5', '', '']);
    expect(s.rows[3]).toEqual(['9', '10', '11', '12']);
    expect(s.rows.every((r) => r.length === 4)).toBe(true);
  });

  it('detects numeric columns, treating NA/?/./empty as missing', () => {
    const text = [
      'int\tfloat\tsci\twithNA\tallEmpty\toneText\tnan',
      '1\t0.5\t1e-5\tNA\t\t1\tNaN',
      '-3\t.25\t2.5E+3\t?\t.\t2\t1',
      '42\t-1.0\t-4e2\t7\tNA\tx\t2',
    ].join('\n');
    const s = parseTable(text, false)!.sections[0];
    // NaN is a missing number (as fgbio/Picard print an undefined ratio), not text.
    expect(s.numeric).toEqual([true, true, true, true, false, false, true]);
  });

  it('keeps a metric column numeric through NaN / Infinity / nan / inf cells', () => {
    const text = [
      'sample\tfrac',
      'a\t0.5',
      'b\t0.25',
      'c\tNaN',
      'd\t-0.2',
      'e\tInfinity',
      'f\t-1.5',
      'g\tnan',
      'h\t-inf',
      'i\t0.125',
    ].join('\n');
    const s = parseTable(text, false)!.sections[0];
    expect(s.numeric).toEqual([false, true]);
    const sorted = sortRows(s.rows, 1, s.numeric[1], false).map((r) => r[1]);
    expect(sorted).toEqual([
      '-1.5',
      '-0.2',
      '0.125',
      '0.25',
      '0.5',
      'NaN',
      'Infinity',
      'nan',
      '-inf',
    ]);
  });

  it('keeps a row of empty cells as a row, not a section break', () => {
    const t = parseTable('sample\treads\tpct\nA\t10\t0.5\n\t\t\nB\t20\t0.7\nC\t30\t0.9\n', false)!;
    expect(t.sections).toHaveLength(1);
    expect(t.sections[0].header).toEqual(['sample', 'reads', 'pct']);
    expect(t.sections[0].rows).toEqual([
      ['A', '10', '0.5'],
      ['', '', ''],
      ['B', '20', '0.7'],
      ['C', '30', '0.9'],
    ]);
    // Only a truly blank line (spaces at most) separates sections.
    expect(parseTable('a\tb\n1\t2\n  \nc\td\n3\t4\n', false)!.sections).toHaveLength(2);
  });

  describe('a header line that starts with #', () => {
    it('is the header of a BED file with a #chrom line', () => {
      const text =
        '#chrom\tchromStart\tchromEnd\tname\nchr1\t100\t200\tgeneA\nchr1\t300\t400\tgeneB\n';
      for (const headerless of ['bed', null] as const) {
        const t = parseTable(text, false, headerless)!;
        expect(t.comments).toEqual([]);
        expect(t.sections[0].header).toEqual(['chrom', 'chromStart', 'chromEnd', 'name']);
        expect(t.sections[0].rows.map((r) => r[3])).toEqual(['geneA', 'geneB']);
      }
    });

    it("is samtools coverage's #rname header, and bcftools query -H's # [1]CHROM", () => {
      const coverage = parseTable(
        '#rname\tstartpos\tendpos\nchr1\t1\t1000\nchr2\t1\t2000\n',
        false,
      )!;
      expect(coverage.sections[0].header).toEqual(['rname', 'startpos', 'endpos']);
      expect(coverage.sections[0].rows).toHaveLength(2);
      const query = parseTable('# [1]CHROM\t[2]POS\nchr1\t100\nchr2\t200\n', false)!;
      expect(query.sections[0].header).toEqual(['[1]CHROM', '[2]POS']);
      expect(query.sections[0].rows).toHaveLength(2);
    });

    it('makes a one-record file with a # header a table', () => {
      const t = parseTable('#chrom\tstart\tend\nchr1\t0\t10\n', false)!;
      expect(t.sections[0].header).toEqual(['chrom', 'start', 'end']);
      expect(t.sections[0].rows).toEqual([['chr1', '0', '10']]);
    });

    it('stays a comment when it is ## metadata, set off by a blank line, or not one name per column', () => {
      // Picard: `## HISTOGRAM\t…` has as many fields as the header below it, but is metadata.
      const histogram = parseTable(
        '## HISTOGRAM\tjava.lang.Integer\ninsert_size\tAll_Reads.fr_count\n100\t5\n101\t7\n',
        false,
      )!;
      expect(histogram.comments).toEqual(['HISTOGRAM\tjava.lang.Integer']);
      expect(histogram.sections[0].header).toEqual(['insert_size', 'All_Reads.fr_count']);
      const apart = parseTable('# made\tby x\n\nname\tn\na\t1\n', false)!;
      expect(apart.comments).toEqual(['made\tby x']);
      expect(apart.sections[0].header).toEqual(['name', 'n']);
      const numbers = parseTable('# 1\t2\nname\tn\na\t1\n', false)!;
      expect(numbers.sections[0].header).toEqual(['name', 'n']);
      const narrow = parseTable('# generated by x\nname\tn\na\t1\n', false)!;
      expect(narrow.comments).toEqual(['generated by x']);
      expect(narrow.sections[0].header).toEqual(['name', 'n']);
    });

    it('stays a comment above a CSV header, however many commas it has', () => {
      const t = parseTable('# made by x, v2\nsample,reads\nA,10\nB,20\n', false)!;
      expect(t.delimiter).toBe(',');
      expect(t.comments).toEqual(['made by x, v2']);
      expect(t.sections[0].header).toEqual(['sample', 'reads']);
      expect(t.sections[0].numeric).toEqual([false, true]);
    });
  });

  describe('a headerless format', () => {
    it('names BED columns by the spec, keeps every record, and skips track lines', () => {
      const text =
        'track name=x\nchr1\t100\t200\tgeneA\nchr1\t300\t400\tgeneB\n\nchr2\t5\t9\tgeneC\n';
      const t = parseTable(text, false, 'bed')!;
      expect(t.comments).toEqual(['track name=x']);
      expect(t.sections).toHaveLength(1);
      expect(t.sections[0].header).toEqual(['chrom', 'chromStart', 'chromEnd', 'name']);
      expect(t.sections[0].rows.map((r) => r[3])).toEqual(['geneA', 'geneB', 'geneC']);
      expect(t.sections[0].numeric).toEqual([false, true, true, false]);
    });

    it('is a table from a single record', () => {
      const t = parseTable('chr1\t0\t10\n', false, 'bed')!;
      expect(t.sections[0].rows).toEqual([['chr1', '0', '10']]);
    });

    it('reads GFF3 as one section through ### directives, and stops at ##FASTA', () => {
      const text = [
        '##gff-version 3',
        'chr1\tsrc\tgene\t1\t100\t.\t+\t.\tID=g1',
        'chr1\tsrc\tmRNA\t1\t100\t.\t+\t.\tParent=g1',
        '###',
        'chr1\tsrc\tgene\t200\t300\t.\t-\t.\tID=g2',
        '##FASTA',
        '>chr1',
        'ACGT',
      ].join('\n');
      const t = parseTable(text, false, 'gff')!;
      expect(t.comments).toEqual(['gff-version 3']);
      expect(t.sections).toHaveLength(1);
      expect(t.sections[0].header).toEqual([
        'seqid',
        'source',
        'type',
        'start',
        'end',
        'score',
        'strand',
        'phase',
        'attributes',
      ]);
      expect(t.sections[0].rows.map((r) => r[8])).toEqual(['ID=g1', 'Parent=g1', 'ID=g2']);
    });

    it('names GTF columns, and columns past the spec by number', () => {
      const t = parseTable('chr1\tsrc\texon\t1\t9\t.\t+\t0\tgene_id "g";\textra\n', false, 'gtf')!;
      expect(t.sections[0].header).toEqual([
        'seqname',
        'source',
        'feature',
        'start',
        'end',
        'score',
        'strand',
        'frame',
        'attribute',
        'column 10',
      ]);
    });

    it('is told apart from a headed file by its name', () => {
      expect(headerlessFormatOf('peaks.bed')).toBe('bed');
      expect(headerlessFormatOf('PEAKS.BED.GZ')).toBe('bed');
      expect(headerlessFormatOf('genes.gff3')).toBe('gff');
      expect(headerlessFormatOf('genes.gff.gz')).toBe('gff');
      expect(headerlessFormatOf('genes.gtf')).toBe('gtf');
      expect(headerlessFormatOf('metrics.tsv')).toBeNull();
      expect(headerlessFormatOf('calls.vcf.gz')).toBeNull();
      expect(headerlessFormatOf('bed')).toBeNull();
      expect(headerlessFormatOf('s3://b/results/peaks.bed')).toBe('bed');
      expect(headerlessFormatOf('results/x.bed/README')).toBeNull();
    });
  });

  describe('CSV quoting across lines', () => {
    it('keeps a quoted newline inside its field', () => {
      const t = parseTable('id,note,val\n1,plain,10\n2,"multi\nline, note",20\n3,x,30\n', false)!;
      expect(t.delimiter).toBe(',');
      expect(t.sections[0].rows).toEqual([
        ['1', 'plain', '10'],
        ['2', 'multi\nline, note', '20'],
        ['3', 'x', '30'],
      ]);
    });

    it('drops a record cut off inside a quoted field by truncation, and only that record', () => {
      const t = parseTable('id,note\n1,a\n2,b\n3,"cut off\nmid field', true)!;
      expect(t.sections[0].rows).toEqual([
        ['1', 'a'],
        ['2', 'b'],
      ]);
    });

    it('reads the lines after a quote that never closes on their own', () => {
      const t = parseTable('id,note\n1,"oops\n2,b\n3,c\n', false)!;
      expect(t.sections[0].rows).toEqual([
        ['1', 'oops'],
        ['2', 'b'],
        ['3', 'c'],
      ]);
    });

    it('treats a quote inside an unquoted field as a character', () => {
      const t = parseTable('size,n\n5" disk,1\n3" disk,2\n', false)!;
      expect(t.sections[0].rows.map((r) => r[0])).toEqual(['5" disk', '3" disk']);
    });

    it('handles CRLF records', () => {
      const t = parseTable('a,b\r\n1,"x"\r\n2,y\r\n', false)!;
      expect(t.sections[0].rows).toEqual([
        ['1', 'x'],
        ['2', 'y'],
      ]);
    });
  });
});

describe('sortRows', () => {
  const rows = [
    ['a', '10'],
    ['b', '9'],
    ['c', ''],
    ['d', '-1.5'],
    ['e', 'NA'],
    ['f', '9'],
  ];
  const col = (rs: string[][], i: number) => rs.map((r) => r[i]);

  it('sorts numeric columns by value with empty/non-numeric cells last', () => {
    expect(col(sortRows(rows, 1, true, false), 0)).toEqual(['d', 'b', 'f', 'a', 'c', 'e']);
  });

  it('sorts descending, keeps equal keys in original order, and empties still last', () => {
    expect(col(sortRows(rows, 1, true, true), 0)).toEqual(['a', 'b', 'f', 'd', 'c', 'e']);
  });

  it('compares non-numeric columns lexically', () => {
    // Collation orders digit runs by value (chr2 before chr10); empty cells sort last.
    const labels = [['chr10'], ['chr2'], [''], ['chrX'], ['chr1']];
    expect(col(sortRows(labels, 0, false, false), 0)).toEqual([
      'chr1',
      'chr2',
      'chr10',
      'chrX',
      '',
    ]);
    expect(col(sortRows(labels, 0, false, true), 0)).toEqual(['chrX', 'chr10', 'chr2', 'chr1', '']);
  });

  it('treats the same column differently when numeric vs lexical', () => {
    const r = [['-2'], ['-10'], ['3']];
    expect(col(sortRows(r, 0, true, false), 0)).toEqual(['-10', '-2', '3']);
    // The collator ignores the '-' and orders the digit runs by value.
    expect(col(sortRows(r, 0, false, false), 0)).toEqual(['-2', '-10', '3']);
  });

  it('is stable and does not modify its input', () => {
    const r = [
      ['x', '1'],
      ['y', '1'],
      ['z', '1'],
    ];
    const copy = r.map((row) => [...row]);
    expect(col(sortRows(r, 1, true, false), 0)).toEqual(['x', 'y', 'z']);
    expect(col(sortRows(r, 1, true, true), 0)).toEqual(['x', 'y', 'z']);
    expect(col(sortRows(r, 1, false, true), 0)).toEqual(['x', 'y', 'z']);
    expect(r).toEqual(copy);
  });
});
