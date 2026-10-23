/** How long an object URL outlives its click: revoked at once, some browsers drop the save. */
export const REVOKE_AFTER_MS = 40_000;

/** Hand the viewer `blob` as a file named `filename` (an object URL clicked through a detached
 * link, revoked once the save has had time to start). */
export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), REVOKE_AFTER_MS);
}
