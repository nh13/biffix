// Sample detection (`POST /samples/detect`) for the samplesheet builder.
import { readJson } from '../lib/http.mjs';

// A simplified stand-in for biffix_workflow::samples::detect (Illumina, _R1/_1 and single-end
// names; chosen parts; a regex), enough to drive the builder in tests.
function mockDetect({ files = [], rule = {}, samplesheet }) {
  const stem = (n) => n.replace(/\.(fastq|fq)(\.gz)?$/i, '');
  const tokenize = (n) =>
    stem(n)
      .split(/([_.-])/)
      .filter((t) => t !== '')
      .map((text) => ({ text, separator: /^[_.-]$/.test(text) }));
  let re = null;
  if (rule.method?.kind === 'regex') {
    try {
      re = new RegExp(rule.method.pattern);
    } catch (e) {
      return `invalid regular expression: ${e.message}`;
    }
    // As the server's `NoGroup`: a pattern with no group has nothing to read the id from.
    if (new RegExp(`${rule.method.pattern}|`).exec('').length === 1)
      return 'the regular expression needs a capture group for the sample id (one named `sample`, else the first group)';
  }
  const groups = new Map();
  const unmatched = [];
  for (const { uri } of files) {
    const name = uri.split('/').pop();
    const m = /^(.+?)(?:_S\d+)?(?:_L(\d{3}))?[._-]R?([12])(?:_\d{3})?$/i.exec(stem(name));
    // A name that is only a read marker (r1, R2, read_1) is named by its folder, except at a
    // bucket root.
    const marker = /^(?:r|read|read_|r_)?([12])$/i.exec(stem(name));
    const folder = uri
      .replace(/^[a-z0-9]+:\/\/[^/]+\/?/i, '')
      .split('/')
      .slice(0, -1)
      .pop();
    const byFolder = marker && folder;
    const read = byFolder ? marker[1] : m ? m[3] : '1';
    let id = byFolder ? folder : m ? m[1] : stem(name);
    if (rule.method?.kind === 'tokens') {
      const tokens = tokenize(name);
      const sel = rule.method.selected ?? [];
      if (!sel.length || sel.some((i) => i >= tokens.length)) {
        unmatched.push(uri);
        continue;
      }
      id = sel.map((i) => tokens[i].text).join('');
    } else if (re) {
      const c = re.exec(name);
      if (!c || !c[1]) {
        unmatched.push(uri);
        continue;
      }
      id = c.groups?.sample ?? c[1];
    }
    const g = groups.get(id) ?? { id, read1: [], read2: [], lanes: 1, problems: [] };
    (read === '2' ? g.read2 : g.read1).push(uri);
    groups.set(id, g);
  }
  const samples = [...groups.values()].sort((a, b) =>
    a.id.localeCompare(b.id, undefined, { numeric: true }),
  );
  for (const s of samples) if (!s.read1.length) s.problems.push('read 2 without read 1');
  const loose = (id) => id.toLowerCase().replace(/(^|\D)0+(\d)/g, '$1$2');
  const suggestions = [];
  for (const a of samples.filter((s) => s.read1.length && !s.read2.length))
    for (const b of samples.filter((s) => !s.read1.length && s.read2.length))
      if (loose(a.id) === loose(b.id))
        suggestions.push({ ids: [a.id, b.id], into: b.id.length >= a.id.length ? b.id : a.id });
  const rows = samplesheet
    ? samples.map((s) =>
        Object.fromEntries(
          samplesheet.columns.map((c) => [
            c.name,
            c.role === 'sample_id'
              ? s.id
              : c.role === 'read1'
                ? (s.read1[0] ?? '')
                : c.role === 'read2'
                  ? (s.read2[0] ?? '')
                  : (c.default ?? ''),
          ]),
        ),
      )
    : [];
  const example = files[0]?.uri.split('/').pop();
  return {
    detection: { samples, unmatched, suggestions },
    rows,
    tokens: example ? tokenize(example) : [],
    example,
  };
}

/** `POST /samples/detect`. */
function detectSamples({ req, send }) {
  readJson(req, send, (body) => {
    const r = mockDetect(body);
    if (typeof r === 'string') send(400, r);
    else send(200, r);
  });
}

export const SAMPLE_ROUTES = [['POST', '/samples/detect', detectSamples]];
