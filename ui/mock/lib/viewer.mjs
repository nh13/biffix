// biffix-workflow's `viewer_of` (crates/biffix-workflow/src/viewer.rs): by extension, looking
// through a `.gz` only for the text kinds (the preview decompresses; a browser can't show a gzipped
// image or page).
export function viewerOf(path) {
  const lower = path.split('/').pop().toLowerCase();
  const gz = lower.endsWith('.gz');
  const base = gz ? lower.slice(0, -3) : lower;
  if (!base.includes('.')) return null;
  const ext = base.split('.').pop();
  if (['tsv', 'csv', 'txt', 'metrics', 'bed', 'gtf', 'gff', 'gff3', 'vcf'].includes(ext))
    return 'table';
  const text = ['log', 'json', 'yaml', 'yml', 'md', 'fa', 'fasta', 'fq', 'fastq', 'sam'];
  if ([...text, 'out', 'err'].includes(ext)) return 'text';
  if (gz) return null;
  if (['png', 'jpg', 'jpeg', 'gif', 'svg', 'webp'].includes(ext)) return 'image';
  if (ext === 'pdf') return 'pdf';
  if (['html', 'htm'].includes(ext)) return 'html';
  return null;
}
