// A run's QC data, as its MultiQC report recorded it.

import { API_PATHS, get, getOrNull, pathFor, type JsonBody } from './client';

export type QcGeneralStats = JsonBody<'qc_general_stats'>;

/** The run's MultiQC General Statistics as data (404 when it has no MultiQC report). */
export function getQcGeneralStats(runId: string): Promise<QcGeneralStats> {
  return get<QcGeneralStats>(pathFor(API_PATHS.qcGeneralStats, { run_id: runId }));
}

export type FastqcStatusReport = JsonBody<'qc_fastqc_status'>;
export type FastqcStatusSample = FastqcStatusReport['samples'][number];

/**
 * Each FastQC result's module statuses and basic statistics as the run's MultiQC data recorded them
 * (for a pipeline that publishes MultiQC's output but not FastQC's zips), each with the run's sample
 * that owns it; `null` when the run has no MultiQC report or it has no FastQC data (404).
 */
export function getQcFastqcStatus(runId: string): Promise<FastqcStatusReport | null> {
  return getOrNull<FastqcStatusReport>(pathFor(API_PATHS.qcFastqcStatus, { run_id: runId }));
}
