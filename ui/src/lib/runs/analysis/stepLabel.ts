// A pipeline step's name for a person: the author's label (`samples.labels` in `biffix.yaml`) when
// there is one, else a readable form of the engine's identifier — `FASTQC` reads "FastQC",
// `STAR_ALIGN` "STAR align", `NFCORE_RNASEQ:RNASEQ:SALMON_QUANT (Tumor_1)` "Salmon quant". One
// helper, so the samples grid, the failure text and every step list agree. Pure, so it is
// unit-tested.

/** Tools spelled their own way, by the identifier's word (upper case). */
const TOOLS: Record<string, string> = {
  FASTQC: 'FastQC',
  MULTIQC: 'MultiQC',
  STAR: 'STAR',
  SALMON: 'Salmon',
  KALLISTO: 'kallisto',
  RSEM: 'RSEM',
  HISAT2: 'HISAT2',
  BOWTIE: 'Bowtie',
  BOWTIE2: 'Bowtie2',
  BWA: 'BWA',
  BWAMEM: 'BWA-MEM',
  BWAMEM2: 'BWA-MEM2',
  MINIMAP2: 'minimap2',
  SAMTOOLS: 'SAMtools',
  BCFTOOLS: 'BCFtools',
  BEDTOOLS: 'BEDTools',
  GATK: 'GATK',
  GATK4: 'GATK4',
  PICARD: 'Picard',
  QUALIMAP: 'Qualimap',
  TRIMGALORE: 'Trim Galore',
  TRIM_GALORE: 'Trim Galore',
  FASTP: 'fastp',
  CUTADAPT: 'Cutadapt',
  FEATURECOUNTS: 'featureCounts',
  SUBREAD: 'Subread',
  STRINGTIE: 'StringTie',
  DESEQ2: 'DESeq2',
  DEEPVARIANT: 'DeepVariant',
  MOSDEPTH: 'mosdepth',
  PRESEQ: 'Preseq',
  RSEQC: 'RSeQC',
  DUPRADAR: 'dupRadar',
  SORTMERNA: 'SortMeRNA',
  UMITOOLS: 'UMI-tools',
  FGBIO: 'fgbio',
  FGUMI: 'fgumi',
  VEP: 'VEP',
  SNPEFF: 'snpEff',
  TABIX: 'tabix',
  BGZIP: 'bgzip',
  GUNZIP: 'gunzip',
  CUSTOM: 'Custom',
  QC: 'QC',
  VCF: 'VCF',
  BAM: 'BAM',
  CRAM: 'CRAM',
  SAM: 'SAM',
  FASTQ: 'FASTQ',
  UMI: 'UMI',
  DNA: 'DNA',
  RNA: 'RNA',
  CNV: 'CNV',
  SV: 'SV',
  SNP: 'SNP',
  PCA: 'PCA',
  BQSR: 'BQSR',
};

/**
 * The engine identifier's own name, without what the engine adds around it: a Nextflow process
 * path (`WF:SUBWF:PROC` → `PROC`), a tag in parentheses (`PROC (Tumor_1)`), a trailing hash
 * (`align_a1b2c3d4e5`).
 */
export function stepIdentifier(raw: string): string {
  let s = raw.trim();
  s = s.replace(/\s*\([^)]*\)\s*$/, '');
  s = s.split(':').pop() ?? s;
  // A hash has a letter among its hex digits: an all-digit suffix (`merge_20240101`) is a date or a
  // number, part of the step's name.
  s = s.replace(/[_-](?=[0-9]*[a-f])[0-9a-f]{8,}$/i, '');
  return s.trim() || raw.trim();
}

/** A readable form of an engine identifier: tools spelled their way, other words in sentence case. */
export function readableStep(raw: string): string {
  const id = stepIdentifier(raw);
  const whole = TOOLS[id.toUpperCase()];
  if (whole) return whole;
  // Split on `_`, `-`, spaces and camelCase humps.
  const words = id
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .split(/[\s_-]+/)
    .filter(Boolean);
  if (words.length === 0) return raw;
  const out = words.map((w) => TOOLS[w.toUpperCase()] ?? w.toLowerCase());
  // Sentence case: the first word capitalized unless a tool spells it otherwise.
  if (!TOOLS[words[0].toUpperCase()]) out[0] = out[0].charAt(0).toUpperCase() + out[0].slice(1);
  return out.join(' ');
}

/**
 * A step's label: the server's (the grid's step label) when it says the label is the author's
 * (`authored`), shown as written; else the author's `labels` map (under the full name, else the last
 * part of a process path); else [`readableStep`]. A label the server made from the group name is
 * worded here instead.
 */
export function stepLabel(
  group: string,
  opts: {
    label?: string | null;
    authored?: boolean | null;
    labels?: Record<string, string> | null;
  } = {},
): string {
  const { label, authored: fromAuthor, labels } = opts;
  if (fromAuthor && label && label.trim()) return label;
  // Own keys only: a step named `toString` or `constructor` must not find Object's members.
  const own = (key: string) =>
    labels && Object.hasOwn(labels, key) && typeof labels[key] === 'string'
      ? labels[key]
      : undefined;
  const authored = own(group) ?? own(group.split(':').pop() ?? group);
  if (authored && authored.trim()) return authored;
  return readableStep(group);
}

/** The author's step labels from a version's form schema (`samples.labels`), if any. */
export function authorLabels(formSchema: unknown): Record<string, string> | null {
  if (!formSchema || typeof formSchema !== 'object') return null;
  const samples = (formSchema as { samples?: unknown }).samples;
  if (!samples || typeof samples !== 'object') return null;
  const labels = (samples as { labels?: unknown }).labels;
  if (!labels || typeof labels !== 'object') return null;
  // `fromEntries` defines own keys, so a step named `__proto__` keeps its label.
  return Object.fromEntries(
    Object.entries(labels as Record<string, unknown>).filter(
      (e): e is [string, string] => typeof e[1] === 'string',
    ),
  );
}
