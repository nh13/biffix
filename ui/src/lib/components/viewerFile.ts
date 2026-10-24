// A file the viewer can show, wherever it lives: a run's output, or a file in a data source. The
// viewer only needs its facts and how to fetch a preview, an inline URL, or a download.

import {
  getOutputDownload,
  getOutputPreview,
  getOutputViewUrl,
  type OutputPreview,
  type OutputUrl,
  type RunOutput,
} from '#lib/api/index.js';
import { errorMessage } from '#lib/util/format.js';

export interface ViewerFile {
  /** Identifies the file: a new key reloads what is shown. */
  key: string;
  name: string;
  /** Where it is, shown under the name. */
  path: string;
  size?: number | null;
  /** How to show it: `table`, `text`, `image`, `pdf` or `html`. */
  viewer?: string | null;
  /** More about it, shown after the size (a run output's task). */
  detail?: string | null;
  preview(): Promise<OutputPreview>;
  viewUrl(): Promise<OutputUrl>;
  download(): void;
}

/**
 * A run's output as the viewer shows it (the Outputs tab, and the Reports tab under the report's
 * own name). Downloading navigates to a short-lived signed URL whose attachment disposition saves
 * the file in place; `onDownloadError` hears `null` as each download starts and a message when
 * one fails.
 */
export function outputViewerFile(
  runId: string,
  output: RunOutput,
  opts: {
    name?: string;
    detail?: string | null;
    onDownloadError: (message: string | null) => void;
  },
): ViewerFile {
  return {
    key: output.id,
    name: opts.name ?? output.name,
    path: output.path,
    size: output.size,
    viewer: output.viewer,
    detail: opts.detail ?? null,
    preview: () => getOutputPreview(runId, output.id),
    viewUrl: () => getOutputViewUrl(runId, output.id),
    download: async () => {
      opts.onDownloadError(null);
      try {
        window.location.assign((await getOutputDownload(runId, output.id)).url);
      } catch (e) {
        const message = errorMessage(e);
        opts.onDownloadError(`Couldn't download ${output.name}: ${message}`);
      }
    },
  };
}
