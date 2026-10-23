// Safe access to this browser's localStorage, for the preferences and remembered choices kept there.

/** A stored preference; `null` when there is none, or storage can't be read. */
export function readPref(key: string): string | null {
  // Storage can throw (private windows, blocked site data).
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** Store a preference. When storage refuses, it still applies, for this page load only. */
export function writePref(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not persisted.
  }
}

/** Forget a preference (best effort, like `writePref`). */
export function removePref(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // Not persisted.
  }
}
