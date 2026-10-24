import { describe, expect, it } from 'vitest';
import cases from '../../../../schemas/validate-cases.json';
import { inactiveFields, parseDecimal, sharedPattern, validateParams, valueKey } from './validate';
import type { FormField } from '#lib/workflows/schema.js';

interface Case {
  name: string;
  fields: FormField[];
  params: Record<string, unknown>;
  inactive: string[];
  errors: [string, string][];
  /** Where the port deliberately reports less (a rule it leaves to the server): what it reports. */
  ui_errors?: [string, string][];
}

// The Rust validator's own cases (emitted by `emit_schemas`): the port must agree on every one.
describe('the UI validator agrees with the Rust validator', () => {
  for (const c of cases as unknown as Case[]) {
    it(c.name, () => {
      expect([...inactiveFields(c.fields, c.params)].sort()).toEqual(c.inactive);
      expect(validateParams(c.fields, c.params).map((e) => [e.field, e.message])).toEqual(
        c.ui_errors ?? c.errors,
      );
    });
  }
});

describe('valueKey', () => {
  it('equates the spellings the Rust value_key equates', () => {
    expect(valueKey(true)).toBe(valueKey('true'));
    expect(valueKey(3)).toBe('3');
    expect(valueKey(3.0)).toBe('3');
    expect(valueKey(2.5)).toBe('2.5');
    expect(valueKey(null)).toBe('');
  });
});

describe('parseDecimal', () => {
  it('reads what the server reads as a finite number, and nothing else', () => {
    expect(parseDecimal(' 8 ')).toBe(8);
    expect(parseDecimal('-2.5')).toBe(-2.5);
    expect(parseDecimal('+.5')).toBe(0.5);
    expect(parseDecimal('3.')).toBe(3);
    expect(parseDecimal('1e3')).toBe(1000);
    for (const text of ['', ' ', '0x10', '0b1', '0o7', '1_000', '1e999', 'inf', 'NaN', '1e', '.'])
      expect(parseDecimal(text), text).toBeNull();
  });
  it("trims as Rust's str::trim does (White_Space), not as JavaScript's trim", () => {
    // U+0085 is White_Space (JavaScript keeps it); U+FEFF is not (JavaScript strips it).
    expect(parseDecimal('5\u0085')).toBe(5);
    expect(parseDecimal('﻿5')).toBeNull();
  });
});

describe('sharedPattern', () => {
  it('keeps the patterns both engines read alike', () => {
    for (const p of [
      '^[ACGT]+$',
      '^\\S+\\.fn?a(sta)?(\\.gz)?$',
      '^\\p{L}+$',
      '^(?:[a-z]+|\\d{1,3})$',
      '^(?<id>[a-z_]\\w*)$',
      '^[\\w.-]+$',
      '^\\u0041\\x42$',
      '^\\W{1,100}$',
    ])
      expect(sharedPattern(p), p).toBe(true);
  });

  it('leaves the rest to the server', () => {
    for (const p of [
      '(?i)^abc$', // inline flags
      '^[[:upper:]]+$', // a POSIX class
      '^[a-z&&[^aeiou]]+$', // class intersection
      '^[a-z--b]+$', // class difference
      '^[a-z~~b]+$', // symmetric difference
      '^\\p{Greek}+$', // a script, not a general category
      '^(?!tmp)', // lookahead
      '^(?<=a)b', // lookbehind
      '^(a)\\1$', // a backreference
      '^\\k<n>(?<n>a)$',
      '^\\0$',
      '^\\cA$',
      '^[\\b]$',
      '^[^]$',
      '^[]a]$',
      '^(?<a$>x)$',
      '^\\W{1,100}\\W{1,100}\\W{1,100}$', // too big for Rust
      '^(\\w{1,20}){1,20}$',
    ])
      expect(sharedPattern(p), p).toBe(false);
  });
});
