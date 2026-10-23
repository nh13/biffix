import { describe, expect, it } from 'vitest';
import { radioKeyTarget } from './radioGroup';

describe('radioKeyTarget', () => {
  it('steps round with the arrows', () => {
    expect(radioKeyTarget('ArrowRight', 3, 4)).toBe(0);
    expect(radioKeyTarget('ArrowDown', 1, 4)).toBe(2);
    expect(radioKeyTarget('ArrowLeft', 0, 4)).toBe(3);
    expect(radioKeyTarget('ArrowUp', 2, 4)).toBe(1);
  });

  it('jumps to either end with Home and End, and ignores other keys', () => {
    expect(radioKeyTarget('Home', 2, 4)).toBe(0);
    expect(radioKeyTarget('End', 0, 4)).toBe(3);
    expect(radioKeyTarget('a', 0, 4)).toBeNull();
  });
});
