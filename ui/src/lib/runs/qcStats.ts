// MultiQC's General Statistics shown natively: how a value reads, and whether
// MultiQC's per-column rules call it a pass, a warning or a failure. Pure, so it is unit-tested.

/** The column hints the helpers read (a subset of the API's `QcColumn`). */
export interface QcColumnHints {
  suffix?: string | null;
  decimals?: number | null;
  cond_formatting_rules?: unknown;
}

export type QcStatus = 'pass' | 'warn' | 'fail';

/**
 * A value as the report shows it: grouped digits, the column's decimals (else MultiQC's default of
 * one, none for a whole number), then its suffix.
 */
export function formatQcValue(value: number | null | undefined, col: QcColumnHints): string {
  if (value == null || !Number.isFinite(value)) return '';
  const decimals = col.decimals ?? (Number.isInteger(value) ? 0 : 1);
  const text = value.toLocaleString('en-US', {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  });
  return `${text}${col.suffix ?? ''}`;
}

type Rule = Record<string, unknown>;

// A rule matcher that mirrors the semantics of MultiQC's table conditional-formatting rules:
// - each operator key of a rule is checked on its own and ANY that holds matches the rule (MultiQC
//   documents one operator per rule, but accepts and ORs several);
// - the string operators compare case-insensitively;
// - a numeric operator whose target is not a number stops that rule (Python's `float()` raises),
//   keeping whatever earlier keys matched, in MultiQC's check order.
// It stays in the browser rather than behind the API: the rules and values are the report's own
// data (read through, not a projection of run state), MultiQC exposes no status per cell, and a
// Rust port would be a second copy of the same few lines. Levels other than pass / warn / fail
// (custom `table_cond_formatting_colours`) carry no meaning the UI can show, so they are skipped.

/** The rule keys in MultiQC's check order. */
const OPERATORS = ['s_eq', 's_contains', 's_ne', 'eq', 'ne', 'gt', 'lt', 'ge', 'le'] as const;

/** Python's `float(x)`, or null where it raises. */
function pyFloat(x: unknown): number | null {
  if (typeof x === 'number') return x;
  if (typeof x === 'boolean') return x ? 1 : 0;
  if (typeof x !== 'string') return null;
  const text = x.trim().toLowerCase();
  const inf = text.match(/^([+-]?)(inf|infinity)$/);
  if (inf) return inf[1] === '-' ? -Infinity : Infinity;
  if (/^[+-]?nan$/.test(text)) return NaN;
  if (!/^[+-]?(\d+\.?\d*|\.\d+)(e[+-]?\d+)?$/.test(text)) return null;
  return Number(text);
}

/** Python's `str(x)` of a float: `30.0`, `0.1`, `1e-07`, `1.5e+16`. */
function pyFloatStr(n: number): string {
  if (Number.isNaN(n)) return 'nan';
  if (!Number.isFinite(n)) return n > 0 ? 'inf' : '-inf';
  const [mantissa, exp] = n.toExponential().split('e');
  const e = Number(exp);
  if (e < -4 || e >= 16) {
    const sign = e < 0 ? '-' : '+';
    return `${mantissa}e${sign}${String(Math.abs(e)).padStart(2, '0')}`;
  }
  const text = String(n);
  return text.includes('.') ? text : `${text}.0`;
}

/** Python's `str(x)` of a rule's target (a string as is, a number as Python prints it). */
function pyStr(x: unknown): string {
  if (typeof x === 'string') return x;
  if (typeof x === 'boolean') return x ? 'True' : 'False';
  if (typeof x === 'number') return Number.isInteger(x) ? String(x) : pyFloatStr(x);
  if (x == null) return 'None';
  return String(x);
}

/**
 * Whether `value` meets one MultiQC rule (`{gt: 30}`, `{s_eq: 'x'}`, …): any of its keys holding.
 * A General Statistics value reaches the UI as a number, and whether the report held it as an int
 * (`30`) or a float (`30.0`) is lost, so a string operator accepts either spelling.
 */
function meets(value: number, rule: Rule): boolean {
  const spellings = Number.isInteger(value)
    ? [String(value), pyFloatStr(value)]
    : [pyFloatStr(value)];
  for (const op of OPERATORS) {
    if (!Object.hasOwn(rule, op)) continue;
    const target = rule[op];
    if (op === 's_eq' || op === 's_contains' || op === 's_ne') {
      const want = pyStr(target).toLowerCase();
      if (op === 's_eq' && spellings.includes(want)) return true;
      if (op === 's_contains' && spellings.some((s) => s.includes(want))) return true;
      if (op === 's_ne' && !spellings.includes(want)) return true;
      continue;
    }
    const n = pyFloat(target);
    if (n === null) return false;
    if (NUMERIC[op](value, n)) return true;
  }
  return false;
}

const NUMERIC: Record<'eq' | 'ne' | 'gt' | 'lt' | 'ge' | 'le', (v: number, n: number) => boolean> =
  {
    eq: (v, n) => v === n,
    ne: (v, n) => v !== n,
    gt: (v, n) => v > n,
    lt: (v, n) => v < n,
    ge: (v, n) => v >= n,
    le: (v, n) => v <= n,
  };

/**
 * The status MultiQC's `cond_formatting_rules` give `value`: rules are checked pass, then warn,
 * then fail, and the last that matches wins (as in the report). Null without rules or a match.
 */
export function qcStatus(value: number | null | undefined, rules: unknown): QcStatus | null {
  if (value == null || !rules || typeof rules !== 'object') return null;
  let status: QcStatus | null = null;
  for (const level of ['pass', 'warn', 'fail'] as const) {
    const list = (rules as Record<string, unknown>)[level];
    if (
      Array.isArray(list) &&
      list.some((r) => r && typeof r === 'object' && meets(value, r as Rule))
    )
      status = level;
  }
  return status;
}

/**
 * Samples in order by one column (missing values last), or by name. Names order naturally (`S2`
 * before `S10`), both when sorting by name and among the samples missing the column, which stay in
 * ascending name order whichever way the column sorts.
 */
export function sortSamples<T extends { name: string; values: Record<string, number> }>(
  samples: T[],
  by: string | null,
  descending: boolean,
): T[] {
  const sign = descending ? -1 : 1;
  const byName = (a: T, b: T) => a.name.localeCompare(b.name, undefined, { numeric: true });
  return [...samples].sort((a, b) => {
    if (!by) return sign * byName(a, b);
    const x = a.values[by];
    const y = b.values[by];
    if (x == null && y == null) return byName(a, b);
    if (x == null) return 1;
    if (y == null) return -1;
    return sign * (x - y);
  });
}
