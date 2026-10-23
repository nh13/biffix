// Uploads: the routes and wire shapes of the upload engine's transport (`upload/transport.ts`).

import type { components } from '#lib/generated/api.js';
import { API_PATHS, pathFor } from './client';

/** Upload routes, filled: the transport builds its requests from these. Every one after `start`
 * takes the object key in its POST body, so no file name reaches a request URL; every one takes the
 * upload's project there too (see `projectField`), absent for the default project. */
export const uploadPaths = {
  start: API_PATHS.uploads,
  status: (uploadId: string) => pathFor(API_PATHS.uploadStatus, { upload_id: uploadId }),
  abort: (uploadId: string) => pathFor(API_PATHS.uploadAbort, { upload_id: uploadId }),
  parts: (uploadId: string) => pathFor(API_PATHS.uploadParts, { upload_id: uploadId }),
  complete: (uploadId: string) => pathFor(API_PATHS.uploadComplete, { upload_id: uploadId }),
};

/** The upload API's wire shapes (generated, so a contract change fails the type check). */
export type StartedUpload = components['schemas']['StartedUpload'];
export type UploadPartUrlList = components['schemas']['UploadPartUrlList'];
export type UploadStatus = components['schemas']['UploadStatus'];
export type UploadedFile = components['schemas']['UploadedFile'];
