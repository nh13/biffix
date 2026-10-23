// A run's MultiQC report and its FastQC tables, as the backend's `multiqc.rs` and qc.rs find and
// read them.
import { readFileSync } from 'node:fs';
import { FIXTURES } from './backend-fixtures.mjs';
import { bytewise } from './paging.mjs';

// MultiQC's FastQC tables (MultiQC 1.35's own: the backend's test fixtures), read as the backend's
// `multiqc.rs` `fastqc_statuses` reads them: `Sample`, FastQC's basic statistics, then each module's
// status (a column named for a FastQC module, as MultiQC names it, or any other column whose every
// filled cell is a status).
const FASTQC_MODULE_NAMES = {
  basic_statistics: 'Basic Statistics',
  per_base_sequence_quality: 'Per base sequence quality',
  per_tile_sequence_quality: 'Per tile sequence quality',
  per_sequence_quality_scores: 'Per sequence quality scores',
  per_base_sequence_content: 'Per base sequence content',
  per_sequence_gc_content: 'Per sequence GC content',
  per_base_n_content: 'Per base N content',
  sequence_length_distribution: 'Sequence Length Distribution',
  sequence_duplication_levels: 'Sequence Duplication Levels',
  overrepresented_sequences: 'Overrepresented sequences',
  adapter_content: 'Adapter Content',
  kmer_content: 'Kmer Content',
};
const FASTQC_STAT_COLUMNS = {
  Filename: 'filename',
  'Total Sequences': 'total_sequences',
  'Total Bases': 'total_bases',
  'Sequences flagged as poor quality': 'poor_quality',
  // FastQC 0.11 renamed 0.10's `Filtered Sequences`.
  'Filtered Sequences': 'poor_quality',
  'Sequence length': 'sequence_length',
  '%GC': 'percent_gc',
  avg_sequence_length: 'avg_sequence_length',
  median_sequence_length: 'median_sequence_length',
  total_deduplicated_percentage: 'percent_duplicates',
};
const isStatus = (cell) => /^(pass|warn|fail)$/i.test(cell);
// As Rust's `str::parse::<f64>` (finite only), not JS's laxer `Number` (which takes '' and '0x1').
const finite = (cell) => {
  if (!/^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(cell)) return undefined;
  const v = Number(cell);
  return Number.isFinite(v) ? v : undefined;
};
function fastqcColumn(name, cells) {
  if (FASTQC_MODULE_NAMES[name]) return { module: FASTQC_MODULE_NAMES[name] };
  if (FASTQC_STAT_COLUMNS[name]) return { stat: FASTQC_STAT_COLUMNS[name] };
  const filled = cells.filter((c) => c !== '');
  if (!name || !filled.length || !filled.every(isStatus)) return {};
  const words = name.replaceAll('_', ' ');
  return { module: words.charAt(0).toUpperCase() + words.slice(1) };
}
function fastqcStat(stat, cell) {
  const v = finite(cell);
  switch (stat) {
    case 'filename':
      return cell || undefined;
    case 'total_sequences':
    case 'poor_quality':
      return v != null && v >= 0 && v < 1e18 ? Math.round(v) : undefined;
    case 'total_bases':
    case 'sequence_length':
      if (!cell) return undefined;
      return v != null && Number.isInteger(v) && Math.abs(v) < 1e15 ? v.toFixed(0) : cell;
    case 'percent_duplicates':
      return v != null && v >= 0 && v <= 100 ? 100 - v : undefined;
    default:
      return v;
  }
}
function parseMultiqcFastqc(text) {
  const [header, ...lines] = text.split(/\r?\n/).filter((l) => l.trim());
  if (header == null) return [];
  const names = header
    .split('\t')
    .slice(1)
    .map((n) => n.trim());
  const rows = lines
    .map((line) => line.split('\t').map((c) => c.trim()))
    .filter((cells) => cells[0])
    .map(([name, ...cells]) => ({ name, cells }));
  const columns = names.map((name, i) =>
    fastqcColumn(
      name,
      rows.map((r) => r.cells[i]).filter((c) => c != null),
    ),
  );
  return rows.map(({ name, cells }) => {
    const out = { fastqc_name: name, modules: [], stats: {} };
    columns.forEach((column, i) => {
      const cell = cells[i];
      if (cell == null) return;
      if (column.module) {
        if (isStatus(cell)) out.modules.push({ name: column.module, status: cell });
      } else if (column.stat) {
        // The file name is the row's own; every other stat goes under `stats`.
        const into = column.stat === 'filename' ? out : out.stats;
        const v = fastqcStat(column.stat, cell);
        if (v !== undefined) into[column.stat] = v;
        else delete into[column.stat];
      }
    });
    return out;
  });
}
// The fixture each mock FastQC table holds, by file name.
export const MULTIQC_FASTQC_TABLES = Object.fromEntries(
  [
    'multiqc_fastqc.txt',
    'sections/multiqc_fastqc_fastqc_raw.txt',
    'sections/multiqc_fastqc_fastqc_trimmed.txt',
  ].map((f) => [
    f.split('/').pop(),
    parseMultiqcFastqc(readFileSync(new URL(`multiqc/${f}`, FIXTURES), 'utf8')),
  ]),
);
// A `multiqc_data/` file's FastQC section, as the backend's `fastqc_table_section`: `null` for
// `multiqc_fastqc.txt`, the anchor for `multiqc_fastqc_<anchor>.txt`, `undefined` for another file.
export const fastqcTableSection = (name) => {
  const m = /^multiqc_fastqc(?:_(.+))?\.txt$/.exec(name);
  return m ? (m[1] ?? null) : undefined;
};
// The format a `multiqc_data/` file says MultiQC wrote its data in when not TSV, as the backend's
// `non_tsv_data_format`.
export const nonTsvDataFormat = (name) => {
  const m = /^(multiqc_fastqc(?:_.*)?|multiqc_sources)\.(csv|json|yaml)$/.exec(name);
  return m ? m[2].toUpperCase() : undefined;
};
// A FASTQ's name without its folder and sequence-file extensions, as the backend's `fastq_stem`.
export const fastqStem = (filename) =>
  filename
    .split('/')
    .pop()
    .replace(/(.)\.(gz|bz2|xz|zst)$/i, '$1')
    .replace(/(.)\.(fastq|fq|bam|sam|cram)$/i, '$1');
// The run's sample that owns a file name, as the backend's `owning_sample`: the longest id the name
// is, or starts with followed by a character that cannot continue an id; on a tie, the first.
export const owningSample = (name, ids) =>
  ids
    .filter((id) => id && name.startsWith(id) && /^($|[^A-Za-z0-9])/.test(name.slice(id.length)))
    .reduce((best, id) => (best == null || id.length > best.length ? id : best), null);
// The run's MultiQC report, as the backend's `shallowest_multiqc`: of the outputs named
// `multiqc_report.html`, the run's own before those of the run it resumed (`reused_from`), then the
// shallowest, then by URI.
export const shallowestMultiqc = (rows) =>
  rows
    .filter((o) => o.name === 'multiqc_report.html')
    .map((o) => [o.reused_from ? 1 : 0, o.s3_uri.split('/').length, o.s3_uri, o])
    .sort((a, b) => a[0] - b[0] || a[1] - b[1] || bytewise(a[2], b[2]))
    .map(([, , , o]) => o)[0];
