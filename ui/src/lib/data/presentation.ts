// How the Data browser (`DataBrowser.svelte`) presents a place, per view: every word it says that
// differs between the views (the Lab view's plain ones, the Expert view's terms), the words of what
// only one view adds, and the folder trail. Both views browse the same way (listing, paging,
// search, the open file in the URL, the viewer, downloads); only what is here differs. Pure, so it
// is unit-tested.

import type { DataSource } from '$lib/api';
import { plural } from '$lib/util/format';
import type { ViewMode } from '$lib/view/viewRoutes';
import { expertTrail, labTrail, type TrailStep } from './dataBrowser';
import { sourceKindLabel } from './labPresentation';

/**
 * The words of the analyses behind the project's results folders (a Lab addition): an analysis
 * named (folders, trail, "Result of"), the *Analysis results* shortcut at the project's top, and a
 * BAM/CRAM or FastQC report opened as its analysis's result.
 */
export interface AnalysesWords {
  /** Before the analysis a folder or file is a result of. */
  resultOf: string;
  viewAlignments: string;
  openFastqc: string;
  /** Under the runs' folder. */
  runsFolderNote: string;
  /** Why file `name` can't be opened as its analysis's result. */
  cannotOpen: Record<'notRecorded' | 'notFound', (name: string) => string>;
}

/** The words of uploads (a Lab addition): *Upload files* and dropping files, where they go. */
export interface UploadWords {
  button: string;
  /** While a batch of `files` files is going to `folder`. */
  uploading: (files: number, folder: string) => string;
  /** Before the (linked) folder a finished batch went to. */
  uploadedTo: string;
  /** A batch none of whose files went. */
  stopped: (folder: string) => string;
  /** A batch with nothing moving, some of it paused or failed (Resume or Retry may finish it). */
  held: (folder: string) => string;
}

export interface DataPresentation {
  /** The view, which also names the few styles that differ (`.lab`, `.expert`). */
  view: ViewMode;
  /** Under the heading: what the page holds. */
  lede: string;
  sourcesError: (error: string) => string;
  /** No source at all. */
  noSources: string;
  /** Only the project's own storage (no data folders connected). */
  noDataFolders: string;
  /** A source's tooltip in the list of sources. */
  sourceTitle: (s: DataSource) => string | undefined;
  /** What a source is, beside its name (nothing for a plain one). */
  sourceKind: (s: DataSource) => string | null;
  /** For a source the URL names that the project lacks. */
  unknownSource: string;
  /** That note's *Details* disclosure, naming the source asked for (`null`: none). */
  unknownSourceDetails: ((sourceId: string) => string) | null;
  /** The trail to a folder (`nameOf` names an analysis by its id). */
  trail: (
    source: Pick<DataSource, 'name' | 'hidden_prefixes' | 'browsable_exceptions'>,
    path: string,
    nameOf: (runId: string) => string | null,
  ) => TrailStep[];
  /** A *Copy path* button for the folder on show, after its trail. */
  copyFolderPath: boolean;
  searchButton: string;
  searchSummary: (matches: number, truncated: boolean) => string;
  retry: string;
  changedColumn: string;
  /** An empty folder or search (`canUpload`: the folder takes uploads). */
  empty: (searching: boolean, canUpload: boolean) => string;
  /** The analyses, named and opened as their results (`null`: shown as plain folders). */
  analyses: AnalysesWords | null;
  /** Uploads, where the folder takes them (`null`: none). */
  uploads: UploadWords | null;
}

const matches = (n: number) => plural(n, 'match', 'matches');

/** The Lab view: plain words, analyses by name, uploads. */
export const labPresentation: DataPresentation = {
  view: 'lab',
  lede: 'Your sequencing data, reference genomes and analysis results. Open a file to look at it, or download it.',
  sourcesError: (e) => `Couldn't load your data: ${e}`,
  noSources: "Nothing is connected yet. Ask your admin to connect your sequencing core's folder.",
  noDataFolders:
    "Only your project's own files are here. Ask your admin to connect your sequencing core's folder.",
  sourceTitle: (s) => s.description ?? undefined,
  sourceKind: (s) => sourceKindLabel(s.kind),
  unknownSource:
    "This folder doesn't exist, or it is in a project you can't open. Pick one of the places listed here.",
  unknownSourceDetails: null,
  trail: labTrail,
  copyFolderPath: false,
  searchButton: 'Search inside folders',
  searchSummary: (n, truncated) =>
    `${matches(n)} in this folder and the folders inside it${truncated ? ' (stopped early: try a longer name)' : ''}.`,
  retry: 'Try again',
  changedColumn: 'Changed',
  empty: (searching, canUpload) =>
    `${searching ? 'Nothing matches.' : 'This folder is empty.'}${canUpload ? ' Upload files to add some.' : ''}`,
  analyses: {
    resultOf: 'Result of',
    viewAlignments: 'View alignments',
    openFastqc: 'Open FastQC',
    runsFolderNote: "Each folder holds one analysis's results.",
    cannotOpen: {
      notRecorded: (name) =>
        `${name} can't be opened here: its analysis didn't record it as one of its results. You can still download it.`,
      notFound: (name) =>
        `${name} can't be opened here: biffix couldn't find it among this analysis's results. You can still download it.`,
    },
  },
  uploads: {
    button: 'Upload files',
    uploading: (n, folder) =>
      `Uploading ${plural(n, 'file')} to ${folder}: they keep going while you work.`,
    uploadedTo: 'Uploaded to',
    stopped: (folder) => `The upload to ${folder} stopped.`,
    held: (folder) =>
      `Some files for ${folder} are paused or stopped: resume or retry them in Uploads.`,
  },
};

/** The Expert view: the bucket's own terms and paths. */
export const expertPresentation: DataPresentation = {
  view: 'expert',
  lede: "The project's bucket and the data sources your admins registered. Open a table, text file, image, PDF or HTML report in place, or download it.",
  sourcesError: (e) => `Couldn't load data sources: ${e}`,
  noSources:
    'No data sources are connected, not even the project bucket — an admin sets them in the deploy config.',
  noDataFolders: 'No data folders are connected — an admin adds them in the deploy config.',
  sourceTitle: (s) => s.uri,
  sourceKind: (s) => (s.kind === 'references' ? 'references' : null),
  unknownSource:
    "This folder doesn't exist, or it is in a project you don't have access to. Pick one of the folders listed here.",
  unknownSourceDetails: (id) => `There is no data source “${id}”.`,
  trail: expertTrail,
  copyFolderPath: true,
  searchButton: 'Search subfolders',
  searchSummary: (n, truncated) =>
    `${matches(n)} under this folder${truncated ? ' (stopped early — narrow the search)' : ''}.`,
  retry: 'Retry',
  changedColumn: 'Modified',
  empty: () => 'Nothing here.',
  analyses: null,
  uploads: null,
};

/** The presentation for `view`. */
export function presentationFor(view: ViewMode): DataPresentation {
  return view === 'lab' ? labPresentation : expertPresentation;
}
