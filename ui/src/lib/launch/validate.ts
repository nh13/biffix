// The launch form's live validation: a port of the shared Rust validator
// (`biffix_workflow::validate`), so the form shows the errors the server would return before a
// launch is attempted. The server stays authoritative; `validate.test.ts` replays the Rust validator's own
// cases (`schemas/validate-cases.json`) against this port so the two cannot drift apart unnoticed.
// Where JS and the Rust `regex` crate may read a `rules` pattern differently (`sharedPattern`), the
// port skips the rule: the form may miss an error the server reports, never report one it wouldn't.

import type { FormField } from '$lib/workflows/schema';

export interface ValidationError {
  field: string;
  message: string;
}

/**
 * The comparable spelling of a param value, so `true`, `"true"` and a form's `"true"` agree, and
 * `3`, `3.0` and `"3"` agree (`formschema::value_key`).
 */
export function valueKey(value: unknown): string {
  if (value === null || value === undefined) return '';
  if (typeof value === 'string') return value;
  if (typeof value === 'boolean') return String(value);
  // A whole number prints without a fraction in both languages (`3.0` → `3`).
  if (typeof value === 'number') return String(value);
  return JSON.stringify(value);
}

function isUnset(v: unknown): boolean {
  return v === undefined || v === null;
}

/** Empty or only whitespace, as Rust's `str::trim` reads it. */
function isBlank(s: string): boolean {
  return /^[\p{White_Space}]*$/u.test(s);
}

/** `s` trimmed as Rust's `str::trim` trims (White_Space; JavaScript's also strips U+FEFF). */
function rustTrim(s: string): string {
  return s.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, '');
}

/**
 * The fields whose `show_if` conditions reach back to themselves (`validate::show_if_cycles`).
 * Registration warns that such a field is always shown, and `inactiveFields` shows it.
 */
export function showIfCycles(fields: readonly FormField[]): Set<string> {
  const byName = new Map(fields.map((f) => [f.name, f]));
  const deps = (name: string) => (byName.get(name)?.show_if ?? []).map((c) => c.field);
  const cyclic = new Set<string>();
  for (const field of fields) {
    const stack = deps(field.name);
    const seen = new Set<string>();
    while (stack.length > 0) {
      const next = stack.pop()!;
      if (next === field.name) {
        cyclic.add(field.name);
        break;
      }
      if (!seen.has(next)) {
        seen.add(next);
        stack.push(...deps(next));
      }
    }
  }
  return cyclic;
}

/**
 * The fields a form hides for these params: a field is inactive when any `show_if` condition
 * fails, or names a field that is itself inactive. A condition compares the other field's value —
 * the given one, else its default. A field on a `show_if` cycle is always shown, whatever the
 * values; a field gated on one is judged as usual.
 */
export function inactiveFields(
  fields: readonly FormField[],
  params: Record<string, unknown>,
): Set<string> {
  const byName = new Map(fields.map((f) => [f.name, f]));
  const cyclic = showIfCycles(fields);
  const effective = (name: string): string => {
    const given = params[name];
    if (!isUnset(given)) return valueKey(given);
    const def = byName.get(name)?.default;
    return isUnset(def) ? '' : valueKey(def);
  };
  // With the cyclic fields settled as shown, the rest of the graph is acyclic, so each memoized
  // answer is final and the result does not depend on field order.
  const memo = new Map<string, boolean>();
  const active = (name: string): boolean => {
    const known = memo.get(name);
    if (known !== undefined) return known;
    const field = byName.get(name);
    if (!field) return true;
    const shown =
      cyclic.has(name) ||
      (field.show_if ?? []).every(
        (c) => active(c.field) && c.values.some((v) => valueKey(v) === effective(c.field)),
      );
    memo.set(name, shown);
    return shown;
  };
  const inactive = new Set<string>();
  for (const f of fields) if (!active(f.name)) inactive.add(f.name);
  return inactive;
}

/**
 * A boolean parameter, recorded by the registration adapter as a two-choice `true`/`false` enum.
 * The field's `json_type` decides when present, so a `type: string` param whose choices happen to
 * be `"true"`/`"false"` stays a string; a form frozen before `json_type` was recorded falls back to
 * the enum's shape (`validate::is_boolean_choice`).
 */
export function isBooleanChoice(field: FormField): boolean {
  const v = field.enum_values;
  const shaped = !!v && v.length === 2 && v.includes('true') && v.includes('false');
  return shaped && (field.json_type == null || field.json_type === 'boolean');
}

/**
 * Whether a number field takes whole numbers only: its `json_type` is `integer`. `step` is a hint
 * for the input's increments, so it neither lifts nor adds the check; a form frozen before
 * `json_type` was recorded falls back to the integer marker `step: 1` (`validate::is_integer`).
 */
function isInteger(field: FormField): boolean {
  return field.json_type == null ? field.step === 1 : field.json_type === 'integer';
}

// Rust's `str::parse::<f64>` grammar: decimal digits with an optional fraction and exponent, or
// inf/infinity/nan in any case; never hex, binary, octal or `_` separators (which `Number` takes).
const DECIMAL = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;
const NON_FINITE = /^([+-])?(inf|infinity|nan)$/i;

/** Text as Rust's `str::parse::<f64>` reads it: may be ±Infinity or NaN, which `checkValue` rejects. */
function parseF64(text: string): number | null {
  const t = rustTrim(text);
  if (DECIMAL.test(t)) return Number(t);
  const m = NON_FINITE.exec(t);
  if (!m) return null;
  if (m[2].toLowerCase() === 'nan') return NaN;
  return m[1] === '-' ? -Infinity : Infinity;
}

/**
 * Text as a finite decimal number, else `null`: the numbers the server reads from text that JSON can
 * also carry (not `0x10`, `1e999` or `inf`). For turning a number field's text into a param.
 */
export function parseDecimal(text: string): number | null {
  const n = parseF64(text);
  return n !== null && Number.isFinite(n) ? n : null;
}

function asNumber(v: unknown): number | null {
  if (typeof v === 'number') return v;
  if (typeof v === 'string') return parseF64(v);
  return null;
}

// -- Patterns: the subset JS and the Rust `regex` crate read alike --------------------------------

// Unicode general categories: named alike, and holding the same ASCII, in both engines.
const CATEGORIES = new Set(
  'L Lu Ll Lt Lm Lo M Mn Mc Me N Nd Nl No P Pc Pd Ps Pe Pi Pf Po S Sm Sc Sk So Z Zs Zl Zp Cc Cf'.split(
    ' ',
  ),
);

// The counted repetition the port enforces, in expanded atoms. Past it Rust may refuse the pattern
// for its compiled size (`\W{1,100}\W{1,100}\W{1,100}`), and so skip the rule.
const MAX_EXPANSION = 100;

/**
 * Whether a `rules` pattern, compiled with the `u` flag and run on ASCII text, means what it means
 * to the server's Rust `regex` (`Regex::is_match`: an unanchored search, like `RegExp.test`). With
 * `u`, `\A`, `\z`, `\pL`, `\x{..}` and `[[:alpha:]]` fail to compile; this catches the rest:
 *
 * - JS only (Rust refuses the pattern, so skips the rule): lookaround, backreferences, `\0`, `\c`,
 *   `[\b]`, `[]` and `[^]`, a group name Rust refuses, and big counted repetition;
 * - Rust only, compiling in JS to something else: flags (`(?i)`), a nested class or a set operation
 *   (`&&`, `--`, `~~`) in a class, and a `\p{..}` that isn't a general category.
 */
export function sharedPattern(pattern: string): boolean {
  const frames: number[] = [0]; // expanded atoms so far, per open group
  let last = 0; // the expanded atoms of the last atom or group, for a quantifier after it
  let expansion = 0;
  const atom = (weight: number) => {
    frames[frames.length - 1] += weight;
    last = weight;
  };
  // The length of the escape at `i` (just past its backslash), or 0 when it is outside the subset.
  const escape = (i: number, inClass: boolean): number => {
    const c = pattern[i];
    if (c === undefined || /[0-9ck]/.test(c) || (inClass && (c === 'b' || c === 'B'))) return 0;
    if (c === 'p' || c === 'P') {
      const m = /^\{([A-Za-z]+)\}/.exec(pattern.slice(i + 1));
      return m && CATEGORIES.has(m[1]) ? 1 + m[0].length : 0;
    }
    if (c === 'u' && pattern[i + 1] === '{') {
      const end = pattern.indexOf('}', i);
      return end < 0 ? 0 : end - i + 1;
    }
    if (c === 'u') return 5;
    if (c === 'x') return 3;
    return 1;
  };
  let i = 0;
  while (i < pattern.length) {
    const c = pattern[i];
    if (c === '\\') {
      const n = escape(i + 1, false);
      if (n === 0) return false;
      atom(1);
      i += 1 + n;
    } else if (c === '[') {
      let j = i + 1;
      if (pattern[j] === '^') j++;
      if (pattern[j] === ']') return false; // `[]`, `[^]`: Rust reads the `]` as a member
      while (pattern[j] !== ']') {
        const d = pattern[j];
        if (d === undefined || d === '[') return false;
        if ((d === '&' || d === '-' || d === '~') && pattern[j + 1] === d) return false;
        if (d === '\\') {
          const n = escape(j + 1, true);
          if (n === 0) return false;
          j += 1 + n;
        } else j++;
      }
      atom(1);
      i = j + 1;
    } else if (c === '(') {
      if (pattern[i + 1] === '?') {
        const named = /^\(\?<[A-Za-z_][A-Za-z0-9_]*>/.exec(pattern.slice(i));
        if (pattern[i + 2] === ':') i += 3;
        else if (named) i += named[0].length;
        else return false;
      } else i++;
      frames.push(0);
    } else if (c === ')') {
      atom(frames.length > 1 ? frames.pop()! : 0);
      i++;
    } else if (c === '{') {
      const m = /^\{(\d+)(?:,(\d*))?\}/.exec(pattern.slice(i));
      if (!m) return true; // not a quantifier, so `u` refuses the pattern
      const bound = Math.max(Number(m[1]), Number(m[2] || 0));
      expansion += last * bound;
      if (expansion > MAX_EXPANSION) return false;
      frames[frames.length - 1] += last * Math.max(bound - 1, 0);
      i += m[0].length;
    } else {
      if (!'|*+?^$'.includes(c)) atom(1);
      i++;
    }
  }
  return true;
}

// Text both engines read alike: ASCII, where `\d`, `\w`, `\s` and `\b` agree, except `\r`, which a
// Rust `.` matches and a JS one doesn't.
const SHARED_TEXT = /^[\x00-\x0c\x0e-\x7f]*$/;

/** A rule's pattern compiled for the port, or `null` when the port leaves the rule to the server. */
function ruleRegExp(pattern: string): RegExp | null {
  if (!sharedPattern(pattern)) return null;
  try {
    return new RegExp(pattern, 'u');
  } catch {
    return null; // an author pattern that does not compile is not the submitter's fault
  }
}

const fmtNum = (n: number) => valueKey(n);

function checkValue(field: FormField, v: unknown): string | null {
  if (field.widget === 'number') {
    const n = asNumber(v);
    // `nan` and `inf` read as numbers, but no range check can bound them.
    if (n === null || !Number.isFinite(n)) return 'must be a number';
    if (isInteger(field) && !Number.isInteger(n)) return 'must be a whole number';
    const lo = field.min ?? null;
    const hi = field.max ?? null;
    if (lo !== null && hi !== null && (n < lo || n > hi))
      return `must be between ${fmtNum(lo)} and ${fmtNum(hi)}`;
    if (lo !== null && hi === null && n < lo) return `must be at least ${fmtNum(lo)}`;
    if (lo === null && hi !== null && n > hi) return `must be at most ${fmtNum(hi)}`;
    const xlo = field.exclusive_min ?? null;
    const xhi = field.exclusive_max ?? null;
    if (xlo !== null && n <= xlo) return `must be greater than ${fmtNum(xlo)}`;
    if (xhi !== null && n >= xhi) return `must be less than ${fmtNum(xhi)}`;
    const m = field.multiple_of ?? null;
    if (m !== null && m > 0) {
      // Within a relative epsilon, so `0.3` is a multiple of `0.1` despite binary rounding.
      const q = n / m;
      if (Math.abs(q - Math.round(q)) > 1e-9 * Math.max(Math.abs(q), 1))
        return `must be a multiple of ${fmtNum(m)}`;
    }
  }
  const rules = field.rules ?? [];
  if (rules.length === 0) return null;
  // A pattern is matched against a scalar as the server compares it (`8` as `8`); a list or an
  // object is never the text a pattern describes.
  if (typeof v !== 'string' && typeof v !== 'number' && typeof v !== 'boolean')
    return 'must be text';
  const text = valueKey(v);
  if (!SHARED_TEXT.test(text)) return null;
  for (const rule of rules) {
    const re = ruleRegExp(rule.pattern);
    if (re && !re.test(text)) return rule.message;
  }
  return null;
}

/**
 * A path input's value: one string path, or (for a `multiple` field) a non-empty list of them, each
 * checked against the field's rules (`validate::check_path`).
 */
function checkPath(field: FormField, v: unknown): string | null {
  if (typeof v === 'string') return checkValue(field, v);
  if (Array.isArray(v) && field.multiple) {
    if (v.length === 0) return 'must list at least one path';
    if (!v.every((item) => typeof item === 'string')) return 'must be a list of string paths';
    for (const [i, item] of v.entries()) {
      const message = checkValue(field, item);
      if (message) return `path ${i + 1}: ${message}`;
    }
    return null;
  }
  return 'must be a string path';
}

/** Every error in `params`, in field order, over the active fields (`validate::validate_params`). */
export function validateParams(
  fields: readonly FormField[],
  params: Record<string, unknown>,
): ValidationError[] {
  const errors: ValidationError[] = [];
  const inactive = inactiveFields(fields, params);
  for (const field of fields) {
    if (inactive.has(field.name)) continue;
    const v = params[field.name];
    const push = (message: string) => errors.push({ field: field.name, message });
    if (isUnset(v)) {
      // The engine runs an omitted param with its declared default, so a default satisfies
      // `required`.
      if (field.required && isUnset(field.default)) push('required');
      continue;
    }
    // A blank string gives a required param no value. With a default it is left to the checks
    // below, like any other value (`validate::validate_params`).
    if (typeof v === 'string' && isBlank(v) && field.required && isUnset(field.default)) {
      push('required');
      continue;
    }
    if (isBooleanChoice(field)) {
      if (!(typeof v === 'boolean' || v === 'true' || v === 'false')) push('must be true or false');
    } else if (field.enum_values) {
      // Compared by `valueKey`, so an integer choice `2` matches `2` and `"2"`; a list or an object
      // is never one of the choices.
      if (!(typeof v !== 'object' && field.enum_values.includes(valueKey(v))))
        push(`not one of ${field.enum_values.join(', ')}`);
    } else {
      const message = field.is_path_input ? checkPath(field, v) : checkValue(field, v);
      if (message) push(message);
    }
  }
  return errors;
}
