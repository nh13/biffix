// A radio group as one Tab stop, the arrow keys (and Home / End) moving the choice: the Jobs view's
// state filters and the Outputs tab's tree/flat switch. The key mapping is pure, so it is
// unit-tested.

/**
 * Where `key` moves along `n` radios from `at`: the arrows step round (right/down forward, left/up
 * back), Home and End go to either end; null for any other key.
 */
export function radioKeyTarget(key: string, at: number, n: number): number | null {
  switch (key) {
    case 'ArrowRight':
    case 'ArrowDown':
      return (at + 1) % n;
    case 'ArrowLeft':
    case 'ArrowUp':
      return (at + n - 1) % n;
    case 'Home':
      return 0;
    case 'End':
      return n - 1;
    default:
      return null;
  }
}

/** Focus radio `to` of the group whose radio `e` was pressed on. */
export function focusRadio(e: KeyboardEvent, to: number): void {
  const group = (e.currentTarget as HTMLElement).parentElement;
  group?.querySelectorAll<HTMLElement>('[role="radio"]')[to]?.focus();
}
