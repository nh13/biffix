// FastQC's results for an output, as the backend's `fastqc.rs` and qc.rs find and read them.
import { readFileSync } from 'node:fs';
import { FIXTURES } from './backend-fixtures.mjs';

// The FastQC data every mock zip holds (FastQC 0.12.1's own: the backend's test fixture), parsed as
// the backend's `fastqc.rs` does: `>>Name\tstatus` … `>>END_MODULE`, the last `#` line the columns,
// earlier `#` lines `[key, value]` extras; the `##FastQC\tversion` line the version.
function parseFastqc(text) {
  const report = { version: null, modules: [] };
  let m = null;
  for (const line of text.split(/\r?\n/)) {
    if (line.startsWith('##FastQC')) report.version = line.split('\t')[1] ?? null;
    else if (line === '>>END_MODULE') m = null;
    else if (line.startsWith('>>')) {
      const [name, status] = line.slice(2).split('\t');
      m = { name, status: status ?? '', columns: [], rows: [], extra: [] };
      report.modules.push(m);
    } else if (m && line.startsWith('#')) {
      if (m.columns.length) m.extra.push(m.columns);
      m.columns = line.slice(1).split('\t');
    } else if (m && line) m.rows.push(line.split('\t'));
  }
  // A header line before the columns is a `key\tvalue` pair (`Total Deduplicated Percentage`).
  for (const mod of report.modules) mod.extra = mod.extra.filter((kv) => kv.length === 2);
  return report;
}
const FASTQC = parseFastqc(readFileSync(new URL('fastqc/fastqc_data.txt', FIXTURES), 'utf8'));
// What every FastQC answer carries: the version, when known, and the modules (an empty `extra`
// left out).
const FASTQC_RESULT = {
  ...(FASTQC.version ? { version: FASTQC.version } : {}),
  modules: FASTQC.modules.map(({ extra, ...mod }) => ({
    ...mod,
    ...(extra.length ? { extra } : {}),
  })),
};

// FastQC's file names, as the backend's fastqc.rs reads them: a suffix in any case, after a
// non-empty stem.
const stemOf = (name, suffix) =>
  name.length > suffix.length && name.toLowerCase().endsWith(suffix)
    ? name.slice(0, name.length - suffix.length)
    : null;
const isFastq = (name) => {
  const lower = name.toLowerCase().replace(/\.(gz|bz2)$/, '');
  return ['.fastq', '.fq', '.csfastq'].some((x) => stemOf(lower, x) != null);
};
// The zip FastQC names for a FASTQ (`zip_name_for`): each of these endings stripped once, in
// order, as written (case-sensitive).
const zipNameFor = (name) => {
  let stem = name;
  for (const x of ['.gz', '.bz2', '.txt', '.fastq', '.fq', '.csfastq', '.sam', '.bam', '.ubam'])
    if (stem.endsWith(x)) stem = stem.slice(0, -x.length);
  return `${stem}_fastqc.zip`;
};
const folderOf = (path) => path.slice(0, path.lastIndexOf('/') + 1);

// The output roles a report lookup counts (store/outputs.rs `REPORT_ROLES`).
const REPORT_ROLES = ['published', 'output'];
// The zip FastQC wrote for a FASTQ (qc/viewer.rs `fastq_zip`), among the outputs of the roles a
// report lookup counts, for a FASTQ of one of them: the shallowest one of its name under the
// FASTQ's own folder, leaving out those in (or below) the folder of a nearer FASTQ of the same
// name, when no other is as shallow; else the only one in the run, when this FASTQ is the only
// output of its name.
function fastqZip(all, o) {
  if (!REPORT_ROLES.includes(o.role)) return null;
  const rows = all.filter((x) => REPORT_ROLES.includes(x.role));
  const zip = zipNameFor(o.name);
  const dir = folderOf(o.path);
  const named = rows.filter((x) => x.name === zip);
  const nearer = rows
    .filter((x) => x.name === o.name && x.path !== o.path)
    .map((x) => folderOf(x.path))
    .filter((f) => f.length > dir.length && f.startsWith(dir));
  const under = named.filter(
    (x) => x.path.startsWith(dir) && !nearer.some((f) => x.path.startsWith(f)),
  );
  if (under.length) {
    const depth = (x) => x.path.slice(dir.length).split('/').length;
    const shallowest = Math.min(...under.map(depth));
    const nearest = under.filter((x) => depth(x) === shallowest);
    return nearest.length === 1 ? nearest[0] : null;
  }
  const sameName = rows.filter((x) => x.name === o.name).length;
  return named.length === 1 && sameName === 1 ? named[0] : null;
}

// The FastQC result for an output (`GET …/fastqc`), as the backend finds it: a zip is itself, an
// HTML report's sibling zip, a FASTQ's zip (`fastqZip`); `null` when there is none (404).
export function fastqcFor(rows, o) {
  const dir = folderOf(o.path);
  const htmlStem = stemOf(o.name, '_fastqc.html');
  const zip = stemOf(o.name, '_fastqc.zip')
    ? o
    : htmlStem
      ? rows.find((x) => x.path === `${dir}${htmlStem}_fastqc.zip`)
      : isFastq(o.name)
        ? fastqZip(rows, o)
        : null;
  if (!zip) return null;
  const html = rows.find(
    (x) => x.path === `${folderOf(zip.path)}${stemOf(zip.name, '_fastqc.zip')}_fastqc.html`,
  );
  return {
    output_id: zip.id,
    ...(html ? { html_output_id: html.id } : {}),
    ...FASTQC_RESULT,
  };
}
