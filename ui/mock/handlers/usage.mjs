// The usage route (usage.rs): estimated spend over a period, grouped.
const row = (key, runs, compute, ebs) => ({
  key,
  runs,
  compute_usd: compute,
  ebs_usd: ebs,
  total_usd: compute + ebs,
});
// The rows of each grouping (`UsageGroup`).
const USAGE_ROWS = {
  month: [row('2026-08', 3, 41.2, 2.1), row('2026-09', 5, 96.5, 4.4)],
  workflow: [row('nf-core/sarek', 4, 102.3, 5.2), row('nf-core/rnaseq', 4, 35.4, 1.3)],
  submitter: [
    row('alice@lab.example.org', 6, 120.0, 5.5),
    row('bob@lab.example.org', 2, 17.7, 1.0),
  ],
  instance_type: [row('c6a.4xlarge', 8, 130.1, 6.0), row('m6i.large', 8, 7.6, 0.5)],
  project: [row('genomics', 8, 137.7, 6.5)],
};

const DAY_MS = 86_400_000;
const formatDate = (ms) => new Date(ms).toISOString().slice(0, 10);
/** usage.rs `parse_date`: exactly `YYYY-MM-DD`, a real day from 1970, as epoch ms; else `null`. */
function parseDate(text) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!m || Number(m[1]) < 1970) return null;
  const ms = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return formatDate(ms) === text ? ms : null;
}
/** usage.rs `period`: the given dates, each defaulting to this month's bounds (`to` exclusive),
 * or the 400's message. */
function period(fromText, toText) {
  const today = new Date(Math.floor(Date.now() / DAY_MS) * DAY_MS);
  const monthStart = Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), 1);
  const nextMonth = Date.UTC(today.getUTCFullYear(), today.getUTCMonth() + 1, 1);
  const parsed = [fromText, toText].map((t) => (t ? parseDate(t) : undefined));
  const badText = [fromText, toText].find((t, i) => t && parsed[i] === null);
  if (badText != null) return { error: `\`${badText}\` is not a date (YYYY-MM-DD)` };
  const [from, to] = parsed;
  if (from != null && to == null && nextMonth <= from)
    return {
      error: `\`from\` ${formatDate(from)} is not before the default \`to\` (${formatDate(nextMonth)}, the start of next month); pass \`to\``,
    };
  if (from == null && to != null && to <= monthStart)
    return {
      error: `\`to\` ${formatDate(to)} is not after the default \`from\` (${formatDate(monthStart)}, the start of this month); pass \`from\``,
    };
  const [f, t] = [from ?? monthStart, to ?? nextMonth];
  if (t <= f) return { error: '`to` must be after `from`' };
  return { from: formatDate(f), to: formatDate(t) };
}

/** `GET /usage`: estimated spend over a period, grouped. As usage.rs reads them: a blank
 * `group_by` is the default (`month`), and blank dates are this month's bounds. */
function getUsage({ query, send }) {
  const groupBy = query.get('group_by') || 'month';
  // Own keys only: `constructor` is not a grouping.
  if (!Object.hasOwn(USAGE_ROWS, groupBy))
    return send(
      400,
      `unknown group_by \`${groupBy}\` (month, workflow, submitter, instance_type or project)`,
    );
  const { from, to, error } = period(query.get('from'), query.get('to'));
  if (error) return send(400, error);
  send(200, {
    from,
    to,
    group_by: groupBy,
    rows: USAGE_ROWS[groupBy],
    total: row('total', 8, 137.7, 6.5),
  });
}

export const USAGE_ROUTES = [['GET', '/usage', getUsage]];
