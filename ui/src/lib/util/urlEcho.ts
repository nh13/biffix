/**
 * A search box that reports its text to the URL (trimmed, after a pause) and also follows the URL
 * (back/forward, a link) must not take its own report back: the echo lands a round trip later, so
 * copying it into the box would drop a trailing space, or keystrokes typed meanwhile (and the next
 * report would then send the rolled-back text). This remembers the reports still in flight so the
 * matching URL change can be told apart from any other.
 */
export class UrlEcho {
  #pending: string[] = [];

  /** `q` (trimmed) was just reported while the URL holds `current`: its change is on the way. */
  reported(q: string, current: string): void {
    // No change, so no echo comes back.
    if (q !== current) this.#pending.push(q);
  }

  /**
   * The URL's value changed to `q`: true when that is the echo of a report (consumed, along with any
   * earlier report it overtook), false for a change made elsewhere, which the box should take.
   */
  isEcho(q: string): boolean {
    const at = this.#pending.indexOf(q);
    if (at >= 0) {
      this.#pending = this.#pending.slice(at + 1);
      return true;
    }
    this.#pending = [];
    return false;
  }
}
