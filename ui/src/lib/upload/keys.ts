// Where a browser upload goes in the project bucket: the default folder, the key a file lands at
// (or why it cannot), and another name when that key is taken. Pure, so it is unit-tested.

import { localDate } from '$lib/util/date';

/** Browser uploads go under this folder of the project bucket (the backend allows no other). */
export const UPLOADS_FOLDER = 'uploads/';

/**
 * Where an upload lands by default: the folder being browsed when it is under `uploads/`, else a
 * folder for the (local) day (`uploads/2026-09-24/`).
 */
export function defaultUploadFolder(path: string, today: Date): string {
  if (path.startsWith(UPLOADS_FOLDER)) return path;
  return `${UPLOADS_FOLDER}${localDate(today)}/`;
}

/** A control character, as Rust's `char::is_control` reads one (C0, DEL and C1). */
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/**
 * The key a file uploads to in `folder`, or why it cannot: the folder must be under `uploads/`, a
 * plain path with no `.` or `..` parts, the file name must be non-empty, there must be no control
 * character, and the key is at most 1024 bytes (the backend's `check_key`).
 */
export function uploadKey(folder: string, name: string): { key: string } | { error: string } {
  const dir = folder.trim().replace(/^\/+/, '');
  const normalized = dir.endsWith('/') ? dir : `${dir}/`;
  if (!normalized.startsWith(UPLOADS_FOLDER))
    return { error: `Uploads go under ${UPLOADS_FOLDER}` };
  const parts = normalized.slice(0, -1).split('/');
  if (parts.some((p) => p === '' || p === '.' || p === '..'))
    return { error: 'The folder has an empty, "." or ".." part' };
  if (CONTROL.test(normalized)) return { error: 'The folder has a control character' };
  if (!name) return { error: 'A file with no name cannot be uploaded' };
  if (CONTROL.test(name))
    return { error: `The file name ${JSON.stringify(name)} has a control character` };
  const key = `${normalized}${name}`;
  if (new TextEncoder().encode(key).length > 1024)
    return { error: 'The file name and folder are too long (at most 1024 bytes)' };
  return { key };
}

/**
 * Another name for an upload whose key is taken (uploads never replace an object): `-2` (or
 * `-<n>`) before the file's extensions, so a compound one stays whole —
 * `uploads/x/P01_R1.fastq.gz` → `uploads/x/P01_R1-2.fastq.gz`. A leading dot is part of the name.
 */
export function renamedKey(key: string, n = 2): string {
  const slash = key.lastIndexOf('/');
  const folder = key.slice(0, slash + 1);
  const name = key.slice(slash + 1);
  const dot = name.indexOf('.', 1);
  const [stem, ext] = dot < 0 ? [name, ''] : [name.slice(0, dot), name.slice(dot)];
  return `${folder}${stem}-${n}${ext}`;
}
