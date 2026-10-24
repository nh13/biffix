// Copying text to the clipboard, which can fail (no permission, an insecure context, an old
// browser): one definition, so every copy button handles that the same way.

import { debounce } from '#lib/util/debounce.js';

/** Put `text` on the clipboard; `false` when the browser refused. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/**
 * Which copy button last succeeded, for a moment after (its "copied" label). One per component; the
 * key tells its buttons apart (it defaults to the copied text).
 */
export class CopiedFlag {
  key = $state<string | null>(null);
  #clear = debounce(() => (this.key = null), 1500);

  async copy(text: string, key: string = text): Promise<boolean> {
    const ok = await copyText(text);
    if (ok) {
      this.key = key;
      this.#clear();
    }
    return ok;
  }

  /** Whether the button keyed `key` copied just now. */
  is(key: string): boolean {
    return this.key === key;
  }
}
