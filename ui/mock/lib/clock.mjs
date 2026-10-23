// The mock's clock: the fixtures' times are relative to when it started (`now`, epoch seconds).
export const now = Math.floor(Date.now() / 1000);
/** The wall clock, in epoch seconds: what a write stamps, and what a live run's times move with
 * (`keepLiveRunsCurrent`), so anything measured against a live run reads it too. */
export const wallClock = () => Math.floor(Date.now() / 1000);
/** An hour, in seconds. */
export const h = 3600;
